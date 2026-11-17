# open-fusor: vcad.app screens

The Analysis → Fields screens the open-fusor video films (blocker #3c in
`docs/videos/open-fusor.md`). Each `N-name.png` is a 1920×1080 frame
**composited** by the snapshot runner: the live viewport's own render (the
section drawn on the device's y = 0 plane) with the Analysis panel, rendered
by AppKit offscreen, placed on the right rail. `7-live-agent-run-window.jpg` is
a real capture of the running app's window. An agent called
`simulate_charged_particles` over the in-app MCP endpoint with an asymmetric
device (one ring at z = +25 mm), the panel switched to Ions ("Run by …"), and
the ring lands on the stalk side, which confirms the section's orientation. `-panel.png` is the panel at native (2×)
resolution; `VCAD_FIELDS_SNAPSHOT_HALVES=1` also writes the viewport alone.

| frame | screen |
|---|---|
| 1-ions | equipotentials + deuteron tracks over the cut-away device; predicted yield bracket, interception, passes, transparency |
| 2-sweep | the same run with the voltage sweep (floor…ceiling band, −20…−40 kV) |
| 3-optimize | `optimize_electrodes` over the grid radius: every evaluation, best-so-far, the winner |
| 4-dose | `simulate_neutron_shield`: dose vs distance, 15 cm HDPE vs no shield, against a 1 µSv/h budget |
| 5-bench-holds | `compare_measurements`: readings bound to the claims, verdict Holds, measured point inside the acceptance window |
| 6-bench-violated | the same with an under-performing reading: Violated |
| 7-live-agent-run-window | real window capture of an agent's tool call landing in the panel |

**The bench readings are EXAMPLE values** (labeled so on screen as the
instrument): nothing has been built or measured. Every predicted number comes
from `vcad-solver-tools` (the code the agent's MCP tools run) at the hobby
fusor of the plan: 76 mm chamber, 19 mm 4-ring grid of 1 mm wire radius,
−30 kV, 10 mA, 5 mTorr D₂, σ_cx = 1e-19 m², 0.25 mm grid (the panel default),
96 deuterons, 4.6 s. It reproduces the plan's finest row: floor 2.46×10⁴,
ceiling 4.36×10⁵ n/s, interception 0.78. Coarser grids fatten the wire mask
and read interception ≈ 1.00. The sweep runs at 0.5 mm.

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
