#!/usr/bin/env python3
"""Storyboard for the reworked second half: the physics of where a plasma lives.
Mock frames drawn in the video's style; three panels reuse real frames.

    python3 storyboard2.py explainer.mp4 <assets> storyboard2.png
"""
import math
import os
import subprocess
import sys

import numpy as np
from PIL import Image

import style
from diagrams import ORANGE, arrow, dish, glow, head
from style import F, FS, LABEL, LINE_HI, SEL, TEXT, W, H, tracked

GREY = LABEL
OKC = style.OK


def grab(video, t):
    raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-ss", f"{t}", "-i", video, "-frames:v", "1",
                          "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], capture_output=True, check=True).stdout
    return Image.frombytes("RGB", (720, 1280), raw)


def blank(title):
    img = style.blank()
    head(style.draw(img), title)
    return img


C, R = (360, 520), 270
MAG = (250, 410)


def magnet_ring(d, c=MAG, r=34, metal=False):
    if metal:
        d.ellipse([c[0] - r, c[1] - r, c[0] + r, c[1] + r], fill=(190, 190, 198))
    for i in range(0, 360, 20):
        d.arc([c[0] - r - 6, c[1] - r - 6, c[0] + r + 6, c[1] + r + 6], i, i + 11, fill=(215, 215, 225), width=3)


def suspect_one_a():
    """electrons under the magnet curl and stay; elsewhere they fly off."""
    img = blank("suspect one: the magnet")
    d = style.draw(img)
    dish(d, C, R)
    magnet_ring(d)
    d.text((MAG[0], MAG[1] - 58), "under the magnet", font=FS(22), fill=TEXT, anchor="mm")
    rng = np.random.default_rng(3)
    for k in range(5):                                   # curled, staying put
        cx, cy = MAG[0] + rng.uniform(-22, 22), MAG[1] + rng.uniform(-22, 22)
        r = rng.uniform(7, 12)
        d.arc([cx - r, cy - r, cx + r, cy + r], 0, 300, fill=TEXT, width=2)
        d.ellipse([cx + r - 3, cy - 3, cx + r + 3, cy + 3], fill=TEXT)
    spot = (470, 620)
    d.text((spot[0], spot[1] + 70), "anywhere else", font=FS(22), fill=GREY, anchor="mm")
    for k in range(5):                                   # straight, leaving
        a = rng.uniform(0, 2 * math.pi)
        p1 = (spot[0] + 55 * math.cos(a), spot[1] + 55 * math.sin(a))
        arrow(d, spot, p1, GREY, width=2, head=10)
    return img


def suspect_one_b():
    """more electrons linger → brighter glow → the foot settles there."""
    img = blank("suspect one: the magnet")
    d = style.draw(img)
    dish(d, C, R)
    img = glow(img, [(MAG[0], MAG[1], 46, 0.55), (MAG[0] + 4, MAG[1] + 6, 12, 1.0)])
    d = style.draw(img)
    magnet_ring(d)
    pts = [(C[0] + 175 * math.cos(a), C[1] + 175 * math.sin(a)) for a in np.linspace(0.4, 3.6, 40)]
    for q in range(1, len(pts)):
        d.line([pts[q - 1], pts[q]], fill=style.mix((60, 36, 22), ORANGE, 0.6 * q / len(pts)), width=3)
    arrow(d, pts[-1], (MAG[0] + 30, MAG[1] + 28), SEL, width=4, head=14)
    d.text((MAG[0] + 70, MAG[1] - 40), "brightest here", font=FS(22, True), fill=SEL, anchor="lm")
    d.text((C[0], C[1] + R + 40), "the foot slides in and stays", font=FS(24), fill=TEXT, anchor="mm")
    return img


def suspect_two_b():
    """side view: metal over the electrode bunches the electric field."""
    img = blank("lever two: the electric field")
    d = style.draw(img)
    y_dish = 700
    d.line([(90, y_dish), (630, y_dish)], fill=(150, 100, 70), width=6)
    d.text((630, y_dish + 30), "dish (high voltage)", font=FS(22), fill=GREY, anchor="rm")
    mx, my = 360, 360
    d.rounded_rectangle([mx - 60, my - 34, mx + 60, my + 34], radius=8, fill=(190, 190, 198))
    d.text((mx, my), "metal", font=FS(24, True), fill=(30, 30, 30), anchor="mm")
    for x in range(120, 620, 50):                         # field lines, drawn toward the metal near it
        pull = math.exp(-((x - mx) / 150.0) ** 2)
        top = (x + (mx - x) * 0.85 * pull, 240 + (my + 34 - 240) * pull)
        pts = [(x + (top[0] - x) * s ** 1.6, y_dish - (y_dish - top[1]) * s) for s in np.linspace(0, 1, 18)]
        d.line(pts, fill=style.mix(LINE_HI, TEXT, pull), width=2)
    img = glow(img, [(mx, y_dish - 8, 40, 0.7)])
    d = style.draw(img)
    d.text((mx, y_dish - 70), "field crowds in: glow brightens", font=FS(22, True), fill=SEL, anchor="mm")
    d.text((mx, 820), "heating grows as field squared", font=FS(24), fill=TEXT, anchor="mm")
    return img


def gain_loss():
    """a plasma lives where electrons gain energy faster than they lose it."""
    img = blank("where a plasma can live")
    d = style.draw(img)
    cx, cy = 360, 470
    d.ellipse([cx - 150, cy - 150, cx + 150, cy + 150], outline=LINE_HI, width=2)
    img = glow(img, [(cx, cy, 60, 0.8)])
    d = style.draw(img)
    for a in (200, 250, 290, 340):                       # gain: the RF field feeds electrons
        r0, r1 = 235, 170
        p0 = (cx + r0 * math.cos(math.radians(a)), cy + r0 * math.sin(math.radians(a)))
        p1 = (cx + r1 * math.cos(math.radians(a)), cy + r1 * math.sin(math.radians(a)))
        arrow(d, p0, p1, SEL, width=4, head=14)
    for a in (20, 70, 110, 160):                         # loss: electrons and heat leak away
        r0, r1 = 110, 215
        p0 = (cx + r0 * math.cos(math.radians(a)), cy + r0 * math.sin(math.radians(a)))
        p1 = (cx + r1 * math.cos(math.radians(a)), cy + r1 * math.sin(math.radians(a)))
        arrow(d, p0, p1, GREY, width=3, head=12, dashed=True)
    d.text((cx, cy - 265), "gain: the field heats electrons", font=FS(24, True), fill=SEL, anchor="mm")
    d.text((cx, cy + 255), "loss: electrons and heat leak out", font=FS(24), fill=GREY, anchor="mm")
    d.text((cx, 830), "gain > loss: it stays lit", font=FS(30, True), fill=TEXT, anchor="mm")
    return img


def lever_one():
    """the magnet cuts the loss: electron temperature map on the dish."""
    img = blank("lever one: the magnet cuts the loss")
    d = style.draw(img)
    dish(d, C, R)
    img = glow(img, [(MAG[0], MAG[1], 70, 0.35), (MAG[0], MAG[1], 40, 0.6)])
    d = style.draw(img)
    magnet_ring(d)
    for a in range(0, 360, 45):                          # leak arrows, short under the magnet
        p0 = (MAG[0] + 44 * math.cos(math.radians(a)), MAG[1] + 44 * math.sin(math.radians(a)))
        p1 = (MAG[0] + 64 * math.cos(math.radians(a)), MAG[1] + 64 * math.sin(math.radians(a)))
        arrow(d, p0, p1, GREY, width=2, head=8)
    d.text((MAG[0] + 90, MAG[1] - 30), "curled electrons", font=FS(22, True), fill=TEXT, anchor="lm")
    d.text((MAG[0] + 90, MAG[1]), "leak heat slowly", font=FS(22, True), fill=TEXT, anchor="lm")
    d.text((C[0], C[1] + 130), "hotter electrons ionize far more", font=FS(24), fill=SEL, anchor="mm")
    d.text((C[0], C[1] + R + 40), "electron temperature, from the glow simulation", font=FS(20), fill=GREY, anchor="mm")
    return img


def synthesis():
    """the foot sits where the plasma is easiest to sustain; both levers move that spot."""
    img = blank("the foot goes where it is easiest to stay lit")
    d = style.draw(img)
    dish(d, C, R)
    img = glow(img, [(MAG[0], MAG[1], 50, 0.5), (MAG[0] + 4, MAG[1] + 6, 12, 1.0)])
    d = style.draw(img)
    magnet_ring(d, metal=True)
    d.text((MAG[0] + 70, MAG[1] - 34), "traps electron heat", font=FS(22, True), fill=TEXT, anchor="lm")
    d.text((MAG[0] + 70, MAG[1] - 4), "crowds the electric field", font=FS(22, True), fill=TEXT, anchor="lm")
    bx, by = 150, 640
    d.text((bx, by - 34), "at 1 Torr", font=FS(22), fill=GREY, anchor="lm")
    d.rectangle([bx, by, bx + 110, by + 26], fill=TEXT)
    d.text((bx + 122, by + 13), "magnetic", font=FS(22), fill=TEXT, anchor="lm")
    d.rectangle([bx, by + 40, bx + 297, by + 66], fill=SEL)
    d.text((bx + 309, by + 53), "electric: 2.7×", font=FS(22, True), fill=SEL, anchor="lm")
    return img


def sun_callback(assets):
    im = Image.open(os.path.join(assets, "sdo", "0060.jpg")).convert("RGB")
    k = 1280 / 1080
    im = im.resize((int(im.width * k), int(im.height * k)), Image.LANCZOS)
    img = Image.new("RGB", (W, H), (0, 0, 0))
    img.paste(im, (-95, 0))
    d = style.draw(img)
    tracked(d, (40, 40), "real footage · NASA / SDO", F(16, True), LABEL)
    d.text((W / 2, 880), "plasma follows field lines", font=FS(34, True), fill=TEXT, anchor="mm",
           stroke_width=5, stroke_fill=(0, 0, 0))
    return img


def hand_callback(assets):
    im = Image.open(os.path.join(assets, "xe_hand", "0060.jpg")).convert("RGB")
    im = im.resize((int(im.width * 1.4), int(im.height * 1.4)), Image.LANCZOS)
    img = Image.new("RGB", (W, H), (0, 0, 0))
    img.paste(im, (-332, 110))
    d = style.draw(img)
    tracked(d, (40, 40), "real footage · Carl Willis · CC BY 3.0", F(16, True), LABEL)
    d.text((W / 2, 880), "a conductor does this alone", font=FS(34, True), fill=SEL, anchor="mm",
           stroke_width=5, stroke_fill=(0, 0, 0))
    return img


def main():
    video, assets, out = sys.argv[1:4]
    panels = [
        ("the push averages out", "46–56 s · kept", "ends in the 54 s dip", grab(video, 52.0), [
            "magnets push on electric currents…", "but this current flips millions of times a second",
            "it averages out to zero", "so what does a magnet do to a plasma?"]),
        ("magnets curl electrons", "56–64 s · on the drop", "56 s drop", grab(video, 60.0), [
            "a magnetic field bends every moving electron into a circle",
            "between hits on atoms it turns about half a circle",
            "so it can't wander off: it is tied to the field"]),
        ("only in thin gas", "64–68 s", "", grab(video, 62.6), [
            "in normal air it hits an atom before it can curl at all",
            "thin the gas 760-fold and the curl takes over"]),
        ("the same physics, sun-sized", "68–72 s · breakdown", "quiet stretch", sun_callback(assets), [
            "this is why the sun's plasma traces arcs", "it is following magnetic field lines"]),
        ("where a plasma can live", "72–78 s · on the drop", "72 s drop", gain_loss(), [
            "a plasma stays lit only where electrons gain energy faster than they lose it"]),
        ("lever one: cut the loss", "78–84 s", "", lever_one(), [
            "under the magnet, curled electrons leak heat slowly",
            "hotter electrons ionize far more: the gas there is easier to keep lit"]),
        ("lever two: raise the gain", "84–90 s", "", suspect_two_b(), [
            "the magnet is also metal: it crowds the electric field beneath it",
            "the same thing drew the thread to the hand"]),
        ("the foot follows the balance", "90–96 s, then end card", "fade from 98.6", synthesis(), [
            "the foot settles where the plasma is easiest to sustain",
            "at this pressure the electric lever is 2.7× the magnetic one",
            "thin the gas and the magnetic lever grows"]),
    ]
    COLS, TW, TH, CW, CH, PAD = 4, 300, 392, 340, 392 + 290, 24
    rows = (len(panels) + COLS - 1) // COLS
    sheet = Image.new("RGB", (COLS * CW + PAD, rows * CH + PAD + 70), style.BGC)
    d = style.draw(sheet)
    tracked(d, (PAD, 30), "plasmoid explainer · second half: the physics of where a plasma lives", F(18, True), LABEL)
    for i, (title, span, music, im, caps) in enumerate(panels):
        x, y = PAD + (i % COLS) * CW, 70 + (i // COLS) * CH
        sheet.paste(im.crop((0, 0, 720, 940)).resize((TW, TH), Image.LANCZOS), (x, y))
        d.rectangle([x - 1, y - 1, x + TW, y + TH], outline=LINE_HI)
        ty = y + TH + 14
        tracked(d, (x, ty + 8), title, F(15, True), TEXT, "lm")
        d.text((x, ty + 28), span, font=FS(16, True), fill=SEL, anchor="la")
        cy = ty + 54
        if music:
            d.text((x, cy), "music: " + music, font=FS(14), fill=LABEL, anchor="la")
            cy += 24
        for c in caps:
            for ln in style.wrap(d, "• " + c, FS(15), TW):
                d.text((x, cy), ln, font=FS(15), fill=TEXT, anchor="la")
                cy += 20
            cy += 4
    sheet.save(out)


if __name__ == "__main__":
    main()
