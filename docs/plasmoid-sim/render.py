#!/usr/bin/env python3
"""Render the plasmoid sim trajectory as a vertical 9:16 clip.

    python3 render.py <sim-out-dir> <frames-dir> [--fps 30] [--jobs 6]
    ffmpeg -framerate 30 -i <frames-dir>/f%04d.png -c:v libx264 \
        -pix_fmt yuv420p -crf 26 -preset slow plasmoid.mp4

Inputs: trajectory.csv from examples/plasmoid_magnet.rs. Geometry mirrors
plasmoid-dish.loon (mm, Z-up, dish surface at z = 0).

What is simulated vs drawn: the foot position (where the filament meets
the dish) and the magnet pose come from the sim. The filament's shape, its
flicker, the faint secondary streamers and the diffuse disc glow are
render-side stylization — the reduced model has no filament geometry.
"""

import csv
import math
import os
import sys
from multiprocessing import Pool

import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy.ndimage import gaussian_filter

W, H = 720, 1280
FOCAL = 1350.0
TARGET = np.array([0.0, 0.0, -38.0])
DIST = 470.0
ELEV = math.radians(30.0)

CAM = TARGET + DIST * np.array([0.0, -math.cos(ELEV), math.sin(ELEV)])
FWD = (TARGET - CAM) / np.linalg.norm(TARGET - CAM)
RIGHT = np.cross(FWD, [0.0, 0.0, 1.0])
RIGHT /= np.linalg.norm(RIGHT)
UP = np.cross(RIGHT, FWD)

DISH_R, DISH_DEPTH = 85.0, 14.0
SPIKE_TIP = np.array([0.0, 0.0, -DISH_DEPTH + 24.0])
GLOW_Z = 2.0


def proj(p):
    """World mm (…, 3) → screen px (…, 2)."""
    p = np.asarray(p, float)
    d = p - CAM
    z = d @ FWD
    x = W / 2 + FOCAL * (d @ RIGHT) / z
    y = H / 2 - FOCAL * (d @ UP) / z
    return np.stack([x, y], -1)


def circle(r, z, n=96, cx=0.0, cy=0.0):
    a = np.linspace(0, 2 * np.pi, n, endpoint=False)
    return np.stack([cx + r * np.cos(a), cy + r * np.sin(a), np.full(n, z)], -1)


def hull(pts):
    pts = sorted(map(tuple, pts))
    if len(pts) < 3:
        return pts

    def cr(o, a, b):
        return (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])

    lo, up = [], []
    for p in pts:
        while len(lo) >= 2 and cr(lo[-2], lo[-1], p) <= 0:
            lo.pop()
        lo.append(p)
    for p in reversed(pts):
        while len(up) >= 2 and cr(up[-2], up[-1], p) <= 0:
            up.pop()
        up.append(p)
    return lo[:-1] + up[:-1]


def cylinder_poly(r, z0, z1, cx=0.0, cy=0.0):
    pts = np.concatenate([proj(circle(r, z0, cx=cx, cy=cy)), proj(circle(r, z1, cx=cx, cy=cy))])
    return [tuple(p) for p in hull(pts)]


def font(size):
    for f in ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/System/Library/Fonts/Helvetica.ttc"):
        if os.path.exists(f):
            return ImageFont.truetype(f, size)
    return ImageFont.load_default(size=size)


def load(path):
    rows = list(csv.DictReader(open(path)))
    return {k: np.array([float(r[k]) for r in rows]) for k in rows[0]}


def shade(c, glow):
    """Base colour lit by the plasma's orange spill."""
    g = np.array([1.0, 0.45, 0.15]) * glow
    return tuple(int(min(255, v)) for v in np.array(c) * (0.12 + 0.3 * glow) + 45 * g)


def base_scene(glow, magnet_mm):
    img = Image.new("RGB", (W, H), (6, 3, 3))
    d = ImageDraw.Draw(img)
    # ring (back half), posts
    ring = proj(circle(105, 95, n=128, cy=25))
    d.line([tuple(p) for p in ring] + [tuple(ring[0])], fill=shade((60, 40, 30), glow * 0.6), width=4)
    for ang in (60, 120, 200, 340):
        a = math.radians(ang)
        p0 = proj([112 * math.cos(a), 112 * math.sin(a), -15])
        p1 = proj([112 * math.cos(a), 112 * math.sin(a), 95])
        d.line([tuple(p0), tuple(p1)], fill=shade((45, 40, 38), glow * 0.5), width=4)
    # driver stack, bottom up
    for r, z0, z1, col in [
        (62, -90, -72, (70, 60, 55)),
        (56, -70, -54, (80, 70, 62)),
        (46, -52, -18, (55, 35, 25)),
    ]:
        d.polygon(cylinder_poly(r, z0, z1), fill=shade(col, glow))
        top = proj(circle(r, z1))
        d.line([tuple(p) for p in top] + [tuple(top[0])], fill=shade((120, 90, 70), glow), width=2)
    # dish: outer shell then the inner bowl
    d.polygon(cylinder_poly(DISH_R + 2, -DISH_DEPTH - 2, 0) , fill=shade((90, 55, 35), glow))
    rim = proj(circle(DISH_R, 0, n=160))
    d.polygon([tuple(p) for p in rim], fill=shade((40, 22, 14), glow * 1.2))
    d.line([tuple(p) for p in rim] + [tuple(rim[0])], fill=shade((200, 120, 70), glow), width=3)
    # spike
    d.polygon(cylinder_poly(7, -DISH_DEPTH, -DISH_DEPTH + 1) + [tuple(proj(SPIKE_TIP))],
              fill=shade((60, 55, 55), glow))
    # magnet + holder
    if magnet_mm[2] < 200:
        mx, my, mz = magnet_mm
        d.polygon(cylinder_poly(2, mz + 2.5, mz + 400, mx, my), fill=(28, 26, 30))
        d.polygon(cylinder_poly(5, mz - 2.5, mz + 2.5, mx, my), fill=shade((150, 150, 160), glow * 0.5))
        t = proj(circle(5, mz + 2.5, cx=mx, cy=my))
        d.line([tuple(p) for p in t] + [tuple(t[0])], fill=(200, 200, 210), width=2)
    return img


def splat(buf, pts, w, sigma):
    """Accumulate points into buf with small gaussians (bilinear deposit,
    blurred afterwards)."""
    for (x, y), wi in zip(pts, w):
        ix, iy = int(x), int(y)
        if 0 <= ix < W - 1 and 0 <= iy < H - 1:
            fx, fy = x - ix, y - iy
            buf[iy, ix] += wi * (1 - fx) * (1 - fy)
            buf[iy, ix + 1] += wi * fx * (1 - fy)
            buf[iy + 1, ix] += wi * (1 - fx) * fy
            buf[iy + 1, ix + 1] += wi * fx * fy


def filament(foot_mm, seed, t, strength=1.0, n=160):
    """Jittered arc from the spike tip to the foot, hugging the glow."""
    rng = np.random.default_rng(seed)
    s = np.linspace(0, 1, n)
    foot = np.array([foot_mm[0], foot_mm[1], GLOW_Z])
    p = SPIKE_TIP[None] * (1 - s[:, None]) + foot[None] * s[:, None]
    p[:, 2] += 9 * np.sin(np.pi * s) * (1 - s) + 3 * np.sin(np.pi * s)
    side = np.cross(foot - SPIKE_TIP, [0, 0, 1.0])
    side /= np.linalg.norm(side) + 1e-9
    wob = np.zeros(n)
    for k in range(1, 6):
        wob += rng.normal() / k * np.sin(k * np.pi * s * 2 + rng.uniform(0, 6.28) + 9 * t * k)
    p += side[None] * (2.2 * wob * np.sin(np.pi * s))[:, None]
    p[:, 2] += 1.2 * wob * np.sin(np.pi * s)
    w = strength * (0.6 + 0.8 * s**2)
    return proj(p), w


def render(args):
    i, fps, tr, out = args
    t = i / fps
    ts = tr["t"]
    # 4 sub-samples across the 1/fps exposure (sim is 120 Hz)
    sub = [np.searchsorted(ts, t + k / (4 * fps)) for k in range(4)]
    sub = [min(j, len(ts) - 1) for j in sub]
    j = sub[0]
    locked = tr["locked"][j] > 0.5
    beta = tr["beta"][j]
    mag = 1e3 * np.array([tr["mx"][j], tr["my"][j], tr["mz"][j]])
    flicker = 0.85 + 0.15 * math.sin(37 * t) * math.sin(11 * t + 1)
    glow = 0.55 * flicker + (0.25 if locked else 0.0)

    img = base_scene(glow, mag)
    buf = np.zeros((H, W))
    disc = np.zeros((H, W))

    # diffuse disc glow (stylized), brighter near the foot
    foot0 = 1e3 * np.array([tr["x"][j], tr["y"][j]])
    rng = np.random.default_rng(i)
    rr = DISH_R * np.sqrt(rng.uniform(0, 1, 2500)) * 0.97
    aa = rng.uniform(0, 2 * np.pi, 2500)
    gx, gy = rr * np.cos(aa), rr * np.sin(aa)
    near = np.exp(-((gx - foot0[0]) ** 2 + (gy - foot0[1]) ** 2) / (2 * 25.0**2))
    pts = proj(np.stack([gx, gy, np.full_like(gx, GLOW_Z)], -1))
    splat(disc, pts, 0.05 * flicker + 0.25 * near, 1)

    # filament + foot, motion-blurred across the exposure
    for k, jj in enumerate(sub):
        foot = 1e3 * np.array([tr["x"][jj], tr["y"][jj]])
        fp, fw = filament(foot, seed=i * 7 + k, t=t + k / (4 * fps), strength=1.6 if locked else 1.2)
        splat(buf, fp, fw / 4, 1)
        fpx = proj([foot[0], foot[1], GLOW_Z])
        splat(buf, [fpx], [(70 if locked else 40) / 4], 1)
    # persistence trail of the foot (camera/eye lag)
    for back in range(0, 20):
        ja, jb = max(0, j - back), max(0, j - back - 1)
        for u in np.linspace(0, 1, 6, endpoint=False):
            fx = 1e3 * (tr["x"][ja] * (1 - u) + tr["x"][jb] * u)
            fy = 1e3 * (tr["y"][ja] * (1 - u) + tr["y"][jb] * u)
            splat(buf, [proj([fx, fy, GLOW_Z])], [1.6 * 0.85 ** (back + u)], 1)
    # faint secondary streamers (stylized)
    for s in range(3):
        a = rng.uniform(0, 2 * np.pi)
        r = rng.uniform(35, 75)
        fp, fw = filament([r * math.cos(a), r * math.sin(a)], seed=10_000 + i * 3 + s, t=t, strength=0.25)
        splat(buf, fp, fw, 1)

    light = (
        gaussian_filter(buf, 1.5) * 2.5
        + gaussian_filter(buf, 5) * 8.0
        + gaussian_filter(buf, 16) * 22.0
        + gaussian_filter(buf, 45) * 40.0
        + gaussian_filter(disc, 10) * 34.0
        + gaussian_filter(disc, 30) * 46.0
    )
    k = np.array([1.0, 0.42, 0.14])
    plasma = 1 - np.exp(-light[..., None] * k[None, None] * 1.4)
    base = np.asarray(img, float) / 255
    frame = 1 - (1 - base) * (1 - plasma)  # screen blend
    im = Image.fromarray((np.clip(frame, 0, 1) * 255).astype(np.uint8))

    # HUD
    d = ImageDraw.Draw(im)
    f_big, f_sm = font(34), font(24)
    d.text((32, 40), "vcad · plasmoid sim", fill=(235, 225, 215), font=f_big)
    d.text((32, 84), "reduced model · N52 10×5 mm · Ar 1 Torr", fill=(150, 140, 135), font=f_sm)
    if mag[2] > 100:
        phase = "racing"
    elif locked:
        phase = "locked to magnet"
    else:
        phase = "magnet near — not locked"
    d.text((32, H - 190), phase, fill=(255, 170, 90) if locked else (220, 210, 200), font=f_big)
    d.text((32, H - 146), f"β at foot {beta:5.2f}   t {t:5.2f} s", fill=(160, 150, 145), font=f_sm)
    # timeline strip
    x0, x1, y0 = 32, W - 32, H - 90
    tmax = ts[-1]
    d.rectangle([x0, y0, x1, y0 + 18], fill=(30, 26, 26))
    for jj in range(0, len(ts), 2):
        x = x0 + (x1 - x0) * ts[jj] / tmax
        if tr["mz"][jj] < 0.1:
            d.line([(x, y0 + 12), (x, y0 + 18)], fill=(110, 110, 120))
        if tr["locked"][jj] > 0.5:
            d.line([(x, y0), (x, y0 + 10)], fill=(255, 140, 50))
    px = x0 + (x1 - x0) * t / tmax
    d.line([(px, y0 - 6), (px, y0 + 24)], fill=(255, 255, 255), width=2)
    d.text((x0, y0 + 26), "orange: locked   grey: magnet down", fill=(120, 112, 108), font=f_sm)
    im.save(os.path.join(out, f"f{i:04d}.png"))
    return i


def main():
    src, out = sys.argv[1], sys.argv[2]
    fps = int(sys.argv[sys.argv.index("--fps") + 1]) if "--fps" in sys.argv else 30
    jobs = int(sys.argv[sys.argv.index("--jobs") + 1]) if "--jobs" in sys.argv else 6
    only = int(sys.argv[sys.argv.index("--frame") + 1]) if "--frame" in sys.argv else None
    os.makedirs(out, exist_ok=True)
    tr = load(os.path.join(src, "trajectory.csv"))
    n = int(tr["t"][-1] * fps)
    frames = [only] if only is not None else range(n)
    with Pool(jobs) as pool:
        for _ in pool.imap_unordered(render, [(i, fps, tr, out) for i in frames]):
            pass


if __name__ == "__main__":
    main()
