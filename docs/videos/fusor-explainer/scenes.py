"""Scenes for the fusor explainer. Each is `f(t, cues, assets) -> RGB image`:
`t` is seconds from the scene's first line, `cues` maps cue names to seconds
from the scene start (plus "dur", the scene's length).

Simulation output (`<assets>/paths.csv`, `fates.csv`, `potential.csv`, from
`fusor_explainer_paths`): the cage, the potential map and hill, and every
ion flight in `scene_ions`. Illustrations, not simulation: `scene_fuel`,
`scene_hit`, `scene_fuse`, the gas dots in `scene_machine`, the neutron
streaks in `scene_count`.
"""
import csv
import math
import os

import numpy as np
from PIL import Image, ImageChops, ImageFilter

import style
from style import (BGC, F, FAINT, FS, H, INFO, LABEL, LINE, LINE_HI, M, RAISED, SEL, SURFACE, TEXT, W,
                   ease, mix, seg, tracked)

GRID_R, WALL_R, WALL_T = 19.0, 76.0, 11.0
VOLTS = 30000.0
M_D = 3.3436e-27          # deuteron mass, kg
EV = 1.602177e-19
_PHI = (1 + 5 ** 0.5) / 2
NORMALS = [np.array(v) / np.linalg.norm(v) for v in
           ([0, 1, _PHI], [0, 1, -_PHI], [1, _PHI, 0], [1, -_PHI, 0], [_PHI, 0, 1], [-_PHI, 0, 1])]
HOT, COLD = (206, 246, 255), (18, 96, 120)   # cyan: things in flight
FLASH = (255, 226, 180)
ELECTRON = (233, 200, 74)
NEUTRON, PROTON = (150, 142, 130), SEL


def _circle(n, r, m=120):
    a = np.cross(n, [0, 0, 1] if abs(n[2]) < 0.9 else [1, 0, 0])
    a /= np.linalg.norm(a)
    b = np.cross(n, a)
    th = np.linspace(0, 2 * math.pi, m + 1)
    return r * (np.outer(np.cos(th), a) + np.outer(np.sin(th), b))


CAGE = [_circle(n, GRID_R) for n in NORMALS]

# ── simulation data ───────────────────────────────────────────────────────
_D = {}


def data(assets):
    if _D:
        return _D
    rows = {}
    with open(os.path.join(assets, "paths.csv")) as f:
        next(f)
        for ln in f:
            k, t, x, y, z, ke = ln.split(",")
            rows.setdefault(int(k), []).append((float(t), float(x), float(y), float(z), float(ke)))
    ions = {k: np.array(v) for k, v in rows.items()}
    fates = {int(r["ion"]): (r["fate"], int(r["core_passes"]), float(r["time_ns"]))
             for r in csv.DictReader(open(os.path.join(assets, "fates.csv")))}
    n = 201
    pot = np.full((n, n), np.nan)
    with open(os.path.join(assets, "potential.csv")) as f:
        next(f)
        for ln in f:
            x, z, p = (float(v) for v in ln.split(","))
            pot[int(round((x + WALL_R) / (2 * WALL_R) * (n - 1))), int(round((z + WALL_R) / (2 * WALL_R) * (n - 1)))] = p
    survivors = [k for k, v in fates.items() if v[0] == "survived"]
    hero = survivors[0]
    a = ions[hero]
    r = np.linalg.norm(a[:, 1:4], axis=1)
    _D.update(ions=ions, fates=fates, pot=pot, hero=hero, t_grid=float(a[np.argmax(r < GRID_R), 0]),
              order=sorted(ions))
    return _D


def speed_kms(ke_ev):
    return math.sqrt(2 * max(ke_ev, 0.0) * EV / M_D) / 1e3


# ── camera ────────────────────────────────────────────────────────────────
def cam(az, el, scale, cx=W / 2, cy=520.0, dist=520.0):
    a, e = math.radians(az), math.radians(el)
    return dict(right=np.array([-math.sin(a), math.cos(a), 0.0]),
                fwd=np.array([math.cos(e) * math.cos(a), math.cos(e) * math.sin(a), math.sin(e)]),
                up=np.array([-math.sin(e) * math.cos(a), -math.sin(e) * math.sin(a), math.cos(e)]),
                scale=scale, cx=cx, cy=cy, dist=dist)


def proj(P, c):
    P = np.asarray(P, dtype=float).reshape(-1, 3)
    d = P @ c["fwd"]
    k = c["scale"] * c["dist"] / (c["dist"] - d)
    return c["cx"] + k * (P @ c["right"]), c["cy"] - k * (P @ c["up"]), d


def glow_layer():
    return style.new("RGB", (0, 0, 0))


def add_glow(img, layer, radius=7.0):
    blur = layer.filter(ImageFilter.GaussianBlur(radius * style.S))
    return ImageChops.add(ImageChops.add(img, blur), layer)


def dim(c, k):
    return tuple(int(v * max(0.0, min(1.0, k))) for v in c)


# ── the machine, drawn from its dimensions ────────────────────────────────
def draw_chamber(img, c, k=1.0, fill=True):
    d = style.draw(img)
    r0, r1 = WALL_R * c["scale"], (WALL_R + WALL_T) * c["scale"]
    cx, cy = c["cx"], c["cy"]
    d.ellipse([cx - r1, cy - r1, cx + r1, cy + r1], fill=mix(BGC, RAISED, k), outline=mix(BGC, LINE_HI, k), width=2)
    d.ellipse([cx - r0, cy - r0, cx + r0, cy + r0], fill=mix(BGC, (6, 5, 4), k) if fill else None,
              outline=mix(BGC, LINE_HI, k), width=2)


def draw_stalk(img, c, k=1.0):
    """the feedthrough conductor: down the +z port to the cage's pole."""
    d = style.draw(img)
    x, y, _ = proj([[0, 0, GRID_R], [0, 0, WALL_R + WALL_T + 26], [0, 0, WALL_R + 2], [0, 0, WALL_R + WALL_T + 14]], c)
    d.line([(x[2], y[2]), (x[3], y[3])], fill=mix(BGC, (196, 186, 168), k), width=5 * c["scale"])   # ceramic
    d.line([(x[0], y[0]), (x[1], y[1])], fill=mix(BGC, LABEL, k), width=max(2, 1.6 * c["scale"]))


def draw_cage(img, c, part, k=1.0, hot=0.0):
    """part: "back" (drawn before the ions) or "front" (after)."""
    d = style.draw(img)
    segs = []
    for ring in CAGE:
        x, y, z = proj(ring, c)
        for i in range(len(x) - 1):
            zm = (z[i] + z[i + 1]) / 2
            if (zm < 0) == (part == "back"):
                segs.append((zm, x[i], y[i], x[i + 1], y[i + 1]))
    segs.sort()
    wd = max(2.0, 2.0 * c["scale"])
    for zm, x0, y0, x1, y1 in segs:
        s = (zm + GRID_R) / (2 * GRID_R)
        col = mix(mix((84, 68, 50), (214, 196, 166), s), SEL, hot * (0.35 + 0.4 * s))
        d.line([(x0, y0), (x1, y1)], fill=mix(BGC, col, k), width=wd)


def leader(d, p, lp, text, k, below=False, sub=None, anchor="m"):
    if k <= 0:
        return
    col = mix(BGC, SEL, k)
    d.line([p, lp], fill=col, width=2)
    d.ellipse([p[0] - 4, p[1] - 4, p[0] + 4, p[1] + 4], fill=col)
    y = lp[1] + (8 if below else -8)
    d.text((lp[0], y), text, font=FS(28, True), fill=col, anchor=anchor + ("t" if below else "b"),
           stroke_width=4, stroke_fill=(0, 0, 0))
    if sub:
        d.text((lp[0], y + (36 if below else -36)), sub, font=F(18), fill=mix(BGC, LABEL, k),
               anchor=anchor + ("t" if below else "b"), stroke_width=4, stroke_fill=(0, 0, 0))


def _rng_points(n, seed, r):
    g = np.random.default_rng(seed)
    v = g.normal(size=(n, 3))
    v /= np.linalg.norm(v, axis=1)[:, None]
    return v * (r * g.random(n) ** (1 / 3))[:, None], g.normal(size=(n, 3))


_AIR = _rng_points(900, 1, WALL_R - 2)
_GAS = _rng_points(46, 2, WALL_R - 4)


def draw_gas(img, c, t, air=0.0, gas=1.0):
    """illustration: dots for air (thinning out) and for the deuterium that replaces it."""
    lay = glow_layer()
    d = style.draw(lay)
    for (P, V), n, col, r in ((_AIR, int(len(_AIR[0]) * air), (120, 130, 140), 1.6), (_GAS, len(_GAS[0]), HOT, 2.6)):
        if n <= 0:
            continue
        Q = P[:n] + V[:n] * 1.5 * np.sin(t * 0.9 + np.arange(n))[:, None]
        x, y, z = proj(Q, c)
        k = 1.0 if col != HOT else gas
        for i in range(n):
            s = 0.45 + 0.55 * (z[i] + WALL_R) / (2 * WALL_R)
            d.ellipse([x[i] - r, y[i] - r, x[i] + r, y[i] + r], fill=dim(col, 0.55 * s * k))
    return add_glow(img, lay, 3.0)


def scene_machine(t, cues, assets):
    """vac: the chamber, the cage and the feedthrough; the air goes, the fuel comes in."""
    c = cam(-60 + 5 * t, 14, 4.3)
    img = style.blank()
    draw_chamber(img, c)
    air = 1.0 - ease(seg(t, cues["pump"], cues["pump"] + 1.6))
    gas = ease(seg(t, cues["trace"], cues["trace"] + 0.8))
    draw_cage(img, c, "back")
    img = draw_gas(img, c, t, air, gas)
    draw_stalk(img, c)
    draw_cage(img, c, "front")
    d = style.draw(img)
    x, y, _ = proj([[0, 0, WALL_R + WALL_T + 8], [0, 0, 0]], c)
    leader(d, (c["cx"] - 250, c["cy"] + 250), (c["cx"] - 250, c["cy"] + 400), "steel chamber", ease(seg(t, 0.2, 0.6)),
           below=True)
    leader(d, (x[1] + 60, y[1] + 50), (c["cx"] + 210, c["cy"] + 400), "wire cage", ease(seg(t, 0.6, 1.0)), below=True)
    leader(d, (x[0], y[0]), (x[0] + 190, y[0] + 6), "high-voltage rod", ease(seg(t, 1.0, 1.4)), anchor="m")
    if gas > 0:
        d.text((W / 2, 930), "deuterium · 5 mTorr", font=F(24, True), fill=mix(BGC, HOT, gas), anchor="mm")
    return img, "fusor · drawn from the cad's dimensions"


# ── the potential ─────────────────────────────────────────────────────────
_STOPS = np.array([0.0, 0.3, 0.6, 0.85, 1.0])
_RGB = np.array([(16, 12, 9), (74, 26, 6), (196, 74, 0), (255, 106, 0), (255, 214, 150)], dtype=float)


def heat(k):
    k = np.clip(k, 0, 1)
    return np.stack([np.interp(k, _STOPS, _RGB[:, i]) for i in range(3)], axis=-1)


_MAP = {}


def pot_map(assets):
    """φ on the y = 0 plane as an RGBA image (x right, z up), with 5 kV contours."""
    if "im" not in _MAP:
        pot = data(assets)["pot"]
        k = -pot / VOLTS
        rgb = heat(np.nan_to_num(k))
        band = np.floor(np.nan_to_num(k) * 6)
        edge = np.zeros_like(band, dtype=bool)
        edge[1:, :] |= band[1:, :] != band[:-1, :]
        edge[:, 1:] |= band[:, 1:] != band[:, :-1]
        rgb[edge] = np.minimum(rgb[edge] * 1.5 + 40, 255)
        a = np.where(np.isnan(k), 0, 255)
        arr = np.dstack([rgb, a]).astype(np.uint8)
        _MAP["im"] = Image.fromarray(np.flipud(arr.transpose(1, 0, 2)), "RGBA")
    return _MAP["im"]


def pot_at(pot, x, z):
    n = pot.shape[0]
    i = int(round((x + WALL_R) / (2 * WALL_R) * (n - 1)))
    j = int(round((z + WALL_R) / (2 * WALL_R) * (n - 1)))
    v = pot[min(max(i, 0), n - 1), min(max(j, 0), n - 1)]
    return 0.0 if np.isnan(v) else float(v)


DEPTH = 78.0   # how deep the hill is drawn, mm, for the full 30 kV


def draw_hill(img, c, pot, grow):
    """the potential as a surface: height = φ. Painter's order, far quads first."""
    d = style.draw(img)
    step = 2
    idx = np.arange(0, pot.shape[0], step)
    xs = -WALL_R + 2 * WALL_R * idx / (pot.shape[0] - 1)
    sub = pot[np.ix_(idx, idx)]
    k = -sub / VOLTS
    P = np.stack(np.broadcast_arrays(xs[:, None], xs[None, :], -np.nan_to_num(k) * DEPTH * grow), axis=-1)
    px, py, pz = proj(P.reshape(-1, 3), c)
    n = len(idx)
    px, py, pz = px.reshape(n, n), py.reshape(n, n), pz.reshape(n, n)
    ok = ~np.isnan(sub)
    quads = []
    for i in range(n - 1):
        for j in range(n - 1):
            if ok[i, j] and ok[i + 1, j] and ok[i, j + 1] and ok[i + 1, j + 1]:
                quads.append((pz[i, j] + pz[i + 1, j + 1], i, j))
    quads.sort()
    cols = heat(np.nan_to_num(k))
    for _, i, j in quads:
        col = tuple(int(v) for v in cols[i, j])
        pts = [(px[i, j], py[i, j]), (px[i + 1, j], py[i + 1, j]), (px[i + 1, j + 1], py[i + 1, j + 1]),
               (px[i, j + 1], py[i, j + 1])]
        d.polygon(pts, fill=mix(BGC, col, 0.55 + 0.1 * grow), outline=mix(BGC, col, 0.6 + 0.3 * grow))


def scene_well(t, cues, assets):
    """well: the voltage map on a slice through the middle, then the same map as a hill."""
    D = data(assets)
    pot = D["pot"]
    g = ease(seg(t, cues["hill"] - 0.3, cues["hill"] + 1.5))
    el = 90 - 60 * g
    c = cam(-90 + 16 * g + 3 * max(0.0, t - cues["hill"]), el, 4.3 - 0.5 * g, cy=520 - 60 * g)
    img = style.blank()
    fadein = ease(seg(t, 0.0, 0.8))
    if g <= 0:
        draw_chamber(img, c, 1.0)
        r = WALL_R * c["scale"]
        im = pot_map(assets).resize((int(2 * r * style.S), int(2 * r * style.S)), Image.BILINEAR)
        lay = style.new("RGB", BGC)
        lay.paste(img)
        lay.paste(im, (int((c["cx"] - r) * style.S), int((c["cy"] - r) * style.S)), im)
        img = Image.blend(img, lay, fadein)
    else:
        draw_hill(img, c, pot, g)
        # one ion from the simulation, rolling on the hill (its path projected onto the slice)
        a = D["ions"][D["hero"]]
        tau = max(0.0, t - cues["hill"] - 0.8) * 60.0 % 560.0
        lay = glow_layer()
        dl = style.draw(lay)
        for j, back in enumerate(np.linspace(0, 26, 12)):
            tt = max(0.0, tau - back)
            x, z = np.interp(tt, a[:, 0], a[:, 1]), np.interp(tt, a[:, 0], a[:, 3])
            h = pot_at(pot, x, z) / VOLTS * DEPTH * g
            sx, sy, _ = proj([[x, z, h + 2.5]], c)
            rr = 9 - 0.6 * j
            dl.ellipse([sx[0] - rr, sy[0] - rr, sx[0] + rr, sy[0] + rr], fill=dim(HOT, (1 - j / 12) ** 2))
        if t >= cues["hill"] + 0.8:
            img = add_glow(img, lay, 8.0)
    d = style.draw(img)
    kc = ease(seg(t, cues["minus"], cues["minus"] + 0.3))
    kw = ease(seg(t, cues["zero"], cues["zero"] + 0.3))
    h0 = -DEPTH * g
    x, y, _ = proj([[0, 0, h0 * 0.96], [WALL_R * 0.98, 0, 0]], c)
    leader(d, (x[0], y[0]), (x[0] - 150, y[0] + (250 if g <= 0 else 170)), "−30,000 V", kc, below=True, sub="the cage")
    leader(d, (x[1], y[1]), (x[1] - 60, y[1] - 300 + 170 * g), "0 V", kw, sub="the wall")
    return img, "voltage · simulated in vcad"


# ── the ions ──────────────────────────────────────────────────────────────
def ion_color(ke, k=1.0):
    return dim(mix(COLD, HOT, ke / 29500.0), k)


def draw_ion(dl, c, a, tau, trail_ns, n=14, size=1.0, k=1.0):
    taus = np.clip(tau - np.linspace(0, trail_ns, n), 0, a[-1, 0])
    P = np.stack([np.interp(taus, a[:, 0], a[:, i]) for i in (1, 2, 3)], axis=1)
    ke = np.interp(taus, a[:, 0], a[:, 4])
    x, y, _ = proj(P, c)
    for j in range(n - 2, -1, -1):
        f = (1 - j / (n - 1)) ** 1.6
        dl.line([(x[j], y[j]), (x[j + 1], y[j + 1])], fill=ion_color(ke[j], f * k), width=3.2 * size)
    r = 4.2 * size
    dl.ellipse([x[0] - r, y[0] - r, x[0] + r, y[0] + r], fill=ion_color(ke[0], k))
    return x[0], y[0], ke[0]


def draw_flash(dl, c, p, age, size=1.0):
    """an ion ending on a wire."""
    if not 0 <= age < 1:
        return
    x, y, _ = proj([p], c)
    r = (5 + 26 * age) * size
    dl.ellipse([x[0] - r, y[0] - r, x[0] + r, y[0] + r], outline=dim((255, 120, 60), (1 - age) ** 1.5), width=3)


def tau_hero(t, cues, D):
    """sim time of the hero ion: slow from release until it reaches the cage on the
    spoken speed, then the same rate until the crowd joins, then faster."""
    rel, hit, tb = cues["falls"], cues["seventeen"], cues["back"]
    r1 = D["t_grid"] / (hit - rel)
    r2 = 110.0
    if t <= rel:
        return 0.0, r1
    if t <= tb:
        return (t - rel) * r1, r1
    u = min(t - tb, 1.5)
    tau = (tb - rel) * r1 + r1 * u + (r2 - r1) * u * u / 3.0 + r2 * max(0.0, t - tb - 1.5)
    return tau, r1 + (r2 - r1) * (u / 1.5)


def scene_ions(t, cues, assets):
    """fall → speed → miss → back → core: real flights from the 3D solver."""
    D = data(assets)
    ions, fates = D["ions"], D["fates"]
    zoom = ease(seg(t, cues["core"], cues["core"] + 2.5))
    c = cam(-60 + 4 * t, 13, 4.3 + 6.2 * zoom)
    img = style.blank()
    draw_chamber(img, c, 1.0 - 0.6 * zoom)
    draw_cage(img, c, "back", hot=0.25)
    lay = glow_layer()
    dl = style.draw(lay)
    hero = ions[D["hero"]]
    tau, rate = tau_hero(t, cues, D)
    size = 1.0 + 0.5 * zoom
    # the crowd: every traced ion, looping, each offset in its own life
    crowd_t = t - cues["back"]
    if crowd_t > 0:
        T = crowd_t * 110.0
        for n, k in enumerate(D["order"]):
            if k == D["hero"] or crowd_t < n * 0.03:
                continue
            a = ions[k]
            life = fates[k][2]
            period = life + 60.0
            tk = (T + (n * 0.6180339) % 1.0 * period) % period
            if tk <= life:
                draw_ion(dl, c, a, tk, 22.0, n=9, size=0.8 * size, k=0.85)
            elif fates[k][0] == "wire":
                draw_flash(dl, c, a[-1, 1:4], (tk - life) / 45.0, size)
    hx, hy, hke = draw_ion(dl, c, hero, tau, 16.0 if rate < 60 else 26.0, size=1.25 * size)
    img = add_glow(img, lay, 7.0)
    if zoom < 0.5:
        draw_stalk(img, c, 1.0 - 2 * zoom)
    draw_cage(img, c, "front", hot=0.25)
    d = style.draw(img)
    # before release: an atom of the gas loses its electron (illustration)
    if t < cues["falls"] + 0.6:
        x0, y0, _ = proj([hero[0, 1:4]], c)
        ke = seg(t, cues["strip"], cues["strip"] + 1.2)
        if 0 < ke < 1:
            ex, ey = x0[0] + 150 * ease(ke), y0[0] - 110 * ease(ke)
            d.ellipse([ex - 4, ey - 4, ex + 4, ey + 4], fill=mix(BGC, ELECTRON, 1 - ke))
        leader(d, (x0[0], y0[0]), (x0[0] + 140, y0[0] - 60),
               "ion  +" if t >= cues["ion"] else "atom", ease(seg(t, 0.1, 0.5)) * (1 - seg(t, cues["falls"], cues["falls"] + 0.6)))
    # the hero's own numbers, from the trace
    kh = ease(seg(t, cues["falls"], cues["falls"] + 0.4)) * (1 - ease(seg(t, cues["back"], cues["back"] + 0.8)))
    if kh > 0:
        y = 905
        tracked(d, (M, y), "one ion", F(16, True), mix(BGC, LABEL, kh))
        for i, (v, unit) in enumerate(((f"{tau:,.0f}", "ns"), (f"{speed_kms(hke):,.0f}", "km/s"),
                                       (f"{hke / 1e3:.1f}", "keV"))):
            x = M + i * 215
            d.text((x, y + 58), v, font=F(40, True), fill=mix(BGC, SEL, kh), anchor="ls")
            d.text((x + d.textlength(v, font=F(40, True)) + 8, y + 58), unit, font=F(20),
                   fill=mix(BGC, LABEL, kh), anchor="ls")
    kp = ease(seg(t, cues["seven"], cues["seven"] + 0.3)) * (1 - ease(seg(t, cues["core"], cues["core"] + 0.6)))
    if kp > 0:
        d.text((W / 2, 930), "7.1 passes on average, with no gas", font=F(24, True), fill=mix(BGC, SEL, kp), anchor="mm")
        d.text((W / 2, 964), "70% open · 6 great circles of 2 mm wire", font=F(18), fill=mix(BGC, LABEL, kp), anchor="mm")
    kd = ease(seg(t, cues["crowd"], cues["crowd"] + 0.3))
    if kd > 0:
        d.text((W / 2, 150), "≈ 60× denser", font=F(44, True), fill=mix(BGC, SEL, kd), anchor="mm", stroke_width=5, stroke_fill=(0, 0, 0))
        d.text((W / 2, 196), "inner 5 mm against the outer chamber · first pass, no gas", font=F(18), fill=mix(BGC, TEXT, kd), anchor="mm", stroke_width=4, stroke_fill=(0, 0, 0))
    return img, "ion flights · simulated in vcad"


def scene_why(t, cues, assets):
    """why: the same flights, close on the cage, every wire strike marked."""
    D = data(assets)
    c = cam(40 + 4 * t, 13, 9.0)
    img = style.blank()
    draw_cage(img, c, "back", hot=0.3)
    lay = glow_layer()
    dl = style.draw(lay)
    T = 400.0 + t * 110.0
    for n, k in enumerate(D["order"]):
        a, life = D["ions"][k], D["fates"][k][2]
        period = life + 60.0
        tk = (T + (n * 0.6180339) % 1.0 * period) % period
        if tk <= life:
            draw_ion(dl, c, a, tk, 22.0, n=9, size=1.2, k=0.7)
        elif D["fates"][k][0] == "wire":
            draw_flash(dl, c, a[-1, 1:4], (tk - life) / 45.0, 1.6)
    img = add_glow(img, lay, 7.0)
    draw_cage(img, c, "front", hot=0.3)
    return img, "ion flights · simulated in vcad"


# ── illustrations ─────────────────────────────────────────────────────────
def ball(d, x, y, r, col, sign=None, k=1.0):
    d.ellipse([x - r, y - r, x + r, y + r], fill=mix(BGC, col, k), outline=mix(BGC, (0, 0, 0), k), width=2)
    d.ellipse([x - r * 0.55, y - r * 0.6, x - r * 0.05, y - r * 0.1], fill=mix(BGC, mix(col, (255, 255, 255), 0.35), k))
    if sign:
        d.text((x, y + 1), sign, font=FS(int(r * 1.1), True), fill=mix(BGC, (0, 0, 0), k), anchor="mm")


def deuteron(d, x, y, r=30, k=1.0, ang=0.0):
    dx, dy = math.cos(ang) * r * 0.62, math.sin(ang) * r * 0.62
    ball(d, x - dx, y - dy, r, NEUTRON, k=k)
    ball(d, x + dx, y + dy, r, PROTON, "+", k=k)


def scene_fuel(t, cues, assets):
    """fuel, repel: what deuterium is, and why two nuclei will not simply touch."""
    img = style.blank()
    d = style.draw(img)
    cy = 500
    if t < cues["repel"]:
        k1 = ease(seg(t, 0.2, 0.6))
        k2 = ease(seg(t, cues["extra"] - 0.2, cues["extra"] + 0.2))
        ball(d, 200, cy, 48, PROTON, "+", k1)
        d.text((200, cy + 110), "hydrogen", font=FS(28, True), fill=mix(BGC, TEXT, k1), anchor="mm")
        d.text((200, cy + 150), "1 proton", font=F(18), fill=mix(BGC, LABEL, k1), anchor="mm")
        deuteron(d, 520, cy, 48, k1 if k2 <= 0 else 1.0)
        if k2 < 1:      # until the word, the neutron is not there yet
            ball(d, 520, cy, 48, PROTON, "+", k1 * (1 - k2))
        d.text((520, cy + 110), "deuterium", font=FS(28, True), fill=mix(BGC, SEL, k1), anchor="mm")
        d.text((520, cy + 150), "1 proton + 1 neutron", font=F(18), fill=mix(BGC, LABEL, k2), anchor="mm")
        return img, "illustration"
    u = t - cues["repel"]
    fast = t >= cues["fast"] - 0.5
    if not fast:
        # a slow pair: closes, stalls, backs off
        ph = (u % 3.2) / 3.2
        gap = 270 - 140 * math.sin(math.pi * ph) ** 0.8
        force = math.sin(math.pi * ph)
    else:
        v = seg(t, cues["fast"] - 0.5, cues["touch"])
        gap = 300 - 258 * v ** 2
        force = v
    hit = fast and t >= cues["touch"]
    if hit:
        age = t - cues["touch"]
        lay = glow_layer()
        r = 40 + 240 * ease(min(age / 0.6, 1))
        style.draw(lay).ellipse([W / 2 - r, cy - r, W / 2 + r, cy + r], fill=dim(FLASH, max(0.0, 1 - age / 0.9)))
        img = add_glow(img, lay, 30.0)
        d = style.draw(img)
        gap = 42
    for s in (-1, 1):
        x = W / 2 + s * gap
        deuteron(d, x, cy, 40, ang=0 if s > 0 else math.pi)
        if not hit:
            L = 30 + 70 * force
            d.line([(x + s * 70, cy - 70), (x + s * (70 + L), cy - 70)], fill=SEL, width=4)
            d.polygon([(x + s * (70 + L + 14), cy - 70), (x + s * (70 + L), cy - 80), (x + s * (70 + L), cy - 60)], fill=SEL)
    msg = "they touch" if hit else ("fast enough" if fast else "too slow: pushed apart")
    d.text((W / 2, cy + 130), msg, font=F(24, True), fill=SEL if hit else LABEL, anchor="mm")
    return img, "illustration"


def scene_hit(t, cues, assets):
    """hit: charge exchange. The fast ion takes an electron and leaves as a fast neutral."""
    img = style.blank()
    lay = glow_layer()
    dl = style.draw(lay)
    d = style.draw(img)
    mx, my = 400, 470
    tg, tn = cues["grabs"], cues["neutral"]
    v = 330.0
    x = mx - 40 - v * (tg - t) if t < tg else mx - 40 + 0.45 * v * (t - tg)
    y = my - 0.35 * (x - mx)
    neutral = t >= tg + 0.25
    col = (176, 170, 160) if neutral else HOT
    for j in range(10):
        f = (1 - j / 10) ** 1.5
        xx = x - j * 18
        yy = my - 0.35 * (xx - mx)
        if not neutral or j % 2 == 0:
            dl.ellipse([xx - 9, yy - 9, xx + 9, yy + 9], fill=dim(col, f))
    # the gas molecule, and the slow ion it becomes
    slow = min(max(0.0, t - tg - 0.4), 1.6)
    gx, gy = mx + 18 - 20 * slow ** 2, my + 40 + 34 * slow ** 2
    kcol = mix((120, 130, 140), SEL, seg(t, tg, tg + 0.3))
    ball(d, gx - 24, gy, 26, kcol)
    ball(d, gx + 24, gy, 26, kcol)
    e = seg(t, tg - 0.05, tg + 0.25)
    if t < tg + 0.25:
        ex, ey = gx + (x - gx) * e, gy - 26 + (y - gy + 26) * e
        d.ellipse([ex - 5, ey - 5, ex + 5, ey + 5], fill=ELECTRON)
    img2 = add_glow(img, lay, 6.0)
    d = style.draw(img2)
    k1 = ease(seg(t, 0.2, 0.5)) * (1 - seg(t, tg, tg + 0.3))
    leader(d, (x, y - 10), (x - 20, y - 120), "fast ion  +", k1)
    leader(d, (gx, gy + 30), (gx + 40, gy + 160), "gas molecule", k1, below=True)
    k2 = ease(seg(t, tn, tn + 0.3))
    leader(d, (min(x, W - 60), max(y, 150) - 10), (min(x, W - 190), max(y, 150) - 110), "neutral: no pull", k2 * (1 - seg(x, W - 60, W + 200)))
    leader(d, (gx, gy + 30), (gx + 30, gy + 160), "new slow ion  +", k2, below=True)
    return img2, "illustration"


def scene_fuse(t, cues, assets):
    """fuse: D + D → helium-3 + neutron."""
    img = style.blank()
    d = style.draw(img)
    cx, cy = W / 2, 500
    tf = cues["fuse"]
    if t < tf:
        v = seg(t, cues["head"] - 1.0, tf)
        gap = 230 * (1 - v) ** 1.0 + 42
        lay = glow_layer()
        dl = style.draw(lay)
        for s in (-1, 1):
            for j in range(8):
                xx = cx + s * (gap + j * 16)
                dl.ellipse([xx - 6, cy - 6, xx + 6, cy + 6], fill=dim(HOT, (1 - j / 8) ** 2 * 0.7))
        img = add_glow(img, lay, 8.0)
        d = style.draw(img)
        for s in (-1, 1):
            deuteron(d, cx + s * gap, cy, 40, ang=0 if s > 0 else math.pi)
        return img, "illustration"
    age = t - tf
    lay = glow_layer()
    r = 30 + 330 * ease(min(age / 0.7, 1))
    style.draw(lay).ellipse([cx - r, cy - r, cx + r, cy + r], fill=dim(FLASH, max(0.0, 1 - age / 1.1)))
    nx = cx + 60 + 900 * age
    for j in range(14):
        xx = nx - j * 22
        if xx > cx:
            style.draw(lay).ellipse([xx - 7, cy - 7, xx + 7, cy + 7], fill=dim((230, 230, 235), (1 - j / 14) ** 2))
    img = add_glow(img, lay, 26.0)
    d = style.draw(img)
    hx = cx - 20 - 60 * min(age, 3.0)
    ball(d, hx - 28, cy - 22, 38, PROTON, "+")
    ball(d, hx + 28, cy - 22, 38, PROTON, "+")
    ball(d, hx, cy + 28, 38, NEUTRON)
    if nx < W + 40:
        ball(d, nx, cy, 34, NEUTRON)
    kh = ease(seg(t, cues["helium"], cues["helium"] + 0.3))
    kn = ease(seg(t, cues["neutron"], cues["neutron"] + 0.3))
    d.text((hx, cy + 120), "helium-3", font=FS(28, True), fill=mix(BGC, SEL, kh), anchor="mm")
    d.text((W - M, cy - 110), "neutron", font=FS(28, True), fill=mix(BGC, SEL, kn), anchor="rm")
    d.text((W - M, cy - 74), "2.45 MeV · 21,600 km/s", font=F(18), fill=mix(BGC, LABEL, kn), anchor="rm")
    d.text((W / 2, 900), "half the time: tritium + a proton instead", font=F(18), fill=mix(BGC, FAINT, kn), anchor="mm")
    return img, "illustration"


_NDIR = _rng_points(400, 7, 1.0)[1]


def scene_count(t, cues, assets):
    """count: the predicted neutron rate. The streaks are an illustration."""
    c = cam(-60 + 4 * t, 13, 2.6, cy=560)
    img = style.blank()
    draw_chamber(img, c)
    draw_cage(img, c, "back", hot=0.3)
    draw_cage(img, c, "front", hot=0.3)
    lay = glow_layer()
    dl = style.draw(lay)
    for i, v in enumerate(_NDIR):
        v = v / np.linalg.norm(v)
        age = (t * 1.3 + i * 0.137) % 2.4
        if age > 1.0:
            continue
        p0, p1 = v * (8 + 520 * age), v * (8 + 520 * age + 34)
        x, y, _ = proj([p0, p1], c)
        dl.line([(x[0], y[0]), (x[1], y[1])], fill=dim((225, 225, 232), 0.8 * (1 - age)), width=2.5)
    img = add_glow(img, lay, 5.0)
    d = style.draw(img)
    k = ease(seg(t, cues["two"] - 0.2, cues["two"] + 0.4))
    n = int(round(1900 * k / 100.0) * 100)
    d.text((W / 2, 170), f"≈ {n:,}", font=F(84, True), fill=mix(BGC, SEL, min(1.0, 4 * k)), anchor="mm")
    d.text((W / 2, 236), "neutrons per second", font=FS(28, True), fill=mix(BGC, TEXT, k), anchor="mm")
    d.text((W / 2, 276), "predicted · band 87 to 18,000 · ring-stack model", font=F(18), fill=mix(BGC, LABEL, k), anchor="mm")
    return img, "prediction · simulated in vcad"


def scene_power(t, cues, assets):
    """power: watts in against watts of fusion out, on a log scale."""
    img = style.blank()
    d = style.draw(img)
    x0, x1 = M + 10, W - M - 10
    lo, hi = -10, 3

    def X(p):
        return x0 + (x1 - x0) * (p - lo) / (hi - lo)

    y = 620
    d.line([(x0, y), (x1, y)], fill=LINE_HI, width=2)
    for p in range(lo, hi + 1):
        d.line([(X(p), y - 6), (X(p), y + 6)], fill=LINE_HI, width=2)
    for p, s in ((-9, "1 nW"), (-6, "1 µW"), (-3, "1 mW"), (0, "1 W"), (3, "1 kW")):
        d.text((X(p), y + 30), s, font=F(16), fill=LABEL, anchor="mm")
    ki = ease(seg(t, cues["three"] - 0.1, cues["three"] + 0.7))
    ko = ease(seg(t, cues["few"] - 0.1, cues["few"] + 0.7))
    pin, pout = math.log10(300.0), math.log10(2.2e-9)
    d.rectangle([x0, y - 150, x0 + (X(pin) - x0) * ki, y - 100], fill=mix(BGC, TEXT, min(1, 3 * ki)))
    d.text((x0, y - 176), "in: 300 W", font=F(28, True), fill=mix(BGC, TEXT, ki), anchor="ls")
    d.text((x0, y - 214), "30,000 V × 10 mA", font=F(18), fill=mix(BGC, LABEL, ki), anchor="ls")
    d.rectangle([x0, y - 70, x0 + (X(pout) - x0) * ko, y - 20], fill=mix(BGC, SEL, min(1, 3 * ko)))
    d.text((X(pout) + 16, y - 45), "fusion out: ≈ 2 nW", font=F(28, True), fill=mix(BGC, SEL, ko), anchor="lm")
    d.text((W / 2, y + 110), "each tick is ten times the last", font=F(18), fill=FAINT, anchor="mm")
    d.text((W / 2, 360), "÷ 100,000,000,000", font=F(44, True), fill=mix(BGC, SEL, ko), anchor="mm")
    return img, "prediction · simulated in vcad"


_STILL = {}


def scene_still(t, cues, assets, name="hero", push=0.06, dimmed=0.0):
    """a path-traced frame with a slow push-in."""
    if name not in _STILL:
        _STILL[name] = Image.open(os.path.join(assets, name + ".jpg")).convert("RGB")
    im = _STILL[name]
    k = 1.0 + push * t / max(cues.get("dur", 8.0), 1e-3)
    w, h = style.PW, style.PH
    s = max(w / im.width, h / im.height) * k
    im2 = im.resize((int(im.width * s) + 1, int(im.height * s) + 1), Image.LANCZOS)
    x, y = (im2.width - w) // 2, (im2.height - h) // 2
    img = im2.crop((x, y, x + w, y + h))
    if dimmed:
        img = Image.blend(img, style.new("RGB", BGC), dimmed)
    # a dark strip under the tag, so "render · glow added" reads on the light backdrop
    band = img.crop((0, 0, w, int(84 * style.S)))
    img.paste(Image.blend(band, Image.new("RGB", band.size, (0, 0, 0)), 0.72), (0, 0))
    return img, "render · path-traced in vcad · glow added"


def scene_hero(t, cues, assets):
    return scene_still(t, cues, assets, "hero")


def scene_close(t, cues, assets):
    return scene_still(t, cues, assets, "close", push=0.08)


def scene_card(t, cues, assets):
    return scene_still(t, cues, assets, "hero", push=0.0, dimmed=0.72)


def scene_so(t, cues, assets):
    """so: the opening shot again, then the three parts named: hill, cage, ions."""
    if t < cues["hill"]:
        return scene_still(t, dict(dur=cues["hill"]), assets, "hero", push=0.05)
    if t < cues["cage"]:
        img = style.blank()
        draw_hill(img, cam(-74 + 14 * (t - cues["hill"]), 30, 3.8, cy=460), data(assets)["pot"], 1.0)
        return img, "voltage · simulated in vcad"
    if t < cues["ions"]:
        c = cam(40 + 14 * (t - cues["cage"]), 13, 9.0)
        img = style.blank()
        draw_cage(img, c, "back", hot=0.3)
        draw_cage(img, c, "front", hot=0.3)
        return img, "fusor · drawn from the cad's dimensions"
    return scene_why(t - cues["ions"] + (cues["ions"] - cues["cage"]) * 14 / 4, cues, assets)


SCENES = dict(hero=scene_hero, close=scene_close, card=scene_card, fuel=scene_fuel, machine=scene_machine,
              well=scene_well, ions=scene_ions, hit=scene_hit, fuse=scene_fuse, count=scene_count,
              power=scene_power, why=scene_why, so=scene_so)
