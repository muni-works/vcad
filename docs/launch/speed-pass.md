# vcad 1.0 speed pass (native Mac app)

Measured 2026-11-07 on the dev Mac (Apple Silicon, ProMotion display, macOS 27),
release build of `apple/VcadApp` at main `6e7f0d3` + this branch. Every number
is a fresh process launched by `apple/VcadApp/scripts/launch-perf.sh`, with marks
from `LaunchPerf.swift` (`VCAD_PERF=1`), timed from **process exec** (dyld and
static init included — what a user waits through after clicking the icon).

"Cold" = empty kernel root cache (`VCAD_CACHE_DIR` pointed at a fresh temp dir):
what a first-time user sees. Numbers are from a machine also running other
agents, so cold solves vary run to run (Stirling: 17–38 s); treat them as
ranges.

## Launch

| Mark | Time since exec |
|---|---|
| Main window up (`shell appeared`) | **0.63–0.79 s** (first run after a rebuild: 1.1–1.5 s, dyld cache) |
| Document handed to the kernel (`open`) | same frame as the shell |

## Open → first full model on screen

| Sample | Parts / tris | Cold (empty cache) | With baked bundle / warm |
|---|---|---|---|
| Sensor mast | 7 inst / 4.2k | 0.05 s | 0.03 s |
| Snap-fit cap | 2 / 5.9k | — | 0.05 s |
| Pulley | 1 / 5.8k | **2.6 s** | **0.07 s** (bundle) |
| PCB axial-flux motor | 11 / 22k | **10.6–18.0 s** | **0.06–0.11 s** (bundle) |
| Stirling engine | 10 inst / 19k | **16.7–37.8 s** | 0.07 s warm — *not bakeable yet, see below* |
| K1 humanoid | 24 inst / 237k | 0.18–0.42 s | 0.23 s |
| FPGA dev board (`ice-viz`, 244 KB) | 1 / 127k | 0.92 s | — |
| PCB stator (564 KB doc) | 1 / 504k | 0.76 s solve, 1.0 s to screen | — |

## Viewport

Orbit benchmark: `VCAD_PERF_ORBIT=<s>` spins the camera one turn per 4 s once
the scene is up; frame intervals come from RealityKit's own `SceneEvents.Update`.

| Sample | Mean | p5 | Worst frame |
|---|---|---|---|
| K1 humanoid (237k tris) | **120 fps** | 107 fps | 9.7 ms |
| PCB stator (504k tris) | **120 fps** | 114 fps | 11.1 ms |
| Stirling engine | 120 fps | 106–113 fps | 11.7 ms |
| PCB motor | 120 fps | 102–108 fps | 10.3 ms |

Every document tested is pinned at the 120 Hz display cap. The viewport is not
the bottleneck; cold kernel solves are.

## What changed

1. **Worst offender: first open of a heavy sample was a cold kernel solve.** A
   first-time user clicking "PCB axial-flux motor" in the welcome window waited
   10–18 s; Pulley 2.6 s. The app already supported `.vcadmesh` bundles
   ("instant open when the file shipped with its meshes") but no sample shipped
   one. Now:
   - `VCAD_BAKE_MESH_BUNDLE=1` (ReleasedDesktop.swift) solves a document,
     writes `<doc>.vcadmesh` and quits.
   - `scripts/bake-sample-meshes.sh` bakes every bundled sample into the built
     app's `Resources/samples`, dropping bundles over 4 MB (boards: 10–42 MB of
     mesh for a 1 s solve isn't worth the download). `bundle.sh` runs it for
     every release build (`BAKE_SAMPLES=1` for dev builds). Bundles are keyed by
     kernel build, so they're baked rather than committed — a stale bundle is a
     cache miss, never wrong geometry.
   - `SampleLibrary.materialize` copies the bundle beside the user's editable
     copy, so `openDocument` → `importMeshBundle` hits. Measured: PCB motor
     10.6 s → 0.06 s, Pulley 2.6 s → 0.07 s. Total bundle weight: ~4 MB.
2. **Instrumentation** (`LaunchPerf.swift`, `scripts/launch-perf.sh`) so this is
   re-measurable before every release instead of a one-off.

## Still slow (follow-ups)

- **Stirling engine cold solve, 17–38 s.** It's an assembly (`partDefs` +
  `instances`); `vcad-eval` caches part-def meshes in the root cache but never
  records their keys in `EvaluatedScene::root_keys`, so `writeMeshBundle` has
  nothing to write (baked 0 roots). Fix: record part-def cache keys (new
  `part_def_keys` field + FFI accessor) and include them in the bundle. Needs a
  `vcad-ffi` rebuild; not done here. Same applies to every assembly sample
  (robot arm, K1 — those are cheap already).
- The time is in one or two booleans (first part lands at ~5 s, the rest
  trickles in). Worth a kernel profile: Stirling is the flagship sample.
- Launch at ~0.65 s is fine; nothing obvious left above the SwiftUI shell.

## Reproduce

```bash
apple/VcadApp/build-ffi.sh && CONFIG=release apple/VcadApp/bundle.sh
BAKE_SAMPLES=1 CONFIG=release apple/VcadApp/bundle.sh   # with baked bundles
RUNS=2 ORBIT=6 apple/VcadApp/scripts/launch-perf.sh examples/k1-floating.vcad examples/stirling-engine/stirling-engine.vcad
```
