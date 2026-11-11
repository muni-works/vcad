#!/usr/bin/env python3
"""Photoreal board assets for the launch page, straight from a .vcad PcbBoard.

For each board document this writes, into packages/app/public/launch/models/:

  <name>.pcb.json         outline (+ cutouts), thickness, and one entry per
                          footprint with a body box and a package class, so
                          the page can extrude real-looking components
  <name>-top.jpg          top-side colour: soldermask over FR4, copper traces
                          showing through the mask, ENIG pads, drills, silk
  <name>-bottom.jpg       the same for the bottom side (mirrored in x)
  <name>-top-rm.jpg       roughness (G) and metalness (B) for the top side
  <name>-bottom-rm.jpg    the same for the bottom side

Everything is drawn from the board's own data: no hand-placed decoration.

  python3 scripts/launch/pcb-board-assets.py examples/ice-viz/ice-viz.vcad:fpga-board \
      examples/pcb-motor/stator-v3.vcad:pcb-stator
"""
import json
import math
import os
import sys

from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(__file__), "../../packages/app/public/launch/models")
PX_PER_MM = 36

# Colours (sRGB). A green LPI soldermask over FR4, ENIG pads, white silk.
MASK_BARE = (24, 78, 44)        # mask over bare laminate
MASK_COPPER = (46, 112, 62)     # mask over copper: lighter, slightly yellow
ENIG = (214, 180, 96)
DRILL = (14, 12, 10)
SILK = (236, 236, 228)
# roughness (G) / metalness (B) per region
RM_MASK = (0, 110, 0)
RM_ENIG = (0, 60, 255)
RM_SILK = (0, 210, 0)
RM_DRILL = (0, 255, 0)


def board_of(doc):
    for node in doc["nodes"].values():
        op = node.get("op", {})
        if op.get("type") == "PcbBoard":
            return op["board"]
    if "pcb" in doc:
        return doc["pcb"]
    raise SystemExit("no PcbBoard in document")


def rot(p, deg):
    a = math.radians(deg)
    c, s = math.cos(a), math.sin(a)
    return (p[0] * c - p[1] * s, p[0] * s + p[1] * c)


def place(fp, local):
    x, y = local
    if not fp.get("front", True):
        x = -x
    r = rot((x, y), fp.get("rotation", 0))
    return (fp["position"]["x"] + r[0], fp["position"]["y"] + r[1])


class Canvas:
    """One side of the board, in board millimetres, y up."""

    def __init__(self, bbox, mirror):
        self.x0, self.y0, self.x1, self.y1 = bbox
        self.mirror = mirror
        self.w = max(1, int(math.ceil((self.x1 - self.x0) * PX_PER_MM)))
        self.h = max(1, int(math.ceil((self.y1 - self.y0) * PX_PER_MM)))
        self.color = Image.new("RGB", (self.w, self.h), MASK_BARE)
        self.rm = Image.new("RGB", (self.w, self.h), RM_MASK)
        self.dc = ImageDraw.Draw(self.color)
        self.dr = ImageDraw.Draw(self.rm)

    def px(self, p):
        x = (p[0] - self.x0) * PX_PER_MM
        if self.mirror:
            x = self.w - x
        return (x, (self.y1 - p[1]) * PX_PER_MM)

    def line(self, a, b, width, col, rm=None):
        w = max(1, int(round(width * PX_PER_MM)))
        A, B = self.px(a), self.px(b)
        self.dc.line([A, B], fill=col, width=w)
        r = w / 2
        for P in (A, B):
            self.dc.ellipse([P[0] - r, P[1] - r, P[0] + r, P[1] + r], fill=col)
        if rm:
            self.dr.line([A, B], fill=rm, width=w)

    def circle(self, c, d, col, rm=None):
        P = self.px(c)
        r = d * PX_PER_MM / 2
        self.dc.ellipse([P[0] - r, P[1] - r, P[0] + r, P[1] + r], fill=col)
        if rm:
            self.dr.ellipse([P[0] - r, P[1] - r, P[0] + r, P[1] + r], fill=rm)

    def poly(self, pts, col, rm=None):
        P = [self.px(p) for p in pts]
        self.dc.polygon(P, fill=col)
        if rm:
            self.dr.polygon(P, fill=rm)

    def text(self, c, s, size_mm, col):
        try:
            font = ImageFont.truetype("/System/Library/Fonts/Supplemental/Arial.ttf", max(8, int(size_mm * PX_PER_MM)))
        except OSError:
            font = ImageFont.load_default()
        P = self.px(c)
        self.dc.text(P, s, fill=col, font=font, anchor="mm")
        self.dr.text(P, s, fill=RM_SILK, font=font, anchor="mm")


def pad_polygon(fp, pad):
    shape = pad["shape"]
    cx, cy = pad["position"]["x"], pad["position"]["y"]
    prot = pad.get("rotation", 0)
    if shape["type"] in ("Rect", "RoundRect", "Oval"):
        hw, hh = shape["width"] / 2, shape["height"] / 2
        corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
        return [place(fp, (cx + q[0], cy + q[1])) for q in (rot(c, prot) for c in corners)]
    return None


def package_class(fp):
    n = (fp.get("footprintName") or "").upper()
    if n.startswith(("QFN", "QFP", "LQFP", "TQFP", "BGA", "DFN")):
        return "ic-flat", 0.9
    if n.startswith(("SOIC", "SOP", "TSSOP", "MSOP")):
        return "ic", 1.5
    if n.startswith("SOT"):
        return "ic", 1.6
    if n.startswith(("R_", "RES")):
        return "resistor", 0.45
    if n.startswith(("C_", "CAP")):
        return "capacitor", 0.8
    if n.startswith("LED"):
        return "led", 0.6
    if n.startswith("CRYSTAL") or n.startswith("XTAL"):
        return "can", 0.8
    if "USB" in n:
        return "usb", 3.2
    if n.startswith("PINHEADER") or n.startswith("JST") or "XH" in n:
        return "header", 2.5
    if n.startswith("SW"):
        return "switch", 1.6
    return "ic", 1.0


def body_box(fp):
    """Body rectangle in footprint-local mm: the silkscreen rect if present,
    else the pad extents (shrunk for 2-pad passives so the end caps sit on
    the pads)."""
    silk = [g for g in fp.get("graphics", []) if g.get("layer", "").endswith("SilkS")]
    rects = [g for g in silk if g["type"] == "Rect"]
    if rects:
        g = rects[0]
        return min(g["start"]["x"], g["end"]["x"]), min(g["start"]["y"], g["end"]["y"]), \
            max(g["start"]["x"], g["end"]["x"]), max(g["start"]["y"], g["end"]["y"])
    lines = [g for g in silk if g["type"] == "Line"]
    if len(lines) >= 3:
        xs = [v for g in lines for v in (g["start"]["x"], g["end"]["x"])]
        ys = [v for g in lines for v in (g["start"]["y"], g["end"]["y"])]
        return min(xs), min(ys), max(xs), max(ys)
    xs, ys = [], []
    for p in fp["pads"]:
        s = p["shape"]
        hw = (s.get("width") or s.get("diameter") or 0.5) / 2
        hh = (s.get("height") or s.get("diameter") or 0.5) / 2
        xs += [p["position"]["x"] - hw, p["position"]["x"] + hw]
        ys += [p["position"]["y"] - hh, p["position"]["y"] + hh]
    if not xs:
        return -0.5, -0.5, 0.5, 0.5
    return min(xs), min(ys), max(xs), max(ys)


def draw_side(board, side, bbox):
    front = side == "top"
    cu = "FCu" if front else "BCu"
    silk_layer = "FSilkS" if front else "BSilkS"
    cv = Canvas(bbox, mirror=not front)
    for t in board.get("traces", []):
        if t.get("layer") == cu:
            cv.line((t["start"]["x"], t["start"]["y"]), (t["end"]["x"], t["end"]["y"]), t.get("width", 0.2), MASK_COPPER)
    for z in board.get("zones", []):
        if z.get("layer") == cu and z.get("outline"):
            cv.poly([(v["x"], v["y"]) for v in z["outline"]], MASK_COPPER)
    for v in board.get("vias", []):
        c = (v["position"]["x"], v["position"]["y"])
        cv.circle(c, v.get("diameter", 0.6), MASK_COPPER)           # tented
        cv.circle(c, v.get("drill", 0.3), DRILL, RM_DRILL)
    # Silk first, then pads on top: the fab clips silkscreen at mask openings.
    for fp in board.get("footprints", []):
        if fp.get("front", True) != front:
            continue
        for g in fp.get("graphics", []):
            if g.get("layer") != silk_layer:
                continue
            w = g.get("width", 0.12)
            if g["type"] == "Line":
                cv.line(place(fp, (g["start"]["x"], g["start"]["y"])), place(fp, (g["end"]["x"], g["end"]["y"])), w, SILK, RM_SILK)
            elif g["type"] == "Rect":
                a, b = g["start"], g["end"]
                pts = [(a["x"], a["y"]), (b["x"], a["y"]), (b["x"], b["y"]), (a["x"], b["y"])]
                for i in range(4):
                    cv.line(place(fp, pts[i]), place(fp, pts[(i + 1) % 4]), w, SILK, RM_SILK)
            elif g["type"] == "Circle":
                cv.circle(place(fp, (g["center"]["x"], g["center"]["y"])), 2 * g["radius"] + w, SILK, RM_SILK)
        x0, y0, x1, y1 = body_box(fp)
        cv.text(place(fp, ((x0 + x1) / 2, y1 + 0.9)), fp["ref"], 0.9, SILK)
    for fp in board.get("footprints", []):
        on_side = fp.get("front", True) == front
        for pad in fp["pads"]:
            tht = pad.get("padType") == "THT"
            if not (tht or on_side):
                continue
            c = place(fp, (pad["position"]["x"], pad["position"]["y"]))
            poly = pad_polygon(fp, pad)
            if poly:
                cv.poly(poly, ENIG, RM_ENIG)
            else:
                cv.circle(c, pad["shape"].get("diameter", 1.0), ENIG, RM_ENIG)
            if tht and pad.get("drill"):
                cv.circle(c, pad["drill"].get("diameter", 0.8), DRILL, RM_DRILL)
    return cv


def main():
    os.makedirs(OUT, exist_ok=True)
    for arg in sys.argv[1:]:
        path, name = arg.split(":")
        board = board_of(json.load(open(path)))
        outline = [(v["x"], v["y"]) for v in board["outline"]["vertices"]]
        cutouts = [[(v["x"], v["y"]) for v in c] for c in board["outline"].get("cutouts", [])]
        xs, ys = [p[0] for p in outline], [p[1] for p in outline]
        bbox = (min(xs), min(ys), max(xs), max(ys))
        for side in ("top", "bottom"):
            cv = draw_side(board, side, bbox)
            cv.color.save(os.path.join(OUT, f"{name}-{side}.jpg"), quality=86, optimize=True)
            cv.rm.save(os.path.join(OUT, f"{name}-{side}-rm.jpg"), quality=80, optimize=True)
        comps = []
        for fp in board.get("footprints", []):
            cls, height = package_class(fp)
            x0, y0, x1, y1 = body_box(fp)
            comps.append({
                "ref": fp["ref"], "name": fp.get("footprintName"), "class": cls, "front": fp.get("front", True),
                "x": fp["position"]["x"], "y": fp["position"]["y"], "rotation": fp.get("rotation", 0),
                "box": [x0, y0, x1, y1], "height": height,
                "pins": [[p["position"]["x"], p["position"]["y"]] for p in fp["pads"] if p.get("padType") == "THT"],
            })
        json.dump({
            "outline": outline, "cutouts": cutouts, "thickness": board["outline"].get("thickness", 1.6),
            "bbox": bbox, "components": comps,
        }, open(os.path.join(OUT, f"{name}.pcb.json"), "w"), separators=(",", ":"))
        print(name, f"{len(comps)} components", f"{int((bbox[2]-bbox[0])*PX_PER_MM)}x{int((bbox[3]-bbox[1])*PX_PER_MM)} px")


if __name__ == "__main__":
    main()
