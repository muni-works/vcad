#!/usr/bin/env python3
"""Composite the ~50 s every-physics puck video, frame by frame.

usage: comp.py <frames_dir> <results_dir> <out_dir> [frame,list]
frames_dir holds turn/ (300-frame turntable) and sect/ (1200-frame section sweep), from render.sh.
results_dir is docs/videos/every-physics/poc/results (port_velocity.csv, antenna_s11.csv).
Numbers on screen are copied from results/run-v3.log. Every animated quantity is a pure
function of t (frames render in a Pool). Timeline: 96 BPM, 2.5 s bars, matches synth.py.
Style: Pilot Light (warm black, one orange accent, cyan for data in flight, IBM Plex)."""
import csv, math, os, sys
from multiprocessing import Pool
from pathlib import Path
import numpy as np
from PIL import Image, ImageDraw, ImageFont

W, H, FPS = 1920, 1080, 30
FR, RES, OUT = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
OUT.mkdir(parents=True, exist_ok=True)
BAR = 2.5; BEAT = BAR / 4
def B(bar, beat=0.0): return bar * BAR + beat * BEAT
DUR = B(20)

_FD = Path(__file__).with_name("fonts")
def F(sz, b=False): return ImageFont.truetype(str(_FD / ("IBMPlexMono-SemiBold.otf" if b else "IBMPlexMono-Regular.otf")), sz)
def FS(sz, b=False): return ImageFont.truetype(str(_FD / ("IBMPlexSans-SemiBold.otf" if b else "IBMPlexSans-Regular.otf")), sz)
def _hex(h): return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))
BGC, SURFACE, RAISED = _hex("#0b0907"), _hex("#15110d"), _hex("#1e1813")
LINE, LINE_HI = _hex("#2a2119"), _hex("#4a3a2a")
TEXT, LABEL, FAINT = _hex("#e8e1d2"), _hex("#a08d70"), _hex("#6b5a44")
SEL = _hex("#ff6a00")
OK, WARN, FAULT, INFO = _hex("#4fd88a"), _hex("#e9c84a"), _hex("#ff5a4a"), _hex("#4cc8e0")

def ease(x):
    """cubic-bezier(0.2, 0, 0, 1)"""
    x = min(1.0, max(0.0, x))
    if x >= 1.0: return 1.0
    if x <= 0.0: return 0.0
    lo, hi = 0.0, 1.0
    for _ in range(18):
        u = (lo + hi) / 2
        if 3 * (1 - u) ** 2 * u * 0.2 + u ** 3 < x: lo = u
        else: hi = u
    return u ** 3
def mix(a, b, k): k = min(1.0, max(0.0, k)); return tuple(int(a[i] + (b[i] - a[i]) * k) for i in range(3))
def fade(c, k): return mix(BGC, c, k)
def tracked(d, xy, text, font, fill, anchor="la", em=0.14):
    text = text.upper(); gap = font.size * em
    w = sum(d.textlength(c, font=font) for c in text) + gap * max(0, len(text) - 1)
    x, y = xy
    if anchor[0] == "m": x -= w / 2
    elif anchor[0] == "r": x -= w
    for c in text:
        d.text((x, y), c, font=font, fill=fill, anchor="l" + anchor[1]); x += d.textlength(c, font=font) + gap

def readcsv(p): return [list(map(float, r)) for r in list(csv.reader(open(p)))[1:]]
PORTV = readcsv(RES / "port_velocity.csv")
S11 = readcsv(RES / "antenna_s11.csv")
OPT = [(120.0, 60.0, 140.5, 208.8, 0), (151.9, 78.6, 110.9, 252.5, 1), (160.3, 75.7, 109.2, 262.4, 1),
       (174.4, 69.4, 107.7, 278.8, 1), (171.1, 72.0, 107.3, 275.2, 1), (173.5, 71.4, 106.8, 278.0, 1),
       (173.9, 71.7, 106.5, 278.6, 1), (173.0, 71.1, 107.2, 277.4, 1), (141.7, 58.5, 129.0, 235.6, 0),
       (146.3, 60.4, 125.1, 241.8, 1), (145.5, 60.0, 125.8, 240.7, 1), (145.2, 59.8, 126.2, 240.2, 1),
       (145.2, 59.8, 126.2, 240.3, 1), (145.1, 59.7, 126.3, 240.2, 1), (142.4, 58.5, 128.6, 236.5, 0)]  # body_h, port_len, f_b, mass, infeasible

def frame_img(kind, t):
    if kind == "turn": k = int(t * FPS) % 300; p = FR / "turn" / f"{k:04d}.jpg"
    else:
        k = min(1199, max(0, int((t - B(4)) * FPS))); p = FR / "sect" / f"{k:04d}.jpg"
        while not p.exists() and k > 0:   # preview while render.sh is still running
            k -= 1; p = FR / "sect" / f"{k:04d}.jpg"
    return Image.open(p).convert("RGB")

_GRAD = None
def left_shade(im, k):
    """darken the left panel so the cards read; k in 0..1."""
    global _GRAD
    if _GRAD is None:
        x = np.linspace(0, 1, W)
        a = np.clip(1.15 - x / 0.55, 0, 1) ** 1.4
        _GRAD = np.repeat(a[None, :, None], H, 0)
    arr = np.asarray(im).astype(np.float32)
    bg = np.array(BGC, np.float32)
    a = _GRAD * 0.92 * k
    return Image.fromarray((arr * (1 - a) + bg * a).astype(np.uint8))

# ---------------- plot helpers (panel-local coords)
def axes(d, x0, y0, w, h, xl, yl):
    d.line([(x0, y0 + h), (x0 + w, y0 + h)], fill=LINE_HI, width=2)
    d.line([(x0, y0), (x0, y0 + h)], fill=LINE_HI, width=2)
    d.text((x0 + w, y0 + h + 14), xl, font=F(20), fill=FAINT, anchor="ra")
    d.text((x0 - 12, y0), yl, font=F(20), fill=FAINT, anchor="ra")
def polyline(d, pts, col, prog, width=4):
    n = max(2, int(len(pts) * prog + 0.999)); seg = pts[:n]
    if len(seg) >= 2: d.line(seg, fill=col, width=width, joint="curve")

PX, PY, PW = 120, 300, 640   # card panel origin / width

def card_header(d, idx, name, tag, k):
    tracked(d, (PX, 210), f"{idx:02d} / 09", F(26, True), fade(SEL, k))
    d.text((PX, 245), name, font=FS(54, True), fill=fade(TEXT, k), anchor="lt")
    tracked(d, (PX, 318), tag, F(20), fade(FAINT, k))
def big(d, y, value, unit, k, col=TEXT):
    d.text((PX, y), value, font=F(108, True), fill=fade(col, k), anchor="ls")
    vw = d.textlength(value, font=F(108, True))
    d.text((PX + vw + 18, y), unit, font=FS(40), fill=fade(LABEL, k), anchor="ls")
def note(d, y, text, k, col=LABEL, sz=28):
    d.text((PX, y), text, font=FS(sz), fill=fade(col, k), anchor="lt")

def c_mass(d, k, p):
    card_header(d, 1, "mass, per part", "doc geometry × material library", k)
    big(d, 480, "533", "g, die not counted", k)
    parts = [("enclosure", 208.8), ("yoke", 117.2), ("magnet", 93.1), ("top plate", 78.6), ("driver frame", 26.8), ("board", 7.4), ("voice coil", 1.4)]
    for i, (n, g) in enumerate(parts):
        y = 540 + i * 46; kk = ease((p - 0.04 * i) * 5)
        d.text((PX, y + 4), n, font=F(22), fill=fade(LABEL, k), anchor="lt")
        d.rectangle([PX + 190, y + 6, PX + 190 + int(380 * g / 208.8 * kk), y + 30], fill=fade(INFO if i else SEL, k))
        d.text((PX + 200 + int(380 * g / 208.8 * kk), y + 4), f"{g:g}", font=F(22), fill=fade(TEXT, k * kk), anchor="lt")
def c_diff(d, k, p):
    card_header(d, 2, "differentiable seam", "exact d(volume)/d(parameter)", k)
    rows = [("wall", 49771.1), ("body_h", 1008.4), ("port_len", 163.4)]
    for i, (n, v) in enumerate(rows):
        y = 420 + i * 120; kk = ease((p - 0.06 * i) * 6)
        d.text((PX, y), f"dV/d{n}", font=F(30), fill=fade(LABEL, k), anchor="lt")
        d.text((PX, y + 40), f"{v * kk:+,.1f}", font=F(54, True), fill=fade(TEXT, k), anchor="lt")
        d.text((PX + 420, y + 58), "mm³/mm", font=FS(26), fill=fade(FAINT, k), anchor="lt")
    note(d, 800, "3 parameters, one 18 s solve", k)
def c_dfm(d, k, p):
    card_header(d, 3, "manufacturability", "FDM pack, 0.4 mm nozzle", k)
    big(d, 480, "2", "warnings", k, WARN)
    note(d, 520, "overhang 180° → supports, ~117 mm column", k)
    note(d, 562, "port tube underside → supports, ~60 mm", k)
    note(d, 640, "print it upside down, or add a chamfer", k, FAINT, 26)
def c_modal(d, k, p):
    card_header(d, 4, "structural modes", "free-free shell, Fine mesh, PLA", k)
    big(d, 480, "448.1", "Hz  first mode", k)
    modes = [448.1, 459.0, 545.4, 630.4, 672.3, 886.8]
    x0, y0, w, h = PX, 560, 600, 200
    axes(d, x0, y0, w, h, "Hz", "")
    for i, f in enumerate(modes):
        x = x0 + (f - 400) / 520 * w; amp = [1, .8, .55, .45, .35, .22][i]
        ring = math.exp(-max(0, p * BAR - 0.0) * (1.1 + 0.6 * i))
        hh = h * amp * (0.35 + 0.65 * ring)
        d.line([(x, y0 + h), (x, y0 + h - hh)], fill=fade(SEL if i == 0 else INFO, k), width=6)
    note(d, 800, "the bell in the track: these six modes, struck", k, FAINT, 26)
def c_acoustics(d, k, p):
    card_header(d, 5, "acoustics", "ported box from the document, motor as obstacles", k)
    big(d, 480, "155.0", "Hz  port tuning", k)
    x0, y0, w, h = PX, 560, 600, 210
    axes(d, x0, y0, w, h, "Hz", "port u")
    fs = [r[0] for r in PORTV]; vs = [r[1] for r in PORTV]
    lo, hi = min(fs), max(fs); vm = max(vs)
    pts = [(x0 + (f - lo) / (hi - lo) * w, y0 + h - v / vm * h * 0.95) for f, v in zip(fs, vs)]
    polyline(d, pts, fade(INFO, k), ease(p * 2.2))
    xm = x0 + (155.0 - lo) / (hi - lo) * w
    if lo < 155 < hi: d.line([(xm, y0), (xm, y0 + h)], fill=fade(SEL, k * ease(p * 3 - 1)), width=2)
    note(d, 800, "V_box 0.882 L · lumped 138–151 Hz", k, FAINT, 26)
def c_thermal(d, k, p):
    card_header(d, 6, "thermal", "board + die voxelized, 0.8 W amp", k)
    big(d, 480, "86.1", "°C  die, h = 5", k, WARN)
    for i, (hh, T) in enumerate([(5, 86.1), (10, 69.8)]):
        y = 560 + i * 90; kk = ease((p - 0.08 * i) * 5)
        d.text((PX, y), f"h = {hh:>2} W/m²K", font=F(24), fill=fade(LABEL, k), anchor="lt")
        x1 = PX + 260; full = 330
        d.rectangle([x1, y + 2, x1 + full, y + 30], outline=fade(LINE_HI, k), width=2)
        d.rectangle([x1, y + 2, x1 + int(full * (T - 20) / 80 * kk), y + 30], fill=fade(mix(INFO, FAULT, (T - 50) / 40), k))
        d.text((x1 + full + 16, y), f"{T * kk + 20 * (1 - kk):.1f} °C", font=F(24), fill=fade(TEXT, k), anchor="lt")
    note(d, 760, "θ_ja 76.4 / 56.0 K/W · energy residual 6e-13", k, FAINT, 26)
def c_antenna(d, k, p):
    card_header(d, 7, "antenna", "printed monopole, from the PcbBoard", k)
    big(d, 480, "−15.2", "dB @ 2.44 GHz", k)
    x0, y0, w, h = PX, 560, 600, 210
    axes(d, x0, y0, w, h, "MHz", "S11")
    fs = [r[0] for r in S11]; ss = [r[1] for r in S11]; lo, hi = fs[0], fs[-1]
    pts = [(x0 + (f - lo) / (hi - lo) * w, y0 + (-s) / 20 * h) for f, s in zip(fs, ss)]
    polyline(d, pts, fade(INFO, k), ease(p * 2.2))
    xm = x0 + (2440 - lo) / (hi - lo) * w
    d.line([(xm, y0), (xm, y0 + h)], fill=fade(SEL, k * ease(p * 3 - 1)), width=2)
    note(d, 800, "resonance 2357 MHz, free space · FR-4 pulls it lower", k, FAINT, 26)
def c_em(d, k, p):
    card_header(d, 8, "magnetics", "axisymmetric motor, nonlinear steel", k)
    big(d, 480, "1.53", "T  in the gap", k)
    d.text((PX, 560), "Bl", font=F(30), fill=fade(LABEL, k), anchor="lt")
    d.text((PX, 600), "3.65 T·m", font=F(54, True), fill=fade(TEXT, k), anchor="lt")
    note(d, 690, "ψ route and sampled route agree", k)
    note(d, 800, "241 × 305 grid · 4 parts from the document", k, FAINT, 26)
def c_tol(d, k, p):
    card_header(d, 9, "tolerance", "driver-frame press fit, ±0.1 on each radius", k)
    big(d, 480, "86%", "yield", k, WARN)
    x0, y0, w, h = PX, 560, 600, 190
    mu, sg = -0.075, 0.1 * math.sqrt(2) / 3
    xs = np.linspace(-0.30, 0.15, 160)
    def X(v): return x0 + (v + 0.30) / 0.45 * w
    pts = [(X(v), y0 + h - h * math.exp(-0.5 * ((v - mu) / sg) ** 2)) for v in xs]
    for v0, v1 in ((-0.15, -0.01),):
        d.rectangle([X(v0), y0, X(v1), y0 + h], fill=fade(RAISED, k))
    polyline(d, pts, fade(INFO, k), ease(p * 2.2))
    d.line([(x0, y0 + h), (x0 + w, y0 + h)], fill=fade(LINE_HI, k), width=2)
    d.text((X(-0.15), y0 + h + 12), "−0.15", font=F(20), fill=fade(FAINT, k), anchor="ma")
    d.text((X(-0.01), y0 + h + 12), "−0.01 mm", font=F(20), fill=fade(FAINT, k), anchor="la")
    note(d, 800, "worst case FAIL → tighten to ±0.05, or ream", k, FAINT, 26)
CARDS = [c_mass, c_diff, c_dfm, c_modal, c_acoustics, c_thermal, c_antenna, c_em, c_tol]

def optimizer(d, t):
    p = (t - B(15)) / (B(17) - B(15)); k = ease((t - B(15)) * 3) * (1 - ease((t - B(17) + 0.25) * 4))
    tracked(d, (PX, 210), "optimize_document", F(26, True), fade(SEL, k))
    d.text((PX, 245), "make it boom lower", font=FS(54, True), fill=fade(TEXT, k), anchor="lt")
    tracked(d, (PX, 318), "body_h, port_len → f_b 90 Hz, light, no floppier", F(18), fade(FAINT, k))
    x0, y0, w, h = PX, 400, 640, 380
    axes(d, x0, y0, w, h, "mass, g", "f_b, Hz")
    def P(m, f): return (x0 + (m - 200) / 90 * w, y0 + (145 - f) / 45 * h)
    n = int(min(1.0, max(0.0, p * 1.15)) * len(OPT) + 0.999)
    pts = [P(o[3], o[2]) for o in OPT[:n]]
    if len(pts) >= 2: d.line(pts, fill=fade(LINE_HI, k), width=2)
    for i, o in enumerate(OPT[:n]):
        x, y = P(o[3], o[2]); r = 9 if i in (0, len(OPT) - 1) else 6
        col = FAINT if o[4] else (SEL if i == len(OPT) - 1 else INFO)
        d.ellipse([x - r, y - r, x + r, y + r], fill=fade(col, k))
    cur = OPT[max(0, n - 1)]
    d.text((PX, 830), f"f_b {OPT[0][2]:.1f} → {cur[2]:.1f} Hz   mass {cur[3]:.1f} g", font=F(30, True), fill=fade(TEXT, k), anchor="lt")
    d.text((PX, 876), "grey = infeasible. the stiffness floor stops it near 129 Hz.", font=FS(26), fill=fade(LABEL, k), anchor="lt")

SCORE = [("mass", "533 g"), ("seam", "∂V exact"), ("DFM", "2 warn"), ("modes", "448 Hz"), ("port", "155 Hz"),
         ("die", "86.1 °C"), ("S11", "−15.2 dB"), ("gap B", "1.53 T"), ("fit yield", "86%")]
def scorecard(d, t):
    k = ease((t - B(17)) * 2) * (1 - ease((t - B(19)) * 3))
    tracked(d, (W / 2, 150), "one document · nine solvers", F(28, True), fade(SEL, k), anchor="mm")
    cw, ch, gx, gy = 470, 190, 30, 30; x0 = (W - 3 * cw - 2 * gx) / 2; y0 = 220
    for i, (n, v) in enumerate(SCORE):
        kk = k * ease((t - B(17) - 0.08 * i) * 3)
        x = x0 + (i % 3) * (cw + gx); y = y0 + (i // 3) * (ch + gy)
        d.rectangle([x, y, x + cw, y + ch], fill=fade(SURFACE, kk), outline=fade(LINE_HI, kk), width=2)
        tracked(d, (x + 28, y + 28), n, F(22), fade(LABEL, kk))
        d.text((x + 28, y + ch - 30), v, font=F(60, True), fill=fade(TEXT, kk), anchor="ls")
        tracked(d, (x + cw - 24, y + 28), "predicted", F(16), fade(WARN, kk), anchor="ra")
    d.text((W / 2, 930), "every number predicted. none measured yet. that's the next video.", font=FS(32), fill=fade(LABEL, k), anchor="mm")

def render(fi):
    t = fi / FPS
    if t < B(4):
        im = frame_img("turn", t)
    else:
        im = frame_img("sect", t)
    # slide the puck right while the cards are up, so the left column is free
    if t >= B(6) - 0.6:
        sh = int(330 * ease((t - B(6) + 0.6) / 0.8))
        if sh:
            moved = Image.new("RGB", (W, H), BGC); moved.paste(im.crop((0, 0, W - sh, H)), (sh, 0)); im = moved
    shade = 0.0
    if B(6) - 0.4 <= t < B(17): shade = ease((t - B(6) + 0.4) / 0.6) * (1 - ease((t - B(17) + 0.3) / 0.5) * 0.0)
    if t >= B(17): shade = 1.0
    if shade > 0: im = left_shade(im, shade)
    if t >= B(17):   # scorecard: dim everything
        a = ease((t - B(17)) * 2) * 0.85 + 0.15 * ease((t - B(19)) * 2)
        im = Image.blend(im, Image.new("RGB", (W, H), BGC), a)
    d = ImageDraw.Draw(im)
    # ---- intro titles over the turntable
    if t < B(4):
        k1 = ease((t - B(1)) * 1.5) * (1 - ease((t - B(4) + 0.3) * 4))
        tracked(d, (110, 400), "every physics", F(24, True), fade(SEL, k1))
        tracked(d, (110, 434), "in one object", F(24, True), fade(SEL, k1))
        d.text((104, 470), "puck", font=FS(150, True), fill=fade(TEXT, k1), anchor="lt")
        k2 = ease((t - B(2)) * 1.5) * (1 - ease((t - B(4) + 0.3) * 4))
        for i, ln in enumerate(["Ø110 × 120 mm bass-reflex", "BLE speaker, one .loon file"]):
            d.text((110, 680 + i * 42), ln, font=FS(32), fill=fade(LABEL, k2), anchor="lt")
        k3 = ease((t - B(3)) * 1.5) * (1 - ease((t - B(4) + 0.3) * 4))
        for i, ln in enumerate(["nine solvers read the same", "document. nobody re-models it."]):
            d.text((110, 790 + i * 36), ln, font=FS(28), fill=fade(FAINT, k3), anchor="lt")
    # ---- section reveal labels
    elif t < B(6):
        rows = [("enclosure", "PLA can, internal port tube"), ("motor", "Y30 ferrite, 1010 steel, copper coil"),
                ("driver frame", "press-fit into the bottom wall"), ("board + die", "FR-4, 0.8 W class-D amp")]
        tracked(d, (120, 120), "cut at y = 0", F(26, True), fade(SEL, ease((t - B(4)) * 3) * (1 - ease((t - B(6) + 0.3) * 4))))
        for i, (n, s) in enumerate(rows):
            k = ease((t - B(4, 1 + i)) * 3) * (1 - ease((t - B(6) + 0.3) * 4))
            y = 170 + i * 80
            d.text((120, y), n, font=FS(40, True), fill=fade(TEXT, k), anchor="lt")
            d.text((120, y + 46), s, font=FS(24), fill=fade(LABEL, k), anchor="lt")
    # ---- solver cards, one per bar
    elif t < B(15):
        ci = int((t - B(6)) // BAR); ci = min(8, ci)
        lt = t - B(6 + ci); p = lt / BAR
        k = ease(lt * 4) * (1 - ease((lt - BAR + 0.18) * 7))
        dx = int((1 - ease(lt * 4)) * -40)
        CARDS[ci](d, k, p)
        if dx: pass
        # beat ticker: 9 cells, current one orange
        for i in range(9):
            x = 120 + i * 30
            d.rectangle([x, 160, x + 20, 166], fill=SEL if i == ci else (LINE_HI if i < ci else LINE))
    elif t < B(17):
        optimizer(d, t)
    else:
        scorecard(d, t)
        if t >= B(19) + 0.3:
            k = ease((t - B(19) - 0.3) * 2)
            d.text((W / 2, H / 2 - 20), "vcad", font=FS(120, True), fill=fade(TEXT, k), anchor="mm")
            tracked(d, (W / 2, H / 2 + 70), "open kernel · runs on your machine", F(24), fade(LABEL, k), anchor="mm")
    # fade in from black, out at the end
    a = min(1.0, t / 1.2) * min(1.0, (DUR - t) / 1.0)
    if a < 1.0: im = Image.blend(Image.new("RGB", (W, H), BGC), im, max(0.0, a))
    im.save(OUT / f"{fi:04d}.png", compress_level=1)
    return fi

if __name__ == "__main__":
    n = int(DUR * FPS)
    frames = [int(x) for x in sys.argv[4].split(",")] if len(sys.argv) > 4 else list(range(n))
    with Pool(int(os.environ.get("NPROC", "6"))) as pool:
        for _ in pool.imap_unordered(render, frames, chunksize=8): pass
    print(f"{len(frames)} frames → {OUT}")
