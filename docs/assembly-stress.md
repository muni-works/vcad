# Assembly stress test: K1 + Voron 2.4

Walking the Fusion-user path headlessly (`AssemblyStressTests`) on two assemblies:

- **Booster K1**: URDF in `third_party/booster-k1`. 24 links, 23 joints with limits, STL meshes, real scale.
- **Voron 2.4r2**: STEP from [VoronDesign/Voron-2](https://github.com/VoronDesign/Voron-2) (`CAD/Voron_2.4r2_Assembly_STEP.zip`, **GPL-3.0**). 241 MB, 1560 solids, 1551 NAUOs. Not in git: `scripts/fetch-voron-step.sh` puts it in `target/stress/voron/`.

Run it with `cd apple/VcadApp && nice swift test --filter AssemblyStressTests`. The Voron case is skipped until the file has been fetched.

## Timings (M-series Mac under load ~8–20, release FFI)

| Step | K1 before | K1 after | Voron before | Voron after |
|---|---|---|---|---|
| import | 0.002 s | 0.001 s | — | parsed once |
| open + first scene | 1.65 s | 1.54 s | **> 25 min, killed during import/open** | import/open OK; buildScene still > 30 min (tessellation) |
| drag joint ×30 | 0.002 s | 0.002 s | — | — |
| commit joint → rebuild | 1.5 s | 1.5 s | — | — |
| interference | 0.105 s | 0.106 s | O(n²) tessellations, unbounded | 1 tessellation per part + AABB broadphase |
| explode 0→1 (11 steps) | 0.001 s | 0.001 s | — | — |
| save / reopen | 0.04 / 0.03 s | 0.03 / 0.03 s | — | — |
| export GLB | 48 meshes, 11.4 MB | **24 meshes, 5.7 MB** | — | — |
| export STEP | refused, listed raw node ids | refused, **names the links** | — | — |

## Rough edges, ranked

1. **(fixed) Opening an imported STEP assembly re-parsed the whole file once per solid.** Each `StepImport` node fell back to `Solid::from_step_all(path)` natively. Voron: 1560 × a 241 MB parse, so it never opened. Fix: a native per-file parse cache in `vcad_eval::step_sources::solids_from_file`, keyed by path, size and mtime. The importer seeds it with the parse it already did (`prime_file`).
2. **(fixed) GLB/STL export of an assembly emitted every part definition a second time, unposed and at the origin.** A URDF import also lists each link's def root as a scene root. The viewport and clearance already hid those roots; export didn't. Exports now follow the same rule: instances replace their def roots.
3. **(fixed) STEP export of an assembly exported the unplaced part definitions.** It ignored instances and joint poses. It now exports the instances posed by FK, named by instance. Refusals now name parts instead of `node 110166022`.
4. **(fixed) Interference re-tessellated both parts for every pair,** which is 2·(n−1) tessellations per part, and it had no broadphase. Each part is now meshed once. Above 64 parts, only pairs whose AABBs come within `near_mm` (default 5 mm) get the exact check. Explicit `pairs` bypass the broadphase.
5. **(fixed) Voron took 30+ min to open.** Profiling on mew showed ~80% of tessellation time in the export-grade watertightness repair (`strip_membranes_once` ray casts), a few huge parts pinning one core each for minutes, and nothing drawn until the last part was done. Fixes, in order of effect:
   - **Instancing by content.** The file has 1560 separate `MANIFOLD_SOLID_BREP`s (every screw is its own copy), so a STEP solid is now keyed by a SHA-256 of its B-rep (`step_sources::solid_key`). The root cache peels off the placement, and identical parts tessellate once: 670 distinct shapes out of 1563.
   - **Preview LOD, then refine.** Documents with 64 or more visible roots open with a preview pass (no export repair, no chord tolerance), and a full-quality pass swaps in afterwards. Preview meshes are cached under their own keys.
   - **Streaming.** The parallel pre-pass hands results to the in-order walk as they land, so `on_root` fires from the first part, not the last.
   - **Faster membrane probes.** The ray index puts big triangles into coarser grid levels (it used to test them on every probe), and the per-triangle probes run in parallel above 4096 triangles.
   - **No clash pass on open.** Nothing reads the open scene's clash meshes.
   - **App main thread.** Mesh copies and feature edges are gathered concurrently. There is one edge-line material per rebuild, not one per part, which had been minutes of shader setup. Edge-line meshes are cached across rebuilds, the contact shadow samples at most about 300k triangles, and streamed parts are landed under a time budget and redrawn at most once a second.

   | Voron 2.4 | before (HEAD bf989d9) | after |
   |---|---|---|
   | mew, evaluate (bench) | 425 s, first part at 425 s | **preview 4.4 s, first part 0.18 s**; full refine 268 s in the background |
   | Mac app, cold cache (launch → …) | 30+ min, never drawn | first part 12 s, preview solved 16 s, **whole assembly on screen 22 s**, main thread free by ~31 s; refine lands in 2.5–13 min (shared machine) |

   Bench: `cargo run --release -p vcad-eval --example assembly_open_bench -- target/stress/voron/Voron_2.4r2_Assembly.step` (`PREVIEW=1 CHORD=0` gives the preview LOD). App screenshots: `VCAD_ASSEMBLY_SNAPSHOT_DIR=… VCAD_OPEN=….step vcad`.
6. **(follow-up, step-interop) STEP import flattens assemblies.** It produces one root per solid named `Voron_2.4r2_Assembly #N`: no product names, no part defs/instances (fasteners are duplicated, not instanced), and no way to tell whether NAUO placements are applied. `vcad_kernel_step::read_step_assembly` already reads the hierarchy, so the importer should emit part defs and instances from it.
7. **(follow-up) Clearance on a mesh-only assembly reports "0 pairs".** K1's links are STL, so every part is skipped. The panel lists the skipped parts, but the headline still reads like "no interference". It should say "can't check N mesh-only parts", or fall back to mesh-vs-mesh clearance.
8. **(follow-up) Every joint commit re-evaluates the whole assembly** (K1: 1.5 s). The rebuild runs off the main thread, but the edit lags. Only the poses changed, so a commit should re-pose the instances, not re-evaluate.
9. **(follow-up) K1 STEP export is impossible.** The links are mesh-only (STL). The refusal is correct; offering GLB or STL in the same dialog would help.

## Not covered here

The UI walkthrough (screenshots of each step in vcad.app) was not done in this pass. The timings come from the headless `EditorModel` path that the panels drive.
