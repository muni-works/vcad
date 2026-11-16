#!/usr/bin/env python3
"""Doc figures for docs/plasmoid-sim.md (PIL only, no matplotlib).

    python3 figures.py <sim-out-dir> <docs/plasmoid-sim>

- beta-map.png: electron Hall parameter on the glow sheet under the held
  magnet, 1 Torr vs 1 atm argon (same colour scale, β = 0…3).
- timeline.png: foot–magnet distance vs time with the magnet height, the
  "comparison against the source behaviour" strip.
"""

import csv
import os
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFont


def font(size):
    for f in ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/System/Library/Fonts/Helvetica.ttc"):
        if os.path.exists(f):
            return ImageFont.truetype(f, size)
    return ImageFont.load_default(size=size)


def ramp(v):
    """0..1 → black→deep orange→white."""
    v = np.clip(v, 0, 1)[..., None]
    k = np.array([1.0, 0.42, 0.14])
    return 1 - np.exp(-v * 4.0 * k)


def beta_map(src, out):
    rows = list(csv.DictReader(open(os.path.join(src, "beta_map.csv"))))
    xs = sorted({float(r["x"]) for r in rows})
    n = len(xs)
    b1 = np.array([float(r["beta_1torr"]) for r in rows]).reshape(n, n).T[::-1]
    ba = np.array([float(r["beta_1atm"]) for r in rows]).reshape(n, n).T[::-1]
    s = 4
    img = Image.new("RGB", (2 * n * s + 60, n * s + 110), (8, 6, 6))
    d = ImageDraw.Draw(img)
    for k, (b, label) in enumerate([(b1, f"Ar 1 Torr  (peak β {b1.max():.2f})"), (ba, f"Ar 760 Torr  (peak β {ba.max():.4f})")]):
        tile = (ramp(b / 3.0) * 255).astype(np.uint8)
        im = Image.fromarray(tile).resize((n * s, n * s), Image.NEAREST)
        x0 = 20 + k * (n * s + 20)
        img.paste(im, (x0, 60))
        d.ellipse([x0, 60, x0 + n * s, 60 + n * s], outline=(120, 110, 100), width=2)
        d.text((x0, 20), label, fill=(230, 220, 210), font=font(22))
    d.text((20, n * s + 72), "Hall parameter β = ω_ce/ν_m on the glow sheet, magnet 10 mm up (scale 0–3)",
           fill=(150, 140, 135), font=font(17))
    img.save(os.path.join(out, "beta-map.png"))


def timeline(src, out):
    rows = list(csv.DictReader(open(os.path.join(src, "trajectory.csv"))))
    t = np.array([float(r["t"]) for r in rows])
    d_mm = 1e3 * np.hypot(np.array([float(r["x"]) for r in rows]) - np.array([float(r["mx"]) for r in rows]),
                          np.array([float(r["y"]) for r in rows]) - np.array([float(r["my"]) for r in rows]))
    mz = 1e3 * np.array([float(r["mz"]) for r in rows])
    locked = np.array([r["locked"] == "1" for r in rows])
    W, H, L, R, T, B = 1200, 420, 70, 20, 50, 80
    img = Image.new("RGB", (W, H), (8, 6, 6))
    d = ImageDraw.Draw(img)
    X = lambda tt: L + (W - L - R) * tt / t[-1]
    Y = lambda mm: T + (H - T - B) * (1 - min(mm, 140) / 140)
    for tt, lk in zip(t, locked):
        if lk:
            d.line([(X(tt), T), (X(tt), H - B)], fill=(50, 26, 10))
    d.line([(X(a), Y(b)) for a, b in zip(t, mz - 2.5)], fill=(130, 130, 145), width=2)
    d.line([(X(a), Y(b)) for a, b in zip(t, d_mm)], fill=(255, 150, 60), width=2)
    d.line([(L, H - B), (W - R, H - B)], fill=(90, 85, 80))
    d.line([(L, T), (L, H - B)], fill=(90, 85, 80))
    f = font(16)
    for mm in (0, 50, 100):
        d.text((10, Y(mm) - 9), f"{mm} mm", fill=(140, 130, 125), font=f)
    for s in range(0, int(t[-1]) + 1, 2):
        d.text((X(s) - 6, H - B + 6), f"{s}", fill=(140, 130, 125), font=f)
    d.text((L, 14), "orange: foot–magnet distance (mm)   grey: magnet gap above dish (mm, clipped)   shaded: locked",
           fill=(220, 210, 200), font=font(18))
    labels = [(1.0, "racing"), (3.5, "magnet lowers"), (6.2, "LOCK"), (9.0, "dragged, follows"),
              (13.6, "flick"), (14.6, "re-lock"), (18.2, "lift → release")]
    for tt, s in labels:
        d.text((X(tt), H - B + 30), s, fill=(255, 170, 90) if "LOCK" in s or "lock" in s else (200, 190, 180), font=f)
    img.save(os.path.join(out, "timeline.png"))


if __name__ == "__main__":
    beta_map(sys.argv[1], sys.argv[2])
    timeline(sys.argv[1], sys.argv[2])
