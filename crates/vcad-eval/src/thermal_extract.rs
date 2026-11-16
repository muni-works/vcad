//! Document → [`ThermalSpec`]: the voxelizer on the vcad side of the
//! thermal seam.
//!
//! `vcad-kernel-thermal` solves conduction on a voxel grid and takes
//! tessellated part geometry through [`VoxelMaterials`] (one material index
//! per voxel), deliberately leaving the voxelizer to "the vcad side". This
//! module is that side, with the division of labor every other
//! document→solver adapter uses: the solver crate knows nothing about
//! documents; this module evaluates the document with the same
//! resolve-and-walk every consumer uses, voxelizes the parts a
//! [`ThermalStudy`] selects, and emits the solver's own schema.
//!
//! ## Voxelization
//!
//! The grid spans the selected parts' union bounding box (plus optional
//! void padding). Each part's tessellation is ray-cast along +Z through a
//! 4 × 4 lattice of sub-columns per voxel column; crossings are paired
//! into inside intervals (ray parity), and each interval's overlap with a
//! voxel's z-extent is accumulated **exactly**. The result is a per-part
//! *fill fraction* per voxel — exact along z, sampled 16× in xy.
//!
//! Crossings use a top-left tie rule on exactly antisymmetric edge
//! functions, so a ray through a shared triangle edge or vertex (the
//! diagonal of a box's top face, say) is counted exactly once — no jitter.
//! An odd crossing count (an open or self-intersecting tessellation) is an
//! error naming the part, never a dropped crossing.
//!
//! **Assignment rule.** A voxel is solid when the parts' total fill is at
//! least ½ (ties go to solid, which reproduces the closed-boundary
//! painting of `Shape::contains`), and it takes the material of the part
//! filling most of it. Parts that overlap in space are refused.
//!
//! **Fail-closed guards** (each error names the part):
//! - *thin walls*: rays along each axis measure solid spans; a part whose
//!   5th-percentile span along an axis is under `min_voxels_across` voxels
//!   (default 2) is refused — a wall one voxel thick conducts through a
//!   single staircase cell, or vanishes outright;
//! - *poorly resolved*: assigned voxel volume outside ½–1½ × the mesh
//!   volume;
//! - *shared heat*: a source or sink whose part shares its voxel bounding
//!   box with another part's solid voxels (sources are boxes in the
//!   solver, so the box must hold only the named part).
//!
//! ## Materials
//!
//! Thermal properties come from a per-part override in the study, else
//! from [`thermal_material_for_key`]: a small handbook table keyed by the
//! part's material string. The table is a stopgap — it is waiting on the
//! materials-in-IR work, after which properties come from the document.
//! Laminates (circuit boards) take their through-plane axis from the
//! part's thinnest bounding-box axis.
//!
//! ## What it does not do
//!
//! Per-part convection: the solver has one `exposed` boundary for every
//! solid↔void face, so a part can be a heat sink (fixed temperature) but
//! not a surface with its own film coefficient. Instances (assembly mode)
//! are not voxelized — only document roots. Radiation boundaries follow
//! the solver's radiation support. See `docs/thermal-voxelizer.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vcad_ir::{
    Document, StudyRegion, StudyValue, ThermalStudy, ThermalStudyBoundary, ThermalStudyMaterial,
};
use vcad_kernel_thermal::model::VoxelMaterials;
use vcad_kernel_thermal::spec::{
    BoundarySpec, FixedSpec, MaterialSpec, ParamValue, ShapeSpec, SourceSpec, ThermalSpec,
};

use crate::{evaluate_document, EvalError, EvalOptions, EvaluatedMesh, EvaluatedScene};

/// Sub-columns per voxel column, per axis (4 × 4 = 16 rays per column).
const SUB: usize = 4;
/// Largest grid this module will allocate.
const MAX_VOXELS: usize = 8_000_000;
/// Fill-fraction tolerance for the solid/void tie (exact ½ counts solid).
const FILL_EPS: f64 = 1e-6;

// ---------------------------------------------------------------------------
// Material table
// ---------------------------------------------------------------------------

/// Thermal conductivity of a table material.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Conductivity {
    /// The same in every direction, W/(m·K).
    Isotropic {
        /// Conductivity, W/(m·K).
        k_w_mk: f64,
    },
    /// A laminate: in-plane vs through-plane, W/(m·K). The through-plane
    /// axis is the part's thinnest bounding-box axis.
    Laminate {
        /// In-plane conductivity.
        in_plane_w_mk: f64,
        /// Through-plane conductivity.
        through_plane_w_mk: f64,
    },
}

/// Handbook thermal properties for a material key.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ThermalMaterial {
    /// Display name.
    pub name: &'static str,
    /// Conductivity.
    pub conductivity: Conductivity,
    /// Density, kg/m³.
    pub density_kg_m3: f64,
    /// Specific heat, J/(kg·K).
    pub specific_heat_j_kgk: f64,
}

impl ThermalMaterial {
    /// Volumetric heat capacity ρ·c_p, J/(m³·K).
    pub fn heat_capacity_j_m3k(&self) -> f64 {
        self.density_kg_m3 * self.specific_heat_j_kgk
    }
}

const fn iso(name: &'static str, k: f64, rho: f64, cp: f64) -> ThermalMaterial {
    ThermalMaterial {
        name,
        conductivity: Conductivity::Isotropic { k_w_mk: k },
        density_kg_m3: rho,
        specific_heat_j_kgk: cp,
    }
}

/// Key substrings → properties, first match wins (so "stainless" precedes
/// "steel"). Room-temperature handbook values; keyed like `vcad-sound`'s
/// table. Waiting on materials-in-IR to move into the document.
const THERMAL_MATERIALS: &[(&[&str], ThermalMaterial)] = &[
    (&["stainless"], iso("stainless steel", 16.2, 8000.0, 500.0)),
    (&["steel", "iron"], iso("steel", 50.0, 7850.0, 490.0)),
    (&["copper"], iso("copper", 390.0, 8960.0, 385.0)),
    (
        &["alum", "6061", "7075"],
        iso("aluminium 6061", 167.0, 2700.0, 896.0),
    ),
    (&["brass"], iso("brass", 109.0, 8500.0, 380.0)),
    (&["bronze"], iso("bronze", 60.0, 8800.0, 380.0)),
    (&["titan"], iso("titanium Ti-6Al-4V", 6.7, 4430.0, 526.0)),
    (&["silicon", "die"], iso("silicon", 148.0, 2330.0, 712.0)),
    // Bare FR4 laminate (no copper). A board with planes/pours conducts
    // ~20–50× better in-plane — override with `laminate` for a real board.
    (
        &["fr4", "fr-4", "pcb"],
        ThermalMaterial {
            name: "FR4 laminate (bare)",
            conductivity: Conductivity::Laminate {
                in_plane_w_mk: 0.81,
                through_plane_w_mk: 0.29,
            },
            density_kg_m3: 1850.0,
            specific_heat_j_kgk: 1100.0,
        },
    ),
    (&["glass"], iso("glass", 1.0, 2500.0, 840.0)),
    (&["petg"], iso("PETG", 0.20, 1270.0, 1200.0)),
    (&["pla"], iso("PLA", 0.13, 1240.0, 1800.0)),
    (&["abs"], iso("ABS", 0.17, 1050.0, 1400.0)),
    (&["nylon", "pa12"], iso("nylon", 0.25, 1140.0, 1700.0)),
    // Still air, conduction only (no convection inside the part).
    (&["air"], iso("air (still)", 0.0263, 1.18, 1005.0)),
];

/// Look up handbook thermal properties by material key (case-insensitive
/// substring match). `None` when nothing matches — the caller fails closed.
pub fn thermal_material_for_key(key: &str) -> Option<ThermalMaterial> {
    let k = key.to_ascii_lowercase();
    THERMAL_MATERIALS
        .iter()
        .find(|(keys, _)| keys.iter().any(|needle| k.contains(needle)))
        .map(|(_, m)| *m)
}

fn table_keys() -> String {
    THERMAL_MATERIALS
        .iter()
        .map(|(k, _)| k[0])
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Extraction failures. Every geometry variant names the part.
#[derive(Debug, thiserror::Error)]
pub enum ThermalExtractError {
    /// Document evaluation failed.
    #[error("document evaluation failed: {0}")]
    Eval(#[from] EvalError),
    /// Parameter resolution failed.
    #[error("parameter resolution failed: {0}")]
    Resolve(String),
    /// The study is malformed.
    #[error("invalid thermal study: {0}")]
    Invalid(String),
    /// No study with that id, or no thermal study to default to.
    #[error("{0}")]
    NoStudy(String),
    /// No root matches the part selection.
    #[error("no part named {name:?} in the document (parts: {available})")]
    UnknownPart {
        /// The selection.
        name: String,
        /// Selectable names.
        available: String,
    },
    /// More than one root matches.
    #[error("{count} parts are named {name:?}; rename one so the selection is unambiguous")]
    AmbiguousPart {
        /// The selection.
        name: String,
        /// Matches.
        count: usize,
    },
    /// The part is hidden (not evaluated).
    #[error("part {0:?} is hidden; hidden parts are not evaluated, so it can't be voxelized")]
    HiddenPart(String),
    /// The part failed to evaluate.
    #[error("part {part:?} failed to evaluate: {error}")]
    PartFailed {
        /// Part.
        part: String,
        /// Evaluator message.
        error: String,
    },
    /// A source/sink/override names a part not in `parts`.
    #[error("{role} names part {part:?}, which is not in the study's `parts` list")]
    NotSelected {
        /// What referred to it.
        role: String,
        /// The part.
        part: String,
    },
    /// The part's material key is not in the table and has no override.
    #[error(
        "part {part:?} has material {material:?}, which has no thermal properties in the table \
         ({keys}); add a `materials` override for it"
    )]
    UnknownMaterial {
        /// Part.
        part: String,
        /// Material key.
        material: String,
        /// Table keys.
        keys: String,
    },
    /// A laminate part has no single thinnest axis.
    #[error(
        "part {part:?} is a laminate but has no clear through-plane axis (bounding box \
         {extent_mm:?} mm; the thinnest axis must be at most 2/3 of the next) — give \
         `k_w_mk` per world axis instead"
    )]
    AmbiguousLaminateAxis {
        /// Part.
        part: String,
        /// Bounding-box extent.
        extent_mm: [f64; 3],
    },
    /// A named parameter is not declared by the document.
    #[error("document has no parameter named {name:?} (needed for {role})")]
    UnknownParameter {
        /// Parameter.
        name: String,
        /// What it drives.
        role: String,
    },
    /// The tessellation is not closed along a ray.
    #[error(
        "part {part:?}: a ray along {axis} at ({u:.4}, {v:.4}) crosses its surface an odd \
         number of times ({count}) — the tessellation is open or self-intersecting"
    )]
    OpenMesh {
        /// Part.
        part: String,
        /// Ray axis.
        axis: char,
        /// First cross-axis coordinate, mm.
        u: f64,
        /// Second cross-axis coordinate, mm.
        v: f64,
        /// Crossing count.
        count: usize,
    },
    /// The part is too thin for the grid.
    #[error(
        "part {part:?} is {thickness_mm:.3} mm thick along {axis} (5th-percentile solid span) \
         but the voxel pitch there is {pitch_mm:.3} mm — under {min_voxels} voxels across, it \
         would conduct through a staircase or vanish; refine the grid along {axis}"
    )]
    ThinWall {
        /// Part.
        part: String,
        /// Axis.
        axis: char,
        /// Measured thickness, mm.
        thickness_mm: f64,
        /// Pitch along that axis, mm.
        pitch_mm: f64,
        /// The guard.
        min_voxels: f64,
    },
    /// No ray along an axis hit the part at all.
    #[error(
        "part {part:?} is missed by every ray along {axis} — it is thinner than one voxel in \
         the other two axes; refine the grid"
    )]
    Unresolved {
        /// Part.
        part: String,
        /// Axis.
        axis: char,
    },
    /// The voxelized volume is far from the mesh volume.
    #[error(
        "part {part:?}: voxelized volume {voxel_mm3:.1} mm³ vs mesh volume {mesh_mm3:.1} mm³ \
         (ratio {ratio:.2}, allowed 0.5–1.5) — the grid can't represent this part"
    )]
    PoorlyResolved {
        /// Part.
        part: String,
        /// Assigned voxel volume.
        voxel_mm3: f64,
        /// Mesh volume.
        mesh_mm3: f64,
        /// Ratio.
        ratio: f64,
    },
    /// Two selected parts overlap in space.
    #[error(
        "parts {a:?} and {b:?} overlap (~{overlap_mm3:.2} mm³); a voxel can hold one material — \
         subtract one from the other or drop one from `parts`"
    )]
    PartsOverlap {
        /// First part.
        a: String,
        /// Second part.
        b: String,
        /// Estimated overlap volume.
        overlap_mm3: f64,
    },
    /// A source/sink box would also cover another part.
    #[error(
        "{role} on part {part:?} would also heat/pin part {other:?}: the solver deposits into a \
         box (the part's voxel bounding box{region}), and that box holds {other:?}'s voxels — \
         give a `region` that holds only {part:?}, or split the part"
    )]
    SharedRegion {
        /// "source \"amp\"" / "sink".
        role: String,
        /// The named part.
        part: String,
        /// The intruding part.
        other: String,
        /// Region note.
        region: String,
    },
    /// A source/sink region covers none of the part's voxels.
    #[error("{role} on part {part:?}: its region covers none of the part's voxels")]
    EmptyRegion {
        /// Role.
        role: String,
        /// Part.
        part: String,
    },
    /// The grid is too large.
    #[error("grid of {voxels} voxels exceeds the {max} cap — coarsen `divisions` / `pitch_mm`")]
    GridTooLarge {
        /// Requested voxels.
        voxels: usize,
        /// Cap.
        max: usize,
    },
}

type R<T> = Result<T, ThermalExtractError>;

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// The voxel grid the spec was built on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThermalGrid {
    /// Minimum corner, mm.
    pub origin_mm: [f64; 3],
    /// Extent, mm.
    pub size_mm: [f64; 3],
    /// Voxels per axis.
    pub divisions: [usize; 3],
    /// Voxel edge lengths, mm.
    pub pitch_mm: [f64; 3],
}

/// Where a part's thermal properties came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialSource {
    /// The study's per-part override.
    Override,
    /// The built-in handbook table (keyed by the part's material string).
    Table,
}

/// Per-part voxelization audit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoxelizedPart {
    /// Part name as selected.
    pub part: String,
    /// The part's material key.
    pub material_key: String,
    /// Where the properties came from.
    pub material_source: MaterialSource,
    /// Table entry name, when `material_source` is `table`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_material: Option<String>,
    /// World-axis conductivity `[x, y, z]` (literal, or the named
    /// parameter's resolved value), W/(m·K).
    pub k_w_mk: [f64; 3],
    /// Through-plane axis for a laminate (0 = x, 1 = y, 2 = z).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub laminate_axis: Option<usize>,
    /// ρ·c_p, J/(m³·K), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heat_capacity_j_m3k: Option<f64>,
    /// Closed-mesh volume, mm³.
    pub mesh_volume_mm3: f64,
    /// Σ fill fraction × voxel volume (the voxelizer's own volume
    /// estimate), mm³.
    pub fill_volume_mm3: f64,
    /// Voxels assigned to this part.
    pub voxels: usize,
    /// `voxels` × voxel volume, mm³ — what the solver conducts through.
    pub voxel_volume_mm3: f64,
    /// 5th-percentile solid span along each axis, mm.
    pub thickness_p05_mm: [f64; 3],
}

/// The extracted thermal problem.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedThermal {
    /// The solver schema: grid literals, per-part materials through
    /// `voxel_materials`, sources/sinks as part-bounding boxes, and named
    /// document parameters wherever the study named one.
    pub spec: ThermalSpec,
    /// The document's resolved parameters (binds every name in `spec`).
    pub parameters: BTreeMap<String, f64>,
    /// The grid.
    pub grid: ThermalGrid,
    /// Per-part audit, index-aligned with `spec.materials`.
    pub parts: Vec<VoxelizedPart>,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

fn extraction_eval_options() -> EvalOptions {
    EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
    }
}

/// Pick a thermal study off the document: by id, or the only one.
pub fn find_thermal_study<'a>(doc: &'a Document, id: Option<&str>) -> R<&'a ThermalStudy> {
    let thermal: Vec<(&str, &ThermalStudy)> = doc
        .analysis_studies
        .iter()
        .filter_map(|s| match &s.study {
            vcad_ir::AnalysisStudyKind::Thermal(t) => Some((s.id.as_str(), t)),
            _ => None,
        })
        .collect();
    match id {
        Some(id) => thermal
            .iter()
            .find(|(sid, _)| *sid == id)
            .map(|(_, t)| *t)
            .ok_or_else(|| {
                ThermalExtractError::NoStudy(format!(
                    "no thermal study with id {id:?} (thermal studies: {})",
                    thermal
                        .iter()
                        .map(|(i, _)| *i)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }),
        None => match thermal.as_slice() {
            [(_, t)] => Ok(*t),
            [] => Err(ThermalExtractError::NoStudy(
                "the document has no thermal analysis study; pass one inline".into(),
            )),
            many => Err(ThermalExtractError::NoStudy(format!(
                "the document has {} thermal studies ({}); pass `study_id`",
                many.len(),
                many.iter().map(|(i, _)| *i).collect::<Vec<_>>().join(", ")
            ))),
        },
    }
}

/// Evaluate `doc` and extract a voxelized [`ThermalSpec`] for `study`.
pub fn extract_thermal_spec(doc: &Document, study: &ThermalStudy) -> R<ExtractedThermal> {
    let scene = evaluate_document(doc, &extraction_eval_options())?;
    extract_thermal_spec_from_scene(doc, &scene, study)
}

// ---------------------------------------------------------------------------
// Part lookup
// ---------------------------------------------------------------------------

fn root_names(doc: &Document, idx: usize) -> Vec<String> {
    let root = doc.roots[idx].root;
    let mut names = Vec::new();
    if let Some(n) = doc.nodes.get(&root).and_then(|n| n.name.clone()) {
        names.push(n);
    }
    if let Some(id) = doc.part_ids.as_ref().and_then(|m| m.get(&root.to_string())) {
        names.push(id.clone());
    }
    names.push(root.to_string());
    names
}

/// Scene index of the root selected by `name` (node name, part id, or
/// stringified root node id).
fn find_part_index(doc: &Document, scene: &EvaluatedScene, name: &str) -> R<usize> {
    let matches: Vec<usize> = (0..doc.roots.len())
        .filter(|&i| root_names(doc, i).iter().any(|n| n == name))
        .collect();
    let idx = match matches.as_slice() {
        [] => {
            let mut available: Vec<String> = (0..doc.roots.len())
                .filter_map(|i| root_names(doc, i).into_iter().next())
                .collect();
            available.sort();
            return Err(ThermalExtractError::UnknownPart {
                name: name.to_string(),
                available: available.join(", "),
            });
        }
        [i] => *i,
        many => {
            return Err(ThermalExtractError::AmbiguousPart {
                name: name.to_string(),
                count: many.len(),
            })
        }
    };
    let entry = &doc.roots[idx];
    if entry.visible == Some(false) {
        return Err(ThermalExtractError::HiddenPart(name.to_string()));
    }
    if let Some(f) = scene.failures.iter().find(|f| f.node_id == entry.root) {
        return Err(ThermalExtractError::PartFailed {
            part: name.to_string(),
            error: f.error.clone(),
        });
    }
    let visible = doc
        .roots
        .iter()
        .filter(|e| e.visible != Some(false))
        .count();
    if scene.parts.len() != visible {
        return Err(ThermalExtractError::Invalid(format!(
            "evaluated scene has {} parts for {visible} visible roots; can't map part names",
            scene.parts.len()
        )));
    }
    Ok(doc.roots[..idx]
        .iter()
        .filter(|e| e.visible != Some(false))
        .count())
}

// ---------------------------------------------------------------------------
// Ray casting
// ---------------------------------------------------------------------------

type Tri = [[f64; 3]; 3];

fn mesh_tris(m: &EvaluatedMesh) -> Vec<Tri> {
    let p = |i: u32| {
        let i = i as usize * 3;
        [
            m.positions[i] as f64,
            m.positions[i + 1] as f64,
            m.positions[i + 2] as f64,
        ]
    };
    m.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [p(t[0]), p(t[1]), p(t[2])])
        .collect()
}

fn mesh_volume(tris: &[Tri]) -> f64 {
    tris.iter()
        .map(|[a, b, c]| {
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// 2D edge function `(a − p) × (b − p)`. Exactly antisymmetric in `a, b`
/// (IEEE multiplication commutes and `x − y = −(y − x)`), which is what
/// makes the tie rule below consistent across a shared edge.
#[inline]
fn edge(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    (a[0] - p[0]) * (b[1] - p[1]) - (a[1] - p[1]) * (b[0] - p[0])
}

/// Top-left rule: of an edge and its reverse, exactly one owns the points
/// lying on it.
#[inline]
fn owns_edge(a: [f64; 2], b: [f64; 2]) -> bool {
    let d = [b[0] - a[0], b[1] - a[1]];
    d[1] > 0.0 || (d[1] == 0.0 && d[0] < 0.0)
}

/// Where a ray along `axis` through cross-axis point `p = (u, v)` crosses
/// `tri`, if it does. `(u, v)` are axes `(axis+1)%3, (axis+2)%3`.
fn ray_crossing(tri: &Tri, axis: usize, p: [f64; 2]) -> Option<f64> {
    let (ua, va) = ((axis + 1) % 3, (axis + 2) % 3);
    let q = |c: &[f64; 3]| [c[ua], c[va]];
    let (mut a, mut b, c) = (q(&tri[0]), q(&tri[1]), q(&tri[2]));
    let (mut ha, mut hb, hc) = (tri[0][axis], tri[1][axis], tri[2][axis]);
    let mut area = edge(a, b, c);
    if area == 0.0 {
        return None; // edge-on: the neighbours own this ray
    }
    if area < 0.0 {
        std::mem::swap(&mut a, &mut b);
        std::mem::swap(&mut ha, &mut hb);
        area = -area;
    }
    // Weight of each vertex = signed area of the opposite sub-triangle.
    let wa = edge(b, c, p);
    let wb = edge(c, a, p);
    let wc = edge(a, b, p);
    let inside = |w: f64, e0: [f64; 2], e1: [f64; 2]| w > 0.0 || (w == 0.0 && owns_edge(e0, e1));
    if !(inside(wa, b, c) && inside(wb, c, a) && inside(wc, a, b)) {
        return None;
    }
    Some((wa * ha + wb * hb + wc * hc) / area)
}

/// Triangles bucketed on a 2D grid in the plane normal to `axis`.
struct Buckets<'a> {
    tris: &'a [Tri],
    axis: usize,
    origin: [f64; 2],
    cell: [f64; 2],
    n: [usize; 2],
    cells: Vec<Vec<u32>>,
}

impl<'a> Buckets<'a> {
    fn new(tris: &'a [Tri], axis: usize, origin: [f64; 2], cell: [f64; 2], n: [usize; 2]) -> Self {
        let (ua, va) = ((axis + 1) % 3, (axis + 2) % 3);
        let mut cells = vec![Vec::new(); n[0] * n[1]];
        let clamp =
            |x: f64, k: usize| -> usize { x.floor().clamp(0.0, (n[k] - 1) as f64) as usize };
        for (t, tri) in tris.iter().enumerate() {
            let lo = [
                tri.iter().map(|c| c[ua]).fold(f64::INFINITY, f64::min),
                tri.iter().map(|c| c[va]).fold(f64::INFINITY, f64::min),
            ];
            let hi = [
                tri.iter().map(|c| c[ua]).fold(f64::NEG_INFINITY, f64::max),
                tri.iter().map(|c| c[va]).fold(f64::NEG_INFINITY, f64::max),
            ];
            // A sliver of slack so a vertex on a cell line reaches both.
            let s = 1e-9;
            let i0 = clamp((lo[0] - origin[0]) / cell[0] - s, 0);
            let i1 = clamp((hi[0] - origin[0]) / cell[0] + s, 0);
            let j0 = clamp((lo[1] - origin[1]) / cell[1] - s, 1);
            let j1 = clamp((hi[1] - origin[1]) / cell[1] + s, 1);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    cells[j * n[0] + i].push(t as u32);
                }
            }
        }
        Self {
            tris,
            axis,
            origin,
            cell,
            n,
            cells,
        }
    }

    /// Sorted crossings of the ray through `p`; odd count is an error.
    fn crossings(&self, part: &str, p: [f64; 2]) -> R<Vec<f64>> {
        let i = ((p[0] - self.origin[0]) / self.cell[0]).floor();
        let j = ((p[1] - self.origin[1]) / self.cell[1]).floor();
        if i < 0.0 || j < 0.0 || i >= self.n[0] as f64 || j >= self.n[1] as f64 {
            return Ok(Vec::new());
        }
        let mut hits: Vec<f64> = self.cells[j as usize * self.n[0] + i as usize]
            .iter()
            .filter_map(|&t| ray_crossing(&self.tris[t as usize], self.axis, p))
            .collect();
        if !hits.len().is_multiple_of(2) {
            return Err(ThermalExtractError::OpenMesh {
                part: part.to_string(),
                axis: AXES[self.axis],
                u: p[0],
                v: p[1],
                count: hits.len(),
            });
        }
        hits.sort_by(f64::total_cmp);
        Ok(hits)
    }
}

const AXES: [char; 3] = ['x', 'y', 'z'];

// ---------------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------------

struct Grid {
    origin: [f64; 3],
    size: [f64; 3],
    n: [usize; 3],
    d: [f64; 3],
}

impl Grid {
    fn idx(&self, i: usize, j: usize, k: usize) -> usize {
        (k * self.n[1] + j) * self.n[0] + i
    }
    fn center(&self, i: usize, j: usize, k: usize) -> [f64; 3] {
        [
            self.origin[0] + (i as f64 + 0.5) * self.d[0],
            self.origin[1] + (j as f64 + 0.5) * self.d[1],
            self.origin[2] + (k as f64 + 0.5) * self.d[2],
        ]
    }
    /// Voxel index range `[lo, hi)` along `a` overlapping `[x0, x1]`.
    fn range(&self, a: usize, x0: f64, x1: f64) -> (usize, usize) {
        let lo = ((x0 - self.origin[a]) / self.d[a]).floor().max(0.0) as usize;
        let hi = (((x1 - self.origin[a]) / self.d[a]).ceil().max(0.0) as usize).min(self.n[a]);
        (lo.min(self.n[a]), hi)
    }
}

fn bbox(tris: &[Tri]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for t in tris {
        for c in t {
            for a in 0..3 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
        }
    }
    (lo, hi)
}

fn build_grid(study: &ThermalStudy, lo: [f64; 3], hi: [f64; 3]) -> R<Grid> {
    let pad = study.padding_mm.unwrap_or(0.0);
    if !(pad.is_finite() && pad >= 0.0) {
        return Err(ThermalExtractError::Invalid(format!(
            "paddingMm must be >= 0 (got {pad})"
        )));
    }
    let origin = [lo[0] - pad, lo[1] - pad, lo[2] - pad];
    let size = [
        hi[0] - lo[0] + 2.0 * pad,
        hi[1] - lo[1] + 2.0 * pad,
        hi[2] - lo[2] + 2.0 * pad,
    ];
    let n: [usize; 3] = match (study.divisions, study.pitch_mm) {
        (Some(d), None) => {
            if d.contains(&0) {
                return Err(ThermalExtractError::Invalid(
                    "divisions must be >= 1 per axis".into(),
                ));
            }
            [d[0] as usize, d[1] as usize, d[2] as usize]
        }
        (None, Some(p)) => {
            let mut n = [0usize; 3];
            for a in 0..3 {
                if !(p[a].is_finite() && p[a] > 0.0) {
                    return Err(ThermalExtractError::Invalid(format!(
                        "pitchMm must be positive (axis {} got {})",
                        AXES[a], p[a]
                    )));
                }
                // Smallest whole count at or below the requested pitch;
                // the relative slack absorbs f32 vertex noise (2.6000004 /
                // 0.2 is 13 voxels, not 14).
                n[a] = ((size[a] / p[a]) * (1.0 - 1e-6)).ceil().max(1.0) as usize;
            }
            n
        }
        _ => {
            return Err(ThermalExtractError::Invalid(
                "give exactly one of `divisions` / `pitchMm`".into(),
            ))
        }
    };
    let voxels = n[0].saturating_mul(n[1]).saturating_mul(n[2]);
    if voxels > MAX_VOXELS {
        return Err(ThermalExtractError::GridTooLarge {
            voxels,
            max: MAX_VOXELS,
        });
    }
    if size.iter().any(|&s| s.is_nan() || s <= 0.0) {
        return Err(ThermalExtractError::Invalid(
            "the selected parts have a degenerate bounding box".into(),
        ));
    }
    Ok(Grid {
        origin,
        size,
        n,
        d: [
            size[0] / n[0] as f64,
            size[1] / n[1] as f64,
            size[2] / n[2] as f64,
        ],
    })
}

// ---------------------------------------------------------------------------
// Per-part voxelization
// ---------------------------------------------------------------------------

/// Fill fractions of one part over its voxel sub-box.
struct PartFill {
    lo: [usize; 3],
    hi: [usize; 3],
    fill: Vec<f64>,
}

impl PartFill {
    fn get(&self, i: usize, j: usize, k: usize) -> f64 {
        let nx = self.hi[0] - self.lo[0];
        let ny = self.hi[1] - self.lo[1];
        self.fill[((k - self.lo[2]) * ny + (j - self.lo[1])) * nx + (i - self.lo[0])]
    }
}

fn voxelize_part(name: &str, tris: &[Tri], g: &Grid) -> R<PartFill> {
    let (blo, bhi) = bbox(tris);
    let (i0, i1) = g.range(0, blo[0], bhi[0]);
    let (j0, j1) = g.range(1, blo[1], bhi[1]);
    let (k0, k1) = g.range(2, blo[2], bhi[2]);
    let (nx, ny, nz) = (i1 - i0, j1 - j0, k1 - k0);
    let mut fill = vec![0.0; nx * ny * nz];
    if nx == 0 || ny == 0 || nz == 0 {
        return Ok(PartFill {
            lo: [i0, j0, k0],
            hi: [i1, j1, k1],
            fill,
        });
    }
    let b = Buckets::new(
        tris,
        2,
        [
            g.origin[0] + i0 as f64 * g.d[0],
            g.origin[1] + j0 as f64 * g.d[1],
        ],
        [g.d[0], g.d[1]],
        [nx, ny],
    );
    let w = 1.0 / (SUB * SUB) as f64;
    for j in j0..j1 {
        for i in i0..i1 {
            for sb in 0..SUB {
                for sa in 0..SUB {
                    let x = g.origin[0] + (i as f64 + (sa as f64 + 0.5) / SUB as f64) * g.d[0];
                    let y = g.origin[1] + (j as f64 + (sb as f64 + 0.5) / SUB as f64) * g.d[1];
                    let hits = b.crossings(name, [x, y])?;
                    for &[z0, z1] in hits.as_chunks::<2>().0 {
                        let (ka, kb) = g.range(2, z0, z1);
                        for k in ka.max(k0)..kb.min(k1) {
                            let vz0 = g.origin[2] + k as f64 * g.d[2];
                            let vz1 = vz0 + g.d[2];
                            let ov = (z1.min(vz1) - z0.max(vz0)).max(0.0) / g.d[2];
                            fill[((k - k0) * ny + (j - j0)) * nx + (i - i0)] += ov * w;
                        }
                    }
                }
            }
        }
    }
    Ok(PartFill {
        lo: [i0, j0, k0],
        hi: [i1, j1, k1],
        fill,
    })
}

/// 5th-percentile solid span along each axis, from rays on the voxel
/// lattice (offset off-centre so they don't graze voxel-aligned faces).
fn thickness_p05(name: &str, tris: &[Tri], g: &Grid) -> R<[Option<f64>; 3]> {
    let (blo, bhi) = bbox(tris);
    let mut out = [None; 3];
    for (axis, slot) in out.iter_mut().enumerate() {
        let (ua, va) = ((axis + 1) % 3, (axis + 2) % 3);
        let (u0, u1) = g.range(ua, blo[ua], bhi[ua]);
        let (v0, v1) = g.range(va, blo[va], bhi[va]);
        if u1 <= u0 || v1 <= v0 {
            continue;
        }
        let b = Buckets::new(
            tris,
            axis,
            [
                g.origin[ua] + u0 as f64 * g.d[ua],
                g.origin[va] + v0 as f64 * g.d[va],
            ],
            [g.d[ua], g.d[va]],
            [u1 - u0, v1 - v0],
        );
        let mut spans = Vec::new();
        for iv in v0..v1 {
            for iu in u0..u1 {
                let pu = g.origin[ua] + (iu as f64 + 0.5137) * g.d[ua];
                let pv = g.origin[va] + (iv as f64 + 0.4861) * g.d[va];
                let hits = b.crossings(name, [pu, pv])?;
                for &[a, b] in hits.as_chunks::<2>().0 {
                    let s = b - a;
                    if s > 0.0 {
                        spans.push(s);
                    }
                }
            }
        }
        if spans.is_empty() {
            continue;
        }
        spans.sort_by(f64::total_cmp);
        *slot = Some(spans[((spans.len() - 1) as f64 * 0.05).round() as usize]);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Value plumbing
// ---------------------------------------------------------------------------

fn pv(v: &StudyValue, env: &BTreeMap<String, f64>, role: &str) -> R<ParamValue> {
    match v {
        StudyValue::Literal(x) => Ok(ParamValue::Literal(*x)),
        StudyValue::Named(n) => {
            if env.contains_key(n) {
                Ok(ParamValue::Named(n.clone()))
            } else {
                Err(ThermalExtractError::UnknownParameter {
                    name: n.clone(),
                    role: role.to_string(),
                })
            }
        }
    }
}

fn value(v: &ParamValue, env: &BTreeMap<String, f64>) -> f64 {
    match v {
        ParamValue::Literal(x) => *x,
        ParamValue::Named(n) => env.get(n).copied().unwrap_or(f64::NAN),
    }
}

fn boundary(b: &ThermalStudyBoundary, env: &BTreeMap<String, f64>, role: &str) -> R<BoundarySpec> {
    Ok(match b {
        ThermalStudyBoundary::Adiabatic => BoundarySpec::Adiabatic,
        ThermalStudyBoundary::Convection { h_w_m2k, ambient_c } => BoundarySpec::Convection {
            h_w_m2k: pv(h_w_m2k, env, &format!("{role} film coefficient"))?,
            ambient_c: pv(ambient_c, env, &format!("{role} ambient"))?,
        },
        ThermalStudyBoundary::FixedTemperature { temperature_c } => {
            BoundarySpec::FixedTemperature {
                temperature_c: pv(temperature_c, env, &format!("{role} temperature"))?,
            }
        }
    })
}

/// Thinnest bounding-box axis of a laminate, if it is clearly thinnest.
fn laminate_axis(extent: [f64; 3]) -> Option<usize> {
    let mut ax = [0usize, 1, 2];
    ax.sort_by(|&a, &b| extent[a].total_cmp(&extent[b]));
    (extent[ax[0]] <= extent[ax[1]] * (2.0 / 3.0)).then_some(ax[0])
}

fn laminate_k(axis: usize, in_plane: ParamValue, through: ParamValue) -> [ParamValue; 3] {
    let mut k = [in_plane.clone(), in_plane.clone(), in_plane];
    k[axis] = through;
    k
}

struct PartMaterial {
    k: [ParamValue; 3],
    rc: Option<ParamValue>,
    source: MaterialSource,
    table_name: Option<String>,
    laminate_axis: Option<usize>,
}

fn part_material(
    part: &str,
    key: &str,
    extent: [f64; 3],
    ov: Option<&ThermalStudyMaterial>,
    env: &BTreeMap<String, f64>,
) -> R<PartMaterial> {
    let ambiguous = || ThermalExtractError::AmbiguousLaminateAxis {
        part: part.to_string(),
        extent_mm: extent,
    };
    let table = thermal_material_for_key(key);
    if let Some(o) = ov {
        let role = |what: &str| format!("{what} of part {part:?}");
        let (k, lam) = match (&o.k_w_mk, &o.laminate) {
            (Some(k), None) => {
                let k: Vec<ParamValue> = k
                    .iter()
                    .map(|v| pv(v, env, &role("conductivity")))
                    .collect::<R<_>>()?;
                match k.as_slice() {
                    [a] => ([a.clone(), a.clone(), a.clone()], None),
                    [a, b, c] => ([a.clone(), b.clone(), c.clone()], None),
                    _ => {
                        return Err(ThermalExtractError::Invalid(format!(
                            "materials override for {part:?}: kWMk takes 1 (isotropic) or 3 \
                             (world x, y, z) values, got {}",
                            k.len()
                        )))
                    }
                }
            }
            (None, Some(l)) => {
                let ax = laminate_axis(extent).ok_or_else(ambiguous)?;
                (
                    laminate_k(
                        ax,
                        pv(&l.in_plane_w_mk, env, &role("in-plane conductivity"))?,
                        pv(
                            &l.through_plane_w_mk,
                            env,
                            &role("through-plane conductivity"),
                        )?,
                    ),
                    Some(ax),
                )
            }
            _ => {
                return Err(ThermalExtractError::Invalid(format!(
                    "materials override for {part:?}: give exactly one of `kWMk` / `laminate`"
                )))
            }
        };
        let rc = match &o.heat_capacity_j_m3k {
            Some(v) => Some(pv(v, env, &role("heat capacity"))?),
            None => table.map(|t| ParamValue::Literal(t.heat_capacity_j_m3k())),
        };
        return Ok(PartMaterial {
            k,
            rc,
            source: MaterialSource::Override,
            table_name: None,
            laminate_axis: lam,
        });
    }
    let t = table.ok_or_else(|| ThermalExtractError::UnknownMaterial {
        part: part.to_string(),
        material: key.to_string(),
        keys: table_keys(),
    })?;
    let (k, lam) = match t.conductivity {
        Conductivity::Isotropic { k_w_mk } => {
            (std::array::from_fn(|_| ParamValue::Literal(k_w_mk)), None)
        }
        Conductivity::Laminate {
            in_plane_w_mk,
            through_plane_w_mk,
        } => {
            let ax = laminate_axis(extent).ok_or_else(ambiguous)?;
            (
                laminate_k(
                    ax,
                    ParamValue::Literal(in_plane_w_mk),
                    ParamValue::Literal(through_plane_w_mk),
                ),
                Some(ax),
            )
        }
    };
    Ok(PartMaterial {
        k,
        rc: Some(ParamValue::Literal(t.heat_capacity_j_m3k())),
        source: MaterialSource::Table,
        table_name: Some(t.name.to_string()),
        laminate_axis: lam,
    })
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// [`extract_thermal_spec`] over an already-evaluated scene.
pub fn extract_thermal_spec_from_scene(
    doc: &Document,
    scene: &EvaluatedScene,
    study: &ThermalStudy,
) -> R<ExtractedThermal> {
    if study.parts.is_empty() {
        return Err(ThermalExtractError::Invalid(
            "`parts` must list at least one part to voxelize".into(),
        ));
    }
    let env: BTreeMap<String, f64> = vcad_ir::resolve_parameters(&doc.parameters)
        .map_err(|e| ThermalExtractError::Resolve(e.to_string()))?
        .into_iter()
        .collect();
    let min_vox = study.min_voxels_across.unwrap_or(2.0);
    if !(min_vox.is_finite() && min_vox > 0.0) {
        return Err(ThermalExtractError::Invalid(format!(
            "minVoxelsAcross must be > 0 (got {min_vox})"
        )));
    }

    // Selected parts → triangles.
    let mut names: Vec<String> = Vec::new();
    let mut tris: Vec<Vec<Tri>> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for name in &study.parts {
        if names.contains(name) {
            return Err(ThermalExtractError::Invalid(format!(
                "part {name:?} is listed twice in `parts`"
            )));
        }
        let si = find_part_index(doc, scene, name)?;
        let t = mesh_tris(&scene.parts[si].mesh);
        if t.is_empty() {
            return Err(ThermalExtractError::PartFailed {
                part: name.clone(),
                error: "evaluated to an empty mesh".into(),
            });
        }
        names.push(name.clone());
        keys.push(scene.parts[si].material.clone());
        tris.push(t);
    }
    let pos = |role: &str, part: &str| -> R<usize> {
        names
            .iter()
            .position(|n| n == part)
            .ok_or_else(|| ThermalExtractError::NotSelected {
                role: role.to_string(),
                part: part.to_string(),
            })
    };
    for m in &study.materials {
        pos("a materials override", &m.part)?;
    }

    // Domain + grid.
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for t in &tris {
        let (l, h) = bbox(t);
        for a in 0..3 {
            lo[a] = lo[a].min(l[a]);
            hi[a] = hi[a].max(h[a]);
        }
    }
    let g = build_grid(study, lo, hi)?;
    let nvox = g.n[0] * g.n[1] * g.n[2];
    let dv = g.d[0] * g.d[1] * g.d[2];

    // Fill fractions, combined: total fill, majority part, overlap.
    let mut total = vec![0.0_f64; nvox];
    let mut best = vec![-1_i32; nvox];
    let mut best_fill = vec![0.0_f64; nvox];
    let mut overlap: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    let mut fill_volume = vec![0.0; names.len()];
    let mut thickness = Vec::with_capacity(names.len());
    for (p, name) in names.iter().enumerate() {
        let th = thickness_p05(name, &tris[p], &g)?;
        let mut t3 = [0.0; 3];
        for a in 0..3 {
            let s = th[a].ok_or_else(|| ThermalExtractError::Unresolved {
                part: name.clone(),
                axis: AXES[a],
            })?;
            if s < min_vox * g.d[a] {
                return Err(ThermalExtractError::ThinWall {
                    part: name.clone(),
                    axis: AXES[a],
                    thickness_mm: s,
                    pitch_mm: g.d[a],
                    min_voxels: min_vox,
                });
            }
            t3[a] = s;
        }
        thickness.push(t3);

        let pf = voxelize_part(name, &tris[p], &g)?;
        for k in pf.lo[2]..pf.hi[2] {
            for j in pf.lo[1]..pf.hi[1] {
                for i in pf.lo[0]..pf.hi[0] {
                    let f = pf.get(i, j, k).min(1.0);
                    if f <= 0.0 {
                        continue;
                    }
                    let v = g.idx(i, j, k);
                    fill_volume[p] += f * dv;
                    let excess = total[v] + f - 1.0;
                    if excess > FILL_EPS && best[v] >= 0 {
                        *overlap.entry((best[v] as usize, p)).or_default() += excess * dv;
                    }
                    total[v] += f;
                    if f > best_fill[v] {
                        best_fill[v] = f;
                        best[v] = p as i32;
                    }
                }
            }
        }
    }
    // Touching faces leave no excess; anything over a thousandth of a
    // voxel is real interpenetration.
    if let Some((&(a, b), &ov)) = overlap.iter().find(|(_, &ov)| ov > 1e-3 * dv) {
        return Err(ThermalExtractError::PartsOverlap {
            a: names[a].clone(),
            b: names[b].clone(),
            overlap_mm3: ov,
        });
    }

    // Assignment: solid when the total fill reaches ½.
    let material_index: Vec<i32> = (0..nvox)
        .map(|v| {
            if total[v] >= 0.5 - FILL_EPS {
                best[v]
            } else {
                -1
            }
        })
        .collect();
    let mut counts = vec![0usize; names.len()];
    for &m in &material_index {
        if m >= 0 {
            counts[m as usize] += 1;
        }
    }

    // Materials + audit.
    let mut materials = Vec::new();
    let mut audit = Vec::new();
    for (p, name) in names.iter().enumerate() {
        let (l, h) = bbox(&tris[p]);
        let extent = [h[0] - l[0], h[1] - l[1], h[2] - l[2]];
        let mesh_mm3 = mesh_volume(&tris[p]).abs();
        let voxel_mm3 = counts[p] as f64 * dv;
        let ratio = if mesh_mm3 > 0.0 {
            voxel_mm3 / mesh_mm3
        } else {
            0.0
        };
        if !(0.5..=1.5).contains(&ratio) {
            return Err(ThermalExtractError::PoorlyResolved {
                part: name.clone(),
                voxel_mm3,
                mesh_mm3,
                ratio,
            });
        }
        let ov = study.materials.iter().find(|m| &m.part == name);
        let pm = part_material(name, &keys[p], extent, ov, &env)?;
        audit.push(VoxelizedPart {
            part: name.clone(),
            material_key: keys[p].clone(),
            material_source: pm.source,
            table_material: pm.table_name.clone(),
            k_w_mk: [
                value(&pm.k[0], &env),
                value(&pm.k[1], &env),
                value(&pm.k[2], &env),
            ],
            laminate_axis: pm.laminate_axis,
            heat_capacity_j_m3k: pm.rc.as_ref().map(|v| value(v, &env)),
            mesh_volume_mm3: mesh_mm3,
            fill_volume_mm3: fill_volume[p],
            voxels: counts[p],
            voxel_volume_mm3: voxel_mm3,
            thickness_p05_mm: thickness[p],
        });
        materials.push(MaterialSpec {
            // Convention for voxel_materials: a domain-covering box; the
            // index field, not the shape, decides membership.
            shape: domain_box(&g),
            k_w_mk: pm.k,
            heat_capacity_j_m3k: pm.rc,
        });
    }

    // Sources and sinks: the part's voxel bounding box (∩ region).
    let mut sources = Vec::new();
    for s in &study.sources {
        let role = format!("source {:?}", s.name);
        let p = pos(&role, &s.part)?;
        let shape = part_box(&g, &material_index, &names, p, s.region.as_ref(), &role)?;
        sources.push(SourceSpec {
            name: s.name.clone(),
            shape,
            power_w: pv(&s.power_w, &env, &format!("{role} power"))?,
        });
    }
    let mut fixed = Vec::new();
    for s in &study.sinks {
        let role = format!("sink on {:?}", s.part);
        let p = pos(&role, &s.part)?;
        let shape = part_box(&g, &material_index, &names, p, s.region.as_ref(), &role)?;
        fixed.push(FixedSpec {
            shape,
            temperature_c: pv(&s.temperature_c, &env, &format!("{role} temperature"))?,
        });
    }

    let exposed = boundary(&study.exposed, &env, "exposed")?;
    let domain_faces: [BoundarySpec; 6] = match &study.domain_faces {
        None => std::array::from_fn(|_| exposed.clone()),
        Some(v) => {
            if v.len() != 6 {
                return Err(ThermalExtractError::Invalid(format!(
                    "domainFaces takes 6 boundaries [-x, +x, -y, +y, -z, +z], got {}",
                    v.len()
                )));
            }
            let labels = ["-x", "+x", "-y", "+y", "-z", "+z"];
            let mut out: [BoundarySpec; 6] = std::array::from_fn(|_| BoundarySpec::Adiabatic);
            for (f, b) in v.iter().enumerate() {
                out[f] = boundary(b, &env, &format!("domain face {}", labels[f]))?;
            }
            out
        }
    };
    let reference_c = study
        .reference_c
        .as_ref()
        .map(|v| pv(v, &env, "reference temperature"))
        .transpose()?;

    let spec = ThermalSpec {
        origin_mm: g.origin.map(ParamValue::Literal),
        size_mm: g.size.map(ParamValue::Literal),
        divisions: g.n,
        materials,
        sources,
        fixed,
        domain_faces,
        exposed,
        reference_c,
        voxel_materials: Some(VoxelMaterials { material_index }),
    };
    Ok(ExtractedThermal {
        spec,
        parameters: env,
        grid: ThermalGrid {
            origin_mm: g.origin,
            size_mm: g.size,
            divisions: g.n,
            pitch_mm: g.d,
        },
        parts: audit,
    })
}

fn domain_box(g: &Grid) -> ShapeSpec {
    ShapeSpec::Box {
        min_mm: g.origin.map(ParamValue::Literal),
        size_mm: g.size.map(ParamValue::Literal),
    }
}

/// The voxel bounding box of part `p`'s voxels (centres inside `region`
/// when given), refusing a box that also holds another part's voxels.
fn part_box(
    g: &Grid,
    mi: &[i32],
    names: &[String],
    p: usize,
    region: Option<&StudyRegion>,
    role: &str,
) -> R<ShapeSpec> {
    let inside =
        |c: [f64; 3]| region.is_none_or(|r| (0..3).all(|a| c[a] >= r.min[a] && c[a] <= r.max[a]));
    let mut lo = [usize::MAX; 3];
    let mut hi = [0usize; 3];
    for k in 0..g.n[2] {
        for j in 0..g.n[1] {
            for i in 0..g.n[0] {
                if mi[g.idx(i, j, k)] == p as i32 && inside(g.center(i, j, k)) {
                    for (a, x) in [i, j, k].into_iter().enumerate() {
                        lo[a] = lo[a].min(x);
                        hi[a] = hi[a].max(x + 1);
                    }
                }
            }
        }
    }
    if lo[0] == usize::MAX {
        return Err(ThermalExtractError::EmptyRegion {
            role: role.to_string(),
            part: names[p].clone(),
        });
    }
    for k in lo[2]..hi[2] {
        for j in lo[1]..hi[1] {
            for i in lo[0]..hi[0] {
                let m = mi[g.idx(i, j, k)];
                if m >= 0 && m != p as i32 {
                    return Err(ThermalExtractError::SharedRegion {
                        role: role.to_string(),
                        part: names[p].clone(),
                        other: names[m as usize].clone(),
                        region: if region.is_some() {
                            " within its region".into()
                        } else {
                            String::new()
                        },
                    });
                }
            }
        }
    }
    Ok(ShapeSpec::Box {
        min_mm: std::array::from_fn(|a| ParamValue::Literal(g.origin[a] + lo[a] as f64 * g.d[a])),
        size_mm: std::array::from_fn(|a| ParamValue::Literal((hi[a] - lo[a]) as f64 * g.d[a])),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_tris(lo: [f64; 3], hi: [f64; 3]) -> Vec<Tri> {
        let c = |x: usize, y: usize, z: usize| {
            [
                if x == 1 { hi[0] } else { lo[0] },
                if y == 1 { hi[1] } else { lo[1] },
                if z == 1 { hi[2] } else { lo[2] },
            ]
        };
        // 12 outward-wound triangles.
        let q = |a: [f64; 3], b: [f64; 3], cc: [f64; 3], d: [f64; 3]| [[a, b, cc], [a, cc, d]];
        let mut t = Vec::new();
        t.extend(q(c(0, 0, 0), c(0, 1, 0), c(1, 1, 0), c(1, 0, 0))); // -z
        t.extend(q(c(0, 0, 1), c(1, 0, 1), c(1, 1, 1), c(0, 1, 1))); // +z
        t.extend(q(c(0, 0, 0), c(1, 0, 0), c(1, 0, 1), c(0, 0, 1))); // -y
        t.extend(q(c(0, 1, 0), c(0, 1, 1), c(1, 1, 1), c(1, 1, 0))); // +y
        t.extend(q(c(0, 0, 0), c(0, 0, 1), c(0, 1, 1), c(0, 1, 0))); // -x
        t.extend(q(c(1, 0, 0), c(1, 1, 0), c(1, 1, 1), c(1, 0, 1))); // +x
        t
    }

    #[test]
    fn ray_through_shared_diagonal_counts_once() {
        // The top/bottom faces are split along the x = y diagonal; a ray
        // exactly on it must still see exactly two crossings.
        let t = cube_tris([0.0; 3], [4.0; 3]);
        let b = Buckets::new(&t, 2, [0.0, 0.0], [4.0, 4.0], [1, 1]);
        for p in [[1.0, 1.0], [2.0, 2.0], [0.5, 3.0]] {
            let h = b.crossings("cube", p).unwrap();
            assert_eq!(h.len(), 2, "ray at {p:?}: {h:?}");
            assert_eq!(h, vec![0.0, 4.0]);
        }
    }

    #[test]
    fn ray_on_a_vertex_counts_once() {
        // Two cubes stacked share no vertices with the ray, but a single
        // cube's corner vertex projects onto (0, 0): the ray through the
        // corner sits on the boundary; it must give 0 or 2, never odd.
        let t = cube_tris([0.0; 3], [4.0; 3]);
        let b = Buckets::new(&t, 2, [-1.0, -1.0], [6.0, 6.0], [1, 1]);
        for p in [[0.0, 0.0], [4.0, 4.0], [0.0, 4.0], [4.0, 0.0], [0.0, 2.0]] {
            let h = b.crossings("cube", p).unwrap();
            assert!(h.len().is_multiple_of(2), "ray at {p:?}: {h:?}");
        }
    }

    #[test]
    fn fill_fraction_is_exact_for_grid_aligned_and_half_voxels() {
        let g = Grid {
            origin: [0.0; 3],
            size: [4.0, 4.0, 4.0],
            n: [4, 4, 4],
            d: [1.0; 3],
        };
        // Box covering x in [0.5, 3.5]: edge voxels half full in x.
        let t = cube_tris([0.5, 0.0, 0.0], [3.5, 4.0, 2.25]);
        let pf = voxelize_part("b", &t, &g).unwrap();
        assert!((pf.get(0, 1, 0) - 0.5).abs() < 1e-12);
        assert!((pf.get(1, 1, 0) - 1.0).abs() < 1e-12);
        assert!((pf.get(1, 1, 2) - 0.25).abs() < 1e-12);
        let total: f64 = pf.fill.iter().sum();
        assert!((total - 3.0 * 4.0 * 2.25).abs() < 1e-9);
    }

    #[test]
    fn laminate_axis_needs_a_clear_minimum() {
        assert_eq!(laminate_axis([50.0, 50.0, 1.6]), Some(2));
        assert_eq!(laminate_axis([1.6, 50.0, 40.0]), Some(0));
        assert_eq!(laminate_axis([10.0, 9.0, 8.0]), None);
    }

    #[test]
    fn table_keys_match_first_hit() {
        assert_eq!(
            thermal_material_for_key("Stainless 304").unwrap().name,
            "stainless steel"
        );
        assert_eq!(
            thermal_material_for_key("fr4").unwrap().name,
            "FR4 laminate (bare)"
        );
        assert!(thermal_material_for_key("unobtainium").is_none());
    }
}
