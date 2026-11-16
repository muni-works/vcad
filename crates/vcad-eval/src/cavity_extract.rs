//! Document → acoustic cavity: the vcad side of the acoustics solver seam.
//!
//! `vcad-kernel-acoustics` solves an axisymmetric [`Cavity`] — a coaxial
//! stack of cylinder segments with rigid annular obstacles carved out — and
//! knows nothing about documents. This module evaluates a `.vcad` document
//! (loon or IR), measures two of its parts' analytic BRep surfaces, and emits
//! a [`PortedBoxModel`] (the solver's own [`CavitySpec`] plus the lumped
//! quantities) — the same division of labour as `sim_extract` for the
//! particle and neutronics specs.
//!
//! Inputs are two parts:
//!
//! - the **enclosure** — the solid walls (shell, port tube, driver cutout);
//! - the **air** — the enclosed box air (cavity minus the port tube's wall
//!   and bore). Its volume is the lumped compliance volume `V_box`.
//!
//! Neither alone is enough: the air part doesn't contain the port, and the
//! enclosure alone can't say which void is "inside" once the driver cutout
//! and the port both connect it to the outside. Together they determine the
//! whole field geometry:
//!
//! 1. Every face of both parts must be a **plane perpendicular to the
//!    axis** or a **cylinder coaxial with it** (the axis is taken from the
//!    enclosure's largest cylinder). Anything else — a box face, a tilted
//!    plane, an off-axis hole, a sphere, a fillet torus — fails closed with
//!    the offending face named (index, surface kind, a point on it). Faces
//!    of that kind can only bound full annuli and full cylinder bands, so a
//!    part that passes is exactly axisymmetric: nothing is approximated.
//! 2. The cylinder radii and plane levels cut the meridian half-plane into
//!    rectangular cells; each part's cells are classified inside/outside
//!    exactly, by ray parity along the axis against the planar faces'
//!    annular extents. Cells are enclosure (E), air (A) or neither (N).
//! 3. Where air meets N, the boundary must be an **axial passage**: a disk
//!    of N cells on the axis, walled by E, running from the air to the
//!    outside. One passage is the **port**; one (optional) passage on the
//!    driver end is the **driver cutout**, which the driver plugs (the
//!    piston sits on the inner face; its cells are not fluid). Any other
//!    exposure of the air — a gap between air and wall, a side vent, an
//!    annular slot, a second port — is an error naming where it is.
//! 4. Fluid = air ∪ port bore. Per axial slab, the fluid's radial intervals
//!    give a segment (outer radius) and obstacles (the gaps — e.g. an
//!    internal port's tube wall). If the port vents through the bottom,
//!    the profile is mirrored so the mouth is the top of the stack.
//!
//! The driver piston radius comes from a named document parameter or a
//! literal (default: the cutout radius, stated in the assumptions). Air
//! temperature defaults to 20 °C (stated).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vcad_ir::Document;
use vcad_kernel::vcad_kernel_primitives::BRepSolid;
use vcad_kernel_acoustics::lumped::PortMount;
use vcad_kernel_acoustics::ported::PortedBoxModel;
use vcad_kernel_acoustics::spec::{CavitySpec, EndSpec, ObstacleSpec, ParamValue, SegmentSpec};
use vcad_kernel_geom::{CylinderSurface, Plane, Surface};
use vcad_kernel_topo::FaceId;

use crate::{evaluate_document, EvalError, EvalOptions, EvaluatedMesh, EvaluatedScene};

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn p3(p: &vcad_kernel_math::Point3) -> V3 {
    [p.x, p.y, p.z]
}
fn d3(d: &vcad_kernel_math::Dir3) -> V3 {
    let v = d.as_vec();
    [v.x, v.y, v.z]
}
fn canonical_axis(a: V3) -> V3 {
    let i = (0..3)
        .max_by(|&i, &j| a[i].abs().total_cmp(&a[j].abs()))
        .unwrap_or(2);
    if a[i] < 0.0 {
        scale(a, -1.0)
    } else {
        a
    }
}
fn line_angle(a: V3, b: V3) -> f64 {
    norm(cross(a, b)).atan2(dot(a, b).abs())
}
fn off_axis(p: V3, o: V3, a: V3) -> f64 {
    let d = sub(p, o);
    norm(sub(d, scale(a, dot(d, a))))
}
fn fmt3(p: V3) -> String {
    format!("({:.3}, {:.3}, {:.3})", p[0], p[1], p[2])
}

/// Matching tolerances. Geometry that is meant to be coaxial is so to
/// machine precision after the evaluator's transforms, so anything beyond
/// these is a modelling fact and is rejected, not snapped.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tolerance {
    /// Length tolerance, mm.
    pub length_mm: f64,
    /// Angle tolerance, radians.
    pub angle_rad: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            length_mm: 1e-6,
            angle_rad: 1e-7,
        }
    }
}

/// Which parts are the box, and the non-geometric inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CavityExtraction {
    /// The enclosure walls (part name, part id, or — when no part has that
    /// name — the material of exactly one part).
    pub enclosure: String,
    /// The enclosed box air, same selector rules.
    pub air: String,
    /// Driver piston radius, mm: a literal, or the name of a document
    /// parameter (kept named in the spec, so scrubbing it re-prices the
    /// simulation). Default: the driver cutout's radius.
    #[serde(default)]
    pub driver_radius: Option<ParamValue>,
    /// Air temperature, °C: literal or parameter name. Default 20 °C.
    #[serde(default)]
    pub temp_c: Option<ParamValue>,
    /// Matching tolerances.
    #[serde(default)]
    pub tolerance: Tolerance,
}

/// The extracted model plus everything needed to audit it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedCavity {
    /// The solver input (spec frame: axis = +z, piston at z = 0).
    pub model: PortedBoxModel,
    /// The document's resolved parameter environment (binds named spec
    /// values as-is).
    pub parameters: BTreeMap<String, f64>,
    /// World point on the axis at spec `z = 0`, mm.
    pub origin: V3,
    /// World unit axis along spec `+z` (toward the port mouth).
    pub axis: V3,
    /// Air part volume from the exact cell profile, mm³ (= `V_box`).
    pub air_volume_mm3: f64,
    /// Air part volume from its tessellated mesh, mm³ (cross-check; the
    /// polygonal tessellation reads slightly low).
    pub air_volume_mesh_mm3: f64,
    /// Driver cutout radius, mm, when the enclosure has one.
    pub driver_cutout_radius_mm: Option<f64>,
    /// Faces measured: `[enclosure, air]`.
    pub face_counts: [usize; 2],
    /// Defaults and modelling choices made during extraction.
    pub assumptions: Vec<String>,
}

/// Extraction failures; every variant names the part (and face) it is about.
#[derive(Debug, thiserror::Error)]
pub enum CavityExtractError {
    /// Document evaluation failed.
    #[error("document evaluation failed: {0}")]
    Eval(#[from] EvalError),
    /// Parameter resolution failed.
    #[error("parameter resolution failed: {0}")]
    Resolve(String),
    /// No part matched the selector.
    #[error("no part named {name:?} (and no single part of that material); parts: {available}")]
    UnknownPart {
        /// The selector.
        name: String,
        /// Selectable names.
        available: String,
    },
    /// The selector matched several parts.
    #[error("{count} parts match {name:?}; rename one so the selection is unambiguous")]
    AmbiguousPart {
        /// The selector.
        name: String,
        /// Matches.
        count: usize,
    },
    /// The part is hidden (not evaluated).
    #[error("part {0:?} is hidden; hidden parts are not evaluated, so it can't be measured")]
    HiddenPart(String),
    /// The part failed to evaluate.
    #[error("part {part:?} failed to evaluate: {error}")]
    PartFailed {
        /// Part.
        part: String,
        /// Evaluator message.
        error: String,
    },
    /// The part has no BRep.
    #[error(
        "part {0:?} has no BRep solid (mesh-only geometry has no analytic surfaces to measure)"
    )]
    NoBrep(String),
    /// A face that is not axisymmetric about the enclosure axis.
    #[error("part {part:?}, face #{face}: {detail} — the cavity solver is axisymmetric, so this part can't be represented (nothing is approximated)")]
    NonAxisymmetricFace {
        /// Part.
        part: String,
        /// Face index in the part's topology.
        face: usize,
        /// What is wrong, with a point on the face.
        detail: String,
    },
    /// A parameter the selection names is not declared.
    #[error("document has no parameter named {name:?} (needed for {role})")]
    UnknownParameter {
        /// Name.
        name: String,
        /// Role.
        role: String,
    },
    /// The geometry is axisymmetric but not a representable ported box.
    #[error("{0}")]
    NotRepresentable(String),
}

fn not_repr(s: impl Into<String>) -> CavityExtractError {
    CavityExtractError::NotRepresentable(s.into())
}

// ---------------------------------------------------------------------------
// Part lookup
// ---------------------------------------------------------------------------

struct ScenePart<'a> {
    name: String,
    brep: &'a BRepSolid,
    mesh: &'a EvaluatedMesh,
}

fn extraction_eval_options() -> EvalOptions {
    // No root cache: a cache hit returns a mesh without a BRep solid.
    EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
    }
}

fn root_names(doc: &Document, idx: usize) -> Vec<String> {
    let root = doc.roots[idx].root;
    let mut names = Vec::new();
    if let Some(n) = doc.nodes.get(&root).and_then(|n| n.name.clone()) {
        names.push(n);
    }
    if let Some(id) = doc.part_ids.as_ref().and_then(|m| m.get(&root.to_string())) {
        names.push(id.clone());
    }
    names
}

fn find_part<'a>(
    doc: &Document,
    scene: &'a EvaluatedScene,
    name: &str,
) -> Result<ScenePart<'a>, CavityExtractError> {
    let by_name: Vec<usize> = (0..doc.roots.len())
        .filter(|&i| root_names(doc, i).iter().any(|n| n == name))
        .collect();
    let matches = if by_name.is_empty() {
        (0..doc.roots.len())
            .filter(|&i| doc.roots[i].material == name)
            .collect()
    } else {
        by_name
    };
    let idx = match matches.as_slice() {
        [] => {
            let mut available: Vec<String> = (0..doc.roots.len())
                .map(|i| {
                    root_names(doc, i)
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| format!("<unnamed {}>", doc.roots[i].material))
                })
                .collect();
            available.sort();
            return Err(CavityExtractError::UnknownPart {
                name: name.to_string(),
                available: available.join(", "),
            });
        }
        [i] => *i,
        many => {
            return Err(CavityExtractError::AmbiguousPart {
                name: name.to_string(),
                count: many.len(),
            })
        }
    };
    let entry = &doc.roots[idx];
    if entry.visible == Some(false) {
        return Err(CavityExtractError::HiddenPart(name.to_string()));
    }
    if let Some(f) = scene.failures.iter().find(|f| f.node_id == entry.root) {
        return Err(CavityExtractError::PartFailed {
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
        return Err(not_repr(format!(
            "evaluated scene has {} parts for {visible} visible roots; can't map part names",
            scene.parts.len()
        )));
    }
    let scene_idx = doc.roots[..idx]
        .iter()
        .filter(|e| e.visible != Some(false))
        .count();
    let part = &scene.parts[scene_idx];
    let brep = part
        .solid
        .as_ref()
        .and_then(|s| s.as_brep())
        .ok_or_else(|| CavityExtractError::NoBrep(name.to_string()))?;
    Ok(ScenePart {
        name: name.to_string(),
        brep,
        mesh: &part.mesh,
    })
}

/// Signed volume of a closed triangle mesh, mm³.
fn mesh_volume(m: &EvaluatedMesh) -> f64 {
    let p = |i: u32| {
        let i = i as usize * 3;
        [
            m.positions[i] as f64,
            m.positions[i + 1] as f64,
            m.positions[i + 2] as f64,
        ]
    };
    (0..m.indices.len() / 3)
        .map(|t| {
            let (a, b, c) = (
                p(m.indices[3 * t]),
                p(m.indices[3 * t + 1]),
                p(m.indices[3 * t + 2]),
            );
            dot(a, cross(b, c)) / 6.0
        })
        .sum::<f64>()
}

// ---------------------------------------------------------------------------
// Face measurement
// ---------------------------------------------------------------------------

/// A planar face as an annulus `r ∈ (r_in, r_out)` at axial level `z`.
#[derive(Debug, Clone, Copy)]
struct Annulus {
    z: f64,
    r_in: f64,
    r_out: f64,
}

/// One part's faces, measured against the axis.
struct AxiPart {
    planes: Vec<Annulus>,
    radii: Vec<f64>,
    faces: usize,
}

fn surface_of(brep: &BRepSolid, face: FaceId) -> &dyn Surface {
    let f = &brep.topology.faces[face];
    brep.geometry.surfaces[f.surface_index].as_ref()
}

fn face_point(brep: &BRepSolid, face: FaceId) -> V3 {
    let topo = &brep.topology;
    let lp = topo.faces[face].outer_loop;
    topo.loop_half_edges(lp)
        .next()
        .map(|he| p3(&topo.vertices[topo.half_edges[he].origin].point))
        .unwrap_or([f64::NAN; 3])
}

/// The axis: the enclosure's largest-radius cylinder.
fn find_axis(part: &ScenePart) -> Result<(V3, V3), CavityExtractError> {
    let mut best: Option<(f64, V3, V3)> = None;
    for (fid, _) in part.brep.topology.faces.iter() {
        if let Some(c) = surface_of(part.brep, fid)
            .as_any()
            .downcast_ref::<CylinderSurface>()
        {
            if best.is_none_or(|(r, _, _)| c.radius > r) {
                best = Some((c.radius, p3(&c.center), canonical_axis(d3(&c.axis))));
            }
        }
    }
    best.map(|(_, o, a)| (o, a)).ok_or_else(|| {
        not_repr(format!(
            "enclosure part {:?} has no cylindrical face, so it has no axis; the cavity \
             solver needs a round (axisymmetric) enclosure",
            part.name
        ))
    })
}

fn measure(
    part: &ScenePart,
    o: V3,
    axis: V3,
    tol: &Tolerance,
) -> Result<AxiPart, CavityExtractError> {
    let topo = &part.brep.topology;
    let mut planes = Vec::new();
    let mut radii = Vec::new();
    let mut faces = 0;
    let bad = |idx: usize, detail: String| CavityExtractError::NonAxisymmetricFace {
        part: part.name.clone(),
        face: idx,
        detail,
    };
    for (idx, (fid, f)) in topo.faces.iter().enumerate() {
        faces += 1;
        let s = surface_of(part.brep, fid);
        let any = s.as_any();
        if let Some(c) = any.downcast_ref::<CylinderSurface>() {
            let (ca, cc) = (d3(&c.axis), p3(&c.center));
            let tilt = line_angle(ca, axis);
            let off = off_axis(cc, o, axis);
            if tilt > tol.angle_rad || off > tol.length_mm {
                return Err(bad(
                    idx,
                    format!(
                        "cylindrical face (r = {:.4} mm, through {}) is not coaxial with the \
                         enclosure axis (offset {off:.4} mm, tilt {tilt:.2e} rad)",
                        c.radius,
                        fmt3(face_point(part.brep, fid))
                    ),
                ));
            }
            radii.push(c.radius);
        } else if let Some(p) = any.downcast_ref::<Plane>() {
            let n = d3(&p.normal_dir);
            let tilt = line_angle(n, axis);
            if tilt > tol.angle_rad {
                return Err(bad(
                    idx,
                    format!(
                        "planar face at {} with normal {} is not perpendicular to the axis",
                        fmt3(face_point(part.brep, fid)),
                        fmt3(n)
                    ),
                ));
            }
            let z = dot(sub(p3(&p.origin), o), axis);
            // Loop radii: every vertex of a loop must sit on one circle
            // about the axis.
            let mut loop_r = Vec::new();
            for lp in std::iter::once(f.outer_loop).chain(f.inner_loops.iter().copied()) {
                let rs: Vec<f64> = topo
                    .loop_half_edges(lp)
                    .map(|he| {
                        off_axis(
                            p3(&topo.vertices[topo.half_edges[he].origin].point),
                            o,
                            axis,
                        )
                    })
                    .collect();
                let (lo, hi) = rs
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &r| {
                        (a.min(r), b.max(r))
                    });
                if rs.is_empty() || hi - lo > tol.length_mm.max(1e-9 * hi) {
                    return Err(bad(
                        idx,
                        format!(
                            "planar face at {} has a boundary loop that is not a circle about \
                             the axis (vertex radii {lo:.4}…{hi:.4} mm)",
                            fmt3(face_point(part.brep, fid))
                        ),
                    ));
                }
                loop_r.push(0.5 * (lo + hi));
            }
            let (r_in, r_out) = match loop_r.as_slice() {
                [r] => (0.0, *r),
                [a, b] => (a.min(*b), a.max(*b)),
                _ => {
                    return Err(bad(
                        idx,
                        format!(
                            "planar face at {} has {} holes; an axisymmetric face is a disk or \
                             an annulus",
                            fmt3(face_point(part.brep, fid)),
                            loop_r.len() - 1
                        ),
                    ))
                }
            };
            planes.push(Annulus { z, r_in, r_out });
        } else {
            return Err(bad(
                idx,
                format!(
                    "{} surface at {} (only planes perpendicular to the axis and coaxial \
                     cylinders are axisymmetric stacks)",
                    format!("{:?}", s.surface_type()).to_lowercase(),
                    fmt3(face_point(part.brep, fid))
                ),
            ));
        }
    }
    Ok(AxiPart {
        planes,
        radii,
        faces,
    })
}

/// Sorted, de-duplicated (within `tol`) values.
fn cluster(mut v: Vec<f64>, tol: f64) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    let mut out: Vec<f64> = Vec::new();
    for x in v {
        match out.last() {
            Some(&l) if (x - l).abs() <= tol => {}
            _ => out.push(x),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Cell profile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Enclosure,
    Air,
    Neither,
}

struct Grid {
    /// Ring edges: `r[k]..r[k+1]` is ring k. `r[0] = 0`; the last ring is
    /// a sentinel beyond everything.
    r: Vec<f64>,
    /// Slab edges, with sentinel slabs below and above.
    z: Vec<f64>,
    cells: Vec<Cell>,
}

impl Grid {
    fn nk(&self) -> usize {
        self.r.len() - 1
    }
    fn nm(&self) -> usize {
        self.z.len() - 1
    }
    fn at(&self, k: usize, m: usize) -> Cell {
        self.cells[m * self.nk() + k]
    }
    fn set(&mut self, k: usize, m: usize, c: Cell) {
        let nk = self.nk();
        self.cells[m * nk + k] = c;
    }
    fn ring_area(&self, k: usize) -> f64 {
        std::f64::consts::PI * (self.r[k + 1].powi(2) - self.r[k].powi(2))
    }
    fn cell_volume(&self, k: usize, m: usize) -> f64 {
        self.ring_area(k) * (self.z[m + 1] - self.z[m])
    }
}

/// Inside test for a cell centre: parity of planar faces above it whose
/// annulus covers its radius.
fn inside(planes: &[Annulus], r: f64, z: f64) -> bool {
    planes
        .iter()
        .filter(|p| p.z > z && r > p.r_in && r < p.r_out)
        .count()
        % 2
        == 1
}

/// An axial interface run between an air cell and an N cell: at level
/// `z[level]`, rings `k0..k1`, N on the `+z` side when `n_above`.
#[derive(Debug, Clone, Copy)]
struct Run {
    level: usize,
    k0: usize,
    k1: usize,
    n_above: bool,
}

/// An axial passage: a disk of N cells on the axis, walled by enclosure,
/// leading from the air at `z[contact]` to the outside at `z[mouth]`.
#[derive(Debug, Clone, Copy)]
struct Passage {
    contact: usize,
    mouth: usize,
    /// Rings `0..k1` are the bore; `r[k1]` is the bore radius.
    k1: usize,
    up: bool,
}

/// Walk an interface run away from the air; `None` if it is not a walled
/// axial passage to the outside.
fn trace_passage(g: &Grid, run: &Run) -> Option<Passage> {
    if run.k0 != 0 || run.k1 >= g.nk() {
        return None;
    }
    let k1 = run.k1;
    let mut m = if run.n_above {
        run.level
    } else {
        run.level - 1
    };
    loop {
        let bore_n = (0..k1).all(|k| g.at(k, m) == Cell::Neither);
        if !bore_n {
            return None;
        }
        if g.at(k1, m) != Cell::Enclosure {
            // The wall stopped: this slab is already outside — unless it is
            // the very first slab (no wall at all → not a passage).
            let first = if run.n_above {
                run.level
            } else {
                run.level - 1
            };
            if m == first {
                return None;
            }
            let mouth = if run.n_above { m } else { m + 1 };
            return Some(Passage {
                contact: run.level,
                mouth,
                k1,
                up: run.n_above,
            });
        }
        if run.n_above {
            m += 1;
            if m >= g.nm() {
                return None;
            }
        } else {
            if m == 0 {
                return None;
            }
            m -= 1;
        }
    }
}

/// Extract a ported-box model from `doc`.
pub fn extract_cavity(
    doc: &Document,
    sel: &CavityExtraction,
) -> Result<ExtractedCavity, CavityExtractError> {
    let env: BTreeMap<String, f64> = vcad_ir::resolve_parameters(&doc.parameters)
        .map_err(|e| CavityExtractError::Resolve(e.to_string()))?
        .into_iter()
        .collect();
    let scene = evaluate_document(doc, &extraction_eval_options())?;
    let encl = find_part(doc, &scene, &sel.enclosure)?;
    let air = find_part(doc, &scene, &sel.air)?;
    let tol = sel.tolerance;
    let (o, axis) = find_axis(&encl)?;
    let pe = measure(&encl, o, axis, &tol)?;
    let pa = measure(&air, o, axis, &tol)?;

    // Cell edges.
    let mut rs: Vec<f64> = vec![0.0];
    for p in [&pe, &pa] {
        rs.extend(p.radii.iter().copied());
        for a in &p.planes {
            rs.push(a.r_out);
            if a.r_in > 0.0 {
                rs.push(a.r_in);
            }
        }
    }
    let rs = cluster(rs, tol.length_mm.max(1e-9));
    let zs = cluster(
        pe.planes.iter().chain(&pa.planes).map(|a| a.z).collect(),
        tol.length_mm.max(1e-9),
    );
    if zs.len() < 2 || rs.len() < 2 {
        return Err(not_repr("enclosure/air have no axial extent"));
    }
    let r_max = *rs.last().unwrap_or(&0.0);
    let span = zs[zs.len() - 1] - zs[0];
    let margin = 0.1 * (r_max + span) + 1.0;
    let mut r = rs.clone();
    r.push(r_max + margin);
    let mut z = vec![zs[0] - margin];
    z.extend(zs.iter().copied());
    z.push(zs[zs.len() - 1] + margin);
    let nk = r.len() - 1;
    let nm = z.len() - 1;
    let mut g = Grid {
        r,
        z,
        cells: vec![Cell::Neither; nk * nm],
    };
    for m in 0..nm {
        for k in 0..nk {
            let (rc, zc) = (0.5 * (g.r[k] + g.r[k + 1]), 0.5 * (g.z[m] + g.z[m + 1]));
            let (e, a) = (inside(&pe.planes, rc, zc), inside(&pa.planes, rc, zc));
            let c = match (e, a) {
                (true, true) => {
                    return Err(not_repr(format!(
                        "enclosure {:?} and air {:?} overlap at r ∈ [{:.3}, {:.3}] mm, \
                         z ∈ [{:.3}, {:.3}] mm (axial position from the enclosure axis \
                         origin); the air must be the void inside the walls",
                        encl.name,
                        air.name,
                        g.r[k],
                        g.r[k + 1],
                        g.z[m],
                        g.z[m + 1]
                    )))
                }
                (true, false) => Cell::Enclosure,
                (false, true) => Cell::Air,
                (false, false) => Cell::Neither,
            };
            g.set(k, m, c);
        }
    }

    // Air volume: exact profile vs. tessellated mesh.
    let mut air_volume = 0.0;
    let mut air_cells = Vec::new();
    for m in 0..nm {
        for k in 0..nk {
            if g.at(k, m) == Cell::Air {
                air_volume += g.cell_volume(k, m);
                air_cells.push((k, m));
            }
        }
    }
    if air_cells.is_empty() {
        return Err(not_repr(format!("air part {:?} has no volume", air.name)));
    }
    let air_volume_mesh = mesh_volume(air.mesh).abs();
    if (air_volume - air_volume_mesh).abs() > 0.03 * air_volume {
        return Err(not_repr(format!(
            "air part {:?}: profile volume {air_volume:.1} mm³ disagrees with its mesh \
             volume {air_volume_mesh:.1} mm³ by more than 3% — the BRep is not a closed \
             axisymmetric solid",
            air.name
        )));
    }
    // Air connectivity (4-connected).
    {
        let mut seen = vec![false; nk * nm];
        let mut stack = vec![air_cells[0]];
        seen[air_cells[0].1 * nk + air_cells[0].0] = true;
        let mut count = 0;
        while let Some((k, m)) = stack.pop() {
            count += 1;
            let mut nb = Vec::with_capacity(4);
            if k > 0 {
                nb.push((k - 1, m));
            }
            if k + 1 < nk {
                nb.push((k + 1, m));
            }
            if m > 0 {
                nb.push((k, m - 1));
            }
            if m + 1 < nm {
                nb.push((k, m + 1));
            }
            for (a, b) in nb {
                if !seen[b * nk + a] && g.at(a, b) == Cell::Air {
                    seen[b * nk + a] = true;
                    stack.push((a, b));
                }
            }
        }
        if count != air_cells.len() {
            return Err(not_repr(format!(
                "air part {:?} is in disconnected pieces; the box air must be one region",
                air.name
            )));
        }
    }

    // Air ↔ N interfaces. Radial exposure is never representable.
    for m in 0..nm {
        for k in 0..nk - 1 {
            let (a, b) = (g.at(k, m), g.at(k + 1, m));
            if matches!(
                (a, b),
                (Cell::Air, Cell::Neither) | (Cell::Neither, Cell::Air)
            ) {
                return Err(not_repr(format!(
                    "air part {:?} meets open space sideways at r = {:.3} mm, z ∈ [{:.3}, \
                     {:.3}] mm — a gap between the air and the enclosure wall, a side vent or \
                     an annular slot; only coaxial axial ports are representable",
                    air.name,
                    g.r[k + 1],
                    g.z[m],
                    g.z[m + 1]
                )));
            }
        }
    }
    let mut runs: Vec<Run> = Vec::new();
    for level in 1..nm {
        let mut k = 0;
        while k < nk {
            let (below, above) = (g.at(k, level - 1), g.at(k, level));
            let n_above = match (below, above) {
                (Cell::Air, Cell::Neither) => Some(true),
                (Cell::Neither, Cell::Air) => Some(false),
                _ => None,
            };
            let Some(n_above) = n_above else {
                k += 1;
                continue;
            };
            let k0 = k;
            while k < nk && {
                let (b, a) = (g.at(k, level - 1), g.at(k, level));
                if n_above {
                    b == Cell::Air && a == Cell::Neither
                } else {
                    b == Cell::Neither && a == Cell::Air
                }
            } {
                k += 1;
            }
            runs.push(Run {
                level,
                k0,
                k1: k,
                n_above,
            });
        }
    }
    let mut passages = Vec::new();
    for run in &runs {
        match trace_passage(&g, run) {
            Some(p) => passages.push(p),
            None => {
                return Err(not_repr(format!(
                    "air part {:?} is exposed at z = {:.3} mm over r ∈ [{:.3}, {:.3}] mm to a \
                     region that is neither enclosure nor a walled coaxial passage to the \
                     outside (an annular slot, an unwalled opening, or a gap); not \
                     representable as a ported box",
                    air.name, g.z[run.level], g.r[run.k0], g.r[run.k1]
                )))
            }
        }
    }
    // Air extent in slabs.
    let a_m0 = air_cells.iter().map(|c| c.1).min().unwrap_or(0);
    let a_m1 = air_cells.iter().map(|c| c.1).max().unwrap_or(0) + 1;
    // A driver-cutout candidate: a passage on an outer end face of the air,
    // venting away from it.
    let is_end = |p: &Passage| (p.up && p.contact == a_m1) || (!p.up && p.contact == a_m0);
    let (driver, port) = match passages.as_slice() {
        [] => {
            return Err(not_repr(format!(
                "air part {:?} is sealed: no port passage leads from it to the outside",
                air.name
            )))
        }
        [p] => (None, *p),
        [a, b] => match (is_end(a) && a.up != b.up, is_end(b) && a.up != b.up) {
            (true, false) => (Some(*a), *b),
            (false, true) => (Some(*b), *a),
            (true, true) => {
                // Both are flush end openings on opposite ends: the wider
                // one is the driver cutout (a port narrower than its driver
                // is the bass-reflex norm), but say so.
                if g.r[a.k1] >= g.r[b.k1] {
                    (Some(*a), *b)
                } else {
                    (Some(*b), *a)
                }
            }
            _ => {
                return Err(not_repr(format!(
                    "air part {:?} has two openings that can't be told apart as driver \
                     cutout + port (a driver cutout is a flush opening on one end face; \
                     the port vents from the other end)",
                    air.name
                )))
            }
        },
        many => {
            return Err(not_repr(format!(
                "air part {:?} has {} openings to the outside; the ported-box model holds \
                 one port (and one driver cutout)",
                air.name,
                many.len()
            )))
        }
    };
    let mut assumptions = Vec::new();
    if let (Some(d), [_, _]) = (driver, passages.as_slice()) {
        if is_end(&port) {
            assumptions.push(format!(
                "Two flush end openings: the wider (r = {:.3} mm) is taken as the driver \
                 cutout, the narrower as the port.",
                g.r[d.k1]
            ));
        }
    }

    // Spec frame: s = +z toward the mouth, s = 0 at the driver face.
    let up = port.up;
    let (s_lo_level, s_hi_level) = if up {
        (a_m0, port.mouth)
    } else {
        (port.mouth, a_m1)
    };
    if (up && port.mouth < a_m1) || (!up && port.mouth > a_m0) {
        return Err(not_repr(
            "port mouth is recessed inside the box's axial extent; the open mouth must be \
             the end of the stack",
        ));
    }
    if let Some(d) = driver {
        if d.up == up {
            return Err(not_repr(
                "driver cutout and port open on the same end; the axisymmetric model puts \
                 the piston and the port mouth on opposite ends",
            ));
        }
    }
    // Fluid cells: air + port bore.
    let port_m: Vec<usize> = if up {
        (port.contact..port.mouth).collect()
    } else {
        (port.mouth..port.contact).collect()
    };
    let fluid = |k: usize, m: usize| -> bool {
        g.at(k, m) == Cell::Air || (k < port.k1 && port_m.contains(&m))
    };
    let s_of = |level: usize| -> f64 {
        if up {
            g.z[level] - g.z[s_lo_level]
        } else {
            g.z[s_hi_level] - g.z[level]
        }
    };
    // Slabs in spec order (ascending s).
    let slabs: Vec<usize> = if up {
        (s_lo_level..s_hi_level).collect()
    } else {
        (s_lo_level..s_hi_level).rev().collect()
    };
    // Per slab: fluid radial intervals [k_start, k_end).
    struct SlabGeom {
        s0: f64,
        s1: f64,
        outer: f64,
        gaps: Vec<(f64, f64)>,
    }
    let mut geo: Vec<SlabGeom> = Vec::new();
    for &m in &slabs {
        let mut intervals: Vec<(usize, usize)> = Vec::new();
        let mut k = 0;
        while k < nk {
            if fluid(k, m) {
                let k0 = k;
                while k < nk && fluid(k, m) {
                    k += 1;
                }
                intervals.push((k0, k));
            } else {
                k += 1;
            }
        }
        let (Some(first), Some(last)) = (intervals.first(), intervals.last()) else {
            return Err(not_repr(format!(
                "no fluid in the slab z ∈ [{:.3}, {:.3}] mm between the driver face and the \
                 port mouth; the cavity must be one stack",
                g.z[m],
                g.z[m + 1]
            )));
        };
        let mut gaps = Vec::new();
        if first.0 > 0 {
            gaps.push((0.0, g.r[first.0]));
        }
        for w in intervals.windows(2) {
            gaps.push((g.r[w[0].1], g.r[w[1].0]));
        }
        let (a, b) = if up {
            (s_of(m), s_of(m + 1))
        } else {
            (s_of(m + 1), s_of(m))
        };
        geo.push(SlabGeom {
            s0: a,
            s1: b,
            outer: g.r[last.1],
            gaps,
        });
    }
    // Merge slabs into segments (equal outer radius) and obstacles (equal
    // radii, contiguous in s).
    let mut segments: Vec<(f64, f64, f64)> = Vec::new();
    let mut obstacles: Vec<(f64, f64, f64, f64)> = Vec::new(); // (r_in, r_out, s0, s1)
    for sg in &geo {
        match segments.last_mut() {
            Some(last) if (last.2 - sg.outer).abs() <= tol.length_mm => last.1 = sg.s1,
            _ => segments.push((sg.s0, sg.s1, sg.outer)),
        }
        for &(ri, ro) in &sg.gaps {
            if let Some(o) = obstacles.iter_mut().find(|o| {
                (o.0 - ri).abs() <= tol.length_mm
                    && (o.1 - ro).abs() <= tol.length_mm
                    && (o.3 - sg.s0).abs() <= tol.length_mm
            }) {
                o.3 = sg.s1;
            } else {
                obstacles.push((ri, ro, sg.s0, sg.s1));
            }
        }
    }

    // Driver piston.
    let cutout_r = driver.map(|d| g.r[d.k1]);
    let bottom_r = geo[0].outer;
    if geo[0].gaps.iter().any(|&(ri, _)| ri <= 0.0) {
        return Err(not_repr(
            "the driver face has a solid post on the axis; the piston must be a disk on the \
             axis",
        ));
    }
    let driver_radius = match &sel.driver_radius {
        Some(ParamValue::Named(n)) => {
            if !env.contains_key(n) {
                return Err(CavityExtractError::UnknownParameter {
                    name: n.clone(),
                    role: "driver piston radius".into(),
                });
            }
            ParamValue::Named(n.clone())
        }
        Some(ParamValue::Literal(v)) => ParamValue::Literal(*v),
        None => match cutout_r {
            Some(r) => {
                assumptions.push(format!(
                    "Driver piston radius not given: using the driver cutout radius {r:.3} mm \
                     (a real cone's effective radiating radius is smaller)."
                ));
                ParamValue::Literal(r)
            }
            None => {
                return Err(not_repr(
                    "no driver cutout found and no driver_radius given; name the piston \
                     radius (a literal or a document parameter)",
                ))
            }
        },
    };
    let piston_r = match &driver_radius {
        ParamValue::Literal(v) => *v,
        ParamValue::Named(n) => env[n],
    };
    if piston_r.is_nan() || piston_r <= 0.0 {
        return Err(not_repr(format!(
            "driver piston radius {piston_r} mm is not positive"
        )));
    }
    if piston_r > bottom_r + tol.length_mm {
        return Err(not_repr(format!(
            "driver piston radius {piston_r:.3} mm exceeds the box radius {bottom_r:.3} mm at \
             the driver face"
        )));
    }
    if let Some(cr) = cutout_r {
        if piston_r > cr + tol.length_mm {
            return Err(not_repr(format!(
                "driver piston radius {piston_r:.3} mm exceeds the driver cutout radius \
                 {cr:.3} mm"
            )));
        }
        assumptions.push(format!(
            "The driver plugs its cutout (r = {cr:.3} mm): the cutout is not fluid, and the \
             piston sits flush with the inside face of the wall."
        ));
    }
    let temp_c = match &sel.temp_c {
        Some(ParamValue::Named(n)) => {
            if !env.contains_key(n) {
                return Err(CavityExtractError::UnknownParameter {
                    name: n.clone(),
                    role: "air temperature".into(),
                });
            }
            ParamValue::Named(n.clone())
        }
        Some(v) => v.clone(),
        None => {
            assumptions.push("Air at 20 °C (temp_c not given).".to_string());
            ParamValue::Literal(20.0)
        }
    };

    // Port.
    let port_r = g.r[port.k1];
    let port_len = (g.z[port.mouth] - g.z[port.contact]).abs();
    let mount = if (up && port.contact < a_m1) || (!up && port.contact > a_m0) {
        PortMount::Internal
    } else {
        PortMount::Flush
    };

    // Probe: in the air, on the axis if possible, in the slab nearest the
    // driver face.
    let probe = slabs
        .iter()
        .flat_map(|&m| (0..nk).map(move |k| (k, m)))
        .find(|&(k, m)| g.at(k, m) == Cell::Air)
        .map(|(k, m)| {
            let r = if k == 0 {
                0.0
            } else {
                0.5 * (g.r[k] + g.r[k + 1])
            };
            [r, 0.5 * (s_of(m) + s_of(m + 1))]
        })
        .unwrap_or([0.0, 0.0]);

    // Grid: half the smallest feature, so every obstacle wall holds a node.
    let mut feats: Vec<f64> = Vec::new();
    for sg in &geo {
        feats.push(sg.s1 - sg.s0);
        feats.push(sg.outer);
        for &(ri, ro) in &sg.gaps {
            feats.push(ro - ri);
        }
    }
    feats.push(port_r);
    let min_feat = feats
        .into_iter()
        .filter(|f| *f > tol.length_mm)
        .fold(f64::INFINITY, f64::min);
    let grid_mm = (0.5 * min_feat).clamp(0.25, 5.0);

    let lit = |v: f64| ParamValue::Literal(v);
    let spec = CavitySpec {
        segments: segments
            .iter()
            .map(|&(a, b, rr)| SegmentSpec {
                z0_mm: lit(a),
                z1_mm: lit(b),
                radius_mm: lit(rr),
            })
            .collect(),
        obstacles: obstacles
            .iter()
            .map(|&(ri, ro, a, b)| ObstacleSpec {
                r_in_mm: lit(ri),
                r_out_mm: lit(ro),
                z0_mm: lit(a),
                z1_mm: lit(b),
            })
            .collect(),
        bottom: EndSpec::Piston {
            radius_mm: driver_radius,
        },
        top: EndSpec::Open,
        temp_c,
    };
    let s0_world = if up { g.z[s_lo_level] } else { g.z[s_hi_level] };
    let spec_axis = if up { axis } else { scale(axis, -1.0) };
    if !up {
        assumptions.push(
            "The port vents through the bottom: the profile is mirrored so the spec's +z \
             runs from the driver face toward the port mouth."
                .to_string(),
        );
    }
    Ok(ExtractedCavity {
        model: PortedBoxModel {
            spec,
            box_volume_mm3: air_volume,
            port_radius_mm: port_r,
            port_length_mm: port_len,
            mount,
            probe_mm: probe,
            grid_mm,
        },
        parameters: env,
        origin: [
            o[0] + axis[0] * s0_world,
            o[1] + axis[1] * s0_world,
            o[2] + axis[2] * s0_world,
        ],
        axis: spec_axis,
        air_volume_mm3: air_volume,
        air_volume_mesh_mm3: air_volume_mesh,
        driver_cutout_radius_mm: cutout_r,
        face_counts: [pe.faces, pa.faces],
        assumptions,
    })
}
