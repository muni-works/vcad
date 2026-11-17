#!/usr/bin/env python3
"""Render given seconds through comp.py and tile a 4-wide contact sheet.
usage: sheet.py <frames_dir> <results_dir> <out.jpg> <sec,sec,...>"""
import subprocess, sys
from pathlib import Path
from PIL import Image, ImageDraw
fr, res, out = sys.argv[1], sys.argv[2], Path(sys.argv[3])
secs = [float(s) for s in sys.argv[4].split(",")]; frames = [int(round(s * 30)) for s in secs]
tmp = out.with_suffix("") ; tmp = Path(str(tmp) + "_frames")
subprocess.run([sys.executable, str(Path(__file__).with_name("comp.py")), fr, res, str(tmp), ",".join(map(str, frames))], check=True)
cols, tw, th = 4, 640, 360; rows = (len(frames) + cols - 1) // cols
sheet = Image.new("RGB", (cols * tw, rows * th))
for i, (s, f) in enumerate(zip(secs, frames)):
    im = Image.open(tmp / f"{f:04d}.png").resize((tw, th), Image.LANCZOS)
    ImageDraw.Draw(im).text((6, th - 16), f"{s:g} s", fill=(255, 106, 0))
    sheet.paste(im, ((i % cols) * tw, (i // cols) * th))
sheet.save(out, quality=88); print(out)
