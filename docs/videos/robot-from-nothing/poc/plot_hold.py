#!/usr/bin/env python3
"""Plot rose-mini hold traces as a two-panel SVG (base height, base tilt vs time).

  python3 plot_hold.py out.svg "label A=traceA.csv" "label B=traceB.csv"

Plain SVG, no dependencies. Colors: categorical slots 1-2 (blue, orange) of the dataviz reference palette,
stepped separately for light and dark; plain class fills so rsvg/GitHub both render.
"""
import csv, sys

out = sys.argv[1]
series = []
for arg in sys.argv[2:]:
    label, path = arg.split('=', 1)
    rows = list(csv.DictReader(open(path)))
    series.append((label, [(float(r['t_s']), float(r['base_z_mm']), float(r['tilt_deg'])) for r in rows]))

W, PH, ML, MR, MT, GAP, MB = 720, 190, 64, 40, 96, 54, 40
H = MT + 2 * PH + GAP + MB
TMAX = 3.0
panels = [('base height (mm)', 1, 0, 600, [0, 200, 400, 600]),
          ('base tilt from upright (°)', 2, 0, 120, [0, 30, 60, 90, 120])]


def x(t):
    return ML + (W - ML - MR) * t / TMAX


svg = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" '
       f'font-family="ui-sans-serif, system-ui, -apple-system, Helvetica, Arial" font-size="12">',
       '<style>'
       '.bg{fill:#fcfcfb} .t{fill:#0b0b0b} .t2{fill:#52514e} .g{stroke:#e6e5e0;stroke-width:1}'
       '.l{fill:none;stroke-width:2;stroke-linejoin:round;stroke-linecap:round} .s1{stroke:#2a78d6} .s2{stroke:#eb6834}'
       '.f1{fill:#2a78d6} .f2{fill:#eb6834}'
       '@media (prefers-color-scheme: dark){.bg{fill:#1a1a19} .t{fill:#f0efec} .t2{fill:#c3c2b7} .g{stroke:#34332f}'
       ' .s1{stroke:#3987e5} .s2{stroke:#d95926} .f1{fill:#3987e5} .f2{fill:#d95926}}'
       '</style>',
       f'<rect class="bg" width="{W}" height="{H}"/>',
       f'<text class="t" x="{ML}" y="22" font-size="15" font-weight="600">rose mini, dropped into vcad\'s gym under a joint PD hold (no policy)</text>',
       f'<text class="t2" x="{ML}" y="40">1 kHz physics, 50 Hz control, ground contact, floating base. ran on mew 2026-11-16.</text>']

for pi, (title, col, lo, hi, ticks) in enumerate(panels):
    top = MT + pi * (PH + GAP)
    def y(v):
        return top + PH - PH * (min(max(v, lo), hi) - lo) / (hi - lo)
    svg.append(f'<text class="t" x="{ML}" y="{top - 8}" font-weight="600">{title}</text>')
    for tk in ticks:
        svg.append(f'<line class="g" x1="{ML}" x2="{W - MR}" y1="{y(tk):.1f}" y2="{y(tk):.1f}"/>')
        svg.append(f'<text class="t2" x="{ML - 8}" y="{y(tk) + 4:.1f}" text-anchor="end">{tk}</text>')
    for t in [0, 0.5, 1, 1.5, 2, 2.5, 3]:
        svg.append(f'<text class="t2" x="{x(t):.1f}" y="{top + PH + 16}" text-anchor="middle">{t:g}</text>')
    for si, (label, pts) in enumerate(series):
        d = 'M' + ' L'.join(f'{x(p[0]):.1f},{y(p[col]):.1f}' for p in [(0.0, pts[0][1], 0.0)] + pts)
        svg.append(f'<path class="l s{si + 1}" d="{d}"/>')
svg.append(f'<text class="t2" x="{(ML + W - MR) / 2:.0f}" y="{H - 10}" text-anchor="middle">time (s)</text>')
# legend (always present for >= 2 series), in the header so it never meets an axis
lg_x, lg_y = ML, 64
for si, (label, _) in enumerate(series):
    svg.append(f'<rect class="f{si + 1}" x="{lg_x}" y="{lg_y - 6}" width="16" height="4" rx="2"/>')
    svg.append(f'<text class="t2" x="{lg_x + 22}" y="{lg_y}">{label}</text>')
    lg_x += 40 + 7 * len(label)
if len(series) > 1:
    svg.append(f'<text class="t2" x="{lg_x}" y="{lg_y}" font-style="italic">(identical: the body falls, not the PD)</text>')
svg.append('</svg>')
open(out, 'w').write('\n'.join(svg))
print('wrote', out)
