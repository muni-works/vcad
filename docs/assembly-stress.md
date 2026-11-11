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
5. **(follow-up) Voron buildScene: about 30+ min tessellating 1560 B-reps on one thread.** The profile is dominated by HashMap hashing inside `tessellate_brep_with`. The fix is parallel per-root tessellation plus a cheaper hasher or a preview LOD for imports.
6. **(follow-up, step-interop) STEP import flattens assemblies.** It produces one root per solid named `Voron_2.4r2_Assembly #N`: no product names, no part defs/instances (fasteners are duplicated, not instanced), and no way to tell whether NAUO placements are applied. `vcad_kernel_step::read_step_assembly` already reads the hierarchy, so the importer should emit part defs and instances from it.
7. **(follow-up) Clearance on a mesh-only assembly reports "0 pairs".** K1's links are STL, so every part is skipped. The panel lists the skipped parts, but the headline still reads like "no interference". It should say "can't check N mesh-only parts", or fall back to mesh-vs-mesh clearance.
8. **(follow-up) Every joint commit re-evaluates the whole assembly** (K1: 1.5 s). The rebuild runs off the main thread, but the edit lags. Only the poses changed, so a commit should re-pose the instances, not re-evaluate.
9. **(follow-up) K1 STEP export is impossible.** The links are mesh-only (STL). The refusal is correct; offering GLB or STL in the same dialog would help.

## Not covered here

The UI walkthrough (screenshots of each step in vcad.app) was not done in this pass. The timings come from the headless `EditorModel` path that the panels drive.
