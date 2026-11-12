# vcad document runtime — design

Status: **draft for review** (no code yet). Owner: Mac app perf track.

## Problem

`EditorModel` (`apple/VcadApp/Sources/VcadApp/Editor.swift`, `@MainActor`)
owns the whole document pipeline, and every stage runs on main:

| stage | where today | cost on main |
|---|---|---|
| CRDT apply + materialize | `DocumentEngine.apply/applyTool/loadIR/restore` → `vcad_engine_*` | sync FFI call per edit |
| delta → dictionary | `DocumentDelta.applied(to:)`, `Result.document: [String: Any]` | Foundation churn |
| dictionary → bytes | `Result.documentJSON` (`JSONSerialization`) — re-serialized per consumer | O(doc) per read |
| eval + tessellate | `EvalSession.scene(data:dir:)` | dominant for scrubs |
| mesh upload | `MeshResource` / `LowLevelMesh` build | per changed part |
| picking, analysis, sound, agent fingerprint | `Picking.swift`, `AnalysisOverlay.swift` (`AnalysisMeshCache`), `SoundStore`, `AgentCore` | each memoizes/evaluates on its own key → stale keys, duplicate evals |

PR #1139 added stopgaps (memoized `pickingDocumentData`, a shared off-main
`EvalSession` via `PickingState.evaluate`, LowLevelMesh overlays, deferred
prewarm). They relocate the caches; this design removes them.

## Decision

One Rust-owned **document runtime** per open document: a new crate
`crates/vcad-runtime`, exposed through `vcad-ffi` as `vcad_doc_runtime_*`.
It holds, on its own thread, everything that is a function of the document:

- the CRDT + materializer (`vcad_app::DocumentApi`, what `vcad_engine_*` wraps),
- the incremental eval session (`vcad-eval` incremental cache),
- tessellation (coarse + fine LOD),
- background analyses keyed by revision: picking BVH, DFM, sound modes / pitch
  locks (`vcad-sound`, merged in #1139 — the runtime calls `part_modes_mesh`,
  `strike_gains`, `solve_pitch_lock` with its own evaluated meshes instead of
  `vcad_sound::evaluate` re-evaluating the doc), agent fingerprint/state hash.

Swift sends **intents** (fire-and-forget, serializable). Rust sends back
immutable **frames** via one callback. Swift never holds the document as a
dictionary again; it holds the latest `Frame`.

Same core serves MCP (`vcad-ffi/src/mcp.rs` host hooks, which today call back
into Swift for "engine edits on the window's engine" — they become intents on
the runtime directly), the CLI, and VcadVision.

## Threading

```
 main (Swift)                  runtime thread (Rust)              worker pool (rayon)
 ───────────                   ─────────────────────              ───────────────────
 send(intent) ──► mpsc inbox ──► drain + coalesce
                                 apply to CRDT (rev += 1)
                                 materialize
                                 publish Frame{geometry: pending}──► eval/tessellate roots
 onFrame ◄── callback (any thread) ◄── Frame (coarse) ◄──────────── coarse LOD done
 onFrame ◄──────────────────────────── Frame (fine)   ◄──────────── fine LOD done
 onFrame ◄──────────────────────────── Frame (analysis) ◄────────── DFM / sound / BVH
```

- **One owner thread** per runtime owns `DocumentApi` and the eval session:
  no locks on document state. It is the only writer; ordering is by inbox order.
- Heavy work (eval, tessellate, analyses) goes to a rayon pool, tagged with the
  revision it was started for.
- The callback fires on a runtime thread. Swift's trampoline does only a
  retain + `DispatchQueue.main.async { model.adopt(frame) }`; `adopt` swaps a
  reference and builds `LowLevelMesh` from already-final buffers (memcpy into
  the mesh's vertex buffer — no per-vertex Swift work).
- Main-thread budget per frame: O(changed parts) pointer swaps + buffer copies.
  Nothing O(document).

## Intents

Rust: `enum Intent` (serde, `#[serde(tag = "type")]`, ts-rs/swift-gen like
`DocCommand`). Swift mirror generated into `Generated/EngineTypes.swift`
(`npm run swift:gen`), sent as JSON bytes (or a compact binary later; JSON is
small for intents).

```rust
pub struct Envelope {
    pub seq: u64,              // client-assigned, monotonically increasing
    pub intent: Intent,
}

pub enum Intent {
    // edits — all carry an undo label
    Command { cmd: DocCommand, undo: UndoMode, label: String },
    Tool    { tool: String, args: Value, label: String },   // agent / MCP lowering
    LoadIr  { document: Value, undo: UndoMode, label: String },
    SetParam { name: String, value: f64, gesture: GestureId },  // scrub
    GestureEnd { gesture: GestureId },
    Strike  { part: PartId, point: [f64; 3], gesture: Option<GestureId> }, // Mallet
    Undo, Redo,
    // document lifecycle
    Open { bytes: Vec<u8>, base_dir: PathBuf }, Restore { snapshot: Vec<u8> },
    Save { reply: ReplyId },                    // snapshot bytes come back in a Frame
    // view-dependent work hints
    SetLod { policy: LodPolicy }, SetAnalyses { enabled: AnalysisSet },
    // queries that need an answer (picking, agent reads)
    Query { id: ReplyId, query: Query },
}

pub enum UndoMode { Step, Snapshot, None }   // same meaning as EditorModel.UndoMode
```

**Queries** (`describe_scene`, `inspect`, ray pick, measure) don't mutate. They
are answered against the latest *published* revision from immutable data
(`Arc<Snapshot>`), on the pool, so a slow query never blocks edits. Reply
arrives as `Frame::Reply { id, rev, payload }`. A synchronous wrapper
(`vcad_doc_runtime_query_blocking`) exists for MCP/CLI and tests only — Swift
UI code may not call it from main.

## Frames

```rust
pub struct Frame {
    pub rev: u64,               // document revision this frame describes
    pub acked_seq: u64,         // last intent seq folded into rev
    pub kind: FrameKind,
}

pub enum FrameKind {
    /// After every applied batch, before geometry. Cheap.
    Document {
        parts: Arc<[PartInfo]>,          // stable id, kind, name, material, visible
        params: Arc<[Param]>,            // name, value, min/max, expr
        feature_tree: Arc<FeatureTree>,  // replaces DocumentGraph.parse on main
        undo: UndoState,                 // can_undo/redo + labels for the Edit menu
        dirty: bool,                     // stateHash != saved
        warnings: Arc<[String]>,
        error: Option<CommandError>,     // a refused intent; rev unchanged
    },
    /// Per LOD pass. Only changed parts carry buffers; others are `Unchanged`.
    Geometry {
        lod: Lod,                        // Coarse | Fine
        parts: Vec<(PartId, PartGeom)>,  // PartGeom::Mesh(Arc<MeshBuffers>) | Unchanged | Removed | Failed(String)
        complete: bool,                  // every visible root has landed at this LOD
    },
    Sound    { locks: Arc<[PitchLock]>, quality: Preview | Fine },
    Dfm      { findings: Arc<[DfmFinding]> },
    Pick     { bvh_ready: bool },        // picking answered via Query, not shipped
    Reply    { id: ReplyId, payload: Vec<u8> },
}

pub struct MeshBuffers {     // layout = LowLevelMesh vertex descriptor, ready to memcpy
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub bounds: [[f32; 3]; 2],
    pub content_key: u128,   // replaces vcad_scene_root_key / partMeshCache keys
}
```

Over the C ABI a frame is an opaque `*const VcadFrame` with accessors
(`vcad_frame_rev`, `vcad_frame_kind`, `vcad_frame_part_mesh(i) -> VcadMeshView`)
— zero-copy borrow like today's `VcadMeshView`; Swift releases it with
`vcad_frame_release` once uploaded. Small metadata (`Document` kind) comes as
one JSON blob decoded on the runtime side of the trampoline or lazily.

**Swift never decodes the IR document.** Callers that today read
`documentJSON` (save, export, agent, history tree) go through `Query`
(`GetIr`, `Save`) or fields of the `Document` frame. Any reader that still needs
the full IR during migration gets `Arc<[u8]>` bytes from a query, not a
dictionary.

## Coalescing

Runtime loop: block on inbox → drain everything currently queued → coalesce →
apply as one batch → bump `rev` once → publish.

- `SetParam` with the same `(name, gesture)`: keep only the latest value.
- Consecutive `Strike`s on one gesture: keep latest.
- `Command`/`Tool`/`LoadIr`/`Undo`/`Redo` are **barriers**: never reordered or
  dropped. Coalescing only happens within a run of coalescible intents between
  barriers.
- `SetLod` / `SetAnalyses`: latest wins.

## Cancellation

Every background job captures `rev` and an `Arc<AtomicU64> latest_rev`.

- Eval / tessellate check `latest_rev` between roots (and the kernel's existing
  cancel token inside long booleans/fillets where available); a job whose rev is
  stale stops and drops its output.
- A stale result that already finished is still **published** if it is the
  newest *complete* geometry for some part that didn't change since — the
  content key decides, not rev. Otherwise discarded.
- Analyses (DFM, sound modes) are debounced: start only after `rev` has been
  stable for ~150 ms *or* `GestureEnd` arrives. Sound runs **preview → fine**:
  preview pitch (coarse mesh, few modes) immediately, fine pass after.
- Queries are never cancelled; they answer against the rev they were issued at.

## LOD

Per changed root: coarse tessellation first (the existing
`vcad_eval_begin_lod(preview = true)` tolerance), publish `Geometry{Coarse}`,
then fine. During an active gesture only coarse is produced; fine runs on
`GestureEnd` or after the debounce. Unchanged roots keep their fine mesh (eval
session cache), so a scrub of one feature re-tessellates one part at coarse.

## Undo, tool bus, ordering

- Undo/redo stacks move into the runtime. `UndoMode` semantics are identical to
  `EditorModel.engineEdit`: `Step` = one engine history group (engine `undo`),
  `Snapshot` = save before applying, restore on undo, `None` = continue gesture.
  A refused intent restores the snapshot and emits `error` with `rev` unchanged.
- A scrub gesture: first `SetParam` of a new `GestureId` opens a snapshot entry;
  the rest are `None`; `GestureEnd` closes it. One gesture = one undo step.
- Sketch-scoped undo (`sketch.undo()`) stays in Swift for now — sketch session
  is not document state until committed.
- **Ordering guarantee:** intents from all sources (UI, MCP host hooks, agent
  `AgentCore`, CLI attach) enter the same inbox; the runtime is the single
  serialization point. `acked_seq` lets Swift drop optimistic UI state once an
  intent is folded in. MCP edits get their reply (`createdFeatureId`, errors)
  via `Reply`, so `mcp.rs` no longer round-trips through Swift to touch the
  engine.
- Generation undo (`generationUndo`) and JSON undo steps become `Snapshot`
  entries in the runtime stack.

## Optimistic UI

Scrub handles and parameter fields show the user's value immediately (Swift
local state keyed by gesture); the frame with `acked_seq >= seq` replaces it.
Geometry is never faked on the Swift side.

## Failure + lifetime

- Kernel panic in a job: caught (`catch_unwind`) per root → `PartGeom::Failed`;
  the runtime thread itself never unwinds. If it does die, frames stop and the
  next `send` returns `VCAD_RUNTIME_DEAD`; Swift reopens from the last `Save`.
- `vcad_doc_runtime_free` closes the inbox, joins the thread, and guarantees no
  callback fires after return.
- One runtime per window/document. Memory: eval cache + two LODs per part; LODs
  of hidden parts are dropped.

## C ABI (sketch)

```c
typedef void (*VcadFrameFn)(void *ctx, const VcadFrame *frame);
VcadDocRuntime *vcad_doc_runtime_new(VcadFrameFn cb, void *ctx);
int  vcad_doc_runtime_send(VcadDocRuntime *, const uint8_t *intent_json, size_t len); // never blocks
char *vcad_doc_runtime_query_blocking(VcadDocRuntime *, const uint8_t *q, size_t len); // MCP/CLI/tests
void vcad_doc_runtime_free(VcadDocRuntime *);
uint64_t vcad_frame_rev(const VcadFrame *);
uint32_t vcad_frame_kind(const VcadFrame *);
const char *vcad_frame_json(const VcadFrame *, size_t *len);      // metadata
size_t vcad_frame_part_count(const VcadFrame *);
VcadMeshView vcad_frame_part_mesh(const VcadFrame *, size_t i);   // borrowed
void vcad_frame_retain(const VcadFrame *); void vcad_frame_release(const VcadFrame *);
```

## Migration order

Each slice lands as its own PR, app usable at every step.

1. **Geometry + parameters (slice 1).**
   - `vcad-runtime` crate: thread, inbox, coalescing, undo, `Document` +
     `Geometry` frames, coarse→fine, cancellation. Wraps existing
     `DocumentApi` and `vcad-eval` incremental session; no kernel changes.
   - `vcad-ffi`: `vcad_doc_runtime_*`, `vcad_frame_*`.
   - Swift: `DocumentRuntime.swift` (owner + trampoline). `EditorModel` adopts
     frames. **Shim:** `DocumentEngine` keeps its API but forwards
     `apply/applyTool/loadIR/restore/undo` to the runtime and returns
     immediately; callers that use the returned `Result` synchronously
     (`createdFeatureId`, add-then-select flows in `engineEdit`) get it from a
     blocking query in the shim for this slice only, flagged `// SHIM(runtime)`
     with a count we drive to zero.
   - `documentJSON` dictionary stays readable through the shim (decoded lazily
     from a query, off main) for un-migrated readers.
   - Done = `sample(1)` of a parameter scrub and a Mallet strike shows no
     `vcad_engine_*`, `vcad_scene_*`, `JSONSerialization` or tessellation on the
     main thread; `nice swift test` + `cargo test -p vcad-runtime -p vcad-ffi`
     green (Rust on `ssh mew`); release bundle (`CONFIG=release ./bundle.sh`).
     #1139 is merged: in slice 1 `Strike` still calls `vcad_sound_call` via
     `SoundStore`, but off main, against frame data; full absorption is slice 4.
2. **Picking.** BVH built per rev on the pool; `Picking.swift` sends
   `Query::Ray` and reads `Reply`. Delete `pickingDocumentData`,
   `PickingState.evaluate`, the second `EvalSession`.
3. **Analysis overlays.** `AnalysisMeshCache` goes; overlay meshes ride
   `Geometry`-like frames keyed by content key. DFM findings via `Dfm` frames.
4. **Sound.** `vcad_sound_call`/`vcad_instrument_call` callers go away; `SoundStore` becomes a frame consumer; modes/pitch locks preview →
   fine, debounced on gesture end.
5. **Agent/MCP.** `mcp.rs` host hooks send intents to the runtime directly;
   `AgentCore` engine-edit hooks and the agent fingerprint deleted (fingerprint
   = `rev` + state hash in the `Document` frame).
6. **Remove shim.** Delete blocking queries from UI paths, `DocumentEngine`,
   `EditorModel.documentJSON`, `partMeshCache`, `DocumentDelta.applied`.

## Open questions for review

1. **Undo in Rust vs Swift.** Proposed: move it (needed for MCP/CLI to share
   history). Alternative: keep Swift stacks, runtime only snapshots. Moving is
   more work in slice 1 but avoids a second migration.
2. **Intent encoding.** JSON for intents (small, generated types already
   exist) and binary for mesh payloads. OK, or binary everything?
3. **One runtime per window vs a process-wide runtime with doc handles.**
   Proposed per-document; VcadVision/CLI create their own.
4. **Shim blocking queries** in slice 1 for `createdFeatureId`-style callers —
   acceptable as a temporary, counted debt?
5. **Fine-LOD trigger:** gesture end + 150 ms stable — or always produce fine
   in the background at lower priority?

## Slice 1 as built

What landed differs from the sketch above in a few deliberate ways:

- **No separate handle.** `VcadEngine` (the existing `vcad_engine_*` handle)
  now wraps `vcad_runtime::Runtime`. Every blocking engine call runs on the
  runtime thread through `Runtime::call`, so the shim needed no new blocking
  API and UI, agent and MCP host-hook edits are ordered with the intents for
  free. New C ABI: `vcad_doc_runtime_{set_sink,send,rev,swap_gesture,
  forget_gesture,set_base_dir}` and `vcad_frame_*`.
- **One frame per batch, no LOD yet.** A frame carries the state envelope,
  IR bytes, state hash and the evaluated scene together, so the editor adopts
  document and meshes atomically. Coarse→fine LOD and pool-side cancellation
  are not in slice 1: evaluation runs on the runtime thread, and "latest
  wins" comes from coalescing (if more intents queued during a batch, the
  frame is skipped and folded into the next one).
- **Undo stays in Swift, data in Rust.** The Swift stack keeps labels and
  sketch scoping; a scrub gesture pushes `UndoEntry.Step.gesture(id)` whose
  snapshot lives in the runtime (`swap_gesture` undoes and redoes). Moving the
  whole stack is still the plan for the MCP slice.
- **Staleness = revision.** Every blocking edit bumps the runtime revision;
  the editor drops any frame not newer than its last blocking edit or last
  adopted frame. While intents are in flight `DocumentEngine` asks for full
  states instead of deltas (its delta base would be stale).
- **Frame decoding** (state JSON → `Result`, `DocumentGraph.parse`, mesh
  gathering via `vcad_scene_part_mesh`/edges) happens on the engine's frame
  queue; main swaps values in and builds `MeshResource`s only for parts whose
  root content key changed. The frame's IR bytes seed
  `documentBytesCache`, so picking/analysis/sound keys no longer serialize
  the document on main.
- **SHIM(runtime) left:** non-scrub edits (blocking calls, now off-thread but
  still waited on), instanced/pattern documents (re-tessellate on main after
  a frame), `flushRuntime()` for callers that read state right after a scrub
  (tests, smoke harnesses). `VCAD_RUNTIME_SCRUB=0` restores the old path.
- **Profiling hook:** `VCAD_SCRUB_BENCH=1` (`RuntimeBench.swift`) drives a
  180-tick parameter scrub and five Mallet strikes for `sample(1)`.

## Slice 2 as built: picking

- Blocking edits publish too: once the inbox is idle after a call that bumped
  the revision, the runtime sends a frame of that revision. `publish` asks for
  one explicitly (the editor sends it when it installs its sink).
- The frame queue builds each part's `PartTopology` (B-rep + face/edge table)
  off main, reusing parts by root content key. `PickingState.adopt` takes the
  frame's scene and topologies; a frame for a revision the editor already
  shows keeps the selection.
- Sync callers that need B-reps before a frame lands (feature ops, agent
  selection resources, measure) get `vcad_doc_runtime_scene`, which evaluates
  through the runtime's own incremental cache. `PickingState.evaluate`'s
  second session now only serves `AnalysisMeshCache` (slice 3) and documents
  without an engine.
- One set of document bytes per revision: frames only fill
  `documentBytesCache` when that revision has none, and the viewport's
  `evalData` shares `pickingDocumentData`, because its prepared-scene key
  depends on the bytes staying stable.
- Found on the way: the Mac app builds without the built-in font, and text
  evaluation `expect`ed it. Text now fails to evaluate with an error. Open
  question: why a panic aborts on the runtime thread instead of unwinding to
  its `catch_unwind` in the app's static lib.
- Release bench (glockenspiel, 12 s `sample`): main idle 79% (the bench
  drives 120 hover picks at 60 Hz plus a scrub). Hover worst tick 3.1 ms,
  120/120 hits, no evaluation on main. Follow-up: `vcad_solid_pick`
  re-tessellates the solid per pick (`topology::analyze`, ~0.3 ms); cache it
  per solid.
