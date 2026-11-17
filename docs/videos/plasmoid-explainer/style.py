"""Pilot Light design system for the plasmoid explainer.

Tokens from frame-bootc palette.toml [warm]; helpers ported from the board
videos (hardware/pcb-motor-video/comp.py): one accent (the flame), one
easing curve, text that lands whole (no typing), panels with a numbered
orange block, the vcad wordmark drawn in code (docs/brand-spec.md).

Everything is laid out in a 720x1280 *logical* frame. `EXPLAINER_SCALE`
(default 1; 1.5 = 1080x1920) scales pixels: `draw(img)` returns a drawing
proxy that takes logical coordinates, and the font loaders return fonts at
the scaled size, so scene code never mentions pixels.
"""
import os

import numpy as np
from PIL import Image, ImageDraw, ImageFont

W, H, FPS = 720, 1280, 30
S = float(os.environ.get("EXPLAINER_SCALE", "1"))
PW, PH = int(round(W * S)), int(round(H * S))
M = 40  # side margin (the 1080p landscape videos used 72)

_FD = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fonts")


def _hex(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))


BGC, SURFACE, RAISED = _hex("#0b0907"), _hex("#15110d"), _hex("#1e1813")
LINE, LINE_HI = _hex("#2a2119"), _hex("#4a3a2a")
TEXT, LABEL, FAINT = _hex("#e8e1d2"), _hex("#a08d70"), _hex("#6b5a44")
SEL, SEL_INK = _hex("#ff6a00"), (0, 0, 0)
OK, WARN, FAULT, INFO = _hex("#4fd88a"), _hex("#e9c84a"), _hex("#ff5a4a"), _hex("#4cc8e0")
CARBON, V_TEXT, V_SEC, V_HAIR = _hex("#0A0A0B"), _hex("#F5F5F6"), _hex("#7A7E86"), _hex("#1A1B1E")

_cache = {}


def _font(name, sz):
    key = (name, sz)
    if key not in _cache:
        f = ImageFont.truetype(os.path.join(_FD, name), max(1, int(round(sz * S))))
        f.logical = sz
        _cache[key] = f
    return _cache[key]


def F(sz, b=False):
    """IBM Plex Mono."""
    return _font("IBMPlexMono-SemiBold.otf" if b else "IBMPlexMono-Regular.otf", sz)


def FS(sz, b=False):
    """IBM Plex Sans."""
    return _font("IBMPlexSans-SemiBold.otf" if b else "IBMPlexSans-Regular.otf", sz)


def FI(sz, b=True):
    """Inter: the wordmark only."""
    return _font("Inter_18pt-Bold.ttf" if b else "Inter_18pt-Regular.ttf", sz)


def _sc(v):
    if isinstance(v, (int, float)):
        return v * S
    return type(v)(_sc(x) for x in v) if isinstance(v, (list, tuple)) else v


class Draw:
    """ImageDraw in logical coordinates."""

    def __init__(self, img):
        self.d = ImageDraw.Draw(img)

    @staticmethod
    def _w(width):
        return max(1, int(round(width * S)))

    def line(self, xy, fill=None, width=1):
        self.d.line(_sc(xy), fill=fill, width=self._w(width), joint="curve")

    def ellipse(self, xy, fill=None, outline=None, width=1):
        self.d.ellipse(_sc(xy), fill=fill, outline=outline, width=self._w(width))

    def rectangle(self, xy, fill=None, outline=None, width=1):
        self.d.rectangle(_sc(xy), fill=fill, outline=outline, width=self._w(width))

    def rounded_rectangle(self, xy, radius=0, fill=None, outline=None, width=1):
        self.d.rounded_rectangle(_sc(xy), radius=radius * S, fill=fill, outline=outline, width=self._w(width))

    def polygon(self, xy, fill=None, outline=None):
        self.d.polygon(_sc(xy), fill=fill, outline=outline)

    def arc(self, xy, start, end, fill=None, width=1):
        self.d.arc(_sc(xy), start, end, fill=fill, width=self._w(width))

    def text(self, xy, text, font=None, fill=None, anchor=None):
        self.d.text(_sc(xy), text, font=font, fill=fill, anchor=anchor)

    def multiline_text(self, xy, text, font=None, fill=None, anchor=None, align="left", spacing=4):
        self.d.multiline_text(_sc(xy), text, font=font, fill=fill, anchor=anchor, align=align,
                              spacing=spacing * S)

    def textlength(self, text, font=None):
        return self.d.textlength(text, font=font) / S


def draw(img):
    return Draw(img)


def new(mode="RGB", color=(0, 0, 0)):
    return Image.new(mode, (PW, PH), color)


def fit(img):
    """a frame from disk, at the output pixel size."""
    return img if img.size == (PW, PH) else img.resize((PW, PH), Image.LANCZOS)


def paste(img, im, xy, width):
    """paste `im` scaled to logical `width` with its top-left at logical xy."""
    w = int(round(width * S))
    im = im.resize((w, int(round(im.height * w / im.width))), Image.LANCZOS)
    img.paste(im, (int(round(xy[0] * S)), int(round(xy[1] * S))))


def ease(x):
    """the one curve: cubic-bezier(0.2, 0, 0, 1)"""
    x = min(1.0, max(0.0, x))
    if x >= 1.0:
        return 1.0
    if x <= 0.0:
        return 0.0
    lo, hi = 0.0, 1.0
    for _ in range(18):
        u = (lo + hi) / 2
        if 3 * (1 - u) ** 2 * u * 0.2 + u ** 3 < x:
            lo = u
        else:
            hi = u
    return u ** 3


def seg(t, a, b):
    return min(1.0, max(0.0, (t - a) / (b - a)))


def mix(a, b, k):
    k = min(1.0, max(0.0, k))
    return tuple(int(a[i] + (b[i] - a[i]) * k) for i in range(3))


def on_bg(c, a):
    """colour `c` at opacity `a` over the background."""
    return mix(BGC, c, a)


def tracked(d, xy, text, font, fill, anchor="la", em=0.12):
    text = text.upper()
    gap = font.logical * em
    w = sum(d.textlength(c, font=font) for c in text) + gap * max(0, len(text) - 1)
    x, y = xy
    if anchor[0] == "m":
        x -= w / 2
    elif anchor[0] == "r":
        x -= w
    for c in text:
        d.text((x, y), c, font=font, fill=fill, anchor="l" + anchor[1])
        x += d.textlength(c, font=font) + gap


def fade(img, alpha):
    if alpha >= 1:
        return img
    arr = np.array(img)
    arr[..., 3] = (arr[..., 3] * max(0.0, alpha)).astype(np.uint8)
    return Image.fromarray(arr)


def make_bg():
    img = new("RGB", BGC)
    d = draw(img)
    for gx in range(0, W, 72):
        d.line([(gx, 0), (gx, H)], fill=_hex("#110e0b"))
    for gy in range(0, H, 36):
        d.line([(0, gy), (W, gy)], fill=_hex("#110e0b"))
    return img


BG = make_bg()


def blank():
    return BG.copy()


def wrap(d, text, font, width):
    """greedy word wrap; explicit newlines are kept."""
    out = []
    for para in text.split("\n"):
        line = ""
        for w in para.split(" "):
            cand = (line + " " + w).strip()
            if d.textlength(cand, font=font) <= width or not line:
                line = cand
            else:
                out.append(line)
                line = w
        out.append(line)
    return out


def panel(img, t, t0, t1, num, title, text):
    """The scene card, lower third: orange number block, tracked title, the
    current caption in sans. In 0.1 s, out 0.15 s; the text lands whole."""
    al = seg(t, t0, t0 + 0.1) * (1 - seg(t, t1 - 0.15, t1))
    if al <= 0:
        return img
    lay = new("RGBA", (0, 0, 0, 0))
    d = draw(lay)
    x, y = M, 958
    tw = W - 2 * M
    lines = wrap(d, text, FS(31, True), tw - 32)
    bh = 64 + 42 * len(lines) + 18
    d.rectangle([x, y, x + tw, y + bh], fill=SURFACE + (238,), outline=LINE_HI + (255,))
    d.rectangle([x, y, x + 64, y + 40], fill=SEL + (255,))
    d.text((x + 32, y + 20), num, font=F(24, True), fill=SEL_INK + (255,), anchor="mm")
    tracked(d, (x + 82, y + 20), title, F(20, True), LABEL + (255,), "lm")
    for i, ln in enumerate(lines):
        d.text((x + 16, y + 60 + 42 * i), ln, font=FS(31, True), fill=TEXT + (255,), anchor="la")
    base = img.convert("RGBA")
    base.alpha_composite(fade(lay, al))
    return base.convert("RGB")


def hud_frame(img, t, dur):
    """persistent top-left line + bottom progress hairline."""
    d = draw(img)
    tracked(d, (M, 40), "plasmoid · simulated in vcad", F(16, True), LABEL)
    bar = (W - 2 * M) * min(t / dur, 1.0)
    y = H - 34
    d.line([(M, y), (W - M, y)], fill=LINE_HI, width=2)
    d.line([(M, y), (M + bar, y)], fill=TEXT, width=2)
    return img


def _wm(d, cx, cy, size):
    f = FI(size)
    xh = 0.546 * size
    tw = 1.20 * 0.98 * d.textlength("v", font=f)
    gap = 0.055 * size
    trk = -0.03 * size
    cw = sum(d.textlength(c, font=f) for c in "cad") + 2 * trk
    x = cx - (tw + gap + cw) / 2
    return f, x, tw, gap, trk, [(x, cy - xh), (x + tw, cy - xh), (x + tw / 2, cy + 0.045 * xh)]


def wordmark(d, cx, cy, size, col):
    """▽cad: the filled inverted triangle is the v."""
    f, x, tw, gap, trk, tri = _wm(d, cx, cy, size)
    d.polygon(tri, fill=col)
    x += tw + gap
    for c in "cad":
        d.text((x, cy), c, font=f, fill=col, anchor="ls")
        x += d.textlength(c, font=f) + trk


def end_card(t, t_end, beat, credit):
    """carbon card: wordmark, then the tagline 2 beats later, then vcad.io
    and the credit line 2 beats after that."""
    img = new("RGB", CARBON)
    d = draw(img)
    m, L = M, 24
    for (x, y, sx, sy) in ((m, m, 1, 1), (W - m, m, -1, 1), (m, H - m, 1, -1), (W - m, H - m, -1, -1)):
        d.line([(x, y), (x + sx * L, y)], fill=V_SEC, width=2)
        d.line([(x, y), (x, y + sy * L)], fill=V_SEC, width=2)
    cy = H / 2 - 40
    wordmark(d, W / 2, cy, 150, V_TEXT)
    if t >= t_end + 2 * beat:
        d.text((W / 2, cy + 74), "build your imagination", font=FI(34, False), fill=V_TEXT, anchor="mm")
    if t >= t_end + 4 * beat:
        d.line([(W / 2 - 120, cy + 126), (W / 2 + 120, cy + 126)], fill=V_HAIR, width=2)
        d.text((W / 2, cy + 168), "vcad.io", font=F(28), fill=V_TEXT, anchor="mm")
        for i, ln in enumerate(credit):
            d.text((W / 2, H - m - 70 + 30 * i), ln, font=F(18), fill=V_SEC, anchor="mm")
    return img
