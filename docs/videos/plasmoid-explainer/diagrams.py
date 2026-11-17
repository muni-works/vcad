"""Procedural diagram scenes for the plasmoid explainer (make_video.py).

Each scene is `fn(u, t, assets) -> PIL.Image` (u in [0, 1) across the scene,
t in seconds since its start). Simulations are precomputed once per process
with a fixed seed, so every render of a frame is identical. They are
teaching cartoons: the *qualitative* behaviour (who moves, which way, by how
much relative to what) follows docs/plasmoid-sim.md and
docs/plasmoid-fluid-m0.md; pixel distances and visual speeds are not physical.
"""
import math
import os
from functools import lru_cache

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

import style
from style import FS, F, W, H, FPS

BG = style.BGC
ORANGE = (255, 150, 60)          # the plasma's own glow colour (renders match)
HOT = style.SEL                  # the one accent
WHITE = style.TEXT
GREY = style.LABEL
DIM = style.LINE_HI
COOL = style.TEXT                # electrons, and the magnetic arrow


F_HEAD = F(26, True)
F_LABEL = FS(28)
F_SMALL = FS(23)
F_BIG = FS(110, True)


def smooth(u):
    u = min(max(u, 0.0), 1.0)
    return u * u * (3 - 2 * u)


def fade(t, t_on, dur=0.4):
    return style.ease((t - t_on) / dur)


def col(c, a):
    return tuple(int(v * a + b * (1 - a)) for v, b in zip(c, BG))


def blank():
    return style.blank()


def arrow(d, p0, p1, color, width=6, head=22, dashed=False):
    x0, y0 = p0
    x1, y1 = p1
    L = math.hypot(x1 - x0, y1 - y0)
    if L < 1:
        return
    ux, uy = (x1 - x0) / L, (y1 - y0) / L
    bx, by = x1 - ux * head, y1 - uy * head
    if dashed:
        n = int(L // 18)
        for i in range(n):
            s0, s1 = i * 18, min(i * 18 + 10, L - head)
            if s0 >= L - head:
                break
            d.line([(x0 + ux * s0, y0 + uy * s0), (x0 + ux * s1, y0 + uy * s1)],
                   fill=color, width=width)
    else:
        d.line([(x0, y0), (bx, by)], fill=color, width=width)
    px, py = -uy, ux
    d.polygon([(x1, y1), (bx + px * head * 0.55, by + py * head * 0.55),
               (bx - px * head * 0.55, by - py * head * 0.55)], fill=color)


def glow_layer(points, size=(W, H)):
    """Additive orange glow: points = [(x, y, radius, intensity)]."""
    layer = Image.new("RGB", size, (0, 0, 0))
    d = ImageDraw.Draw(layer)
    for x, y, r, k in points:
        c = tuple(int(v * k) for v in ORANGE)
        d.ellipse([x - r, y - r, x + r, y + r], fill=c)
    halo = layer.filter(ImageFilter.GaussianBlur(12))
    core = layer.filter(ImageFilter.GaussianBlur(3))
    return halo, core


def add(img, layer):
    a = np.asarray(img, dtype=np.int16) + np.asarray(layer, dtype=np.int16)
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8))


def head(d, text, y=120, a=1.0):
    style.tracked(d, (W / 2, y), text, F_HEAD, col(GREY, a), "mm")


# ── scene 2: what's glowing ──────────────────────────────────────────────
GAS_BOX = (60, 200, 660, 820)
GAS_FIELD_ON, GAS_ION_ON = 6.0, 8.6
GAS_T = 12.0


@lru_cache(maxsize=1)
def gas_sim():
    rng = np.random.default_rng(7)
    x0, y0, x1, y1 = GAS_BOX
    atoms = np.column_stack([rng.uniform(x0 + 30, x1 - 30, 26),
                             rng.uniform(y0 + 30, y1 - 30, 26)])
    e_pos = [np.array([rng.uniform(x0 + 40, x1 - 40), rng.uniform(y0 + 40, y1 - 40)])
             for _ in range(5)]
    e_vel = [rng.normal(0, 1.2, 2) for _ in range(5)]
    cool = np.zeros(len(atoms))          # glow timers (s)
    frames = []
    n = int(GAS_T * FPS)
    for k in range(n):
        t = k / FPS
        E = math.sin(2 * math.pi * 1.0 * t) * smooth((t - GAS_FIELD_ON) / 0.6)
        new_pos, new_vel = [], []
        for p, v in zip(e_pos, e_vel):
            v = v * 0.985 + np.array([0.0, 1.1 * E]) + rng.normal(0, 0.25, 2)
            p = p + v
            for a in (0, 1):
                lo, hi = (x0, x1) if a == 0 else (y0, y1)
                if p[a] < lo + 6 or p[a] > hi - 6:
                    v[a] = -v[a]
                    p[a] = min(max(p[a], lo + 6), hi - 6)
            dist = np.hypot(*(atoms - p).T)
            j = int(np.argmin(dist))
            if dist[j] < 18:
                speed = float(np.hypot(*v))
                if t > GAS_ION_ON and speed > 4.5 and cool[j] <= 0 and len(e_pos) + len(new_pos) < 34:
                    cool[j] = 1.4
                    ang = rng.uniform(0, 2 * math.pi)
                    new_pos.append(atoms[j] + 20 * np.array([math.cos(ang), math.sin(ang)]))
                    new_vel.append(2.0 * np.array([math.cos(ang), math.sin(ang)]))
                ang = rng.uniform(0, 2 * math.pi)
                v = 0.6 * speed * np.array([math.cos(ang), math.sin(ang)])
                p = atoms[j] + 19 * (p - atoms[j]) / max(dist[j], 1e-6)
            new_pos.append(p)
            new_vel.append(v)
        e_pos, e_vel = new_pos, new_vel
        cool = np.maximum(cool - 1 / FPS, 0)
        frames.append((E, [q.copy() for q in e_pos], cool.copy()))
    return atoms, frames


def scene_gas(u, t, assets):
    atoms, frames = gas_sim()
    E, electrons, cool = frames[min(int(t * FPS), len(frames) - 1)]
    img = blank()
    d = ImageDraw.Draw(img)
    x0, y0, x1, y1 = GAS_BOX
    a_box = fade(t, 0.2)
    d.rounded_rectangle(GAS_BOX, radius=24, outline=col(DIM, a_box), width=2)
    a_atoms = fade(t, 3.0)
    glows = []
    for (ax, ay), c in zip(atoms, cool):
        d.ellipse([ax - 13, ay - 13, ax + 13, ay + 13], fill=col(style.RAISED, a_atoms),
                  outline=col(style.LINE_HI, a_atoms), width=2)
        if c > 0:
            glows.append((ax, ay, 16, min(1.0, c / 0.7)))
    if a_atoms > 0 and t < 6.2:
        ax, ay = atoms[0]
        d.line([(ax + 14, ay - 10), (ax + 60, ay - 44)], fill=col(GREY, a_atoms), width=2)
        d.text((ax + 64, ay - 48), "argon atom", font=F_SMALL, fill=col(GREY, a_atoms), anchor="lm")
    a_e = fade(t, 3.6)
    for p in electrons:
        d.ellipse([p[0] - 4, p[1] - 4, p[0] + 4, p[1] + 4], fill=col(COOL, a_e))
    if 6.0 < t < 9.0 and electrons:
        p = electrons[0]
        a = fade(t, 6.2) * (1 - fade(t, 8.6))
        d.text((p[0] + 10, p[1] - 14), "electron", font=F_SMALL, fill=col(COOL, a), anchor="lb")
    # the RF field: a flipping arrow at the side
    a_f = fade(t, GAS_FIELD_ON)
    if a_f > 0:
        cy = (y0 + y1) / 2
        L = 150 * E
        arrow(d, (30, cy - L / 2), (30, cy + L / 2 + (1 if L >= 0 else -1)), col(WHITE, a_f), width=5, head=16)
        d.text((W / 2, y1 + 40), "RF field  ↕  (flipping)", font=F_SMALL, fill=col(GREY, a_f), anchor="mm")
    if glows:
        halo, core = glow_layer(glows)
        img = add(add(img, halo), core)
    return img


# ── scene 4: RF averages the push away ───────────────────────────────────
RF_F = 1.0  # one flip per beat at 120 BPM


def scene_rf(u, t, assets):
    img = blank()
    d = ImageDraw.Draw(img)
    s = math.sin(2 * math.pi * RF_F * max(t - 4.0, 0)) if t > 4.0 else 1.0
    # the filament as a straight glowing wire, and a magnet under it
    wy = 330
    halo, core = glow_layer([(x, wy, 9, 0.8) for x in range(140, 590, 6)])
    img = add(add(img, halo), core)
    d = ImageDraw.Draw(img)
    d.text((600, wy - 34), "filament", font=F_LABEL, fill=ORANGE, anchor="mm")
    mx0, my0 = 290, 500
    d.rectangle([mx0, my0, mx0 + 140, my0 + 40], fill=(170, 60, 60))
    d.rectangle([mx0, my0 + 40, mx0 + 140, my0 + 80], fill=(60, 90, 170))
    d.text((mx0 + 70, my0 + 20), "N", font=F_LABEL, fill=WHITE, anchor="mm")
    d.text((mx0 + 70, my0 + 60), "S", font=F_LABEL, fill=WHITE, anchor="mm")
    d.text((W / 2, my0 + 112), "magnet", font=F_LABEL, fill=GREY, anchor="mm")
    for fx in (mx0 + 30, mx0 + 110):
        arrow(d, (fx, my0 - 8), (fx, wy + 70), DIM, width=3, head=12, dashed=True)
    d.text((mx0 + 150, (my0 + wy) / 2 + 30), "field", font=F_SMALL, fill=GREY, anchor="lm")
    a_i = fade(t, 4.0)
    if a_i > 0:
        x_mid = W / 2
        L = 150 * (1 if s >= 0 else -1)
        arrow(d, (x_mid - L / 2, wy + 40), (x_mid + L / 2, wy + 40), col(WHITE, a_i), width=5, head=18)
        d.text((x_mid + 200, wy + 40), "current", font=F_SMALL, fill=col(WHITE, a_i), anchor="lm")
        # current along x, the magnet's field straight up through the wire:
        # I×B points out of / into the screen. ⊙ = out, ⊗ = in.
        cx, cy, R = x_mid, wy - 90, 30 * (0.6 + 0.4 * abs(s))
        c = col(HOT, a_i)
        d.ellipse([cx - R, cy - R, cx + R, cy + R], outline=c, width=5)
        if s >= 0:
            d.ellipse([cx - 7, cy - 7, cx + 7, cy + 7], fill=c)
        else:
            k = R * 0.62
            d.line([(cx - k, cy - k), (cx + k, cy + k)], fill=c, width=5)
            d.line([(cx - k, cy + k), (cx + k, cy - k)], fill=c, width=5)
        d.text((cx - 50, cy), "push", font=F_SMALL, fill=c, anchor="rm")
        d.text((cx + 50, cy), "out of the screen" if s >= 0 else "into the screen",
               font=F_SMALL, fill=c, anchor="lm")
    a_p = fade(t, 7.5)
    if a_p > 0:
        # scrolling trace of the push, with its running average
        px0, px1, py, amp = 80, 640, 760, 70
        d.line([(px0, py), (px1, py)], fill=col(DIM, a_p), width=2)
        d.text((px0, py - amp - 26), "push over time", font=F_SMALL, fill=col(GREY, a_p), anchor="lm")
        tt = max(t - 4.0, 0)
        pts = []
        span = 3.0
        for i in range(0, 161):
            x = px0 + (px1 - px0) * i / 160
            ts = tt - span + span * i / 160
            pts.append((x, py - amp * (math.sin(2 * math.pi * RF_F * ts) if ts > 0 else 1.0)))
        d.line(pts, fill=col(HOT, a_p), width=3)
        d.text((W / 2, py + amp + 34), "(slowed way down: really millions of flips a second)",
               font=F_SMALL, fill=col(GREY, a_p), anchor="mm")
    a_avg = fade(t, 11.0)
    if a_avg > 0:
        px0, px1, py = 80, 640, 760
        for x in range(px0, px1, 24):
            d.line([(x, py), (x + 12, py)], fill=col(WHITE, a_avg), width=5)
        d.text((px1, py - 16), "average = 0", font=F_LABEL, fill=col(WHITE, a_avg), anchor="rb")
    return img


# ── scene 5: magnets curl electrons (β) ──────────────────────────────────
BETA_PANELS = [(60, 190, 660, 480), (60, 540, 660, 830)]
BETA_T = 14.0


def _walk(rng, box, steps, speed, omega, tau):
    """An electron that curls at `omega` rad/frame and scatters every ~tau frames."""
    x0, y0, x1, y1 = box
    p = np.array([(x0 + x1) / 2, (y0 + y1) / 2])
    ang = rng.uniform(0, 2 * math.pi)
    next_hit = rng.exponential(tau)
    out = []
    for _ in range(steps):
        ang += omega
        p = p + speed * np.array([math.cos(ang), math.sin(ang)])
        if p[0] < x0 + 10 or p[0] > x1 - 10:
            ang = math.pi - ang
        if p[1] < y0 + 10 or p[1] > y1 - 10:
            ang = -ang
        p = np.clip(p, [x0 + 10, y0 + 10], [x1 - 10, y1 - 10])
        next_hit -= 1
        if next_hit <= 0:
            ang = rng.uniform(0, 2 * math.pi)
            next_hit = rng.exponential(tau)
        out.append(p.copy())
    return out


@lru_cache(maxsize=1)
def beta_sims():
    rng = np.random.default_rng(11)
    n = int(BETA_T * FPS)
    # 1 atm: β ≈ 0.004, so straight hops (omega·tau = 0.004).
    air = _walk(rng, BETA_PANELS[0], n, 9.0, 0.004 / 2.0, 2.0)
    # 1 Torr under the magnet: β ≈ 2.9, an electron turns 2.9 rad between hits.
    tau = 12.0
    thin = _walk(rng, BETA_PANELS[1], n, 6.0, 2.9 / tau, tau)
    return air, thin


def scene_beta(u, t, assets):
    air, thin = beta_sims()
    img = blank()
    d = ImageDraw.Draw(img)
    k = min(int(t * FPS), len(air) - 1)
    a_map = fade(t, 11.0, 0.5)
    a_p = 1 - a_map
    labels = [("normal air · 1 atm", "β ≈ 0.004", 5.5), ("thin gas, under the magnet · 1 Torr", "β ≈ 2.9", 8.5)]
    for box, path, (title, beta, t_beta) in zip(BETA_PANELS, (air, thin), labels):
        d.rounded_rectangle(box, radius=20, outline=col(DIM, a_p), width=2)
        d.text((box[0] + 16, box[1] + 14), title, font=F_SMALL, fill=col(GREY, a_p), anchor="la")
        a_b = fade(t, t_beta) * a_p
        if a_b > 0:
            d.text((box[2] - 16, box[1] + 14), beta, font=F_LABEL, fill=col(HOT, a_b), anchor="ra")
        trail = path[max(0, k - 75):k + 1]
        for i in range(1, len(trail)):
            a = (i / len(trail)) * a_p
            d.line([tuple(trail[i - 1]), tuple(trail[i])], fill=col(COOL, a), width=3)
        if trail:
            x, y = trail[-1]
            d.ellipse([x - 6, y - 6, x + 6, y + 6], fill=col(WHITE, a_p))
    if a_p > 0:
        bx, by = BETA_PANELS[1][2] - 30, BETA_PANELS[1][3] - 30
        d.ellipse([bx - 14, by - 14, bx + 14, by + 14], outline=col(GREY, a_p), width=2)
        d.ellipse([bx - 3, by - 3, bx + 3, by + 3], fill=col(GREY, a_p))
        d.text((bx - 24, by), "magnet's field", font=F_SMALL, fill=col(GREY, a_p), anchor="rm")
    if a_map > 0:
        m = Image.open(os.path.join(assets, "beta-map.png")).convert("RGB")
        m = m.resize((680, int(m.height * 680 / m.width)), Image.LANCZOS)
        m = Image.blend(Image.new("RGB", m.size, BG), m, a_map)
        img.paste(m, ((W - m.width) // 2, 300))
        d = ImageDraw.Draw(img)
        d.text((W / 2, 250), "β on the dish, magnet 10 mm up", font=F_LABEL, fill=col(WHITE, a_map), anchor="mm")
    return img


# ── scene 7: which way is the foot pulled ────────────────────────────────
def _dish(d, c, r, a=1.0):
    d.ellipse([c[0] - r, c[1] - r, c[0] + r, c[1] + r], fill=col((60, 36, 22), a),
              outline=col((150, 100, 70), a), width=3)


def scene_pull(u, t, assets):
    img = blank()
    d = ImageDraw.Draw(img)
    head(d, "the dish, from above", 150)
    c, r = (360, 470), 250
    _dish(d, c, r)
    foot = np.array([270.0, 545.0])
    mag = np.array([470.0, 385.0])
    to_mag = (mag - foot) / np.linalg.norm(mag - foot)
    perp = np.array([-to_mag[1], to_mag[0]])
    a_ni = fade(t, 6.0)
    ring = col((200, 200, 210), 0.5 + 0.5 * a_ni)
    for i in range(0, 360, 20):
        d.arc([mag[0] - 40, mag[1] - 40, mag[0] + 40, mag[1] + 40], i, i + 11, fill=ring, width=3)
    if a_ni > 0:
        d.ellipse([mag[0] - 30, mag[1] - 30, mag[0] + 30, mag[1] + 30], fill=col((185, 185, 195), a_ni))
        d.ellipse([mag[0] - 18, mag[1] - 22, mag[0] - 4, mag[1] - 10], fill=col((250, 250, 255), a_ni))
        d.text((mag[0], mag[1] - 62), "nickel-plated: a conductor", font=F_SMALL,
               fill=col(WHITE, a_ni), anchor="mm")
    d.text((mag[0] + 50, mag[1] + 6), "magnet\nabove", font=F_SMALL, fill=GREY, anchor="lm")
    halo, core = glow_layer([(foot[0], foot[1], 14, 1.0)])
    img = add(add(img, halo), core)
    d = ImageDraw.Draw(img)
    d.text((foot[0] - 26, foot[1] + 4), "foot", font=F_SMALL, fill=ORANGE, anchor="rm")
    unit = 58  # the derived magnetic pull's length; the metal pull is 2.7x

    a1 = fade(t, 0.4)
    a2 = fade(t, 3.0)
    a4 = fade(t, 9.0)
    o1 = foot + perp * 30
    arrow(d, tuple(o1 + to_mag * 20), tuple(o1 + to_mag * 120), col(GREY, a1), width=4, head=16, dashed=True)
    arrow(d, tuple(foot - to_mag * 22), tuple(foot - to_mag * (22 + unit)), col(COOL, a2), width=7, head=20)
    o4 = foot - perp * 30
    arrow(d, tuple(o4 + to_mag * 20), tuple(o4 + to_mag * (20 + unit * 2.7)), col(HOT, a4), width=9, head=24)

    # legend under the dish
    rows = [(a1, GREY, True, 1.0, "our small model assumed: pulled in"),
            (a2, COOL, False, -1.0, "glow model: magnetism pushes away"),
            (a4, HOT, False, 2.7, "metal: pulls in, 2.7× as hard")]
    for i, (a, color, dashed, k, text) in enumerate(rows):
        y = 770 + 42 * i
        x0 = 70
        L = 34 * abs(k)
        p0, p1 = ((x0, y), (x0 + L, y)) if k > 0 else ((x0 + 58, y), (x0 + 58 - 58, y))
        if k < 0:
            p0, p1 = (x0 + 58, y), (x0, y)
        arrow(d, p0, p1, col(color, a), width=5 if not dashed else 3, head=14, dashed=dashed)
        d.text((x0 + 110, y), text, font=F_SMALL, fill=col(color, a), anchor="lm")
    a_note = fade(t, 3.0)
    d.text((W / 2, 905), "arrow lengths: direction and ratio, not speed", font=F_SMALL,
           fill=col(GREY, a_note), anchor="mm")
    return img


# ── scene 8: the experiment, then the end card ───────────────────────────
def scene_test(u, t, assets):
    img = blank()
    d = ImageDraw.Draw(img)
    head(d, "the test", 170)
    for cx, title, metal, label in [(190, "magnet", (190, 190, 198), "N"),
                                    (530, "aluminum slug", (165, 168, 176), "Al")]:
        _dish(d, (cx, 500), 140)
        d.text((cx, 320), title, font=F_LABEL, fill=WHITE, anchor="mm")
        ang = 2 * math.pi * 0.5 * t
        fx, fy = cx + 100 * math.cos(ang), 500 + 100 * math.sin(ang)
        halo, core = glow_layer([(fx, fy, 9, 1.0)])
        img = add(add(img, halo), core)
        d = ImageDraw.Draw(img)
        a_m = style.ease(style.seg(t, 4.0, 4.5))
        if a_m > 0:
            my = 500 - 60 * (1 - a_m)
            mx = cx + 70
            d.ellipse([mx - 24, my - 84, mx + 24, my - 36], fill=col(metal, a_m))
            d.text((mx, my - 60), label, font=F_SMALL, fill=col((30, 30, 30), a_m), anchor="mm")
        if t >= 7.0:
            d.text((cx, 700), "locks?", font=FS(30, True), fill=HOT, anchor="mm")
    return img


DIAGRAMS = {
    "gas": scene_gas,
    "rf": scene_rf,
    "beta": scene_beta,
    "pull": scene_pull,
    "test": scene_test,
}
