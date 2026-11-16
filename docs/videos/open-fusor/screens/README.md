# open-fusor: vcad.app screens

The Analysis → Fields screens the open-fusor video films (blocker #3c in
`docs/videos/open-fusor.md`). Each `N-name.png` is a 1920×1080 frame: the
live viewport (the section drawn on the device's own y = 0 plane) with the
Analysis panel on the right rail. `-panel.png` is the panel at native (2×)
resolution; `VCAD_FIELDS_SNAPSHOT_HALVES=1` also writes the viewport alone.

| frame | screen |
|---|---|
| 1-ions | equipotentials + deuteron tracks over the cut-away device; predicted yield bracket, interception, passes, transparency |
| 2-sweep | the same run with the voltage sweep (floor…ceiling band, −20…−40 kV) |
| 3-optimize | `optimize_electrodes` over the grid radius: every evaluation, best-so-far, the winner |
| 4-dose | `simulate_neutron_shield`: dose vs distance, 15 cm HDPE vs no shield, against a 1 µSv/h budget |
| 5-bench-holds | `compare_measurements`: readings bound to the claims, verdict Holds, measured point inside the acceptance window |
| 6-bench-violated | the same with an under-performing reading: Violated |

**The bench readings are EXAMPLE values** (labeled so on screen as the
instrument): nothing has been built or measured. Every predicted number comes
from `vcad-solver-tools` (the code the agent's MCP tools run) at the hobby
fusor of the plan: 76 mm chamber, 19 mm 4-ring grid of 1 mm wire radius,
−30 kV, 10 mA, 5 mTorr D₂, σ_cx = 1e-19 m², 0.5 mm grid, 96 deuterons. It
reproduces the plan's 0.5 mm convergence row: floor 2.34×10⁴, ceiling
3.25×10⁵ n/s, interception 1.00.

Dose uses the plan's 5×10⁶ n/s upper-bound source and is neutron only (no
x-rays, no capture gammas). The panel says so.

## Regenerate

```bash
CONFIG=release apple/VcadApp/bundle.sh
VCAD_FIELDS_SNAPSHOT_DIR=$PWD/docs/videos/open-fusor/screens VCAD_FIELDS_SNAPSHOT_EXIT=1 \
  apple/VcadApp/dist/vcad.app/Contents/MacOS/vcad -vcad.prefs.presentation YES
```

The runner (`FieldsSnapshots.swift`) loads the cut-away device, drives
`FieldsStore` through every screen, and writes the PNGs. It needs no
screen-recording rights. Don't touch the window while it runs: it shares the
live screen.
