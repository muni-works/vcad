#!/usr/bin/env python3
"""Compose the narrated plasmoid explainer (see script.md).

The narration (narration.py, voiced and word-timed by voice.py) drives the
cut: each spoken line owns one shot, captions pop in on the words as they
are said, and diagram cues fire on named words. The soundtrack plays from
its start underneath, ducked by the voice.

    EXPLAINER_SCALE=1.5 python3 make_video.py --raw <clip frames> \
        --traj <trajectory.csv> --assets <dir> --vo <voice.py out dir> \
        [--track track.mp3] -o explainer.mp4 [--only LINE_ID] [--jobs 8]

`--raw`/`--traj` come from `vcad-render --sim plasmoid` (docs/plasmoid-sim.md);
`--assets` holds beta-map.png, lock_compare.csv and the footage frame dirs
(script.md). EXPLAINER_SCALE=1 gives 720x1280, 1.5 gives 1080x1920. Every
frame is a pure function of t.
"""
import argparse
import csv
import json
import math
import os
import subprocess
import sys
import wave
from bisect import bisect_left
from multiprocessing import Pool

import numpy as np
from PIL import Image, ImageFilter

import style
from diagrams import DIAGRAMS
from narration import CAPTIONS, LINES
from style import F, FS, H, LABEL, M, SEL, TEXT, W, FPS, tracked

BEAT = 0.5                    # the track is 120 BPM
T_LOCK = 5.0 + 28 / 30        # clip frame 178: the foot locks (model: 5.93 s)
T_FLICK = 14.0 + 1 / 30       # clip frame 421: the flick (model: break-away 14.04 s)
MUSIC_SS = 8.04               # the track starts on its 8 s downbeat, so that
T_DROP = 24.0                 # its big energy jump (32 s in) lands here: "…and it locks on"
GAP = 0.15                    # breath between lines, after the last spoken word
TAIL = 2.0                    # the end card holds this long after the last word

# line id → (kind, args, {cue name: word prefix})
SHOTS = {
    "sun1": ("footage", dict(src="sdo", from_line="sun1"), {}),
    "sun2": ("footage", dict(src="sdo", from_line="sun1"), {}),
    "jar1": ("footage", dict(src="xe_thread", from_line="jar1"), {}),
    "jar2": ("footage", dict(src="xe_hand", from_line="jar2"), {}),
    "dish1": ("clip", dict(tag="slowed: one lap = two beats"), {}),
    "dish2": ("clip", dict(tag="slowed: one lap = two beats"), {}),
    "pay1": ("clip", dict(), {}),
    "pay2": ("clip", dict(), {}),
    "pay3": ("clip", dict(), {}),
    "ask": ("card", dict(), {}),
    "def": ("labels", dict(), {}),
    "gas": ("diagram", dict(name="gas"), {"field": "radio", "ion": "slam"}),
    "rf": ("diagram", dict(name="rf"), {"flip": "but", "avg": "averages"}),
    "curl": ("diagram", dict(name="curl"), {"half": "half"}),
    "thin": ("diagram", dict(name="thin"), {"air": "normal"}),
    "arcs": ("footage", dict(src="sdo", from_line="arcs", offset=8.0), {}),
    "live": ("diagram", dict(name="live"), {"gain": "gain", "lose": "lose"}),
    "lever1": ("diagram", dict(name="lever1"), {"leak": "leak", "hot": "hotter"}),
    "lever2": ("diagram", dict(name="lever2"), {"crowd": "crowds"}),
    "hand": ("footage", dict(src="xe_hand", from_line="hand"), {}),
    "foot": ("diagram", dict(name="foot"), {"bars": "at"}),
}
BIG = {"sun1"}                # captions shown large, mid-frame
CREDIT = ["footage: Carl Willis (CC BY 3.0) · NASA/SDO · voice: ribt",
          "simulated and path-traced in vcad · open source"]

# Real footage, extracted to <assets>/<src>/%04d.jpg at 30 fps (script.md):
# source scaled by k, its top-left at logical (x, y) on black.
FOOTAGE = {
    "xe_thread": dict(k=1.4, x=-337, y=110, credit="real footage · Carl Willis · CC BY 3.0"),
    "xe_hand": dict(k=1.4, x=-332, y=110, credit="real footage · Carl Willis · CC BY 3.0"),
    "sdo": dict(k=1280 / 1080, x=-95, y=0, credit="real footage · NASA / SDO"),
}


# ── the timeline, from the voice ──────────────────────────────────────────
def word_time(timing, lid, prefix):
    for w, a, _ in timing[lid]["words"]:
        if w.lower().strip(".,:;?!…").startswith(prefix):
            return a
    raise KeyError(f"{lid}: no word starting {prefix!r}")


def spoken(timing, lid):
    """when the line's last word ends (the wavs carry ~0.5 s of trailing silence)."""
    return timing[lid]["words"][-1][2] + 0.1


def layout(timing):
    """start time of every line. The opening is spread so "locks" lands on
    T_DROP; the payoff lines are pinned to the clip's own events; the rest
    follow one another, starting on half-beats."""
    ids = [l for l, _ in LINES]
    start = {}
    opening = ids[:ids.index("dish2") + 1]
    natural = sum(spoken(timing, l) for l in opening[:-1]) + word_time(timing, "dish2", "locks")
    gap = (T_DROP - 0.5 - natural) / (len(opening) - 1)
    assert gap >= 0.2, f"the opening runs past the drop (gap {gap:.2f})"
    t = 0.5
    for l in opening:
        start[l] = t
        t += spoken(timing, l) + gap
    lock = start["dish2"] + word_time(timing, "dish2", "locks")
    start["pay1"] = lock + 2.0                                            # the drag starts at clip 8.0
    start["pay2"] = lock + (T_FLICK - T_LOCK) - word_time(timing, "pay2", "flick")
    start["pay3"] = max(lock + 11.6, start["pay2"] + spoken(timing, "pay2") + 0.2)
    prev = "pay3"
    for l in ids[ids.index("pay3") + 1:]:
        t = start[prev] + spoken(timing, prev) + GAP
        start[l] = math.ceil(t / BEAT - 1e-6) * BEAT
        prev = l
    for a, b in zip(ids, ids[1:]):
        assert start[b] >= start[a] + spoken(timing, a), f"{a} overlaps {b}"
    t_end = start["outro"]                    # the end card comes up as the sign-off starts
    total = start["outro"] + spoken(timing, "outro") + TAIL
    return ids, start, lock, t_end, total


def captions(timing, ids, start):
    """[(t0, t1, chunk text, line id)]: each chunk pops in on its first word."""
    out = []
    for l, text in LINES:
        words = timing[l]["words"]
        k = 0
        parts = [(text, len(words))] if l in BIG else CAPTIONS.get(l) or [
            (c, len(c.split())) for c in style.chunks(text)]
        for i, (part, n) in enumerate(parts):
            if k >= len(words):
                break
            t0 = start[l] + words[k][1]
            k += n
            t1 = start[l] + (words[k][1] if k < len(words) else timing[l]["dur"] + 0.35)
            out.append((t0 if i else start[l] + min(words[0][1], 0.05), t1, part, l))
    return out


# ── sources ───────────────────────────────────────────────────────────────
_frames = {}


def _load(raw_dir, n):
    n = min(max(n, 0), 599)
    if n not in _frames:
        if len(_frames) > 6:
            _frames.clear()
        _frames[n] = style.fit(Image.open(os.path.join(raw_dir, f"frame_{n:04d}.png")).convert("RGB"))
    return _frames[n]


def clip_frame(raw_dir, tc):
    """the clip at time tc; fractional frame times cross-fade (slow motion)."""
    f = tc * FPS
    i = int(math.floor(f + 1e-6))
    a = f - i
    if a < 0.03:
        return _load(raw_dir, i).copy()
    return Image.blend(_load(raw_dir, i), _load(raw_dir, i + 1), a)


def load_traj(path):
    rows = list(csv.DictReader(open(path)))
    ts = [float(r["t"]) for r in rows]
    ang = np.unwrap(np.arctan2([float(r["y"]) for r in rows], [float(r["x"]) for r in rows]))
    i0, i1 = bisect_left(ts, 0.2), bisect_left(ts, 3.8)
    period = (ts[i1] - ts[i0]) * 2 * math.pi / abs(ang[i1] - ang[i0])    # one free lap, s
    return ts, rows, period


def loop_time(t):
    """free racing, one lap per two beats, looping seamlessly over six laps."""
    period = TL["period"]
    return (t * period / (2 * BEAT)) % (6 * period)


def dish_frame(raw_dir, t):
    """the dish through the opening and the payoff: slowed to one lap per
    two beats until the lock, real time after it, then free racing."""
    lock, period = TL["lock"], TL["period"]
    if t < lock:
        tc = T_LOCK - (lock - t) * period / (2 * BEAT)
        while tc < 0:
            tc += 6 * period
        return clip_frame(raw_dir, tc)
    tc = T_LOCK + (t - lock)
    tc = T_LOCK + round((t - lock) * FPS) / FPS          # whole frames at 1×
    end = 19.9
    if tc <= end:
        return clip_frame(raw_dir, tc)
    a = min((tc - end) / 0.3, 1.0)                       # the clip is over: fade into free racing
    return Image.blend(clip_frame(raw_dir, end), clip_frame(raw_dir, loop_time(t)), a)


def footage_frame(assets, src, local):
    f = FOOTAGE[src]
    n = int(round(local * FPS)) + 1
    path = os.path.join(assets, src, f"{n:04d}.jpg")
    while not os.path.exists(path) and n > 1:            # hold the last frame
        n -= 1
        path = os.path.join(assets, src, f"{n:04d}.jpg")
    im = Image.open(path).convert("RGB")
    k = f["k"] * style.S
    im = im.resize((int(round(im.width * k)), int(round(im.height * k))), Image.LANCZOS)
    img = style.new("RGB", (0, 0, 0))
    img.paste(im, (int(round(f["x"] * style.S)), int(round(f["y"] * style.S))))
    return img


SPIKE_TIP = (360, 655)


def find_foot(img):
    """The foot is the brightest blob on the dish, away from the spike tip (logical px)."""
    g = img.convert("L").resize((W // 4, H // 4)).filter(ImageFilter.GaussianBlur(2))
    a = np.asarray(g, dtype=np.float32).copy()
    ys, xs = np.mgrid[0:a.shape[0], 0:a.shape[1]]
    a[(np.hypot(xs * 4 - SPIKE_TIP[0], ys * 4 - SPIKE_TIP[1]) < 45)] = 0
    a[:140, :] = 0
    a[230:, :] = 0
    y, x = np.unravel_index(int(np.argmax(a)), a.shape)
    return x * 4 + 2, y * 4 + 2


def draw_labels(img, t):
    """name the filament and the spike, then follow the foot."""
    d = style.draw(img)

    def label(p, lp, text, on, below=False):
        if not on:
            return
        d.line([p, lp], fill=SEL, width=2)
        d.text((lp[0], lp[1] + (6 if below else -6)), text, font=FS(28, True), fill=SEL,
               anchor="mt" if below else "mb")

    fx, fy = find_foot(img)
    mx, my = (SPIKE_TIP[0] + fx) / 2, (SPIKE_TIP[1] + fy) / 2 - 34
    label((mx, my - 6), (mx, 470), "filament", t >= 0.5)
    label((SPIKE_TIP[0] + 14, SPIKE_TIP[1] + 20), (SPIKE_TIP[0] + 130, 590), "spike", t >= 1.0)
    label((fx, fy + 16), (fx, fy + 120), "foot", t >= 1.5, below=True)
    return img


# ── one frame ─────────────────────────────────────────────────────────────
def frame(t, a):
    ids, start, t_end = TL["ids"], TL["start"], TL["t_end"]
    if t >= t_end:
        # the card's lines come up as the voice says them
        at = lambda w: t_end + word_time(TL["timing"], "outro", w)
        img = style.end_card(t, at("build"), at("build"), at("cad"), CREDIT)
        for t0, t1, part, l in TL["captions"]:
            if l == "outro" and t0 <= t < min(t1, at("build")):
                img = style.draw_chunk(img, part, t - t0, big=True)
        return img
    lid = ids[0]
    for l in ids:
        if start[l] <= t:
            lid = l
    kind, args, cue_words = SHOTS[lid]
    local = t - start[lid]
    label = "plasmoid · simulated in vcad"
    if kind == "footage":
        src = args["src"]
        img = footage_frame(a.assets, src, t - start[args["from_line"]] + args.get("offset", 0.0)
                            if t >= start[args["from_line"]] else 0.0)
        label = FOOTAGE[src]["credit"]
    elif kind == "clip":
        img = dish_frame(a.raw, t)
        if args.get("tag") and t < TL["lock"]:
            tracked(style.draw(img), (W - M, 70), args["tag"], F(16, True), LABEL, "ra")
    elif kind == "card":
        img = Image.blend(style.new("RGB", style.BGC), clip_frame(a.raw, loop_time(t)), 0.3)
    elif kind == "labels":
        img = draw_labels(clip_frame(a.raw, loop_time(t / 2)), local)
    else:
        nxt = ids[ids.index(lid) + 1] if lid != ids[-1] else None
        cues = {k: word_time(TL["timing"], lid, w) for k, w in cue_words.items()}
        cues["dur"] = start[nxt] - start[lid]
        img = DIAGRAMS[args["name"]](local, a.assets, cues)
    img = style.hud_frame(img, t, TL["total"], label)
    for t0, t1, part, l in TL["captions"]:
        if t0 <= t < t1:
            img = style.draw_chunk(img, part, t - t0, big=l in BIG or kind == "card")
            break
    return img


TL = None
ARGS = None


def build_timeline(a):
    timing = json.load(open(os.path.join(a.vo, "timing.json")))
    ids, start, lock, t_end, total = layout(timing)
    return dict(timing=timing, ids=ids, start=start, lock=lock, t_end=t_end, total=total,
                captions=captions(timing, ids, start), period=load_traj(a.traj)[2])


def _init(a):
    global TL, ARGS
    ARGS = a
    TL = build_timeline(a)


def _render(n):
    out = os.path.join(ARGS.frames, f"{n:05d}.png")
    frame(n / FPS, ARGS).save(out, compress_level=1)
    return n


def voice_track(a, tl, path):
    """every line at its start time, 24 kHz mono."""
    dur = tl["total"]
    sr = 24000
    buf = np.zeros(int(dur * sr) + sr, dtype=np.float32)
    for l in tl["ids"]:
        with wave.open(os.path.join(a.vo, f"{l}.wav")) as w:
            assert w.getframerate() == sr and w.getnchannels() == 1
            x = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768
        i = int(round(tl["start"][l] * sr))
        buf[i:i + len(x)] += x
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes((np.clip(buf, -1, 1) * 32767).astype(np.int16).tobytes())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--raw", required=True)
    ap.add_argument("--traj", required=True)
    ap.add_argument("--assets", required=True)
    ap.add_argument("--vo", required=True)
    ap.add_argument("--track")
    ap.add_argument("--frames", default="frames")
    ap.add_argument("-o", "--out", required=True)
    ap.add_argument("--only", help="render only this line's shot")
    ap.add_argument("--jobs", type=int, default=8)
    a = ap.parse_args()

    tl = build_timeline(a)
    dur = tl["total"]
    for l in tl["ids"]:
        print(f"{tl['start'][l]:7.2f}  {l:7s} {tl['timing'][l]['dur']:5.2f} s", file=sys.stderr)
    print(f"lock {tl['lock']:.2f} s, end card {tl['t_end']:.1f} s, total {dur:.1f} s", file=sys.stderr)

    os.makedirs(a.frames, exist_ok=True)
    t0, t1 = 0.0, dur
    if a.only:
        i = tl["ids"].index(a.only)
        t0 = tl["start"][a.only] if i else 0.0
        t1 = tl["start"][tl["ids"][i + 1]] if i + 1 < len(tl["ids"]) else tl["t_end"]
    ns = list(range(int(round(t0 * FPS)), int(round(t1 * FPS))))
    with Pool(a.jobs, initializer=_init, initargs=(a,)) as pool:
        for k, _ in enumerate(pool.imap(_render, ns, chunksize=8)):
            pass

    cmd = ["ffmpeg", "-loglevel", "error", "-y", "-framerate", str(FPS),
           "-start_number", str(ns[0]), "-i", os.path.join(a.frames, "%05d.png")]
    if not a.only:
        voice = os.path.join(a.frames, "voice.wav")
        voice_track(a, tl, voice)
        cmd += ["-i", voice]
        if a.track:
            # music dips only gently under the voice, fades at the end
            cmd += ["-ss", str(MUSIC_SS), "-i", a.track, "-filter_complex",
                    f"[1:a]volume=1.7,asplit=2[v][key];"
                    f"[2:a]volume=0.6,afade=t=out:st={dur - 2.5}:d=2.5[m];"
                    f"[m][key]sidechaincompress=threshold=0.05:ratio=2.5:attack=20:release=300[duck];"
                    f"[duck][v]amix=inputs=2:normalize=0,alimiter=limit=0.89:level=0[a]",
                    "-map", "0:v", "-map", "[a]"]
        cmd += ["-t", f"{dur}", "-c:a", "aac", "-b:a", "256k"]
    cmd += ["-frames:v", str(len(ns)), "-c:v", "libx264", "-crf", "17", "-preset", "slow",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart", a.out]
    subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
