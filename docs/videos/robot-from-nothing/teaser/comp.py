#!/usr/bin/env python3
"""Composite the robot-from-nothing teaser: Pilot Light cards + vcad-render shots.

  python3 comp.py <shots_dir> <trace.csv> <fonts_dir> <out_frames_dir>
  ffmpeg -framerate 30 -i out/f_%05d.png -c:v libx264 -pix_fmt yuv420p -crf 18 teaser.mp4

Look: Pilot Light (frame-bootc/branding/theme/palette.toml [warm]): warm black,
IBM Plex Sans/Mono, zero radius, one orange for focus only. The vcad-render
frames are `--theme dark` RGBA line art laid onto the same warm black.
Silent: VO and music are added in the edit.
"""
import csv, os, sys
from PIL import Image, ImageDraw, ImageFont

shots, trace_path, fonts, out = sys.argv[1:5]
os.makedirs(out, exist_ok=True)
W, H, FPS = 1920, 1080, 30
BG, TEXT, LABEL, FAINT, LINE, SEL = '#0b0907', '#e8e1d2', '#a08d70', '#6b5a44', '#2a2119', '#ff6a00'


def font(name, size):
    return ImageFont.truetype(os.path.join(fonts, name), size)


SANS_L, SANS, MONO = 'IBMPlexSans-Light.otf', 'IBMPlexSans-Regular.otf', 'IBMPlexMono-Regular.otf'
n = 0


def emit(img):
    global n
    img.save(os.path.join(out, f'f_{n:05d}.png'))
    n += 1


def frame_bg():
    return Image.new('RGB', (W, H), BG)


def chrome(d, right='ep 1 · teaser'):
    """the same quiet header on every frame: series name left, episode right."""
    d.text((72, 56), 'vcad — build a robot from nothing', font=font(MONO, 22), fill=LABEL)
    w = d.textlength(right, font=font(MONO, 22))
    d.text((W - 72 - w, 56), right, font=font(MONO, 22), fill=LABEL)


def fade(img, a):
    """fade toward the warm black (a=1 full image, 0 = black)."""
    if a >= 1:
        return img
    return Image.blend(frame_bg(), img, max(0.0, a))


def card(lines, secs, label=None, caret=False, accent=None, size=88):
    """centred text card. `accent` = substring drawn in the focus orange."""
    total = int(secs * FPS)
    for i in range(total):
        img = frame_bg()
        d = ImageDraw.Draw(img)
        chrome(d)
        f = font(SANS_L, size)
        lh = int(size * 1.25)
        y = H // 2 - lh * len(lines) // 2
        if label:
            d.text((W // 2 - d.textlength(label, font=font(MONO, 26)) // 2, y - 70), label, font=font(MONO, 26), fill=LABEL)
        for line in lines:
            w = d.textlength(line, font=f)
            x = W // 2 - w // 2
            if accent and accent in line:
                a, b = line.split(accent, 1)
                d.text((x, y), a, font=f, fill=TEXT)
                xa = x + d.textlength(a, font=f)
                d.text((xa, y), accent, font=f, fill=SEL)
                d.text((xa + d.textlength(accent, font=f), y), b, font=f, fill=TEXT)
            else:
                d.text((x, y), line, font=f, fill=TEXT)
            end_x = x + w
            y += lh
        if caret and (i // 15) % 2 == 0:
            d.rectangle((end_x + 14, y - lh + 18, end_x + 30, y - lh + size + 6), fill=SEL)
        k = min(i, total - 1 - i)
        emit(fade(img, min(1.0, k / 8)))


def lower_third(d, title, sub):
    d.line((72, H - 190, 72 + 520, H - 190), fill=LINE, width=2)
    d.text((72, H - 172), title, font=font(SANS, 44), fill=TEXT)
    d.text((72, H - 112), sub, font=font(MONO, 26), fill=LABEL)


def shot(name, title=None, sub=None, lt_from=0.0, lt_to=1e9, pick=None, hud=None, tag=None, fade_in=8, fade_out=8):
    """an oversized render is cut to a W x H window centred on the union of every
    frame's drawn pixels: vcad-render fits the whole timeline's bounds, which
    leaves a shot that moves a lot small in frame, so those render big + crop."""
    files = sorted(f for f in os.listdir(os.path.join(shots, f'frames_{name}')) if f.endswith('.png'))
    idx = pick(len(files)) if pick else range(len(files))
    idx = list(idx)
    crop = None
    first = Image.open(os.path.join(shots, f'frames_{name}', files[0]))
    if first.size != (W, H):
        b = None
        for f in files[::6] + [files[-1]]:
            bb = Image.open(os.path.join(shots, f'frames_{name}', f)).getchannel('A').getbbox()
            b = bb if b is None else (min(b[0], bb[0]), min(b[1], bb[1]), max(b[2], bb[2]), max(b[3], bb[3]))
        cx, cy = (b[0] + b[2]) // 2, (b[1] + b[3]) // 2
        crop = (min(max(0, cx - W // 2), first.size[0] - W), min(max(0, cy - H // 2), first.size[1] - H))
        print(f'{name}: {first.size} drawn {b} -> crop at {crop}')
    for k, i in enumerate(idx):
        img = frame_bg()
        fr = Image.open(os.path.join(shots, f'frames_{name}', files[i])).convert('RGBA')
        if crop:
            fr = fr.crop((crop[0], crop[1], crop[0] + W, crop[1] + H))
        if fr.size != (W, H):
            fr = fr.resize((W, H))
        img.paste(fr, (0, 0), fr)
        d = ImageDraw.Draw(img)
        chrome(d)
        t = k / FPS
        if title and lt_from <= t <= lt_to:
            lower_third(d, title, sub)
        if hud:
            hud(d, i)
        if tag:
            d.text((W - 72 - d.textlength(tag, font=font(MONO, 40)), 110), tag, font=font(MONO, 40), fill=SEL)
        kk = min(k, len(idx) - 1 - k)
        emit(fade(img, min(1.0, kk / max(1, min(fade_in, fade_out)))))


def despeck(im, min_px=60):
    """drop tiny isolated blobs: hidden instances and framing anchors are 0.01 mm
    cubes, and the outliner still draws each as a dot."""
    import numpy as np
    from scipy import ndimage
    a = np.array(im)
    lab, n = ndimage.label(a[..., 3] > 0, structure=np.ones((3, 3)))
    size = ndimage.sum(np.ones_like(lab), lab, range(n + 1))
    a[size[lab] < min_px] = 0
    return Image.fromarray(a)


def stills(prefix, hold, last_hold, title=None, sub=None, lt_from=0.0, xf=6):
    """a shot built from still states (stills/<prefix>_NN.png): each held `hold` s,
    crossfading over `xf` frames into the next, the last held `last_hold` s.
    the animator can't hide or show parts, so build-up and cut-down are stills."""
    d0 = os.path.join(shots, 'stills')
    files = sorted(f for f in os.listdir(d0) if f.startswith(prefix + '_') and f.endswith('.png'))
    ims = [Image.open(os.path.join(d0, f)).convert('RGBA') for f in files]
    if ims[0].size != (W, H):  # oversized: crop around the fullest state (first or last)
        a, c = ims[0].getchannel('A').getbbox(), ims[-1].getchannel('A').getbbox()
        b = (min(a[0], c[0]), min(a[1], c[1]), max(a[2], c[2]), max(a[3], c[3]))
        cx, cy = (b[0] + b[2]) // 2, (b[1] + b[3]) // 2
        x0 = min(max(0, cx - W // 2), ims[0].size[0] - W)
        y0 = min(max(0, cy - H // 2), ims[0].size[1] - H)
        ims = [im.crop((x0, y0, x0 + W, y0 + H)) for im in ims]
    ims = [despeck(im) for im in ims]
    seq = []  # (image, image_to_blend_toward, alpha)
    for k, im in enumerate(ims):
        n_hold = int((last_hold if k == len(ims) - 1 else hold) * FPS)
        for j in range(n_hold):
            if k < len(ims) - 1 and j >= n_hold - xf:
                seq.append((im, ims[k + 1], (j - (n_hold - xf) + 1) / (xf + 1)))
            else:
                seq.append((im, None, 0))
    for k, (a, b, al) in enumerate(seq):
        img = frame_bg()
        layer = Image.new('RGBA', (W, H), (0, 0, 0, 0))
        layer.paste(a, (0, 0), a)
        if b is not None:
            lb = Image.new('RGBA', (W, H), (0, 0, 0, 0))
            lb.paste(b, (0, 0), b)
            layer = Image.blend(layer, lb, al)
        img.paste(layer, (0, 0), layer)
        d = ImageDraw.Draw(img)
        chrome(d)
        if title and k / FPS >= lt_from:
            lower_third(d, title, sub)
        kk = min(k, len(seq) - 1 - k)
        emit(fade(img, min(1.0, kk / 8)))


# gym trace for the fall HUD: video frame i of shot_fall -> sim time
rows = [(float(r['t_s']), float(r['tilt_deg']), float(r['base_z_mm'])) for r in csv.DictReader(open(trace_path))]
HOLD, SLOW = 1.0, 4.0


def sim_at(i):
    tv = i / FPS
    ts = max(0.0, (tv - HOLD) / SLOW)
    if tv < HOLD:
        return 0.0, 0.0, rows[0][2]
    prev = (0.0, 0.0, rows[0][2])
    for r in rows:
        if r[0] >= ts:
            a = 0 if r[0] == prev[0] else (ts - prev[0]) / (r[0] - prev[0])
            return ts, prev[1] + a * (r[1] - prev[1]), prev[2] + a * (r[2] - prev[2])
        prev = r
    return rows[-1]


def fall_hud(d, i):
    ts, tilt, z = sim_at(i)
    x0, y0 = W - 72 - 430, H - 230
    d.text((x0, y0), f't     {ts:5.2f} s', font=font(MONO, 30), fill=TEXT)
    d.text((x0, y0 + 44), f'tilt  {tilt:5.1f} °', font=font(MONO, 30), fill=TEXT)
    d.text((x0, y0 + 88), f'chest {z:5.0f} mm', font=font(MONO, 30), fill=TEXT)


# ---- the cut ------------------------------------------------------------------
card(['this document is empty.'], 5.5, caret=True)
stills('assemble', 0.6, 4.5, 'rose mini', '142 parts · 22 joints · one loon file', lt_from=12.6)
stills('cut', 0.9, 3.4, 'rml-12', '12 joints. the legs come first.', lt_from=2.8)
card(['then we dropped it', 'into the simulator.'], 2.6, size=76)
F = len([f for f in os.listdir(os.path.join(shots, 'frames_fall')) if f.endswith('.png')])
# real time: 1 s hold, then every 4th frame through the fall
shot('fall', 'vcad gym · 1 kHz physics · no policy', 'joint PD holding the zero pose',
     pick=lambda N: list(range(0, 30)) + list(range(30, N, 4)), hud=fall_hud, tag='1×', fade_out=1)
shot('fall', 'vcad gym · 1 kHz physics · no policy', 'joint PD holding the zero pose',
     pick=lambda N: range(24, N), hud=fall_hud, tag='¼×', fade_in=1)
card(["it can't walk yet."], 2.8, label="what vcad can't do — today", accent='yet')
card(["it can't talk to a motor yet."], 2.8, label="what vcad can't do — today", accent='yet')
card(["the file isn't the robot yet."], 2.8, label="what vcad can't do — today", accent='yet')
card(['~20 weeks.'], 3.2, label='one person · one file · critical path to a first walk', size=120)
card(['[ live action: the front door ]'], 2.4, label='placeholder', size=56)
card(['build a robot from nothing'], 5.0, label='ep 1 · the blank document', size=96)
print(f'{n} frames, {n / FPS:.1f} s')
