"""Diagram scenes for the plasmoid explainer (make_video.py).

Each scene is `fn(t, assets) -> PIL.Image`, t in seconds since its start,
and a pure function of t (fixed seeds), so any frame re-renders alone.

What is data and what is cartoon:
- `scene_beta`: electron paths are exact. Between collisions an electron
  gyrates on a circle (uniform B over the ~1 mm shown); free times are
  exponential with the kernel's collision frequency ν = n·σ·v̄ (argon,
  1e-19 m², 3 eV — `plasmoid::collision_frequency`), scattering is
  isotropic, and β = ω_ce/ν = 2.88 at 1 Torr under the magnet (61 mT,
  docs/plasmoid-sim.md). Lengths are to scale (see the scale bar).
- `scene_compare`: both foot trajectories are simulation output
  (`lock_compare.csv`, from examples/plasmoid_lock_compare.rs on the
  glow-fluid PR): the same magnet move under the reduced model's assumed
  pull and under the pull the glow-fluid model derives.
- `scene_gas`, `scene_rf`, `scene_test`: cartoons of the mechanism; pixel
  distances and speeds are not physical.
"""
import csv
import math
import os
from functools import lru_cache

import numpy as np
from PIL import Image, ImageFilter

import style
from style import FS, F, W, H, FPS, S, ease, seg

BG = style.BGC
ORANGE = (255, 150, 60)          # the plasma's own glow colour (matches the renders)
HOT = style.SEL                  # the one accent
WHITE = style.TEXT
GREY = style.LABEL
DIM = style.LINE_HI

F_HEAD = F(22, True)
F_LABEL = FS(28)
F_SMALL = FS(23)


def smooth(u):
    u = min(max(u, 0.0), 1.0)
    return u * u * (3 - 2 * u)


def fade(t, t_on, dur=0.4):
    return ease((t - t_on) / dur)


def col(c, a):
    return style.on_bg(c, a)


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


def glow(img, points):
    """Add an orange glow: points = [(x, y, radius, intensity)], logical."""
    layer = style.new("RGB", (0, 0, 0))
    d = style.draw(layer)
    for x, y, r, k in points:
        d.ellipse([x - r, y - r, x + r, y + r], fill=tuple(int(v * k) for v in ORANGE))
    out = np.asarray(img, dtype=np.int16)
    for rad in (12, 3):
        out = out + np.asarray(layer.filter(ImageFilter.GaussianBlur(rad * S)), dtype=np.int16)
    return Image.fromarray(np.clip(out, 0, 255).astype(np.uint8))


def head(d, text, y=150, a=1.0):
    style.tracked(d, (W / 2, y), text, F_HEAD, col(GREY, a), "mm")


def dish(d, c, r, a=1.0):
    d.ellipse([c[0] - r, c[1] - r, c[0] + r, c[1] + r], fill=col((60, 36, 22), a),
              outline=col((150, 100, 70), a), width=3)


# ── what is glowing ──────────────────────────────────────────────────────
GAS_BOX = (60, 200, 660, 820)
GAS_FIELD_ON, GAS_ION_ON = 3.0, 5.6
GAS_T = 9.5


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
    for k in range(int(GAS_T * FPS)):
        t = k / FPS
        E = math.sin(2 * math.pi * t) * smooth((t - GAS_FIELD_ON) / 0.6)   # one flip per beat
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


def scene_gas(t, assets):
    """atoms and loose electrons (0 s), the RF field (3 s), ionization + glow (5.6 s)."""
    atoms, frames = gas_sim()
    E, electrons, cool = frames[min(int(t * FPS), len(frames) - 1)]
    img = blank()
    d = style.draw(img)
    x0, y0, x1, y1 = GAS_BOX
    head(d, "inside the glow")
    d.rounded_rectangle(GAS_BOX, radius=24, outline=DIM, width=2)
    a_atoms = fade(t, 0.0, 0.3)
    glows = []
    for (ax, ay), c in zip(atoms, cool):
        d.ellipse([ax - 13, ay - 13, ax + 13, ay + 13], fill=col(style.RAISED, a_atoms),
                  outline=col(style.LINE_HI, a_atoms), width=2)
        if c > 0:
            glows.append((ax, ay, 16, min(1.0, c / 0.7)))
    if t < GAS_FIELD_ON + 0.2:
        ax, ay = atoms[0]
        d.line([(ax + 14, ay - 10), (ax + 60, ay - 44)], fill=col(GREY, a_atoms), width=2)
        d.text((ax + 64, ay - 48), "atom", font=F_SMALL, fill=col(GREY, a_atoms), anchor="lm")
    for p in electrons:
        d.ellipse([p[0] - 4, p[1] - 4, p[0] + 4, p[1] + 4], fill=col(WHITE, a_atoms))
    if t < GAS_ION_ON and electrons:
        p = electrons[0]
        d.text((p[0] + 10, p[1] - 14), "loose electron", font=F_SMALL, fill=col(WHITE, a_atoms), anchor="lb")
    a_f = fade(t, GAS_FIELD_ON)
    if a_f > 0:
        cy = (y0 + y1) / 2
        L = 150 * E
        arrow(d, (30, cy - L / 2), (30, cy + L / 2 + (1 if L >= 0 else -1)), col(WHITE, a_f), width=5, head=16)
        d.text((W / 2, y1 + 40), "radio-frequency field, flipping", font=F_SMALL, fill=col(GREY, a_f), anchor="mm")
    if glows:
        img = glow(img, glows)
    return img


# ── the obvious answer: the push averages out ────────────────────────────
RF_F = 1.0                       # one flip per beat at 120 BPM
RF_FLIP, RF_TRACE, RF_AVG = 2.5, 2.5, 5.0


def scene_rf(t, assets):
    img = blank()
    s = math.sin(2 * math.pi * RF_F * max(t - RF_FLIP, 0)) if t > RF_FLIP else 1.0
    wy = 330
    img = glow(img, [(x, wy, 9, 0.8) for x in range(140, 590, 6)])
    d = style.draw(img)
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
    x_mid = W / 2
    L = 150 * (1 if s >= 0 else -1)
    arrow(d, (x_mid - L / 2, wy + 40), (x_mid + L / 2, wy + 40), WHITE, width=5, head=18)
    d.text((x_mid + 200, wy + 40), "current", font=F_SMALL, fill=WHITE, anchor="lm")
    # current along x, the magnet's field straight up through the wire:
    # I×B points out of / into the screen. ⊙ = out, ⊗ = in.
    cx, cy, R = x_mid, wy - 90, 30 * (0.6 + 0.4 * abs(s))
    d.ellipse([cx - R, cy - R, cx + R, cy + R], outline=HOT, width=5)
    if s >= 0:
        d.ellipse([cx - 7, cy - 7, cx + 7, cy + 7], fill=HOT)
    else:
        k = R * 0.62
        d.line([(cx - k, cy - k), (cx + k, cy + k)], fill=HOT, width=5)
        d.line([(cx - k, cy + k), (cx + k, cy - k)], fill=HOT, width=5)
    d.text((cx - 50, cy), "push", font=F_SMALL, fill=HOT, anchor="rm")
    d.text((cx + 50, cy), "out of the screen" if s >= 0 else "into the screen", font=F_SMALL, fill=HOT, anchor="lm")
    a_p = fade(t, RF_TRACE)
    if a_p > 0:
        px0, px1, py, amp = 80, 640, 760, 70
        d.line([(px0, py), (px1, py)], fill=col(DIM, a_p), width=2)
        d.text((px0, py - amp - 26), "push over time", font=F_SMALL, fill=col(GREY, a_p), anchor="lm")
        tt = max(t - RF_FLIP, 0)
        span = 3.0
        pts = []
        for i in range(0, 161):
            x = px0 + (px1 - px0) * i / 160
            ts = tt - span + span * i / 160
            pts.append((x, py - amp * (math.sin(2 * math.pi * RF_F * ts) if ts > 0 else 1.0)))
        d.line(pts, fill=col(HOT, a_p), width=3)
        d.text((W / 2, py + amp + 34), "(slowed way down: really millions of flips a second)",
               font=F_SMALL, fill=col(GREY, a_p), anchor="mm")
    a_avg = fade(t, RF_AVG)
    if a_avg > 0:
        px0, px1, py = 80, 640, 760
        for x in range(px0, px1, 24):
            d.line([(x, py), (x + 12, py)], fill=col(WHITE, a_avg), width=5)
        d.text((px1, py - 16), "average = 0", font=F_LABEL, fill=col(WHITE, a_avg), anchor="rb")
    return img


# ── the key idea: magnets curl electrons (exact paths, to scale) ─────────
BETA_1TORR = 2.88            # Hall parameter under the magnet at 1 Torr (docs/plasmoid-sim.md §3)
MFP_1TORR_UM = 311.0         # v̄/ν at 1 Torr: 1.16e6 m/s / 3.73e9 s⁻¹
N_FLIGHTS = 22
E_BOX = (60, 200, 660, 830)
E_PX = 44.0                  # logical px per mean free path
E_DRAW, E_BETA, E_SWEEP, E_MAP = 0.0, 2.5, 5.0, 8.0


@lru_cache(maxsize=1)
def flights():
    """One electron's collisions: free times (in 1/ν), pitch sines and
    headings after each isotropic scatter. The seed is the first whose
    unmagnetized path stays inside the panel."""
    for seed in range(400):
        rng = np.random.default_rng(seed)
        tau = rng.exponential(1.0, N_FLIGHTS)
        sin_th = np.sqrt(1 - rng.uniform(-1, 1, N_FLIGHTS) ** 2)
        phi = rng.uniform(0, 2 * math.pi, N_FLIGHTS)
        fl = (tau, sin_th, phi)
        pts, _ = e_path(0.0, fl=fl)
        xs, ys = [p[0] for p in pts], [p[1] for p in pts]
        if max(xs) - min(xs) < 9.5 and 5 < max(ys) - min(ys) < 9.0:
            c = ((max(xs) + min(xs)) / 2, (max(ys) + min(ys)) / 2)
            return fl, c
    raise RuntimeError("no seed keeps the path in the panel")


def e_path(beta, upto=None, fl=None, n=14):
    """The path in units of the mean free path, seen along B: between hits
    the electron turns through β·τ on a circle of radius sinθ/β."""
    tau, sin_th, phi = fl if fl is not None else flights()[0]
    x = y = 0.0
    pts, hits, T = [(0.0, 0.0)], [], 0.0
    for ta, s, ph in zip(tau, sin_th, phi):
        tt = ta if upto is None else min(ta, upto - T)
        if tt <= 0:
            break
        for k in range(1, n + 1):
            u = tt * k / n
            if beta < 1e-3:
                px, py = x + s * u * math.cos(ph), y + s * u * math.sin(ph)
            else:
                px = x + (s / beta) * (math.sin(ph + beta * u) - math.sin(ph))
                py = y - (s / beta) * (math.cos(ph + beta * u) - math.cos(ph))
            pts.append((px, py))
        x, y = pts[-1]
        T += ta
        if tt < ta:
            break
        hits.append((x, y))
    return pts, hits


def _fmt_len(um):
    return f"{um:.1f} µm" if um < 10 else f"{um:.0f} µm" if um < 1000 else f"{um / 1000:.1f} mm"


def scene_beta(t, assets):
    fl, c = flights()
    total = float(fl[0].sum())
    bx0, by0, bx1, by1 = E_BOX
    ox, oy = (bx0 + bx1) / 2 - c[0] * E_PX, (by0 + by1) / 2 - c[1] * E_PX

    def to_px(pts):
        return [(ox + p[0] * E_PX, oy + p[1] * E_PX) for p in pts]

    img = blank()
    d = style.draw(img)
    a_map = fade(t, E_MAP, 0.5)
    a_p = 1 - a_map
    if a_p > 0:
        head(d, "one electron, seen from above", a=a_p)
        d.rounded_rectangle(E_BOX, radius=24, outline=col(DIM, a_p), width=2)
        sweeping = t >= E_SWEEP
        if not sweeping:
            # the same electron and the same collisions, with and without the magnet
            upto = total * seg(t, E_DRAW, E_SWEEP)
            free, _ = e_path(0.0, upto)
            mag, hits = e_path(BETA_1TORR, upto)
            d.line(to_px(free), fill=col(GREY, 0.7 * a_p), width=3)
            d.line(to_px(mag), fill=col(WHITE, a_p), width=3)
            for hx, hy in to_px(hits):
                d.ellipse([hx - 4, hy - 4, hx + 4, hy + 4], fill=col(HOT, a_p))
            for pts, cc in ((free, GREY), (mag, WHITE)):
                x, y = to_px(pts[-1:])[0]
                d.ellipse([x - 6, y - 6, x + 6, y + 6], fill=col(cc, a_p))
            d.line([(bx0 + 20, by0 + 28), (bx0 + 56, by0 + 28)], fill=col(GREY, 0.7 * a_p), width=3)
            d.text((bx0 + 66, by0 + 28), "no magnet", font=F_SMALL, fill=col(GREY, a_p), anchor="lm")
            d.line([(bx0 + 20, by0 + 60), (bx0 + 56, by0 + 60)], fill=col(WHITE, a_p), width=3)
            d.text((bx0 + 66, by0 + 60), "under the magnet", font=F_SMALL, fill=col(WHITE, a_p), anchor="lm")
            d.ellipse([bx0 + 34, by0 + 88, bx0 + 42, by0 + 96], fill=col(HOT, a_p))
            d.text((bx0 + 66, by0 + 92), "hits an atom", font=F_SMALL, fill=col(GREY, a_p), anchor="lm")
            a_b = fade(t, E_BETA, 0.15) * a_p
            if a_b > 0:
                d.text((bx1 - 20, by0 + 28), "β ≈ 2.9", font=FS(34, True), fill=col(HOT, a_b), anchor="rm")
                d.text((bx1 - 20, by0 + 62), "about half a turn between hits", font=F_SMALL,
                       fill=col(GREY, a_b), anchor="rm")
            beta, p_torr = BETA_1TORR, 1.0
        else:
            # one continuous sweep: normal air → the thin gas of the demo
            u = ease(seg(t, E_SWEEP, E_MAP - 0.6))
            p_torr = math.exp(math.log(760.0) * (1 - u))
            beta = BETA_1TORR / p_torr
            pts, hits = e_path(beta)
            d.line(to_px(pts), fill=col(WHITE, a_p), width=3)
            for hx, hy in to_px(hits):
                d.ellipse([hx - 4, hy - 4, hx + 4, hy + 4], fill=col(HOT, a_p))
            d.text((bx0 + 20, by0 + 30), "pressure", font=F_SMALL, fill=col(GREY, a_p), anchor="lm")
            d.text((bx0 + 20, by0 + 66), f"{p_torr:.0f} Torr" if p_torr >= 10 else f"{p_torr:.1f} Torr",
                   font=FS(34, True), fill=col(WHITE, a_p), anchor="lm")
            d.text((bx1 - 20, by0 + 30), "β", font=F_SMALL, fill=col(GREY, a_p), anchor="rm")
            d.text((bx1 - 20, by0 + 66), f"{beta:.3f}" if beta < 0.1 else f"{beta:.2f}" if beta < 1 else f"{beta:.1f}",
                   font=FS(34, True), fill=col(HOT, a_p), anchor="rm")
        # scale bar: 2 mean free paths
        bar = 2 * E_PX
        sx, sy = bx0 + 24, by1 - 30
        d.line([(sx, sy), (sx + bar, sy)], fill=col(GREY, a_p), width=3)
        for x in (sx, sx + bar):
            d.line([(x, sy - 6), (x, sy + 6)], fill=col(GREY, a_p), width=3)
        d.text((sx + bar + 12, sy), _fmt_len(2 * MFP_1TORR_UM / p_torr), font=F_SMALL,
               fill=col(GREY, a_p), anchor="lm")
        d.text((bx1 - 20, by1 - 30), "field: out of the screen", font=F_SMALL,
               fill=col(GREY, a_p), anchor="rm")
    if a_map > 0:
        m = Image.open(os.path.join(assets, "beta-map.png")).convert("RGB")
        m = Image.blend(Image.new("RGB", m.size, BG), m, a_map)
        style.paste(img, m, ((W - 680) / 2, 320), 680)
        d = style.draw(img)
        head(d, "how hard the magnet curls electrons, across the dish", y=270, a=a_map)
    return img


# ── the twist: the same magnet move under the two models (sim data) ──────
DISH_MM = 85.0
C_TOP, C_BOT, R_D = (360, 345), (360, 690), 140
T_METAL, T_PULL = 5.0, 7.5


@lru_cache(maxsize=1)
def lock_runs(assets):
    out = {}
    with open(os.path.join(assets, "lock_compare.csv")) as f:
        for r in csv.DictReader(f):
            out.setdefault(r["model"], []).append((float(r["t"]), float(r["x"]), float(r["y"]), float(r["gap"])))
    return {k: np.array(v) for k, v in out.items()}


MAG_XY_MM = (-38.9, 38.9)     # where the magnet comes down (lock_compare: 135° on the orbit)


def scene_compare(t, assets):
    runs = lock_runs(assets)
    img = blank()
    d = style.draw(img)
    k = R_D / DISH_MM
    glows = []
    for (cx, cy), key, title, verdict in ((C_TOP, "fitted", "simple model", "locks"),
                                          (C_BOT, "derived", "detailed model", "keeps racing")):
        dish(d, (cx, cy), R_D)
        d.text((cx - R_D - 40, cy - R_D - 4), title, font=F_SMALL, fill=WHITE, anchor="ls")
        run = runs[key]
        i = min(int(np.searchsorted(run[:, 0], t)), len(run) - 1)
        gap = run[i, 3]
        mx, my = cx + MAG_XY_MM[0] * k, cy - MAG_XY_MM[1] * k
        near = 1 - seg(gap, 0.010, 0.25)                      # 0 far above → 1 at the 10 mm hold
        r_m = 5.0 * k * (1 + 2.2 * (1 - near))               # the magnet drawn larger while high
        a_ni = fade(t, T_METAL, 0.15)
        ring = col(style.mix(GREY, (215, 215, 225), a_ni), 0.25 + 0.75 * near)
        d.ellipse([mx - r_m, my - r_m, mx + r_m, my + r_m], outline=ring, width=3,
                  fill=col((185, 185, 195), a_ni) if a_ni > 0 else None)
        # foot trail (0.35 s) and foot
        j0 = max(0, i - 42)
        trail = [(cx + x * 1000 * k, cy - y * 1000 * k) for _, x, y, _ in run[j0:i + 1]]
        for q in range(1, len(trail)):
            d.line([trail[q - 1], trail[q]], fill=style.mix((60, 36, 22), ORANGE, 0.7 * q / len(trail)), width=3)
        fx, fy = trail[-1]
        glows.append((fx, fy, 8, 1.0))
        if t >= 3.5:
            d.text((cx + R_D + 40, cy - R_D - 4), verdict, font=FS(23, True),
                   fill=style.OK if key == "fitted" else HOT, anchor="rs")
        if key == "derived" and t >= T_PULL:
            # the electrostatic pull of a conductor: inward, 2.7× the magnetic push
            ux, uy = (mx - cx) / math.hypot(mx - cx, my - cy), (my - cy) / math.hypot(mx - cx, my - cy)
            tx, ty = -uy, ux
            p0 = (mx + tx * 62, my + ty * 62)
            arrow(d, p0, (p0[0] - tx * 40, p0[1] - ty * 40), HOT, width=7, head=18)
            d.text((cx, cy + R_D + 26), "as metal it pulls the glow in, 2.7× harder than it pushes",
                   font=F_SMALL, fill=HOT, anchor="mm")
    if t >= T_METAL:
        mx, my = C_TOP[0] + MAG_XY_MM[0] * k, C_TOP[1] - MAG_XY_MM[1] * k
        d.text((mx - 22, my - 2), "nickel-plated", font=F_SMALL, fill=WHITE, anchor="rm")
    head(d, "same magnet, same move", y=150)
    return glow(img, glows)


# ── the test ─────────────────────────────────────────────────────────────
T_SLUG, T_ASK = 3.5, 6.0


def scene_test(t, assets):
    img = blank()
    d = style.draw(img)
    head(d, "the test", 170)
    glows = []
    for cx, title, metal, label in [(190, "magnet", (190, 190, 198), "N"),
                                    (530, "aluminum slug", (165, 168, 176), "Al")]:
        dish(d, (cx, 500), 140)
        d.text((cx, 320), title, font=F_LABEL, fill=WHITE, anchor="mm")
        ang = 2 * math.pi * 0.5 * t
        glows.append((cx + 100 * math.cos(ang), 500 + 100 * math.sin(ang), 9, 1.0))
        a_m = ease(seg(t, T_SLUG, T_SLUG + 0.5))
        if a_m > 0:
            my = 500 - 60 * (1 - a_m)
            mx = cx + 70
            d.ellipse([mx - 24, my - 84, mx + 24, my - 36], fill=col(metal, a_m))
            d.text((mx, my - 60), label, font=F_SMALL, fill=col((30, 30, 30), a_m), anchor="mm")
        if t >= T_ASK:
            d.text((cx, 700), "locks?", font=FS(30, True), fill=HOT, anchor="mm")
    return glow(img, glows)


DIAGRAMS = {
    "gas": scene_gas,
    "rf": scene_rf,
    "beta": scene_beta,
    "compare": scene_compare,
    "test": scene_test,
}
