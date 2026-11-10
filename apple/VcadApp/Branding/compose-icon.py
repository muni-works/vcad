#!/usr/bin/env python3
"""Composite a transparent vcad-render mark onto the macOS 26 icon plate.

  python3 compose-icon.py <mark.png> <out-1024.png>
  python3 compose-icon.py --sheet <out.png> <icon1.png> [icon2.png ...]

Plate: 1024 canvas, 824 px continuous-corner rounded rect centred (Apple's
macOS icon grid), near-black carbon gradient, faint brand-orange glow under the mark.
Needs Pillow.
"""
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter

S = 1024
BODY = 824
ORANGE = (242, 92, 31)  # brand-spec §4


def _bezier(p0, p1, p2, p3, n=24):
    return [tuple((1 - t) ** 3 * a + 3 * (1 - t) ** 2 * t * b + 3 * (1 - t) * t * t * c + t ** 3 * d
                  for a, b, c, d in zip(p0, p1, p2, p3)) for t in (k / n for k in range(1, n + 1))]


def squircle_mask(size, body, radius=185.4, ss=4):
    """Apple's continuous-corner rounded rect (the iOS 7+/Big Sur icon
    shape): each corner is three cubic Béziers spanning 1.528665·r along
    each edge, so curvature ramps up from the straight edge instead of
    jumping as a circular arc does. Body 824 on a 1024 canvas, r = 185.4
    (22.5% of the body), per Apple's macOS app icon template."""
    k = ss
    o = (size - body) / 2 * k
    W = body * k
    r = radius * k
    # One corner in local coords (x along the edge from the corner, y in).
    def corner(map_pt):
        seg = [(1.52866483, 0.0)]
        pts = []
        cur = (1.52866483, 0.0)
        for c1, c2, end in (((1.08849296, 0.0), (0.86840694, 0.0), (0.66993427, 0.06549600)),
                            ((0.37282383, 0.16905956), (0.16906001, 0.37282400), (0.06549600, 0.66993427)),
                            ((0.0, 0.86840694), (0.0, 1.08849296), (0.0, 1.52866483))):
            pts += _bezier(cur, c1, c2, end)
            cur = end
        return [map_pt(seg[0])] + [map_pt(p) for p in pts]
    tr = corner(lambda p: (o + W - p[0] * r, o + p[1] * r))
    br = corner(lambda p: (o + W - p[1] * r, o + W - p[0] * r))
    bl = corner(lambda p: (o + p[0] * r, o + W - p[1] * r))
    tl = corner(lambda p: (o + p[1] * r, o + p[0] * r))
    poly = tr + br + bl + tl
    m = Image.new("L", (size * k, size * k), 0)
    ImageDraw.Draw(m).polygon(poly, fill=255)
    return m.resize((size, size), Image.LANCZOS)


def vertical_gradient(size, top, bottom):
    g = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / (size - 1)
        g.putpixel((0, y), tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)))
    return g.resize((size, size))


def compose(mark_path, out_path):
    mask = squircle_mask(S, BODY)
    plate = vertical_gradient(S, (30, 31, 36), (10, 10, 11)).convert("RGBA")

    # Faint brand glow pooled beneath the mark.
    glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse((262, 640, 762, 820), fill=ORANGE + (120,))
    glow = glow.filter(ImageFilter.GaussianBlur(70))
    plate.alpha_composite(glow)

    # Soft top sheen, the glass edge every Big Sur+ icon carries.
    sheen = Image.new("L", (S, S), 0)
    ImageDraw.Draw(sheen).ellipse((40, -560, 984, 360), fill=22)
    sheen = sheen.filter(ImageFilter.GaussianBlur(60))
    plate.alpha_composite(Image.merge("RGBA", (*[Image.new("L", (S, S), 255)] * 3, sheen)))

    # The mark: trimmed, scaled to ~58% of the body width, optically centred.
    mark = Image.open(mark_path).convert("RGBA")
    mark = mark.crop(mark.getbbox())
    target_w = int(BODY * 0.60)
    mark = mark.resize((target_w, int(mark.height * target_w / mark.width)), Image.LANCZOS)
    x = (S - mark.width) // 2
    y = (S - mark.height) // 2 - 6
    # Contact shadow: blurred, darkened alpha of the mark, dropped down.
    a = mark.split()[3]
    shadow = Image.new("RGBA", mark.size, (0, 0, 0, 0))
    shadow.putalpha(a.point(lambda v: int(v * 0.55)))
    pad = 80
    sh = Image.new("RGBA", (mark.width + 2 * pad, mark.height + 2 * pad), (0, 0, 0, 0))
    sh.alpha_composite(shadow, (pad, pad))
    sh = sh.filter(ImageFilter.GaussianBlur(26))
    plate.alpha_composite(sh, (x - pad, y - pad + 34))
    plate.alpha_composite(mark, (x, y))

    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(plate, (0, 0), mask)
    out.save(out_path)


def sheet(out_path, icons):
    """One row per icon: full 1024, then actual-size 128 and 32 px on a dark
    and a light ground, for judging Dock / Finder-list legibility."""
    rows = []
    for p in icons:
        im = Image.open(p).convert("RGBA")
        row = Image.new("RGBA", (1024 + 2 * (128 + 32) + 5 * 40, 1024), (24, 24, 27, 255))
        row.alpha_composite(im, (0, 0))
        x = 1024 + 40
        for bg in ((24, 24, 27, 255), (236, 236, 238, 255)):
            tile = Image.new("RGBA", (128 + 32 + 60, 200), bg)
            tile.alpha_composite(im.resize((128, 128), Image.LANCZOS), (10, 36))
            tile.alpha_composite(im.resize((32, 32), Image.LANCZOS), (128 + 30, 84))
            row.alpha_composite(tile, (x, 412))
            x += tile.width + 20
        rows.append(row)
    W = rows[0].width
    out = Image.new("RGBA", (W, 1024 * len(rows)), (24, 24, 27, 255))
    for i, r in enumerate(rows):
        out.alpha_composite(r, (0, 1024 * i))
    out.save(out_path)


if __name__ == "__main__":
    if sys.argv[1] == "--sheet":
        sheet(sys.argv[2], sys.argv[3:])
    else:
        compose(sys.argv[1], sys.argv[2])
