#!/usr/bin/env python3
"""Compose the narrated fusor explainer (see script.md).

The narration (narration.py, voiced and word-timed by voice.py) drives the
cut: each spoken line owns a shot, captions pop in on the words as they are
said, and scene cues fire on named words. Every frame is a pure function of t.

    EXPLAINER_SCALE=1.5 python3 make_video.py --assets <dir> --vo <voice.py out dir> \
        [--track track.mp3] -o explainer.mp4 [--only LINE_ID] [--jobs 8]
    python3 make_video.py --assets <dir> --sheet storyboard.png   # contact sheet, no voice needed

`--assets` holds paths.csv, fates.csv, potential.csv (the
`fusor_explainer_paths` example) and hero.jpg / close.jpg (script.md).
Without `--vo`, word times are estimated from word lengths (for the sheet).
EXPLAINER_SCALE=1 gives 720x1280, 1.5 gives 1080x1920.
"""
import argparse
import json
import math
import os
import subprocess
import sys
import wave
from multiprocessing import Pool

import numpy as np
from PIL import Image

import style
from narration import CAPTIONS, LINES
from scenes import SCENES
from style import FPS, H, W

GAP = 0.3                     # breath between lines, after the last spoken word
TAIL = 2.0                    # the end card holds this long after the last word

# line id → (scene, the line the scene started on, {cue: word prefix | (line, prefix | None)})
SHOTS = {
    "open1": ("hero", None, {}),
    "open2": ("close", None, {}),
    "ask": ("card", None, {}),
    "fuel": ("fuel", None, {"extra": "extra", "repel": ("repel", None), "fast": ("repel", "fast"),
                            "touch": ("repel", "touch")}),
    "repel": ("fuel", "fuel", {}),
    "vac": ("machine", None, {"pump": "pump", "trace": "trace"}),
    "well": ("well", None, {"minus": "minus", "zero": "zero", "hill": "hill", "bottom": "bottom"}),
    "fall": ("ions", None, {"strip": "strip", "ion": "ion", "falls": "falls", "seventeen": ("speed", "seventeen"),
                            "back": ("back", None), "seven": ("back", "seven"), "core": ("core", None),
                            "crowd": ("core", "crowd")}),
    "speed": ("ions", "fall", {}),
    "miss": ("ions", "fall", {}),
    "back": ("ions", "fall", {}),
    "core": ("ions", "fall", {}),
    "hit": ("hit", None, {"grabs": "grabs", "neutral": "neutral"}),
    "fuse": ("fuse", None, {"head": "head", "fuse": "fuse", "helium": "helium", "neutron": "neutron"}),
    "count": ("count", None, {"two": "two"}),
    "power": ("power", None, {"three": "three", "few": "few"}),
    "why": ("why", None, {}),
    "so": ("so", None, {"hill": "hill", "cage": "cage", "ions": "ions"}),
}
BIG = {"ask"}                 # captions shown large, mid-frame
CREDIT = ["voice: ribt · simulated and path-traced in vcad", "open source"]


# ── the timeline, from the voice ──────────────────────────────────────────
def fake_timing():
    """word times estimated from word lengths: for storyboards before the voice exists."""
    out = {}
    for lid, text in LINES:
        t, words = 0.15, []
        for w in text.split():
            dt = 0.12 + 0.055 * len(w) + (0.25 if w[-1] in ".,:;?" else 0.0)
            words.append([w, round(t, 3), round(t + dt * 0.9, 3)])
            t += dt
        out[lid] = {"dur": round(t + 0.4, 3), "words": words}
    return out


def word_time(timing, lid, prefix):
    for w, a, _ in timing[lid]["words"]:
        if w.lower().strip(".,:;?!…").startswith(prefix):
            return a
    raise KeyError(f"{lid}: no word starting {prefix!r}")


def spoken(timing, lid):
    """when the line's last word ends (the wavs carry trailing silence)."""
    return timing[lid]["words"][-1][2] + 0.1


def layout(timing):
    ids = [l for l, _ in LINES]
    start, t = {}, 0.5
    for l in ids:
        start[l] = round(t * FPS) / FPS
        t = start[l] + spoken(timing, l) + GAP
    t_end = start["outro"]                    # the end card comes up as the sign-off starts
    total = start["outro"] + spoken(timing, "outro") + TAIL
    return ids, start, t_end, total


def captions(timing, ids, start):
    """[(t0, t1, chunk text, line id)]: each chunk pops in on its first word."""
    out = []
    for l, text in LINES:
        words = timing[l]["words"]
        k = 0
        parts = CAPTIONS.get(l) or [(c, len(c.split())) for c in style.chunks(text)]
        for i, (part, n) in enumerate(parts):
            if k >= len(words):
                break
            t0 = start[l] + words[k][1]
            k += n
            t1 = start[l] + (words[k][1] if k < len(words) else timing[l]["dur"] + 0.35)
            out.append((t0 if i else start[l] + min(words[0][1], 0.05), t1, part, l))
    return out


def build_timeline(a):
    timing = json.load(open(os.path.join(a.vo, "timing.json"))) if a.vo else fake_timing()
    ids, start, t_end, total = layout(timing)
    return dict(timing=timing, ids=ids, start=start, t_end=t_end, total=total,
                captions=captions(timing, ids, start))


# ── one frame ─────────────────────────────────────────────────────────────
def frame(t, a):
    ids, start, t_end, timing = TL["ids"], TL["start"], TL["t_end"], TL["timing"]
    if t >= t_end:
        at = lambda w: t_end + word_time(timing, "outro", w)
        img = style.end_card(t, at("build"), at("build"), at("cad"), CREDIT)
        for t0, t1, part, l in TL["captions"]:
            if l == "outro" and t0 <= t < min(t1, at("build")):
                img = style.draw_chunk(img, part, t - t0, big=True)
        return img
    lid = ids[0]
    for l in ids:
        if start[l] <= t:
            lid = l
    name, base, _ = SHOTS[lid]
    base = base or lid
    cues = {}
    for k, w in SHOTS[base][2].items():
        cl, w = w if isinstance(w, tuple) else (base, w)
        cues[k] = start[cl] + (word_time(timing, cl, w) if w else 0.0) - start[base]
    i = ids.index(lid) + 1
    while ids[i] in SHOTS and SHOTS[ids[i]][1] == base:
        i += 1
    cues["dur"] = start[ids[i]] - start[base]
    img, label = SCENES[name](max(0.0, t - start[base]), cues, a.assets)
    img = style.hud_frame(img, t, TL["total"], label)
    for t0, t1, part, l in TL["captions"]:
        if t0 <= t < t1:
            img = style.draw_chunk(img, part, t - t0, big=l in BIG)
            break
    return img


TL = None
ARGS = None


def _init(a):
    global TL, ARGS
    ARGS = a
    TL = build_timeline(a)


def _render(n):
    frame(n / FPS, ARGS).save(os.path.join(ARGS.frames, f"{n:05d}.png"), compress_level=1)
    return n


def _thumb(t):
    return t, frame(t, ARGS).resize((W // 3, H // 3), Image.LANCZOS)


def sheet(a, tl):
    """a contact sheet: three frames per line."""
    ids, start = tl["ids"], tl["start"]
    ts = []
    for i, l in enumerate(ids):
        end = start[ids[i + 1]] if i + 1 < len(ids) else tl["total"]
        ts += [start[l] + (end - start[l]) * f for f in (0.25, 0.6, 0.95)]
    with Pool(a.jobs, initializer=_init, initargs=(a,)) as pool:
        thumbs = pool.map(_thumb, ts)
    cols = 9
    tw, th = W // 3, H // 3
    rows = math.ceil(len(thumbs) / cols)
    out = Image.new("RGB", (cols * (tw + 6) + 6, rows * (th + 6) + 6), (0, 0, 0))
    for i, (_, im) in enumerate(thumbs):
        out.paste(im, (6 + (i % cols) * (tw + 6), 6 + (i // cols) * (th + 6)))
    out.save(a.sheet)


def voice_track(a, tl, path):
    """every line at its start time, 24 kHz mono."""
    sr = 24000
    buf = np.zeros(int(tl["total"] * sr) + sr, dtype=np.float32)
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
    ap.add_argument("--assets", required=True)
    ap.add_argument("--vo")
    ap.add_argument("--track")
    ap.add_argument("--track-ss", type=float, default=0.0)
    ap.add_argument("--frames", default="frames")
    ap.add_argument("--sheet")
    ap.add_argument("-o", "--out")
    ap.add_argument("--only", help="render only this line's shot")
    ap.add_argument("--jobs", type=int, default=8)
    a = ap.parse_args()

    tl = build_timeline(a)
    dur = tl["total"]
    for l in tl["ids"]:
        print(f"{tl['start'][l]:7.2f}  {l:7s} {tl['timing'][l]['dur']:5.2f} s", file=sys.stderr)
    print(f"end card {tl['t_end']:.1f} s, total {dur:.1f} s", file=sys.stderr)
    if a.sheet:
        return sheet(a, tl)

    os.makedirs(a.frames, exist_ok=True)
    t0, t1 = 0.0, dur
    if a.only:
        i = tl["ids"].index(a.only)
        t0 = tl["start"][a.only] if i else 0.0
        t1 = tl["start"][tl["ids"][i + 1]] if i + 1 < len(tl["ids"]) else dur
    ns = list(range(int(round(t0 * FPS)), int(round(t1 * FPS))))
    with Pool(a.jobs, initializer=_init, initargs=(a,)) as pool:
        for _ in pool.imap(_render, ns, chunksize=8):
            pass

    cmd = ["ffmpeg", "-loglevel", "error", "-y", "-framerate", str(FPS),
           "-start_number", str(ns[0]), "-i", os.path.join(a.frames, "%05d.png")]
    if a.vo and not a.only:
        voice = os.path.join(a.frames, "voice.wav")
        voice_track(a, tl, voice)
        cmd += ["-i", voice]
        if a.track:
            # music dips gently under the voice, fades at the end
            cmd += ["-ss", str(a.track_ss), "-i", a.track, "-filter_complex",
                    f"[1:a]volume=1.7,asplit=2[v][key];"
                    f"[2:a]volume=0.6,afade=t=out:st={dur - 2.5}:d=2.5[m];"
                    f"[m][key]sidechaincompress=threshold=0.05:ratio=2.5:attack=20:release=300[duck];"
                    f"[duck][v]amix=inputs=2:normalize=0,alimiter=limit=0.89:level=0[a]",
                    "-map", "0:v", "-map", "[a]"]
        else:
            cmd += ["-filter:a", "volume=1.7"]
        cmd += ["-t", f"{dur}", "-c:a", "aac", "-b:a", "256k"]
    cmd += ["-frames:v", str(len(ns)), "-c:v", "libx264", "-crf", "17", "-preset", "slow",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart", a.out]
    subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
