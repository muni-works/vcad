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
import math
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

    def text(self, xy, text, font=None, fill=None, anchor=None, stroke_width=0, stroke_fill=None):
        self.d.text(_sc(xy), text, font=font, fill=fill, anchor=anchor,
                    stroke_width=int(round(stroke_width * S)), stroke_fill=stroke_fill)

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


_TAIL = {"a", "an", "the", "of", "to", "in", "on", "it", "is", "=", "and", "or", "so", "its", "it's",
         "this", "that", "than", "with", "under", "before", "how", "one", "by"}


def chunks(text, n=3):
    """split a caption into pop-in chunks of up to n words: break after
    punctuation and before "(", never end a chunk on a small word, and
    leave no one-word tail."""
    if "|" in text:                      # hand-placed breaks
        return text.split("|")
    out, cur = [], []
    for w in text.split(" "):
        if w.startswith("(") and cur:
            out.append(cur)
            cur = []
        cur.append(w)
        if w[-1] in ",:;.?…)":
            out.append(cur)
            cur = []
        elif len(cur) >= n:
            keep = []
            while len(cur) > 1 and cur[-1].lower() in _TAIL:
                keep.insert(0, cur.pop())
            out.append(cur)
            cur = keep
    if cur:
        out.append(cur)
    if len(out) > 1 and len(out[-1]) == 1:
        last = out.pop()
        out[-1] += last
    return [" ".join(c) for c in out]


def chunk_times(n, dur, beat=0.5):
    """when each of n chunks pops in, within a caption lasting `dur`: on
    half-beats where they fit, the last chunk holding to the next cue."""
    step = max(beat, math.floor((dur * 0.8 / n) / (beat / 2)) * (beat / 2))
    if step * n > dur:
        step = dur / n
    return [i * step for i in range(n)]


def pop_caption(img, t, t0, t1, text):
    """short-form captions: a few words at a time, popping in (scale
    0.86 → 1.05 → 1 over 0.14 s), numbers and β in the accent colour."""
    parts = chunks(text)
    times = chunk_times(len(parts), t1 - t0)
    i = max(k for k, c in enumerate(times) if t0 + c <= t + 1e-9)
    age = t - (t0 + times[i])
    k = age / 0.14
    scale = 0.86 + 0.19 * k / 0.6 if k < 0.6 else (1.05 - 0.05 * (k - 0.6) / 0.4 if k < 1 else 1.0)
    part = parts[i]
    d = draw(img)
    size = 50
    f = FS(size, True)
    lines = wrap(d, part, f, W - 2 * M)
    while len(lines) > 2 and size > 34:
        size -= 4
        f = FS(size, True)
        lines = wrap(d, part, f, W - 2 * M)
    f = FS(int(round(size * scale)), True)
    hot = any(ch.isdigit() for ch in part) or "β" in part
    lh = size * 1.2 * scale
    y0 = 1050 - lh * (len(lines) - 1) / 2
    for j, ln in enumerate(lines):
        d.text((W / 2, y0 + j * lh), ln, font=f, fill=SEL if hot else TEXT, anchor="mm",
               stroke_width=5, stroke_fill=(0, 0, 0))
    return img


def hud_frame(img, t, dur, label="plasmoid · simulated in vcad"):
    """persistent top-left line + bottom progress hairline."""
    d = draw(img)
    tracked(d, (M, 40), label, F(16, True), LABEL)
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
