#!/usr/bin/env python3
"""Compose the plasmoid explainer (see script.md): path-traced clip frames,
procedural diagrams (diagrams.py) and the Pilot Light design system
(style.py), cut to the soundtrack's beat grid (beat.py → beat.json).

    python3 make_video.py --raw <frame_NNNN.png dir, no HUD> \
        --traj <trajectory.csv> --assets <dir> --beat beat.json \
        [--track track.mp3] -o explainer.mp4 [--only N] [--jobs 8]

The clip frames and trajectory come from `vcad-render --sim plasmoid`
(docs/plasmoid-sim.md): 600 frames, 30 fps, 720x1280. Every frame is a pure
function of t, so a single scene re-renders alone (`--only`).
"""
import argparse
import csv
import json
import os
import subprocess
import sys
from bisect import bisect_left
from multiprocessing import Pool

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

import style
from diagrams import DIAGRAMS
from style import F, FS, H, LABEL, M, OK, SEL, TEXT, W, FPS, tracked

# The soundtrack starts at TRACK_OFFSET s, so its first drop (16.04 s) lands
# on the lock (scene 1) and its third (72.04 s) on the flick break-away
# (scene 6). The offset keeps the grid phase at 0 in video time: every scene
# boundary and caption cue below is a whole number of beats (0.5 s).
TRACK_OFFSET = 11.04

# (start, end, kind, args, num, title, [(cue s from scene start, caption)])
SCENES = [
    (0.0, 8.0, "clip", dict(t0=0.93), "01", "the clip", [
        (0.0, "a glowing thread races around a dish"),
        (3.0, "bring a magnet close…"),
        (5.0, "…and it locks on"),           # drop 1, and the lock (clip 5.93 s)
    ]),
    (8.0, 20.0, "diagram", dict(name="gas"), "02", "what's glowing", [
        (0.0, "what's glowing? (our best guess from the video)"),
        (3.0, "thin gas, about 1/760 of an atmosphere"),
        (6.0, "a radio-frequency field shakes its electrons"),
        (9.0, "fast electrons knock more loose; atoms glow as they settle"),
    ]),
    (20.0, 30.0, "labels", dict(t0=0.0, speed=0.5), "03", "plasma", [
        (0.0, "glowing, electrified gas is a plasma"),
        (3.5, "a plasmoid: a blob of plasma that holds its shape and moves as one"),
        (7.0, "this one's foot races around the dish (we didn't model why it races)"),
    ]),
    (30.0, 45.0, "diagram", dict(name="rf"), "04", "the obvious answer", [
        (0.0, "so why would a magnet grab it?"),
        (4.0, "the obvious answer: magnets push on currents"),
        (7.5, "but this current flips millions of times a second"),
        (11.0, "push left, push right: it averages out to zero"),
    ]),
    (45.0, 59.0, "diagram", dict(name="beta"), "05", "curling electrons", [
        (0.0, "what survives: magnets curl electrons"),   # drop 2
        (3.0, "β = how much an electron curls before it hits an atom"),
        (5.5, "at normal air pressure: β ≈ 0.004. nothing"),
        (8.5, "in thin gas under this magnet: β ≈ 2.9"),
        (11.0, "curled electrons stick around, so the glow might prefer the magnet"),
    ]),
    (59.0, 66.0, "sim", dict(t0=12.04), "06", "the simulation", [
        (0.0, "we built a small model on that idea and simulated it in vcad"),
        (2.0, "flick the magnet: the foot breaks away, then gets recaptured"),  # drop 3
        (4.5, "we script the magnet; the model decides when the foot locks and lets go"),
    ]),
    (66.0, 78.0, "diagram", dict(name="pull"), "07", "the twist", [
        (0.0, "then we modelled the glow itself, from first principles"),
        (3.0, "result: the magnet pushes the foot away"),
        (6.0, "but a magnet is nickel-plated metal"),
        (9.0, "metal near the electrode pulls the glow in: 2.7× harder, at 1 Torr"),
    ]),
    (78.0, 88.0, "diagram", dict(name="test"), "08", "the test", [
        (0.0, "so the lock is probably electric, not magnetic (or it's where the foot re-forms)"),
        (4.0, "test it: swap the magnet for an aluminum slug"),
        (7.0, "still locks? it was never magnetism"),
    ]),
]
T_END = 88.0
DUR = T_END + 4.6
BEAT = 0.5
CREDIT = ["plasmoid · reduced model + glow fluid model", "simulated and path-traced in vcad · open source"]


# ── sources ───────────────────────────────────────────────────────────────
_frames = {}


def clip_frame(raw_dir, tc):
    n = min(max(int(round(tc * FPS)), 0), 599)
    if n not in _frames:
        if len(_frames) > 6:
            _frames.clear()
        _frames[n] = Image.open(os.path.join(raw_dir, f"frame_{n:04d}.png")).convert("RGB")
    return _frames[n].copy()


def load_traj(path):
    rows = list(csv.DictReader(open(path)))
    return [float(r["t"]) for r in rows], rows


SPIKE_TIP = (360, 655)


def find_foot(img):
    """The foot is the brightest blob on the dish, away from the spike tip."""
    g = img.convert("L").resize((W // 4, H // 4)).filter(ImageFilter.GaussianBlur(2))
    a = np.asarray(g, dtype=np.float32)
    ys, xs = np.mgrid[0:a.shape[0], 0:a.shape[1]]
    a[(np.hypot(xs * 4 - SPIKE_TIP[0], ys * 4 - SPIKE_TIP[1]) < 45)] = 0
    a[:140, :] = 0
    a[230:, :] = 0
    y, x = np.unravel_index(int(np.argmax(a)), a.shape)
    return x * 4 + 2, y * 4 + 2


def draw_labels(img, t):
    """Scene 3: name the spike and the filament, then follow the foot."""
    d = ImageDraw.Draw(img)

    def label(p, lp, text, a, below=False):
        if a <= 0:
            return
        c = style.on_bg(SEL, a)
        d.line([p, lp], fill=c, width=2)
        d.text((lp[0], lp[1] + (6 if below else -6)), text, font=FS(28, True), fill=c,
               anchor="mt" if below else "mb")

    a1 = style.ease(style.seg(t, 3.5, 3.6))
    label((SPIKE_TIP[0] + 14, SPIKE_TIP[1] + 20), (SPIKE_TIP[0] + 130, 590), "spike", a1)
    fx, fy = find_foot(img)
    mx, my = (SPIKE_TIP[0] + fx) / 2, (SPIKE_TIP[1] + fy) / 2 - 34
    label((mx, my - 6), (mx, 470), "filament", a1)
    a2 = style.ease(style.seg(t, 7.0, 7.1))
    label((fx, fy + 16), (fx, fy + 120), "foot", a2, below=True)
    tracked(d, (W - M, 70), "½× speed", F(16, True), LABEL, "ra")
    return img


def draw_sim_hud(img, tc, traj):
    """Scene 6: the model's state at clip time tc, from trajectory.csv."""
    ts, rows = traj
    r = rows[min(bisect_left(ts, tc), len(rows) - 1)]
    locked = r["locked"] == "1"
    d = ImageDraw.Draw(img)
    tracked(d, (W - M, 70), "locked" if locked else "racing", F(24, True), OK if locked else LABEL, "ra")
    d.text((W - M, 110), f"β at foot {float(r['beta']):.2f}", font=FS(21), fill=TEXT, anchor="ra")
    d.text((W - M, 140), f"sim t {tc:5.2f} s", font=F(20), fill=LABEL, anchor="ra")
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
    start, end, kind, args, num, title, _ = scene
    local = t - start
    if kind == "clip":
        img = clip_frame(a.raw, args["t0"] + local)
    elif kind == "sim":
        tc = args["t0"] + local
        img = draw_sim_hud(clip_frame(a.raw, tc), tc, TRAJ)
    elif kind == "labels":
        img = draw_labels(clip_frame(a.raw, args["t0"] + args["speed"] * local), local)
    else:
        img = DIAGRAMS[args["name"]]((t - start) / (end - start), local, a.assets)
    img = style.hud_frame(img, t, DUR)
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
    ap.add_argument("--only", type=int, help="render only scene N (1-based; 9 = end card)")
    ap.add_argument("--jobs", type=int, default=8)
    a = ap.parse_args()

    bj = json.load(open(a.beat))
    assert abs(bj["bpm"] - 120.0) < 0.05, f"the cut is timed for 120 BPM, track says {bj['bpm']}"
    frac = ((bj["phase"] - TRACK_OFFSET) / BEAT) % 1.0
    assert min(frac, 1 - frac) < 0.02, "TRACK_OFFSET breaks the beat grid"

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
        st = DUR - 2.0
        cmd += ["-ss", f"{TRACK_OFFSET}", "-i", a.track, "-t", f"{DUR}",
                "-af", f"afade=t=in:st=0:d=0.3,afade=t=out:st={st}:d=2.0",
                "-c:a", "aac", "-b:a", "256k"]
    cmd += ["-frames:v", str(len(ns)), "-c:v", "libx264", "-crf", "17", "-preset", "slow",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart", a.out]
    subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
