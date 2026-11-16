# Thermal: document → voxel model (`vcad_eval::thermal_extract`)

Gap G1a from `docs/videos/every-physics.md`. Until this landed,
`vcad-kernel-thermal` only took hand-built geometry: Box and Tube regions
painted onto a grid. The `VoxelMaterials` seam (one material index per
voxel) existed, but nothing produced it. This page covers the producer: a
voxelizer on the vcad side of the seam, with studies stored on the document.

## The pieces

| Piece | Where |
|---|---|
| Study schema (`AnalysisStudyKind::Thermal(ThermalStudy)`, `StudyValue`) | `crates/vcad-ir/src/lib.rs` |
| Voxelizer + spec projection | `crates/vcad-eval/src/thermal_extract.rs` |
| Handbook material table (stopgap) | `thermal_extract::THERMAL_MATERIALS` |
| WASM: extract + solve in one call | `thermalSolveDocument` (`crates/vcad-kernel-wasm`) |
| MCP: `solve_thermal { document_id, study \| study_id }` | `packages/mcp/src/tools/thermal.ts` |
| Puck regression | `crates/vcad-eval/tests/thermal_extract_puck.rs` |

The solver crate does not change. `thermal_extract` evaluates the document,
voxelizes the parts the study selects, and emits an ordinary `ThermalSpec`:
grid literals, one `MaterialSpec` per part through `voxel_materials`, and
sources and sinks as boxes. Wherever the study names a document parameter,
the spec carries `ParamValue::Named`, and the resolved environment comes
back alongside it. So `spec.resolve(&parameters)` runs as-is, and scrubbing
`amp_loss_w` in the document re-prices the solve.

## Authoring: the study lives on the document

```jsonc
{ "type": "thermal",
  "parts": ["board", "die"],              // explicit; unlisted parts are void
  "divisions": [50, 50, 13],              // or "pitchMm": [1, 1, 0.2]
  "materials": [
    { "part": "board", "laminate": { "inPlaneWMk": 15, "throughPlaneWMk": 0.4 } },
    { "part": "die",   "kWMk": [120] } ],
  "sources": [ { "name": "amp", "part": "die", "powerW": "amp_loss_w" } ],
  "sinks":   [],                          // { part, temperatureC, region? }
  "exposed": { "type": "convection", "hWM2k": 5, "ambientC": 25 },
  "domainFaces": [ ... 6 ... ]            // omitted = all take `exposed`
}
```

It is persisted in `Document.analysis_studies` next to the structural and
tolerance studies, so a solve re-runs against current geometry. The MCP tool
also accepts the same object inline as `study`. **Loon cannot author a study
yet** (it has no form for `analysis_studies` or `clearance_specs`), so
`puck.loon` alone does not carry its thermal setup. A `[thermal-study ...]`
loon form is the follow-up that closes the one-document story for loon-first
authors.

## Voxelization

- **Grid.** It spans the union bounding box of the selected parts, plus
  `paddingMm` of void (default 0). With `pitchMm`, each axis gets the
  smallest whole voxel count at or below the requested pitch that spans the
  box exactly. A relative slack of 1e-6 absorbs f32 vertex noise, so
  2.6000004 mm at a 0.2 mm pitch gives 13 voxels, not 14.
- **Fill fraction.** Each part's tessellation is ray-cast along +z through a
  4 × 4 lattice of sub-columns per voxel column. Crossings pair into inside
  intervals, and each interval's overlap with a voxel's z-span is
  accumulated exactly. The result is exact in z and sampled 16× in xy.
- **Edge-exact parity.** The edge functions are exactly antisymmetric, and a
  top-left tie rule decides points on an edge. A ray through a shared edge
  or vertex (such as the diagonal of a box face) is therefore counted
  exactly once. There is no jitter. An odd crossing count is
  `OpenMesh { part }`.
- **Assignment.** A voxel is solid when the parts' total fill is at least ½
  (ties count as solid), and it takes the part with the largest fill. The
  tie rule reproduces the closed-boundary `Shape::contains` painting: a face
  that sits exactly on a voxel centre includes that voxel.
- **Guards.** Every guard names the part:
  - `ThinWall` fires when a part's 5th-percentile solid span along an axis is
    below `minVoxelsAcross` (default 2) times the pitch. The 5th percentile,
    not the minimum, keeps a cylinder's silhouette chords from tripping it.
  - `Unresolved` fires when no ray along an axis hits the part.
  - `PoorlyResolved` fires when assigned voxel volume falls outside 0.5–1.5
    of the mesh volume.
  - `PartsOverlap` fires when total fill exceeds 1 by more than 10⁻³ voxel.
  - `UnknownMaterial` fires when the material is missing from the table and
    has no override.
  - `SharedRegion` is covered under sources and sinks below.
- **Audit.** For each part the result reports mesh volume, fill volume
  (Σ fraction × dV), voxel count and volume, 5th-percentile thickness per
  axis, the conductivity used, and whether it came from the table or an
  override.

## Sources and sinks

The solver deposits power into **shapes**, not voxel sets. A source therefore
becomes the box that bounds the part's voxels, optionally intersected with a
`region`. The extractor then checks that the box holds **no other part's
voxels**. Void inside the box is harmless, because power goes only to
conducting voxels. If another part's voxels are inside, the source is
refused with `SharedRegion`; the fix is to give a `region` or split the part.
This is exact for the usual cases: a die, a regulator, or a resistor body.

A **sink** is a part, or a region of one, held at a fixed temperature
(`FixedSpec`, the same box rule).

## Materials

When a part has no override, its properties come from a small handbook table
keyed by substring of the part's material string, the same way vcad-sound's
`MATERIALS` table works:

| Key | Conductivity (W/m·K) | ρc_p (MJ/m³·K) |
|---|---|---|
| pla | 0.13 | 2.23 |
| petg | 0.20 | 1.52 |
| abs | 0.17 | 1.47 |
| nylon / pa12 | 0.25 | 1.94 |
| fr4 / pcb | **laminate** 0.81 in-plane, 0.29 through (bare, no copper) | 2.04 |
| aluminium | 167 | 2.42 |
| copper | 390 | 3.45 |
| steel | 50 | 3.85 |
| stainless | 16.2 | 4.00 |
| brass | 109 | 3.23 |
| bronze | 60 | 3.34 |
| titanium | 6.7 | 2.33 |
| silicon / die | 148 | 1.66 |
| glass | 1.0 | 2.10 |
| air (still) | 0.0263 | 0.0012 |

A laminate takes its through-plane axis from the part's thinnest
bounding-box axis. That axis must be at most ⅔ of the next one; otherwise the
extractor returns `AmbiguousLaminateAxis` and asks for `kWMk` per world
axis.

**The table is a stopgap.** It is waiting on the materials-in-IR work. Once
materials carry thermal properties in the document, the lookup reads them
from there and the table goes away.

**FR4 caution.** The table entry is *bare* laminate. A real board with
copper planes or pours conducts 20–50× better in-plane. On the puck at h = 10,
the bare-table board runs at 184.5 °C, against 66.3 °C for the poured board
(override 15 / 0.4). Always give a board a `laminate` override that
reflects its copper.

## The puck: reproducing the PoC

The every-physics PoC (PR #1190, `docs/videos/every-physics/poc`) hand-built
this model:

- the board alone, on a 50 × 50 × 13 grid (1 × 1 × 0.2 mm voxels);
- the board at k = [15, 15, 0.4] W/m·K;
- a 5 × 5 × 1 mm die at k = 120, dissipating 0.8 W;
- convection on ±z and on every exposed face, with ±x/±y adiabatic.

It printed T_max **80.6 °C at h = 5** and **64.3 °C at h = 10**.

The test fixture `tests/fixtures/puck_thermal.loon` is the same puck with
named roots and a `die` part. Results (`tests/thermal_extract_puck.rs`):

| Grid | Die footprint | h = 5 | h = 10 |
|---|---|---|---|
| PoC, as printed | — | 80.6 | 64.3 |
| 1 mm, hand-built (PoC code) | 36 mm² (6 × 6, closed painting) | 80.56 | 64.32 |
| 1 mm, document (voxelizer) | 32 mm² (6 × 6 minus corners) | 82.53 | 66.28 |
| 0.5 mm, document **=** hand-built | 25 mm² (exact) | 86.09 | 69.80 |

**Grid-aligned, the two agree exactly.** At 0.5 mm in-plane pitch the die
edges land on voxel faces, so no voxel centre sits on a boundary. The
voxelized field and the hand-painted field are identical, and T_max matches
to solver tolerance (86.091 / 69.804 °C both ways). The board voxelizes to
100 × 100 × 8 and the die to 10 × 10 × 5. Fill volumes equal the true
volumes (4000 and 25 mm³).

**On the PoC's 1 mm grid, the difference is the die footprint.** The die
edges at ±2.5 mm sit exactly on voxel centres:

- The PoC's closed `Box` painting takes every voxel whose centre is on the
  boundary. That gives a 6 × 6 footprint: 36 mm² for a 25 mm² die.
- The voxelizer's ½-fill rule keeps the edge voxels, which are exactly half
  full, and drops the four corners, which are a quarter full. That gives
  32 mm².

A smaller footprint concentrates the same power. The document therefore
reads 2.0 °C hotter than the PoC (3.5% of the temperature rise). Both are
staircase artifacts.

**The finding for the video.** The PoC's numbers under-read. The in-plane
refinement converges:

| In-plane pitch | h = 5 | h = 10 |
|---|---|---|
| 1 mm | 82.53 | 66.28 |
| 0.5 mm | 86.09 | 69.80 |
| 0.25 mm | 85.90 | 69.61 |

This is the ignored test `puck_in_plane_convergence`. With the true footprint
and a converged in-plane grid, T_max is about **85.9 °C at h = 5** and
**69.6 °C at h = 10**. That is roughly 10% (h = 5) and 13% (h = 10) more
temperature rise than the PoC printed. The z pitch is fixed at 0.2 mm in all
of these runs.

Other modelling choices from the PoC are carried unchanged, and each is
visible in the study:

- **Board edges adiabatic**, because `domainFaces` was set explicitly. With
  the default (board edges convect too), T_max at h = 5 on the 1 mm grid is
  80.80 °C instead of 82.53 °C.
- **Board conductivity 15 / 0.4.** This is FR4 with copper pour. It is
  assumed, not measured.
- **h is supplied, not derived.**

The bare-FR4 table entry, used with no override, gives **184.5 °C at
h = 10**, against 66.3 °C with the poured board. This is the reason for the
FR4 caution above.

## Honest limits

- **No per-part convection.** The solver has one `exposed` boundary for every
  solid↔void face. A part can be a sink (fixed temperature), but it cannot
  have its own film coefficient. "This heatsink's fins see h = 25 while the
  case sees h = 5" needs per-face boundary slots in the solver.
- **Radiation** follows the solver. The IR boundary enum has adiabatic,
  convection, and fixed temperature. Radiation joins it once the radiation
  boundary (PR #1189) is on main.
- **Steady only through the document path.** `transient` with `document_id`
  is refused for now; the table carries ρc_p, so wiring it is mechanical.
- **Roots only.** Assembly instances are not voxelized.
- **Staircase geometry.** Curved walls are voxel staircases, and their face
  area does not converge: a cylinder carries ×4/π extra area at every
  resolution. With convection on exposed faces, that over-counts cooling and
  reads temperatures low, which is the unsafe direction. The radiation PR's
  `area_scale` is the correction hook. Convection has no equivalent yet.
- **Conduction only.** The air inside the puck is not a fluid. Listing the
  `air` part gives still-air conduction, which is a lower bound on heat
  transport, not a prediction.
- **Sampled in xy.** Fill fractions are exact in z but use 16 samples per
  column in xy, so a fill accurate to about 1/16 decides voxels near a
  slanted wall. The thin-wall and volume guards bound the damage. They do
  not remove it.
