# Timeline editing — plan

Status: design (no implementation). Mockup: https://claude.ai/artifact/AxJj23u4pbAtBaRcpGKtHq
(source: [`timeline-editing-mockup.html`](timeline-editing-mockup.html)).

## Decision: the minimal version first

The feature tree *is* the history. Click a feature, edit it in the inspector, and everything
after it rebuilds. Undo/redo stays as it is today, one named step per edit. A feature that
loses its reference gets a red badge and a fix (re-pick, delete, undo). Move Up/Down stays,
but refuses moves that would put a feature before its input.

Undo alone isn't enough: changing an early decision (plate thickness) with undo means
throwing away everything built after it. History editing keeps that work. What makes it
safe is persistent naming, so that is where the effort goes.

Deferred until users ask to insert features mid-history: the timeline strip, rollback
marker, drag reorder, live ghost previews, thumbnails, and real suppress. Until then,
rename today's "Suppress" to **Hide**, since that is what it does. The full design is kept
in §2–§9 below as the roadmap for that later phase.

### Minimal milestones (~15 days)

| # | Milestone | Scope | Days |
|---|---|---|---|
| A | Honest tree | Rename Suppress → Hide; `reorder_feature` checks dependencies and returns `OrderViolation`; structured `FeatureDiag` replaces warning strings; red badge + error card in the tree/inspector | 2 |
| B | Naming for sketch solids | §4.1 + feature-scoped prefixes + `resolve_face` (was M3) | 6 |
| C | Naming through blends/shell/pattern/mirror | §4.2–4.3 + torture corpus v1 (was M4) | 5 |
| D | Per-edge fillet/chamfer + sketch-on-face | §3.5, with Re-pick using existing viewport picking (was M5, minus candidate ranking) | 2–3 |

---

## Full design (later phase)

Goal: Fusion-class parametric history in `apple/VcadApp`. You can roll back, edit a sketch or
feature mid-history, reorder, and suppress. Downstream features re-resolve on stable references,
and when one breaks you get a clear error with a fix. Then go past Fusion: a live downstream
preview while you edit, reorder that only allows legal drops, and broken-reference fixes with
ranked candidates.

## 1. Audit: what exists today

| Capability | Where | State |
|---|---|---|
| History timeline UI | `TimelineEditor.swift` | **Missing.** This file is the *animation keyframe* editor (joint lanes, playhead, video export). It has nothing to do with feature history. |
| Feature tree | `Shell.swift` `FeatureTreeView`, `Editor.featureNodes` | Exists. Lists parts and features with rename, isolate, hide and delete. |
| Rollback marker | — | **Missing.** Nothing evaluates a prefix of the feature list. |
| Reorder | `Editor.reorderPart` → `DocCommand::Reorder` → `DocumentApi::reorder_feature` (`crates/vcad-app/src/document_api.rs:418`) | Exists as Move Up/Down on **parts** only. It assigns a new `FractionalIndex` with **no dependency check**. |
| Suppress | `Editor.toggleSuppressed` → `SetVisible` | **Is actually hide.** It writes `visible=false` on the feature; the materializer still builds it and only sets `SceneEntry.visible` (`materializer.rs:246`). It only works on scene roots, so a feature consumed by a later one (a boolean tool, say) can't be suppressed. |
| Feature edit | `FeatureTreeParameters.swift`, `irEdit` | Parameter edits rebuild. There is no "edit this feature in its historical context" mode: no rollback, and no sketch re-entry at a point in history. |
| Persistent naming | `crates/vcad-kernel-naming` | Good foundation, but coverage is narrow (see below). |
| Fast re-eval | `vcad-eval` `IncrementalCache` (subtree fingerprints, `incremental.rs`) + `RootMeshCache` (content-addressed, `cache.rs`) | Good foundation. An edit re-evaluates only the edited node and what consumes it. There are no prefix snapshots for scrubbing. |

### Persistent naming coverage

`vcad-kernel-naming` names faces by generating op (`cube:top`, `cylinder:side`), propagates them
through booleans with split ordinals (`n1:top.0`, `.1`), names edges by their two faces
(`EdgeRef`), and resolves **fail-closed** (`Resolved` / `Ambiguous` / `Lost`, with an
`EdgeHint` geometric fallback). That is the right design.

Coverage gaps:

- Names are seeded only for the 7 primitives (`vcad-kernel/src/lib.rs:502–578`) and propagated
  only through booleans (`:847`). **24** other `Solid` construction sites set `names: None`:
  `from_brep`, `from_mesh`, the blend paths, and the rest. Extrude, revolve, sweep, loft,
  fillet, chamfer, shell, pattern, mirror and STEP import all produce **unnamed** topology.
  A sketch-based part, the normal Fusion workflow, has no names at all.
- Feature-level `Fillet`/`Chamfer` (`crates/vcad-app/src/feature.rs:227`) take only
  `input` + size and blend *every* edge. Per-edge selection exists only on the IR's `EdgeBlend`
  (`EdgeQuery::Named`), which the app's feature schema doesn't expose.
- Sketches sit on numeric planes, not on face references. Editing the plate thickness
  doesn't carry a sketch drawn on its top face along with it.
- Shell's `FaceRef` already has the right shape: name first, then point/normal fallback,
  fail-closed. Reuse it for sketch-on-face.

### Where it breaks (code-path repros, not executed; heavy builds go to `mew`)

1. **Reorder drops a feature silently.** Move `Fillet` (input = `Boolean1`) above `Boolean1`.
   `reorder_feature` accepts it. On the next materialize, `feature_roots` doesn't hold
   `Boolean1` yet when `Fillet` is visited, so `resolve_input` pushes the warning string
   *"Fillet feature … references input "…" which no longer resolves — skipped"*
   (`materializer.rs:118`) and the fillet vanishes. The only trace is that warning string.
2. **Suppress doesn't suppress.** Suppress a boolean's tool body. It is consumed, so it never
   gets a `SceneEntry`, and the `visible` flag does nothing. The cut stays.
3. **References can't survive a sketch edit.** On an extrude, every face has `names: None`, so
   any `EdgeQuery::Named` against it is `Lost` on first evaluation. The geometric queries
   (`Near`, `Direction`) re-bind silently to whatever edge is nearest after the edit.

## 2. UX (see the mockup)

- **History strip** under the viewport: one chip per feature, showing its icon, a state dot
  (built / re-resolved-review / broken / skipped / past marker), eval time, and a ⚡ when it
  comes from cache. Selecting a chip draws dependency arcs: solid lines to its inputs, dashed
  lines to the features that consume it.
- **Rollback marker**: drag it, or use ←/→ with keyboard focus. The marker also shows in the
  navigator. Scrubbing is a cache lookup, not a rebuild (§4).
- **Ghosts past the marker**: features after the marker draw as dashed wireframes, so you see
  what's coming while you edit earlier history.
- **Edit in context**: double-click a feature to roll to just after it and open its editor
  (sketch mode for sketches). **Downstream features keep re-resolving live in the background**
  and their dots update as you drag a dimension. Finish shows a rebuild summary and lists
  anything that changed state.
- **Reorder**: drag a chip. Gaps turn green where the feature can legally go and red where it
  can't, and the tooltip names the dependency ("Fillet1 needs Extrude2").
- **Suppress**: a real suppress, with the feature not built. Its dependents show as *skipped*
  (grey) with the reason, never as errors.
- **Broken reference card**: it names the lost reference, explains why in geometry terms
  ("the boss no longer touches the plate top"), and offers **Re-pick** (with candidates ranked
  by `EdgeHint` similarity, highlighted in the viewport), **Suppress**, and **Undo the edit
  that caused it**. A split edge resolved by lineage is amber, with **Accept** / **Fillet all
  fragments** / **Re-pick**.

## 3. Data model and IR changes

All of this goes in the feature layer (`vcad-crdt` + `vcad-app`). The IR stays a pure DAG that
the materializer emits.

1. **`suppressed: bool` on `FeatureState`**, separate from `visible`. The materializer skips
   suppressed features entirely. A dependent of a skipped feature is itself skipped with a
   structured reason. Migration: none. `visible` keeps meaning hidden.
2. **`rollback: Option<FeatureId>` on the document**, persisted like Fusion's marker. The
   materializer stops after it. Features added while rolled back are inserted at the marker.
3. **Structured materializer diagnostics** replace `warnings: Vec<String>`:
   `FeatureDiag { feature, kind: MissingInput | OrderViolation | RefLost{ref, hint} |
   RefAmbiguous{candidates} | RefSplit{fragments} | InputSkipped{by}, message }`. Keep the strings
   as `message` for MCP and logs.
4. **Dependency graph API**: `DocumentApi::feature_deps() -> {id: {inputs, refs}}`, built from
   each `FeatureInput`'s input fields plus its face and edge refs. `reorder_feature` validates
   against it and returns `CommandError::OrderViolation { needs }` instead of accepting the move.
5. **Reference-bearing features**:
   - `Fillet`/`Chamfer` gain `edges: Option<Vec<EdgeRefInput>>`; `None` keeps blend-all. This
     materializes to `EdgeBlend` with `EdgeQuery::Named { face_a, face_b, hint }`.
   - `Sketch` gains `on_face: Option<FaceRef>`. The plane is re-derived from the resolved face
     on every build, and the stored numeric plane becomes the fallback and the hint.
   - Face names get **feature-scoped** prefixes (`Extrude1:top`, not `n17:top`). Node ids
     shift; feature stable ids don't. `NameMap::rescope` already exists for this.
6. Regenerate TS (`npm run ir:gen`) only if the IR `EdgeQuery`/`FaceRef` shapes change. The
   plan above reuses them as they are.

## 4. Kernel and naming work

The critical path: naming has to cover sketch-based solids before references mean anything.

1. **Name extrude/revolve/sweep/loft topology from the sketch.** Each sketch segment gets a
   stable id (constraint-solver entity ids already exist). An extruded side face is
   `Extrude1:side[<segment-id>]`, and caps are `:start`/`:end`. Revolve uses
   `:rev[<seg>]`; sweep and loft follow the same pattern. This is what keeps Fusion stable
   through sketch edits: names follow sketch entities, not indices.
2. **Propagate through blends**: fillet/chamfer faces are named `Fillet1:blend[<edge-ref>]`,
   and trimmed neighbours keep their names (the same "lies on one input surface" argument as
   booleans).
3. **Propagate through shell** (`:inner[<face>]`), **patterns** (`<name>#k` per instance),
   **mirror** (`<name>~m`) and **transforms** (identity).
4. **Face resolution parity**: add `resolve_face(&BRepSolid, &NameMap, &FaceRef) -> FaceResolution`
   beside `resolve_edge`, with the same fail-closed variants.
5. **Candidate ranking**: `rank_edge_candidates(brep, names, hint, k)` returns scored edges
   for the Re-pick card. It reuses `EdgeHint` (direction / midpoint / length).
6. STEP-imported solids stay unnamed, with geometric names only (`step:face#hash` from the
   surface plus a quantized centroid). Document this as a known limitation.

## 5. Eval and caching for fast rollback

- `IncrementalCache` already keys solids by subtree fingerprint, so a rolled-back prefix *is*
  a cache hit as long as the materializer emits identical node ids for that prefix. Per-feature
  id blocks already guarantee that.
- **Prefix snapshots**: after a full build, keep one `(feature index → per-root mesh set)`
  entry for each feature boundary. LRU-bound it at ~256 MB, and cap each entry by triangle
  count. Scrubbing swaps meshes in RealityKit with no kernel call. For a feature not in the LRU,
  evaluate the prefix; incremental reuse makes that cost about one feature.
- **Live downstream preview** while editing: a background evaluation task on a cancellable
  generation counter. Each slider tick bumps the generation, and stale results are dropped.
  Only the dependents of the edited feature re-run (fingerprints already give exactly that
  set). Results stream into the strip's dots. Ghost meshes render from the last completed
  generation.
- Budget: scrub under 16 ms (cache), live preview under 150 ms for the typical part, and a cold
  rebuild reported per feature in the strip (`timings.rs` already measures this).

## 6. UI work (Swift)

- `HistoryStrip.swift` (new): chips, marker, dependency arcs, drag reorder with legal-drop
  validation calling `feature_deps` over FFI. Keep `TimelineEditor` for animation and rename
  its menu item to "Animation" to avoid confusion.
- Navigator: a marker row, a state dot per feature, and a real Suppress (new `DocCommand`).
- Inspector: a diagnostics card driven by `FeatureDiag`, with Re-pick mode (viewport edge
  picking already exists in `Picking.swift`) and candidate highlight.
- Edit-in-context: roll, open the editor, live preview, Finish/Cancel as one named undo step.
- FFI: `vcad-ffi` exports `feature_deps`, `set_rollback`, `set_suppressed`, and diagnostics as
  JSON.

## 7. Tests

- **Naming torture**: a corpus of parts (plate+boss, sketch-on-face chains, patterns, shells).
  For each, apply a scripted list of edits (dimension changes, adding or removing sketch
  segments, reorder) and assert every reference resolves to the *geometrically same* edge or
  face (hint match), or fails closed. A silent rebind fails the test. Extend `vcad-torture`.
- Materializer: suppress/skip propagation, rollback prefix equivalence (a prefix build equals
  a cold build of the truncated document), and reorder validation (proptest over random orders
  of a dependency DAG: an accepted order never produces `MissingInput`).
- Eval: scrub determinism (a cache result equals a cold result, using the existing
  `tests/incremental.rs` pattern) and preview cancellation.
- Swift: snapshot tests for strip states (`VCAD_CNC_SNAPSHOTS=1`-style) and a UI test for drag
  reorder rejection.

## 8. Milestones

| # | Milestone | Scope | Days |
|---|---|---|---|
| M0 | Honest suppress + reorder guard | `suppressed` flag, skip propagation, `feature_deps`, `OrderViolation`, structured `FeatureDiag` | 3 |
| M1 | Rollback marker | persisted marker, prefix materialize, navigator marker row, strip v1 (chips, marker, states) | 4 |
| M2 | Prefix cache + scrub | snapshot LRU, mesh swap on scrub, per-feature timings in strip | 3 |
| M3 | Naming for sketch solids | extrude/revolve/sweep/loft naming from sketch entity ids, feature-scoped prefixes, `resolve_face` | 6 |
| M4 | Naming through blends/shell/pattern/mirror | propagation + torture corpus v1 | 5 |
| M5 | Reference-bearing features | per-edge fillet/chamfer picks, sketch-on-face, migration of blend-all docs | 4 |
| M6 | Edit in context + live preview | roll-and-edit flow, background cancellable preview, ghosts | 5 |
| M7 | Broken-reference UX | diagnostics card, candidate ranking, Re-pick mode, split accept/all | 4 |
| M8 | Drag reorder + polish | legal-drop strip DnD, arcs, a11y (keyboard marker, VoiceOver), docs + changelog | 3 |

About 37 engineering days. M0–M2 ship value on their own (honest suppress, rollback, fast
scrub) without the naming work. M3 is the critical path for "references survive edits". It
can start in parallel with M1/M2 because it is kernel-only.

## 9. Risks

- **Sketch entity id stability.** If the sketch solver re-creates entities on edit, names
  churn. Mitigation: make entity ids a persisted part of the sketch document (audit
  `vcad-kernel-sketch` first, which is a ½-day spike).
- **Topology changes that are legitimately ambiguous** (a face split in three, a segment
  deleted). Policy: never guess. Amber when lineage plus hint picks exactly one, red otherwise.
  The torture corpus enforces "no silent rebind".
- **Boolean robustness**: re-resolution exposes kernel booleans on many more parameter values.
  Expect torture-track regressions to surface as `Lost`. That is the correct failure, but it
  will look like a naming bug.
- **CRDT merge**: a concurrent reorder and edit can produce an order violation after merge.
  The materializer must diagnose it (`OrderViolation`), not crash; the UI offers "move after
  X".
- **Memory**: prefix snapshots on large assemblies. Bound by bytes and fall back to incremental
  re-eval.
- **Two timelines**: the animation `TimelineEditor` and the history strip both read as
  "timeline". Name them distinctly (History / Animation).
