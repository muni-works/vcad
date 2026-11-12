#!/usr/bin/env python3
"""Generate the four animated tour illustrations for www/index.html.

Isometric line art in the Pilot Light style (phyz.dev / tang.toys): theme
colours come from CSS classes (il-*), motion is SMIL so the cards need no JS.
Run:  python3 www/scripts/gen-tour-art.py   (rewrites the block between the
TOUR-ART markers in www/index.html)
"""
import math, pathlib, re

C30 = math.cos(math.pi / 6)

def iso(S, cx, cy):
    def P(x, y, z):
        return (cx + (x - y) * C30 * S, cy + (x + y) * 0.5 * S - z * S)
    return P

def pts(P, quad):
    return " ".join(f"{a:.1f},{b:.1f}" for a, b in (P(*q) for q in quad))

def faces(P, x, y, z, w, d, h):
    return {
        "top": pts(P, [(x, y, z + h), (x + w, y, z + h), (x + w, y + d, z + h), (x, y + d, z + h)]),
        "right": pts(P, [(x + w, y, z), (x + w, y + d, z), (x + w, y + d, z + h), (x + w, y, z + h)]),
        "front": pts(P, [(x, y + d, z), (x + w, y + d, z), (x + w, y + d, z + h), (x, y + d, z + h)]),
    }

def box(P, x, y, z, w, d, h, top="il-top", extra=""):
    f = faces(P, x, y, z, w, d, h)
    return (f'<polygon class="il-front" points="{f["front"]}"/>'
            f'<polygon class="il-right" points="{f["right"]}"/>'
            f'<polygon class="{top}" points="{f["top"]}"/>{extra}')

def anim_points(values, kt, dur, extra=""):
    return (f'<animate attributeName="points" dur="{dur}s" repeatCount="indefinite" '
            f'keyTimes="{kt}" values="{";".join(values)}" {extra}/>')

def anim_attr(attr, values, kt, dur, extra=""):
    return (f'<animate attributeName="{attr}" dur="{dur}s" repeatCount="indefinite" '
            f'keyTimes="{kt}" values="{values}" {extra}/>')

def svg(body, label):
    return (f'<svg class="il" viewBox="0 0 400 260" role="img" aria-label="{label}">'
            f'{body}</svg>')

EASE = 'calcMode="spline" keySplines="{}"'
def ease(n):  # n segments of ease-in-out
    return EASE.format(";".join(["0.45 0 0.25 1"] * n))

# ---------------------------------------------------------------- 1. model
def card_model():
    S, D = 2.0, 7
    P = iso(S, 200, 150)
    flat = faces(P, -30, -20, 0, 60, 40, 0.01)
    up = faces(P, -30, -20, 0, 60, 40, 26)
    kt = "0;0.32;0.58;0.9;1"
    sp = ease(4)
    def seq(key):
        return [flat[key], flat[key], up[key], up[key], flat[key]]
    g = []
    # sketch grid on the XY plane
    for i in range(-60, 61, 12):
        a, b = P(i, -50, 0), P(i, 50, 0)
        c, d = P(-60, i * 50 / 60, 0), P(60, i * 50 / 60, 0)
        g.append(f'<line class="il-grid" x1="{a[0]:.1f}" y1="{a[1]:.1f}" x2="{b[0]:.1f}" y2="{b[1]:.1f}"/>')
        g.append(f'<line class="il-grid" x1="{c[0]:.1f}" y1="{c[1]:.1f}" x2="{d[0]:.1f}" y2="{d[1]:.1f}"/>')
    # sketch outline draws itself
    g.append(f'<polygon class="il-sketch" pathLength="1" points="{flat["top"]}">'
             + anim_attr("stroke-dashoffset", "1;0;0;0;1", "0;0.16;0.5;0.9;1", D) + '</polygon>')
    # solid
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.32;0.36;0.9;1", D))
    g.append(f'<polygon class="il-front" points="{flat["front"]}">{anim_points(seq("front"), kt, D, sp)}</polygon>')
    g.append(f'<polygon class="il-right" points="{flat["right"]}">{anim_points(seq("right"), kt, D, sp)}</polygon>')
    g.append(f'<polygon class="il-sel" points="{flat["top"]}">{anim_points(seq("top"), kt, D, sp)}</polygon>')
    g.append('</g>')
    # constraint glyphs + sketch dimension
    a, b = P(-30, -20, 0), P(30, -20, 0)
    mx, my = (a[0] + b[0]) / 2, (a[1] + b[1]) / 2
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0;0", "0;0.14;0.2;0.34;0.4;1", D))
    for q in [(-30, -20), (30, 20)]:
        x, y = P(q[0], q[1], 0)
        g.append(f'<rect class="il-glyph" x="{x-3.5:.1f}" y="{y-3.5:.1f}" width="7" height="7"/>')
    g.append(f'<text class="il-t il-acc" x="{mx:.1f}" y="{my-10:.1f}" text-anchor="middle" '
             f'transform="rotate(-30 {mx:.1f} {my-10:.1f})">60.00</text></g>')
    # press-pull arrow rides the top face
    cx, cy = P(0, 0, 0)
    dz = 26 * S
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.3;0.34;0.9;1", D)
             + f'<g><animateTransform attributeName="transform" type="translate" dur="{D}s" repeatCount="indefinite" '
               f'keyTimes="{kt}" values="0 0;0 0;0 {-dz:.1f};0 {-dz:.1f};0 0" {sp}/>'
             + f'<line class="il-arrow" x1="{cx:.1f}" y1="{cy:.1f}" x2="{cx:.1f}" y2="{cy-34:.1f}"/>'
             + f'<polygon class="il-arrowhead" points="{cx-6:.1f},{cy-32:.1f} {cx+6:.1f},{cy-32:.1f} {cx:.1f},{cy-44:.1f}"/>'
             + '</g></g>')
    # live height readout
    hx, hy = P(30, 20, 13)
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.5;0.58;0.9;1", D)
             + f'<rect class="il-chip" x="{hx+14:.1f}" y="{hy-11:.1f}" width="72" height="20"/>'
             + f'<text class="il-t il-fg" x="{hx+50:.1f}" y="{hy+3:.1f}" text-anchor="middle">h 26.00</text></g>')
    return svg("".join(g), "A sketched rectangle with constraints is press-pulled into a solid")

# ---------------------------------------------------------------- 2. drawing
def card_drawing():
    D = 8
    g = ['<rect class="il-sheet" x="36" y="18" width="328" height="224"/>',
         '<rect class="il-frame" x="44" y="26" width="312" height="208"/>']
    def draw(el, t0, t1):
        kt = f"0;{t0};{t1};0.9;1"
        return el.replace("/>", f' pathLength="1" class="il-view">' +
                          anim_attr("stroke-dashoffset", "1;1;0;0;1", kt, D) + "</" + el.split()[0][1:] + ">", 1)
    # front view: L bracket
    g.append(draw('<path d="M74 128 h92 v-22 h-56 v-56 h-36 z"/>', 0.02, 0.18))
    g.append(draw('<path d="M92 128 v-10 M148 128 v-10" />', 0.1, 0.2))
    # top view below
    g.append(draw('<path d="M74 152 h92 v40 h-92 z"/>', 0.16, 0.3))
    g.append(draw('<circle cx="92" cy="172" r="7"/>', 0.24, 0.32))
    g.append(draw('<circle cx="148" cy="172" r="7"/>', 0.26, 0.34))
    # side view, sectioned
    g.append(draw('<path d="M194 128 h40 v-78 h-14 v56 h-26 z"/>', 0.3, 0.44))
    hatch = "".join(f'<line x1="{194+i}" y1="128" x2="{194+i+16}" y2="112"/>' for i in range(2, 40, 6))
    g.append('<g class="il-hatch" clip-path="url(#ilSec)">' + anim_attr("opacity", "0;0;1;1;0", "0;0.44;0.5;0.9;1", D) + hatch + "</g>")
    g.append('<clipPath id="ilSec"><path d="M194 128 h40 v-78 h-14 v56 h-26 z"/></clipPath>')
    # dimensions
    dims = ('<g class="il-dim">'
            '<line x1="74" y1="140" x2="166" y2="140"/><line x1="74" y1="135" x2="74" y2="145"/><line x1="166" y1="135" x2="166" y2="145"/>'
            '<line x1="60" y1="50" x2="60" y2="128"/><line x1="55" y1="50" x2="65" y2="50"/><line x1="55" y1="128" x2="65" y2="128"/>'
            '</g><text class="il-t il-acc" x="120" y="137" text-anchor="middle">80.00</text>'
            '<text class="il-t il-acc" x="56" y="89" text-anchor="middle" transform="rotate(-90 56 89)">40.00</text>'
            '<text class="il-t" x="214" y="146" text-anchor="middle">A–A</text>')
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.52;0.6;0.9;1", D) + dims + "</g>")
    # title block
    tb = ('<g class="il-frame-g"><rect x="262" y="190" width="94" height="44"/><line x1="262" y1="206" x2="356" y2="206"/>'
          '<line x1="309" y1="206" x2="309" y2="234"/></g>'
          '<text class="il-t il-fg" x="270" y="202">L BRACKET</text>'
          '<text class="il-t" x="270" y="224">1:1</text><text class="il-t" x="317" y="224">REV A</text>')
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.6;0.68;0.9;1", D) + tb + "</g>")
    # export chips
    chips = ('<rect class="il-chip" x="262" y="40" width="40" height="18"/><text class="il-t il-fg" x="282" y="53" text-anchor="middle">PDF</text>'
             '<rect class="il-chip" x="308" y="40" width="40" height="18"/><text class="il-t il-fg" x="328" y="53" text-anchor="middle">DXF</text>')
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.7;0.76;0.9;1", D) + chips + "</g>")
    return svg("".join(g), "A dimensioned drawing sheet draws itself: front, top and section views, title block")

# ---------------------------------------------------------------- 3. print
def card_print():
    D, S, N, LH = 8, 1.9, 11, 4
    P = iso(S, 200, 168)
    g = ['<defs>'
         '<pattern id="ilLayL" width="8" height="7.6" patternUnits="userSpaceOnUse" patternTransform="skewY(30)"><line x1="0" y1="0.5" x2="8" y2="0.5" class="il-layer"/></pattern>'
         '<pattern id="ilLayR" width="8" height="7.6" patternUnits="userSpaceOnUse" patternTransform="skewY(-30)"><line x1="0" y1="0.5" x2="8" y2="0.5" class="il-layer"/></pattern>'
         '</defs>']
    bed = faces(P, -62, -42, -3, 124, 84, 3)
    g.append(f'<polygon class="il-front" points="{bed["front"]}"/><polygon class="il-right" points="{bed["right"]}"/><polygon class="il-bed" points="{bed["top"]}"/>')
    for i in range(-60, 61, 15):
        a, b = P(i, -42, 0), P(i, 42, 0)
        g.append(f'<line class="il-grid" x1="{a[0]:.1f}" y1="{a[1]:.1f}" x2="{b[0]:.1f}" y2="{b[1]:.1f}"/>')
    steps = [faces(P, -26, -18, 0, 52, 36, max(k * LH, 0.01)) for k in range(N + 1)]
    seq = list(range(N + 1)) + [N, N, 0]
    kts = [round(0.06 + 0.06 * k, 3) for k in range(N + 1)]
    kt = ";".join(["0"] + [str(t) for t in kts] + ["0.9", "1"])
    seq = [0] + list(range(N + 1)) + [N, 0]
    def vals(key):
        return [steps[k][key] for k in seq]
    disc = 'calcMode="discrete"'
    g.append(f'<polygon class="il-front" points="{steps[0]["front"]}">{anim_points(vals("front"), kt, D, disc)}</polygon>')
    g.append(f'<polygon fill="url(#ilLayL)" points="{steps[0]["front"]}">{anim_points(vals("front"), kt, D, disc)}</polygon>')
    g.append(f'<polygon class="il-right" points="{steps[0]["right"]}">{anim_points(vals("right"), kt, D, disc)}</polygon>')
    g.append(f'<polygon fill="url(#ilLayR)" points="{steps[0]["right"]}">{anim_points(vals("right"), kt, D, disc)}</polygon>')
    g.append(f'<polygon class="il-top" points="{steps[0]["top"]}">{anim_points(vals("top"), kt, D, disc)}</polygon>')
    # nozzle: climbs with the layers, scans the current layer
    cx, cy = P(0, 0, 0)
    ys = ";".join(f"0 {-k*LH*S:.1f}" for k in seq)
    a, b = P(-20, 0, 0), P(20, 0, 0)
    c, d = P(0, -14, 0), P(0, 14, 0)
    scan = f"{a[0]-cx:.1f} {a[1]-cy:.1f};{c[0]-cx:.1f} {c[1]-cy:.1f};{b[0]-cx:.1f} {b[1]-cy:.1f};{d[0]-cx:.1f} {d[1]-cy:.1f};{a[0]-cx:.1f} {a[1]-cy:.1f}"
    g.append('<g>' + anim_attr("opacity", "1;1;0;0;1", "0;0.84;0.86;0.98;1", D)
             + f'<g><animateTransform attributeName="transform" type="translate" dur="{D}s" repeatCount="indefinite" keyTimes="{kt}" values="{ys}" {disc}/>'
             + f'<g><animateTransform attributeName="transform" type="translate" dur="0.48s" repeatCount="indefinite" values="{scan}"/>'
             + f'<rect class="il-head" x="{cx-12:.1f}" y="{cy-34:.1f}" width="24" height="18"/>'
             + f'<polygon class="il-arrowhead" points="{cx-5:.1f},{cy-16:.1f} {cx+5:.1f},{cy-16:.1f} {cx:.1f},{cy-5:.1f}"/>'
             + '</g></g></g>')
    for lab, x, vals_, kts_ in [("orient", 138, "il-t il-step il-on;il-t il-step", "0;0.04"),
                                ("slice", 200, "il-t il-step;il-t il-step il-on;il-t il-step", "0;0.04;0.08"),
                                ("print", 262, "il-t il-step;il-t il-step il-on;il-t il-step", "0;0.08;0.9")]:
        g.append(f'<text class="il-t il-step" x="{x}" y="34" text-anchor="middle">{lab}'
                 + anim_attr("class", vals_, kts_, D, 'calcMode="discrete"') + '</text>')
    g.append('<text class="il-t" x="169" y="34" text-anchor="middle">→</text><text class="il-t" x="231" y="34" text-anchor="middle">→</text>')
    return svg("".join(g), "A part prints layer by layer on a build plate: orient, slice, print")

# ---------------------------------------------------------------- 4. assembly
def card_assembly():
    D, S = 8, 1.65
    P = iso(S, 200, 168)
    kt = "0;0.18;0.5;0.86;1"
    sp = ease(4)
    def part(body, dz, dx=0):
        ox, oy = (dx * C30 * S, dx * 0.5 * S - dz * S)
        v = f"{ox:.1f} {oy:.1f}"
        return (f'<g><animateTransform attributeName="transform" type="translate" dur="{D}s" repeatCount="indefinite" '
                f'keyTimes="{kt}" values="{v};{v};0 0;0 0;{v}" {sp}/>{body}</g>')
    g = []
    g.append(box(P, -60, -40, 0, 120, 80, 8))
    # mate guides (visible while exploded)
    guides = []
    for (x, y, dz) in [(-35, 0, 40), (35, -10, 64), (35, 10, 64), (-44, 32, 92), (-4, 32, 92)]:
        a, b = P(x, y, 8), P(x, y, 8 + dz)
        guides.append(f'<line class="il-guide" x1="{a[0]:.1f}" y1="{a[1]:.1f}" x2="{b[0]:.1f}" y2="{b[1]:.1f}"/>')
    g.append('<g>' + anim_attr("opacity", "1;1;0;0;1", "0;0.2;0.45;0.88;1", D) + "".join(guides) + "</g>")
    g.append(part(box(P, -50, -20, 8, 30, 40, 44), 40))
    g.append(part(box(P, 18, -24, 8, 34, 48, 18, top="il-sel"), 64))
    # bolts: hex-ish caps
    for (x, y) in [(-44, 32), (-4, 32)]:
        cx, cy = P(x, y, 8)
        cap = (f'<rect class="il-right" x="{cx-4:.1f}" y="{cy-14:.1f}" width="8" height="14"/>'
               f'<ellipse class="il-top" cx="{cx:.1f}" cy="{cy-14:.1f}" rx="7" ry="3.6"/>')
        g.append(part(cap, 92))
    # mated badge
    g.append('<g>' + anim_attr("opacity", "0;0;0;1;0", "0;0.2;0.5;0.56;1", D)
             + '<rect class="il-chip" x="138" y="22" width="124" height="22"/>'
             + '<text class="il-t il-ok" x="200" y="37" text-anchor="middle">✓ mated · 0 clashes</text></g>')
    g.append('<g>' + anim_attr("opacity", "1;1;0;0;1", "0;0.18;0.22;0.86;1", D)
             + '<text class="il-t" x="200" y="37" text-anchor="middle">exploded</text></g>')
    return svg("".join(g), "Exploded parts slide together onto a base plate and mate with no clashes")

# ---------------------------------------------------------------- 5. versions
def card_versions():
    D = 9
    g = []
    y0 = 150
    g.append(f'<line class="il-rail" x1="40" y1="{y0}" x2="360" y2="{y0}"/>')
    hs = [10, 16, 16, 24, 30]
    times = ["10:02", "10:07", "10:12", "10:17", "now"]
    n = len(hs)
    for i, (h, t) in enumerate(zip(hs, times)):
        x = 64 + i * 68
        P = iso(1.05, x, y0 - 34)
        t0 = round(0.06 + i * 0.13, 2)
        last = i == n - 1
        thumb = (f'<rect class="{"il-thumb-on" if last else "il-thumb"}" x="{x-28}" y="{y0-80}" width="56" height="62"/>'
                 + box(P, -12, -9, 0, 24, 18, h, top="il-sel" if last else "il-top")
                 + f'<circle class="{"il-dot-on" if last else "il-dot"}" cx="{x}" cy="{y0}" r="{5 if last else 3.5}"/>'
                 + f'<text class="il-t{" il-acc" if last else ""}" x="{x}" y="{y0+22}" text-anchor="middle">{t}</text>')
        g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", f"0;{t0};{t0+0.04};0.9;1", D) + thumb + "</g>")
    # restore: a marker hops back to 10:12 and the "now" thumbnail matches it
    rx = 64 + 2 * 68
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.7;0.74;0.9;1", D)
             + f'<path class="il-guide" d="M{64+4*68} {y0-88} C {64+4*68} {y0-118}, {rx} {y0-118}, {rx} {y0-88}" fill="none"/>'
             + f'<polygon class="il-arrowhead" points="{rx-5},{y0-92} {rx+5},{y0-92} {rx},{y0-84}"/>'
             + f'<rect class="il-chip" x="{rx+40}" y="{y0-134}" width="96" height="20"/>'
             + f'<text class="il-t il-fg" x="{rx+88}" y="{y0-120}" text-anchor="middle">restore 10:12</text></g>')
    g.append('<g>' + anim_attr("opacity", "1;1;0;0;1", "0;0.66;0.7;0.9;1", D)
             + f'<text class="il-t" x="200" y="226" text-anchor="middle">autosaved · every change</text></g>')
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.7;0.74;0.9;1", D)
             + f'<text class="il-t il-ok" x="200" y="226" text-anchor="middle">✓ crash recovery · safe save</text></g>')
    return svg("".join(g), "Autosaved versions appear on a timeline; an older version is restored")

# ---------------------------------------------------------------- 6. image to CAD
def card_photo():
    D = 8
    g = []
    # the photo: a soft, shaded bracket on a table
    g.append('<defs><linearGradient id="ilPhoto" x1="0" y1="0" x2="1" y2="1">'
             '<stop offset="0" class="il-ph0"/><stop offset="1" class="il-ph1"/></linearGradient>'
             '<clipPath id="ilPhotoClip"><rect x="34" y="50" width="140" height="160"/></clipPath></defs>')
    g.append('<rect class="il-photo" x="34" y="50" width="140" height="160" fill="url(#ilPhoto)"/>')
    g.append('<g clip-path="url(#ilPhotoClip)">'
             '<ellipse class="il-shadow" cx="106" cy="172" rx="58" ry="10"/>'
             '<path class="il-blob" d="M58 166 L58 104 Q58 96 66 96 L82 96 L82 150 L150 150 Q156 150 156 158 L156 166 Z"/>'
             '<path class="il-blob-hi" d="M60 104 Q60 98 66 98 L78 98 L78 160 L62 160 Z"/>'
             + '<line class="il-scan" x1="34" y1="50" x2="174" y2="50">'
             + '<animate attributeName="y1" dur="8s" repeatCount="indefinite" keyTimes="0;0.06;0.36;1" values="50;50;210;210"/>'
             + '<animate attributeName="y2" dur="8s" repeatCount="indefinite" keyTimes="0;0.06;0.36;1" values="50;50;210;210"/>'
             + anim_attr("opacity", "0;1;1;0;0", "0;0.06;0.36;0.38;1", D) + '</line></g>')
    g.append('<text class="il-t" x="104" y="228" text-anchor="middle">photo.jpg</text>')
    # arrow
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.36;0.4;0.9;1", D)
             + '<line class="il-arrow" x1="186" y1="130" x2="214" y2="130"/>'
             + '<polygon class="il-arrowhead" points="212,124 224,130 212,136"/></g>')
    # the CAD result: an iso L-bracket drawing itself
    P = iso(1.55, 300, 150)
    L = box(P, -30, -20, 0, 60, 40, 8) + box(P, -30, -20, 8, 14, 40, 40)
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.42;0.56;0.9;1", D) + L + "</g>")
    a, b = P(30, 20, 0), P(30, -20, 0)
    g.append('<g>' + anim_attr("opacity", "0;0;1;1;0", "0;0.6;0.66;0.9;1", D)
             + f'<text class="il-t il-acc" x="{(a[0]+b[0])/2+14:.1f}" y="{(a[1]+b[1])/2+12:.1f}" text-anchor="middle" '
               f'transform="rotate(-30 {(a[0]+b[0])/2+14:.1f} {(a[1]+b[1])/2+12:.1f})">40.0</text>'
             + '<text class="il-t" x="300" y="228" text-anchor="middle">editable features</text></g>')
    return svg("".join(g), "A photo of a bracket is scanned and becomes an editable CAD model")

if __name__ == "__main__":
    html = pathlib.Path(__file__).resolve().parent.parent / "index.html"
    s = html.read_text()
    arts = {"model": card_model(), "drawing": card_drawing(), "print": card_print(), "assembly": card_assembly(), "versions": card_versions(), "photo": card_photo()}
    for k, v in arts.items():
        s, n = re.subn(rf"(<!-- ART:{k} -->).*?(<!-- /ART:{k} -->)", lambda m: m.group(1) + v + m.group(2), s, flags=re.S)
        assert n == 1, k
    html.write_text(s)
    print("ok", {k: len(v) for k, v in arts.items()})
