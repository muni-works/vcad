# vcad 2 — wave 2 session briefs

Start these when `ffi-groundwork` (vcad-ffi: sketch constraints, topology picking,
feature builders, STEP import) has merged. Plan of record:
`docs/launch/vcad2-parity.html` (parity matrix + milestones) and
`docs/launch/full-tang.html` (tang ladder).

Shared rules for every brief:
- Cargo builds/tests/clippy run on `ssh mew`, never on the Mac. Swift builds run
  on the Mac — one lane at a time, light.
- One branch + PR per lane. Don't commit `packages/kernel-wasm/vcad_kernel_wasm*`.
- File ownership below is binding; touch another lane's files only through a
  message to that lane.
- Each lane ends with a gate demonstrated in the PR (screenshots or test output).

## M1 — native modeling UI (critical path, sole writer of `Editor.swift`)

Build on the new vcad-ffi entry points:
- Sketch mode: arcs, splines, offset, sketch-on-face; constraint glyphs and
  driving dimensions; DOF status; solve on edit.
- Edge/face selection with highlight; fillet/chamfer on chosen edges; shell
  with chosen faces removed; draft.
- Feature dialogs: revolve, sweep, loft, cut-extrude/to-face, linear/circular
  pattern, mirror, scale, text, stitch, cone.
- Feature tree: edit-in-place, reorder, suppress. Parameter table. Command
  palette.
- Viewport tools found missing in M0: measure (point/edge/face), section plane.
- Owns: `Editor.swift`, new `Sketch*.swift`, `FeatureOps*.swift`,
  `FeatureTree*.swift`, `Measure*.swift`, `Section*.swift`, menu entries in
  `Shell.swift` for these.
- Gate: every op in web `packages/app/src/components/dialogs/` is authorable
  natively; a set of web-authored sample `.vcad` files opens, edits, saves, and
  reopens identically on the web.

## M2b — DFM & FEA (crown jewel)

- DFM drawer: run `lib/dfm` rule packs through `vcad-kernel-dfm`, annotate
  faces, apply suggested fixes as undoable edits.
- FEA setup: pick faces for loads/supports (uses M1 picking), assign material,
  mesh, solve in-process; results through existing `FieldOverlay.swift`.
- Clearance/interference panel at web `AnalyzePanel` parity (missing per M0).
- Owns: new `Analysis*.swift`, `Dfm*.swift`, `Fea*.swift`;
  `crates/vcad-ffi/src/dfm.rs`, `fea.rs`, `clearance.rs`. Start with the FFI
  side; add UI once M1's picking API is merged.
- Gate: cantilever bracket loaded/solved in-app, max stress within tolerance of
  the kernel reference; a DFM fix applied clears its violation; two
  interfering parts report the overlap volume.

## M3 — drawings

- Drawing workspace on `vcad-kernel-drafting`: ortho/section/detail views,
  dimensions, GD&T, title block; DXF and PDF export.
- Owns: new `Drawing*.swift`; `crates/vcad-ffi/src/drafting.rs`.
- Gate: web drawing samples render natively; DXF matches the web export.

## tang T2/T3 (after `tang-t0-t1` merges)

- T2: thermal and FEA assembly as tang-compile comprehensions (gather view
  element→node, scatter-add monoid); GPU forward through existing backends.
- T3: phyz `Rollout` and kernel-diff `SeamEval` as operator nodes; replace the
  hand wiring in `vcad-kernel-physics/src/diff.rs:840-900` with a three-node
  graph.
- Owns: tang repo; `vcad-kernel-thermal`, `vcad-kernel-fea`,
  `vcad-kernel-physics::diff`.
- Gates: T2 results match current solvers, GPU forward time published; T3 same
  numbers as the hand-wired path to round-off, hand wiring deleted.

## Wave 3 (after M1)

- M2c electronics co-design: board↔enclosure, new PCB, schematic, layers,
  length tuning, circuit analysis, Gerber with clean-DRC gate.
- M4: sheet metal, 3D print, CAM tool library, URDF import, insert
  instance/add joint, keyframe editing.
- Launch: hero demo video, sample gallery, fresh-Mac speed pass, prerelease
  dry run of `desktop-release.yml` (needs signing secrets — see M6 blockers).
