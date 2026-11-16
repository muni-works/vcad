#!/usr/bin/env python3
"""Render docs/videos/open-fusor/poc.csv -> prediction.svg (stdlib only).

Two panels, one y-axis each:
  left  — predicted D-D neutron rate vs cathode voltage (bracket band:
          CX-on single-generation floor .. CX-off ceiling), log y
  right — H*(10) neutron dose vs distance at the 5e6 n/s upper-bound
          source, bare vs 15 cm HDPE, log y, with the 2.5 uSv/h budget

Usage: python3 docs/videos/open-fusor/plot.py
"""
import csv
import math
import pathlib

HERE = pathlib.Path(__file__).parent
SRC = HERE / "poc.csv"
OUT = HERE / "prediction.svg"

BLUE, ORANGE = "#2a78d6", "#eb6834"
INK, INK2, GRID, SURF = "#0b0b0b", "#52514e", "#e4e3df", "#fcfcfb"


def sections(path):
    out, cur = {}, None
    for line in path.read_text().splitlines():
        if line.startswith("# "):
            name = line[2:].split(",")[0]
            cur = out.setdefault(name, [])
        elif cur is not None and line.strip():
            cur.append(line)
    return {k: list(csv.DictReader(v)) for k, v in out.items() if v and "," in v[0]}


def panel(x0, y0, w, h, xs, xlim, ylim, title, xlabel, ylabel, xticks, series, band=None, hlines=()):
    lx = lambda v: x0 + (v - xlim[0]) / (xlim[1] - xlim[0]) * w
    ly = lambda v: y0 + h - (math.log10(v) - ylim[0]) / (ylim[1] - ylim[0]) * h
    s = [f'<text x="{x0}" y="{y0 - 18}" font-size="15" font-weight="600" fill="{INK}">{title}</text>']
    for d in range(ylim[0], ylim[1] + 1):
        y = ly(10 ** d)
        s.append(f'<line x1="{x0}" x2="{x0 + w}" y1="{y:.1f}" y2="{y:.1f}" stroke="{GRID}" stroke-width="1"/>')
        s.append(f'<text x="{x0 - 8}" y="{y + 4:.1f}" font-size="11" fill="{INK2}" text-anchor="end">10<tspan dy="-5" font-size="8">{d}</tspan></text>')
    for t in xticks:
        s.append(f'<text x="{lx(t):.1f}" y="{y0 + h + 18}" font-size="11" fill="{INK2}" text-anchor="middle">{t:g}</text>')
    s.append(f'<line x1="{x0}" x2="{x0 + w}" y1="{y0 + h}" y2="{y0 + h}" stroke="{INK2}" stroke-width="1"/>')
    s.append(f'<text x="{x0 + w / 2}" y="{y0 + h + 38}" font-size="12" fill="{INK2}" text-anchor="middle">{xlabel}</text>')
    s.append(f'<text transform="translate({x0 - 46},{y0 + h / 2}) rotate(-90)" font-size="12" fill="{INK2}" text-anchor="middle">{ylabel}</text>')
    for val, label in hlines:
        y = ly(val)
        s.append(f'<line x1="{x0}" x2="{x0 + w}" y1="{y:.1f}" y2="{y:.1f}" stroke="{INK2}" stroke-width="1.5" stroke-dasharray="5 4"/>')
        s.append(f'<text x="{x0 + w - 4}" y="{y - 6:.1f}" font-size="11" fill="{INK2}" text-anchor="end">{label}</text>')
    if band:
        lo, hi, color = band
        pts = [f"{lx(x):.1f},{ly(v):.1f}" for x, v in zip(xs, hi)] + [
            f"{lx(x):.1f},{ly(v):.1f}" for x, v in reversed(list(zip(xs, lo)))
        ]
        s.append(f'<polygon points="{" ".join(pts)}" fill="{color}" fill-opacity="0.14"/>')
    for ys, color, label in series:
        pts = " ".join(f"{lx(x):.1f},{ly(v):.1f}" for x, v in zip(xs, ys))
        s.append(f'<polyline points="{pts}" fill="none" stroke="{color}" stroke-width="2"/>')
        for x, v in zip(xs, ys):
            s.append(f'<circle cx="{lx(x):.1f}" cy="{ly(v):.1f}" r="4" fill="{color}" stroke="{SURF}" stroke-width="2"/>')
        s.append(f'<text x="{lx(xs[-1]) + 8:.1f}" y="{ly(ys[-1]) + 4:.1f}" font-size="11" fill="{INK}">{label}</text>')
    return "\n".join(s)


def main():
    d = sections(SRC)
    sw = d["sweep"]
    kv = [float(r["kv"]) for r in sw]
    lo = [float(r["rate_low_cx_on"]) for r in sw]
    hi = [float(r["rate_high_cx_off"]) for r in sw]
    left = panel(
        80, 70, 360, 300, kv, (18, 42), (3, 7),
        "Predicted neutron rate — hobby fusor, 10 mA, 5 mTorr D₂",
        "cathode voltage (−kV)", "neutrons / s", [20, 25, 30, 35, 40],
        [(hi, BLUE, "CX off (ceiling)"), (lo, ORANGE, "CX on (floor)")],
        band=(lo, hi, BLUE),
        hlines=[(5e6, "amateur record ~5×10⁶")],
    )
    dose = d["dose"]
    dist = sorted({float(r["detector_m"]) for r in dose})
    bare = [float(r["dose_usv_h"]) for r in dose if float(r["shield_hdpe_mm"]) == 0]
    hdpe = [float(r["dose_usv_h"]) for r in dose if float(r["shield_hdpe_mm"]) == 150]
    right = panel(
        640, 70, 300, 300, dist, (0.3, 3.2), (-1, 3),
        "Neutron dose at a 5×10⁶ n/s upper-bound source",
        "distance from chamber center (m)", "H*(10), µSv/h", [0.5, 1, 2, 3],
        [(bare, BLUE, "bare"), (hdpe, ORANGE, "15 cm HDPE")],
        hlines=[(2.5, "2.5 µSv/h design budget")],
    )
    foot = (
        f'<text x="80" y="450" font-size="11" fill="{INK2}">vcad-kernel-particle + vcad-kernel-neutronics, '
        "examples/open_fusor_poc.rs. Band = physics bracket, not a confidence interval; both edges omit the CX chain, "
        "volume ionization and beam–beam.</text>"
        f'<text x="80" y="466" font-size="11" fill="{INK2}">Ceiling is not grid-converged (1.8→3.3→4.4×10⁵ n/s at h = 1/0.5/0.25 mm); '
        "floor converged to ~5%. Dose is neutron-only — no X-rays, no capture gammas, no room return.</text>"
    )
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1100 490" font-family="Inter, Helvetica, Arial, sans-serif">'
        f'<rect width="1100" height="490" fill="{SURF}"/>{left}{right}{foot}</svg>\n'
    )
    OUT.write_text(svg)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
