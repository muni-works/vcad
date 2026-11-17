#!/usr/bin/env python3
"""Storyboard sheet for the proposed re-cut (track from 0): one frame per
scene, its narrative beat, timing and captions.

    python3 storyboard.py explainer.mp4 storyboard.png
"""
import subprocess
import sys

from PIL import Image, ImageDraw

import style
from style import F, FS, LABEL, LINE_HI, SEL, SURFACE, TEXT, tracked

# (num, title, beat, t span, source frame (s into the current render), music note, [(t, caption)])
SCENES = [
    ("01", "the phenomenon", "phenomenon", "0–16 s", 3.0, "racing at 0.37×; intro, then near-silence at 14.5", [
        ("0", "a thread of light races around a dish"),
        ("6", "it laps the dish almost twice a second"),
        ("12", "bring a magnet close…"),
        ("16 ▲", "…and it locks on"),
    ]),
    ("02", "what it can do", "the payoff", "16–30 s", 60.0, "1× from the lock; the flick lands on beat 24", [
        ("18", "drag the magnet: it follows, like it's on rails"),
        ("22", "flick it…"),
        ("24", "…it snaps off, and gets caught on the next lap"),
        ("27.5", "lift the magnet: it's free again"),
    ]),
    ("", "the question", "the question", "30–34 s", ("card", 7.0, "but what is\na plasmoid?"), "second line lands on the 32 s jump", [
        ("30", "but what is a plasmoid?"),
        ("32 ▲", "and why would a magnet grab it?"),
    ]),
    ("03", "what is a plasmoid", "building blocks", "34–46 s", 27.5, "steady", [
        ("34", "a blob of plasma that holds its shape and moves as one (labels: filament · foot)"),
        ("37", "plasma: gas whose electrons are knocked loose from their atoms"),
        ("40", "here a radio-frequency field shakes electrons in thin gas"),
        ("43", "they knock more loose, and the atoms glow as they settle"),
    ]),
    ("04", "the obvious answer", "the natural guess fails", "46–56 s", 41.0, "ends in the 54 s dip", [
        ("46", "magnets push on electric currents…"),
        ("48.5", "but this current flips millions of times a second"),
        ("51", "push one way, then the other: it averages out to zero"),
        ("54", "so what's left? (dip)"),
    ]),
    ("05", "curling electrons", "the key idea", "56–68 s", 54.0, "insight on the 56 s drop; β map into the breakdown", [
        ("56 ▲", "magnets curl electrons"),
        ("58.5", "β = how far an electron curls before it hits an atom"),
        ("61", "normal air: β ≈ 0.004. thin gas under a magnet: β ≈ 2.9"),
        ("64", "curled electrons linger, so the glow could favor the magnet"),
    ]),
    ("06", "the opening, explained", "back to the opening", "68–78 s", 61.5, "drag in the breakdown, flick on the 72 s drop", [
        ("68", "one idea, one small model: it reproduces everything you saw"),
        ("72 ▲", "the lock, the break-away, the recapture"),
        ("75", "the magnet's path is scripted; lock and release come out of the model"),
    ]),
    ("07", "the twist", "twist", "78–88 s", 76.5, "", [
        ("78", "but a detailed model of the glow disagrees"),
        ("80.5", "magnetism alone pushes the foot away"),
        ("83", "and the magnet is nickel-plated metal"),
        ("85.5", "metal near the electrode pulls the glow in: 2.7× harder, at 1 Torr"),
    ]),
    ("08", "the test", "open question", "88–96 s", 86.5, "", [
        ("88", "so the lock is probably electric, not magnetic (or it's where the foot re-forms)"),
        ("91.5", "the test: swap the magnet for an aluminum slug"),
        ("94", "still locks? it was never magnetism"),
    ]),
    ("", "end card", "sign-off", "96–100.6 s", 91.0, "fade out", [
        ("96", "vcad wordmark · build your imagination · vcad.io"),
    ]),
]

COLS, TW, TH = 5, 300, 392       # thumbnail: the picture area of a 720x940 crop
CW, CH = 340, 392 + 380
PAD = 24


def grab(video, t):
    raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-ss", f"{t}", "-i", video, "-frames:v", "1",
                          "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], capture_output=True, check=True).stdout
    img = Image.frombytes("RGB", (720, 1280), raw)
    return img.crop((0, 0, 720, 940)).resize((TW, TH), Image.LANCZOS)


def main():
    video, out = sys.argv[1], sys.argv[2]
    rows = (len(SCENES) + COLS - 1) // COLS
    W, H = COLS * CW + PAD, rows * CH + PAD + 70
    sheet = Image.new("RGB", (W, H), style.BGC)
    d = ImageDraw.Draw(sheet)
    tracked(d, (PAD, 30), "plasmoid explainer · storyboard (track from 0, 120 BPM)", F(18, True), LABEL)
    for i, (num, title, beat, span, ft, note, caps) in enumerate(SCENES):
        x = PAD + (i % COLS) * CW
        y = 70 + (i // COLS) * CH
        d.rectangle([x - 1, y - 1, x + TW, y + TH], outline=LINE_HI)
        if isinstance(ft, tuple):
            th = grab(video, ft[1])
            th = Image.blend(Image.new("RGB", th.size, style.BGC), th, 0.35)
            td = ImageDraw.Draw(th)
            td.multiline_text((TW / 2, TH / 2 - 20), ft[2], font=FS(34, True), fill=TEXT,
                              anchor="mm", align="center", spacing=8)
            sheet.paste(th, (x, y))
        else:
            sheet.paste(grab(video, ft), (x, y))
        ty = y + TH + 14
        if num:
            d.rectangle([x, ty, x + 44, ty + 28], fill=SEL)
            d.text((x + 22, ty + 14), num, font=F(18, True), fill=(0, 0, 0), anchor="mm")
        tracked(d, (x + (56 if num else 0), ty + 14), title, F(16, True), TEXT, "lm")
        d.text((x, ty + 44), f"{beat} · {span}", font=FS(16, True), fill=SEL, anchor="la")
        cy = ty + 70
        if note:
            for ln in style.wrap(d, "music: " + note, FS(14), TW):
                d.text((x, cy), ln, font=FS(14), fill=LABEL, anchor="la")
                cy += 19
            cy += 6
        for t, c in caps:
            drop = "▲" in t
            d.text((x, cy), t.replace(" ▲", ""), font=F(13, True), fill=SEL if drop else LABEL, anchor="la")
            if drop:
                d.text((x, cy + 17), "drop", font=F(11, True), fill=SEL, anchor="la")
            lines = style.wrap(d, c, FS(15), TW - 52)
            for ln in lines:
                d.text((x + 52, cy), ln, font=FS(15), fill=TEXT, anchor="la")
                cy += 20
            cy += 5 + (14 if drop and len(lines) == 1 else 0)
    sheet.save(out)


if __name__ == "__main__":
    main()
