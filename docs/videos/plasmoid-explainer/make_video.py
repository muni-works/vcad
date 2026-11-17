#!/usr/bin/env python3
"""Compose the plasmoid explainer (see script.md): path-traced clip frames,
diagrams (diagrams.py) and the Pilot Light design system (style.py), cut to
the soundtrack's beat grid (beat.py → beat.json), track from 0.

    EXPLAINER_SCALE=1.5 python3 make_video.py --raw <frame_NNNN.png dir> \
        --traj <trajectory.csv> --assets <dir> --beat beat.json \
        [--track track.mp3] -o explainer.mp4 [--only N] [--jobs 8]

The clip frames and trajectory come from `vcad-render --sim plasmoid`
(docs/plasmoid-sim.md): 600 frames at 30 fps. `--assets` holds beta-map.png
and lock_compare.csv. EXPLAINER_SCALE=1 gives 720x1280, 1.5 gives 1080x1920.
Every frame is a pure function of t, so one scene re-renders alone (--only).
"""
import argparse
import csv
import json
import math
import os
import subprocess
import sys
from bisect import bisect_left
from multiprocessing import Pool

import numpy as np
from PIL import Image, ImageFilter

import style
from diagrams import DIAGRAMS
from style import F, FS, H, LABEL, M, OK, SEL, TEXT, W, FPS, tracked

BEAT = 0.5                    # 120 BPM; the track's drops are at 16, 32, 56, 72 s
T_LOCK = 5.0 + 28 / 30        # clip frame 178: the foot locks (model: 5.93 s)
T_FLICK = 14.0 + 1 / 30       # clip frame 421: the flick (model: break-away 14.04 s)

# (start, end, kind, args, num, title, [(cue s from scene start, caption)])
# Every boundary and cue is a whole beat. "clip" args give clip time as a
# function of scene-local time (see clip_time).
SCENES = [
    (0.0, 16.0, "clip", dict(mode="metronome", tag="slowed: one lap = two beats"), "01", "the phenomenon", [
        (0.0, "a thread of light races around a dish"),
        (6.0, "it laps the dish almost twice a second"),
        (12.0, "bring a magnet close…"),
    ]),
    (16.0, 30.0, "clip", dict(mode="1x", t0=T_LOCK), "02", "what it can do", [
        (0.0, "…and it locks on"),                                      # drop 1
        (2.0, "drag the magnet: it follows, like it's on rails"),
        (6.0, "flick it…"),
        (8.0, "…it snaps off, and gets caught on the next lap"),         # the flick lands at 24.1 s
        (11.5, "lift the magnet: it's free again"),
    ]),
    (30.0, 34.0, "card", dict(), "", "", []),
    (34.0, 37.0, "labels", dict(), "03", "what is a plasmoid", [
        (0.0, "a blob of plasma that holds its shape and moves as one"),
    ]),
    (37.0, 46.0, "diagram", dict(name="gas"), "03", "what is a plasmoid", [
        (0.0, "plasma: gas whose electrons are knocked loose from their atoms"),
        (3.0, "here a radio-frequency field shakes electrons in thin gas"),
        (6.0, "they knock more loose, and the atoms glow as they settle"),
    ]),
    (46.0, 56.0, "diagram", dict(name="rf"), "04", "the obvious answer", [
        (0.0, "magnets push on electric currents…"),
        (2.5, "but this current flips millions of times a second"),
        (5.0, "push one way, then the other: it averages out to zero"),
        (8.0, "so what's left?"),                                        # the dip before drop 3
    ]),
    (56.0, 68.0, "diagram", dict(name="beta"), "05", "curling electrons", [
        (0.0, "magnets curl electrons"),                                 # drop 3
        (2.5, "β = how far an electron curls before it hits an atom"),
        (5.0, "normal air: β ≈ 0.004. thin gas under a magnet: β ≈ 2.9"),
        (8.0, "curled electrons linger, so the glow could favor the magnet"),
    ]),
    (68.0, 78.0, "sim", dict(mode="1x", t0=T_FLICK - 4.0), "06", "the opening, explained", [
        (0.0, "one idea, one small model: it reproduces everything you saw"),
        (4.0, "the lock, the break-away, the recapture"),                 # drop 4, and the flick
        (7.0, "the magnet's path is scripted; lock and release come out of the model"),
    ]),
    (78.0, 88.0, "diagram", dict(name="compare"), "07", "the twist", [
        (0.0, "but a detailed model of the glow disagrees"),
        (2.5, "magnetism alone pushes the foot away"),
        (5.0, "and the magnet is nickel-plated metal"),
        (7.5, "metal near the electrode pulls the glow in: 2.7× harder, at 1 Torr"),
    ]),
    (88.0, 96.0, "diagram", dict(name="test"), "08", "the test", [
        (0.0, "so the lock is probably electric, not magnetic (or it's where the foot re-forms)"),
        (3.5, "the test: swap the magnet for an aluminum slug"),
        (6.0, "still locks? it was never magnetism"),
    ]),
]
T_END = 96.0
DUR = T_END + 4.6
CREDIT = ["plasmoid · reduced model + glow fluid model", "simulated and path-traced in vcad · open source"]


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


def clip_time(args, local, start):
    """clip time shown at scene-local time `local`."""
    period = TRAJ[2]
    speed = period / (2 * BEAT)                 # one lap per two beats
    loop = 6 * period                           # six free laps: a seamless loop while the magnet is far
    if args["mode"] == "1x":
        return args["t0"] + local
    if args["mode"] == "metronome":             # ends exactly on the lock at the scene's end
        tc = T_LOCK - (16.0 - local) * speed
        while tc < 0:
            tc += loop
        return tc
    if args["mode"] == "loop":
        return (local * speed) % loop
    raise ValueError(args["mode"])


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


def draw_sim_hud(img, tc):
    """the model's state at clip time tc, from trajectory.csv."""
    ts, rows, _ = TRAJ
    r = rows[min(bisect_left(ts, tc), len(rows) - 1)]
    locked = r["locked"] == "1"
    d = style.draw(img)
    tracked(d, (W - M, 70), "locked" if locked else "racing", F(24, True), OK if locked else LABEL, "ra")
    d.text((W - M, 110), f"β at foot {float(r['beta']):.2f}", font=FS(21), fill=TEXT, anchor="ra")
    d.text((W - M, 140), f"sim t {tc:5.2f} s", font=F(20), fill=LABEL, anchor="ra")
    return img


def draw_card(img, local):
    """the question, full frame, over the dimmed racing plasmoid."""
    img = Image.blend(style.new("RGB", style.BGC), img, 0.32)
    d = style.draw(img)
    d.multiline_text((W / 2, 520), "but what is\na plasmoid?", font=FS(74, True), fill=TEXT,
                     anchor="mm", align="center", spacing=14)
    if local >= 2.0:                             # lands on the 32 s jump
        d.text((W / 2, 700), "and why would a magnet grab it?", font=FS(34, True), fill=SEL, anchor="mm")
    return img


# ── one frame ─────────────────────────────────────────────────────────────
def scene_at(t):
    for s in SCENES:
        if s[0] <= t < s[1]:
            return s
    return None


def caption_at(scene, t):
    start, end, *_, cues = scene
    local = t - start
    i = max(k for k, (c, _) in enumerate(cues) if c <= local + 1e-9)
    c0 = start + cues[i][0]
    c1 = start + cues[i + 1][0] if i + 1 < len(cues) else end
    return cues[i][1], c0, c1


def frame(t, a):
    if t >= T_END:
        return style.end_card(t, T_END, BEAT, CREDIT)
    scene = scene_at(t)
    start, end, kind, args, num, title, cues = scene
    local = t - start
    if kind == "clip":
        img = clip_frame(a.raw, clip_time(args, local, start))
        if args.get("tag"):
            tracked(style.draw(img), (W - M, 70), args["tag"], F(16, True), LABEL, "ra")
    elif kind == "sim":
        tc = clip_time(args, local, start)
        img = draw_sim_hud(clip_frame(a.raw, tc), tc)
    elif kind == "labels":
        img = draw_labels(clip_frame(a.raw, clip_time(dict(mode="loop"), local, start)), local)
    elif kind == "card":
        img = draw_card(clip_frame(a.raw, clip_time(dict(mode="loop"), local, start)), local)
    else:
        img = DIAGRAMS[args["name"]](local, a.assets)
    img = style.hud_frame(img, t, DUR)
    if not cues:
        return img
    text, c0, c1 = caption_at(scene, t)
    return style.panel(img, t, c0, c1, num, title, text)


TRAJ = None
ARGS = None


def _init(a):
    global TRAJ, ARGS
    ARGS = a
    TRAJ = load_traj(a.traj)


def _render(n):
    out = os.path.join(ARGS.frames, f"{n:05d}.png")
    frame(n / FPS, ARGS).save(out, compress_level=1)
    return n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--raw", required=True)
    ap.add_argument("--traj", required=True)
    ap.add_argument("--assets", required=True)
    ap.add_argument("--beat", required=True)
    ap.add_argument("--track")
    ap.add_argument("--frames", default="frames")
    ap.add_argument("-o", "--out", required=True)
    ap.add_argument("--only", type=int, help="render only SCENES[N-1] (len+1 = end card)")
    ap.add_argument("--jobs", type=int, default=8)
    a = ap.parse_args()

    bj = json.load(open(a.beat))
    assert abs(bj["bpm"] - 120.0) < 0.05, f"the cut is timed for 120 BPM, track says {bj['bpm']}"
    offset = bj["phase"] % BEAT                 # start the track here so beats land on k·0.5 s

    os.makedirs(a.frames, exist_ok=True)
    t0, t1 = 0.0, DUR
    if a.only:
        t0, t1 = (SCENES[a.only - 1][0], SCENES[a.only - 1][1]) if a.only <= len(SCENES) else (T_END, DUR)
    ns = list(range(int(round(t0 * FPS)), int(round(t1 * FPS))))
    with Pool(a.jobs, initializer=_init, initargs=(a,)) as pool:
        for k, _ in enumerate(pool.imap(_render, ns, chunksize=8)):
            if k % 300 == 0:
                print(f"{ns[0] / FPS + k / FPS:5.1f} s", file=sys.stderr, flush=True)

    cmd = ["ffmpeg", "-loglevel", "error", "-y", "-framerate", str(FPS),
           "-start_number", str(ns[0]), "-i", os.path.join(a.frames, "%05d.png")]
    if a.track and not a.only:
        cmd += ["-ss", f"{offset}", "-i", a.track, "-t", f"{DUR}",
                "-af", f"afade=t=out:st={DUR - 2.0}:d=2.0", "-c:a", "aac", "-b:a", "256k"]
    cmd += ["-frames:v", str(len(ns)), "-c:v", "libx264", "-crf", "17", "-preset", "slow",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart", a.out]
    subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
