"""Plot the PoC CSVs as plain SVG (stdlib only).

usage: python3 plot.py out/
writes out/port_velocity.svg and out/antenna_s11.svg
"""
import csv
import json
import sys
from pathlib import Path

W, H, PAD = 640, 360, 56
INK, GRID, ACCENT, MARK = "#1d3557", "#d9dee5", "#2a9d8f", "#e76f51"


def svg_line_plot(xs, ys, title, xlabel, ylabel, marks=(), logy=False):
    import math

    if logy:
        ys = [math.log10(max(y, 1e-30)) for y in ys]
    x0, x1 = min(xs), max(xs)
    y0, y1 = min(ys), max(ys)
    if y1 == y0:
        y1 = y0 + 1
    sx = lambda x: PAD + (x - x0) / (x1 - x0) * (W - 2 * PAD)
    sy = lambda y: H - PAD - (y - y0) / (y1 - y0) * (H - 2 * PAD)
    pts = " ".join(f"{sx(x):.1f},{sy(y):.1f}" for x, y in zip(xs, ys))
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" font-family="Helvetica, Arial, sans-serif" font-size="12">',
        f'<rect width="{W}" height="{H}" fill="#f6f5f2"/>',
        f'<text x="{PAD}" y="28" font-size="15" fill="{INK}" font-weight="600">{title}</text>',
    ]
    for i in range(6):
        gx = x0 + (x1 - x0) * i / 5
        gy = y0 + (y1 - y0) * i / 5
        out.append(f'<line x1="{sx(gx):.1f}" y1="{PAD}" x2="{sx(gx):.1f}" y2="{H-PAD}" stroke="{GRID}"/>')
        out.append(f'<line x1="{PAD}" y1="{sy(gy):.1f}" x2="{W-PAD}" y2="{sy(gy):.1f}" stroke="{GRID}"/>')
        out.append(f'<text x="{sx(gx):.1f}" y="{H-PAD+16}" text-anchor="middle" fill="{INK}">{gx:.0f}</text>')
        lab = f"1e{gy:.1f}" if logy else f"{gy:.1f}"
        out.append(f'<text x="{PAD-6}" y="{sy(gy)+4:.1f}" text-anchor="end" fill="{INK}">{lab}</text>')
    out.append(f'<polyline fill="none" stroke="{ACCENT}" stroke-width="2.2" points="{pts}"/>')
    for k, (mx, mlabel) in enumerate(marks):
        if x0 <= mx <= x1:
            out.append(f'<line x1="{sx(mx):.1f}" y1="{PAD}" x2="{sx(mx):.1f}" y2="{H-PAD}" stroke="{MARK}" stroke-dasharray="4 3"/>')
            anchor = "end" if k % 2 == 0 else "start"
            dx = -4 if k % 2 == 0 else 4
            out.append(f'<text x="{sx(mx)+dx:.1f}" y="{PAD+14+16*k}" text-anchor="{anchor}" fill="{MARK}">{mlabel}</text>')
    out.append(f'<text x="{W/2}" y="{H-12}" text-anchor="middle" fill="{INK}">{xlabel}</text>')
    out.append(f'<text x="16" y="{H/2}" text-anchor="middle" fill="{INK}" transform="rotate(-90 16 {H/2})">{ylabel}</text>')
    out.append("</svg>")
    return "\n".join(out)


def read(path):
    with open(path) as f:
        rows = list(csv.DictReader(f))
    return rows


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "out")
    summary = json.loads((out / "summary.json").read_text())
    rows = read(out / "port_velocity.csv")
    xs = [float(r["freq_hz"]) for r in rows]
    ys = [float(r["port_volume_velocity_m3s"]) for r in rows]
    lo, nom, hi = summary["fb_lumped_hz"]
    marks = [(nom, f"lumped f_b {nom:.0f} Hz")]
    if summary.get("fb_field_hz"):
        marks.append((summary["fb_field_hz"], f"field {summary['fb_field_hz']:.0f} Hz"))
    (out / "port_velocity.svg").write_text(
        svg_line_plot(xs, ys, "puck: port volume velocity per unit piston velocity (axisym field solve)",
                      "frequency (Hz)", "|U_port| (m³/s, log)", marks, logy=True))

    rows = read(out / "antenna_s11.csv")
    xs = [float(r["freq_mhz"]) for r in rows]
    ys = [float(r["s11_db"]) for r in rows]
    (out / "antenna_s11.svg").write_text(
        svg_line_plot(xs, ys, "puck: 2.4 GHz monopole S11 vs 50 Ω (thin-wire MoM, infinite ground)",
                      "frequency (MHz)", "S11 (dB)", [(2440, "2.44 GHz")]))


if __name__ == "__main__":
    main()
