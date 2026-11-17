//! Document → simulation-spec extraction: the vcad side of the solver seams.
//!
//! `vcad-kernel-particle` ([`DeviceSpec`]) and `vcad-kernel-neutronics`
//! ([`ShieldSpec`]) each define a serde schema with named parameters and
//! deliberately leave BRep extraction to "the vcad side of the seam". This
//! module is that side, with the same division of labor as
//! [`crate::diff::document_parameter_gradient`]: the solver crates know
//! nothing about documents; `vcad-eval` evaluates the document (loon or IR)
//! to BRep solids with the same resolve-and-walk every other consumer uses,
//! measures the analytic surfaces, and emits the solver's own schema.
//!
//! Two layers:
//!
//! 1. **Geometry features**, spec-agnostic: [`wire_features`] (toroidal
//!    wire rings in any orientation), [`chamber_cavity`] (an axisymmetric
//!    closed-can cavity) and [`shell_feature`] (a concentric spherical
//!    shell). A later 3D wire-segment electrode spec consumes
//!    [`WireFeature`] directly — the axisymmetric coaxiality check lives in
//!    the projection below, not here — and grows the enum (arcs, straight
//!    segments) rather than replacing it.
//! 2. **Spec projections**: [`extract_device_spec`] and
//!    [`extract_shield_spec`].
//!
//! Everything is **fail-closed**. A face the target spec can't represent, a
//! ring off the chamber axis, a shell off-center, a parameter the document
//! doesn't declare, or a selected part without analytic BRep geometry is an
//! error naming the offending part. Nothing is snapped, fitted or
//! approximated silently.
//!
//! Geometry comes out as **literals measured from the evaluated BRep** (the
//! solids the drawings are made from); electrical quantities (potentials,
//! ampere-turns, source rate) are **named document parameters**, carried
//! into the spec as [`ParamValue::Named`](vcad_kernel_particle::spec::ParamValue)
//! and returned with the document's resolved parameter environment, so
//! `spec.resolve(&extracted.parameters)` runs as-is and scrubbing a
//! parameter in the document re-prices the simulation.
//!
//! Topology notes (measured, not assumed): the evaluator's shell grouping
//! is not a connectivity guarantee — a union of disjoint tori, or a sphere
//! minus a concentric sphere, lands in one shell. So rings are delimited by
//! half-edge adjacency, and inward faces are recognised by face orientation
//! on surfaces with a canonical outward normal (cylinders, spheres).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vcad_ir::Document;
use vcad_kernel::vcad_kernel_primitives::BRepSolid;
use vcad_kernel_geom::{CylinderSurface, Plane, SphereSurface, Surface, TorusSurface};
use vcad_kernel_neutronics::materials;
use vcad_kernel_neutronics::spec::{
    DetectorSpec, LayerSpec, ParamValue as NParam, RunSpec, ShieldSpec, SourceSpec,
};
use vcad_kernel_particle::spec::{DeviceSpec, ParamValue as PParam, RingSpec};
use vcad_kernel_topo::{FaceId, Orientation};

use crate::{evaluate_document, EvalError, EvalOptions, EvaluatedScene};

pub mod em;
pub mod tolerance;

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
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
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

/// Orient an axis canonically: the component of largest magnitude is
/// positive (so a Z-parallel axis is always `+Z`, and ring ampere-turn signs
/// keep their "CCW viewed from +Z" meaning).
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

/// Angle (radians, in `[0, π/2]`) between two lines with unit directions.
fn line_angle(a: V3, b: V3) -> f64 {
    norm(cross(a, b)).atan2(dot(a, b).abs())
}

/// Distance from `p` to the line through `o` along unit `a`.
fn off_axis(p: V3, o: V3, a: V3) -> f64 {
    let d = sub(p, o);
    norm(sub(d, scale(a, dot(d, a))))
}

/// Matching tolerances. Defaults are numerical-noise tight: geometry that
/// is *meant* to be coaxial/concentric is so to machine precision after the
/// evaluator's transform chain, so anything larger is a modelling fact, not
/// noise, and is rejected.
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

/// Extraction failures. Every variant names the part it is about.
#[derive(Debug, thiserror::Error)]
pub enum SimExtractError {
    /// Document evaluation failed.
    #[error("document evaluation failed: {0}")]
    Eval(#[from] EvalError),
    /// Parameter resolution failed (cycle, malformed formula, …).
    #[error("parameter resolution failed: {0}")]
    Resolve(String),
    /// No scene part matched the selection.
    #[error("no part named {name:?} in the document (parts: {available})")]
    UnknownPart {
        /// The selection.
        name: String,
        /// Comma-separated selectable part names.
        available: String,
    },
    /// More than one scene part matched the selection.
    #[error("{count} parts are named {name:?}; rename one so the selection is unambiguous")]
    AmbiguousPart {
        /// The selection.
        name: String,
        /// Number of matches.
        count: usize,
    },
    /// The selected part is hidden, so it is not evaluated.
    #[error("part {0:?} is hidden; hidden parts are not evaluated, so it can't be measured")]
    HiddenPart(String),
    /// The selected part failed to evaluate.
    #[error("part {part:?} failed to evaluate: {error}")]
    PartFailed {
        /// Part name.
        part: String,
        /// Evaluator message.
        error: String,
    },
    /// The selected part evaluated to a mesh, not a BRep.
    #[error(
        "part {0:?} has no BRep solid (mesh-only or imported geometry has no analytic surfaces to measure)"
    )]
    NoBrep(String),
    /// A named parameter the selection refers to is not declared.
    #[error("document has no parameter named {name:?} (needed for {role})")]
    UnknownParameter {
        /// The parameter name.
        name: String,
        /// What it was supposed to drive.
        role: String,
    },
    /// The part holds geometry the target spec can't represent.
    #[error("part {part:?}: {detail}")]
    NotRepresentable {
        /// Part name.
        part: String,
        /// What is unrepresentable and why.
        detail: String,
    },
    /// A wire ring is not coaxial with the chamber.
    #[error(
        "part {part:?}: wire ring (R = {ring_radius_mm} mm) is not coaxial with the chamber axis \
         ({detail}); the axisymmetric DeviceSpec only holds coaxial rings — globe/meridian \
         grids need the 3D electrode spec"
    )]
    NonCoaxialRing {
        /// Part name.
        part: String,
        /// Ring major radius, mm.
        ring_radius_mm: f64,
        /// Measured misalignment.
        detail: String,
    },
    /// A wire ring does not fit inside the chamber cavity.
    #[error("part {part:?}: wire ring does not fit inside the chamber cavity ({detail})")]
    RingOutsideChamber {
        /// Part name.
        part: String,
        /// Which bound it crosses.
        detail: String,
    },
    /// Two wire rings (possibly from different parts) overlap.
    #[error("wire rings in parts {a:?} and {b:?} overlap ({detail})")]
    RingsOverlap {
        /// First ring's part.
        a: String,
        /// Second ring's part.
        b: String,
        /// Measured overlap.
        detail: String,
    },
    /// Two shield shells (or a shell and the source point) are not concentric.
    #[error(
        "shield part {part:?} is not concentric with {reference} (centre offset {offset_mm} mm)"
    )]
    NonConcentric {
        /// Part name.
        part: String,
        /// What it was compared against.
        reference: String,
        /// Centre offset, mm.
        offset_mm: f64,
    },
    /// Two shield shells overlap radially.
    #[error("shield parts {a:?} and {b:?} overlap radially ({detail})")]
    ShellOverlap {
        /// Inner part.
        a: String,
        /// Outer part.
        b: String,
        /// Radii.
        detail: String,
    },
    /// A shield material is not in the neutronics library.
    #[error(
        "{part}: material {material:?} is not in the neutronics library \
         (hdpe, paraffin, borated-hdpe-5, water, lead, concrete, air, void); \
         set an explicit `material` on the selection"
    )]
    UnknownMaterial {
        /// Part name (or "fill").
        part: String,
        /// The material key.
        material: String,
    },
    /// The selection itself is malformed.
    #[error("invalid selection: {0}")]
    Invalid(String),
}

fn not_repr(part: &str, detail: impl Into<String>) -> SimExtractError {
    SimExtractError::NotRepresentable {
        part: part.to_string(),
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------------
// Scene part lookup
// ---------------------------------------------------------------------------

/// A selected scene part with its BRep.
struct ScenePart<'a> {
    name: String,
    material: String,
    brep: &'a BRepSolid,
    solid: &'a vcad_kernel::Solid,
}

fn extraction_eval_options() -> EvalOptions {
    // No root cache: a cache hit returns a mesh without a BRep solid.
    EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
        on_root_ready: None,
    }
}

/// Selectable names of a scene root: its root node's name and its stable
/// part id (`partIds`), when present.
fn root_names(doc: &Document, idx: usize) -> Vec<String> {
    let root = doc.roots[idx].root;
    let mut names = Vec::new();
    if let Some(n) = doc.nodes.get(&root).and_then(|n| n.name.clone()) {
        names.push(n);
    }
    if let Some(id) = doc.part_ids.as_ref().and_then(|m| m.get(&root.to_string())) {
        names.push(id.clone());
    }
    // Clearance assertions name parts by stringified root node id.
    names.push(root.to_string());
    names
}

fn find_part<'a>(
    doc: &Document,
    scene: &'a EvaluatedScene,
    name: &str,
) -> Result<ScenePart<'a>, SimExtractError> {
    let matches: Vec<usize> = (0..doc.roots.len())
        .filter(|&i| root_names(doc, i).iter().any(|n| n == name))
        .collect();
    let idx = match matches.as_slice() {
        [] => {
            let mut available: Vec<String> = (0..doc.roots.len())
                .filter_map(|i| root_names(doc, i).into_iter().next())
                .collect();
            available.sort();
            return Err(SimExtractError::UnknownPart {
                name: name.to_string(),
                available: if available.is_empty() {
                    "none are named".to_string()
                } else {
                    available.join(", ")
                },
            });
        }
        [i] => *i,
        many => {
            return Err(SimExtractError::AmbiguousPart {
                name: name.to_string(),
                count: many.len(),
            })
        }
    };
    let entry = &doc.roots[idx];
    if entry.visible == Some(false) {
        return Err(SimExtractError::HiddenPart(name.to_string()));
    }
    if let Some(f) = scene.failures.iter().find(|f| f.node_id == entry.root) {
        return Err(SimExtractError::PartFailed {
            part: name.to_string(),
            error: f.error.clone(),
        });
    }
    // `scene.parts` holds exactly one entry per visible root, in order.
    let visible = doc
        .roots
        .iter()
        .filter(|e| e.visible != Some(false))
        .count();
    if scene.parts.len() != visible {
        return Err(SimExtractError::Invalid(format!(
            "evaluated scene has {} parts for {visible} visible roots; can't map part names",
            scene.parts.len()
        )));
    }
    let scene_idx = doc.roots[..idx]
        .iter()
        .filter(|e| e.visible != Some(false))
        .count();
    let part = &scene.parts[scene_idx];
    let solid = part
        .solid
        .as_ref()
        .filter(|s| s.as_brep().is_some())
        .ok_or_else(|| SimExtractError::NoBrep(name.to_string()))?;
    let brep = solid.as_brep().expect("filtered to BRep solids");
    Ok(ScenePart {
        name: name.to_string(),
        material: part.material.clone(),
        brep,
        solid,
    })
}

fn resolve_env(doc: &Document) -> Result<BTreeMap<String, f64>, SimExtractError> {
    let env = vcad_ir::resolve_parameters(&doc.parameters)
        .map_err(|e| SimExtractError::Resolve(e.to_string()))?;
    Ok(env.into_iter().collect())
}

fn require_param(
    env: &BTreeMap<String, f64>,
    name: &str,
    role: impl Into<String>,
) -> Result<(), SimExtractError> {
    if env.contains_key(name) {
        Ok(())
    } else {
        Err(SimExtractError::UnknownParameter {
            name: name.to_string(),
            role: role.into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Layer 1: geometry features
// ---------------------------------------------------------------------------

fn surface_of(brep: &BRepSolid, face: FaceId) -> &dyn Surface {
    let f = &brep.topology.faces[face];
    brep.geometry.surfaces[f.surface_index].as_ref()
}

fn kind_name(s: &dyn Surface) -> String {
    format!("{:?}", s.surface_type()).to_lowercase()
}

/// Faces adjacent to `face` across each of its edges. `None` entries mark an
/// open (twinless) edge.
fn neighbours(brep: &BRepSolid, face: FaceId) -> Vec<Option<FaceId>> {
    let topo = &brep.topology;
    let f = &topo.faces[face];
    let mut out = Vec::new();
    for lp in std::iter::once(f.outer_loop).chain(f.inner_loops.iter().copied()) {
        for he in topo.loop_half_edges(lp) {
            let other = topo.half_edges[he]
                .twin
                .and_then(|t| topo.half_edges[t].loop_id)
                .and_then(|l| topo.loops[l].face);
            out.push(other);
        }
    }
    out
}

/// A wire electrode feature measured from a BRep, in world coordinates.
///
/// Spec-agnostic: the axisymmetric [`DeviceSpec`] projection requires every
/// ring to be coaxial with the chamber; a 3D wire spec accepts rings in any
/// orientation and will add variants here (arcs, straight segments) for the
/// welded junctions of a globe grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireFeature {
    /// A complete (untrimmed) torus: a closed wire loop.
    Ring {
        /// Torus centre, mm.
        center: V3,
        /// Unit axis (canonically oriented, see module docs).
        axis: V3,
        /// Major radius (centre to wire centreline), mm.
        ring_radius_mm: f64,
        /// Minor radius (wire radius), mm.
        wire_radius_mm: f64,
    },
}

/// Distance from point `p` to the centreline circle of torus `t` (exact).
fn point_circle_distance(p: V3, t: &TorusSurface) -> f64 {
    let (c, a) = (p3(&t.center), d3(&t.axis));
    let d = sub(p, c);
    let h = dot(d, a);
    let rho = norm(sub(d, scale(a, h)));
    ((rho - t.major_radius).powi(2) + h * h).sqrt()
}

/// Minimum distance between two tori's centreline circles: dense sampling
/// of one circle against the exact point–circle distance to the other, then
/// golden-section refinement around the best sample.
fn ring_centreline_distance(a: &TorusSurface, b: &TorusSurface) -> f64 {
    let (c, ax) = (p3(&a.center), d3(&a.axis));
    let u = d3(&a.ref_dir);
    let u = sub(u, scale(ax, dot(u, ax)));
    let u = scale(u, 1.0 / norm(u));
    let v = cross(ax, u);
    let at = |th: f64| {
        let p = add(
            c,
            add(
                scale(u, a.major_radius * th.cos()),
                scale(v, a.major_radius * th.sin()),
            ),
        );
        point_circle_distance(p, b)
    };
    const N: usize = 1024;
    let step = std::f64::consts::TAU / N as f64;
    let (mut best_th, mut best) = (0.0, f64::INFINITY);
    for k in 0..N {
        let th = k as f64 * step;
        let d = at(th);
        if d < best {
            (best_th, best) = (th, d);
        }
    }
    let (mut lo, mut hi) = (best_th - step, best_th + step);
    let g = 0.5 * (5f64.sqrt() - 1.0);
    for _ in 0..60 {
        let (m1, m2) = (hi - g * (hi - lo), lo + g * (hi - lo));
        if at(m1) < at(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    best.min(at(0.5 * (lo + hi)))
}

fn same_torus(a: &TorusSurface, b: &TorusSurface, tol: &Tolerance) -> bool {
    (a.major_radius - b.major_radius).abs() <= tol.length_mm
        && (a.minor_radius - b.minor_radius).abs() <= tol.length_mm
        && norm(sub(p3(&a.center), p3(&b.center))) <= tol.length_mm
        && line_angle(d3(&a.axis), d3(&b.axis)) <= tol.angle_rad
}

/// Measure every wire ring in an electrode part.
///
/// Every face must lie on a torus (a stalk, feedthrough or support belongs in
/// its own part), face outward (no toroidal grooves), and be adjacent only to
/// faces of the *same* torus. A closed set of faces lying on one torus is the
/// whole torus, so that adjacency test is exactly "the ring is complete": a
/// ring trimmed by another ring (crossing wires) or by any other surface is
/// rejected rather than measured as if it were whole.
pub fn wire_features(
    part: &str,
    brep: &BRepSolid,
    tol: &Tolerance,
) -> Result<Vec<WireFeature>, SimExtractError> {
    let mut faces: Vec<(FaceId, TorusSurface)> = Vec::new();
    for (fid, f) in brep.topology.faces.iter() {
        let s = surface_of(brep, fid);
        let Some(t) = s.as_any().downcast_ref::<TorusSurface>() else {
            return Err(not_repr(
                part,
                format!(
                    "electrode part has a {} face; an electrode part may hold only toroidal \
                     wire rings (model stalks, feedthroughs and supports as separate parts)",
                    kind_name(s)
                ),
            ));
        };
        if f.orientation == Orientation::Reversed {
            return Err(not_repr(
                part,
                "electrode part has an inward-facing toroidal face (a groove, not a wire)",
            ));
        }
        faces.push((fid, t.clone()));
    }
    if faces.is_empty() {
        return Err(not_repr(part, "electrode part has no faces"));
    }
    // Class index per face: faces on the same torus share a class.
    let mut classes: Vec<TorusSurface> = Vec::new();
    let mut class_of: BTreeMap<FaceId, usize> = BTreeMap::new();
    for (fid, t) in &faces {
        let c = match classes.iter().position(|c| same_torus(c, t, tol)) {
            Some(c) => c,
            None => {
                classes.push(t.clone());
                classes.len() - 1
            }
        };
        class_of.insert(*fid, c);
    }
    for (fid, t) in &faces {
        for n in neighbours(brep, *fid) {
            let Some(n) = n else {
                return Err(not_repr(
                    part,
                    format!(
                        "wire ring (R = {} mm) has an open edge — not a closed torus",
                        t.major_radius
                    ),
                ));
            };
            if class_of.get(&n) != class_of.get(fid) {
                return Err(not_repr(
                    part,
                    format!(
                        "wire ring (R = {} mm) is trimmed by another ring (crossing or touching \
                         wires) — only complete, separate rings are representable",
                        t.major_radius
                    ),
                ));
            }
        }
    }
    // The kernel does not always trim crossing tori against each other (a
    // union of two crossing rings can come back as two whole tori), so
    // adjacency alone can't prove the rings are separate: check the wire
    // clearance geometrically too.
    for (i, a) in classes.iter().enumerate() {
        for b in &classes[i + 1..] {
            let gap = ring_centreline_distance(a, b) - (a.minor_radius + b.minor_radius);
            if gap < -tol.length_mm {
                return Err(not_repr(
                    part,
                    format!(
                        "wire rings R = {} mm and R = {} mm intersect (wires overlap by {:.3} mm) \
                         — only complete, separate rings are representable",
                        a.major_radius, b.major_radius, -gap
                    ),
                ));
            }
        }
    }
    let mut rings: Vec<WireFeature> = classes
        .iter()
        .map(|t| WireFeature::Ring {
            center: p3(&t.center),
            axis: canonical_axis(d3(&t.axis)),
            ring_radius_mm: t.major_radius,
            wire_radius_mm: t.minor_radius,
        })
        .collect();
    rings.sort_by(|a, b| {
        let (WireFeature::Ring { center: ca, .. }, WireFeature::Ring { center: cb, .. }) = (a, b);
        ca.iter()
            .zip(cb)
            .map(|(x, y)| x.total_cmp(y))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(rings)
}

/// An axisymmetric cylindrical cavity: the inside of a closed can.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cavity {
    /// Point on the axis at the cavity's mid-height, mm (the device origin).
    pub origin: V3,
    /// Unit axis (canonically oriented).
    pub axis: V3,
    /// Cavity (inner wall) radius, mm.
    pub radius_mm: f64,
    /// Cavity half-height, mm.
    pub half_height_mm: f64,
}

/// Measure the cavity of a chamber part modelled as a closed can.
///
/// The cavity wall is the part's inward-facing cylindrical faces (cylinder
/// surfaces have a canonical outward normal, so "reversed" means inward).
/// They must all share one axis line and radius — a port, stepped bore or
/// other inward feature off that cylinder is rejected, as is any inward
/// sphere/cone/torus (the device chamber is a cylinder). The caps are the
/// planes perpendicular to the axis at the wall's two ends, facing into the
/// cavity; both must exist (an open tube is not a closed chamber).
pub fn chamber_cavity(
    part: &str,
    brep: &BRepSolid,
    tol: &Tolerance,
) -> Result<Cavity, SimExtractError> {
    let topo = &brep.topology;
    let mut wall: Option<(V3, V3, f64)> = None; // (point on axis, axis, radius)
    let mut wall_faces: Vec<FaceId> = Vec::new();
    // Perpendicular planes: (signed position of the face along the axis is
    // computed once the axis is known) — collect first.
    let mut planes: Vec<(V3, V3)> = Vec::new(); // (origin, outward face normal)
    for (fid, f) in topo.faces.iter() {
        let s = surface_of(brep, fid);
        let any = s.as_any();
        let reversed = f.orientation == Orientation::Reversed;
        if let Some(c) = any.downcast_ref::<CylinderSurface>() {
            if !reversed {
                continue;
            }
            let (o, a, r) = (p3(&c.center), canonical_axis(d3(&c.axis)), c.radius);
            match wall {
                None => wall = Some((o, a, r)),
                Some((wo, wa, wr)) => {
                    if (r - wr).abs() > tol.length_mm
                        || line_angle(a, wa) > tol.angle_rad
                        || off_axis(o, wo, wa) > tol.length_mm
                    {
                        return Err(not_repr(
                            part,
                            format!(
                                "chamber has more than one inward cylindrical surface (r = {wr} mm \
                                 and r = {r} mm, or off-axis): ports and stepped bores are not \
                                 representable in the axisymmetric device"
                            ),
                        ));
                    }
                }
            }
            wall_faces.push(fid);
        } else if let Some(p) = any.downcast_ref::<Plane>() {
            let n = d3(&p.normal_dir);
            let n = if reversed { scale(n, -1.0) } else { n };
            planes.push((p3(&p.origin), n));
        } else if reversed {
            return Err(not_repr(
                part,
                format!(
                    "chamber has an inward-facing {} face; the device chamber is a closed \
                     cylinder (spherical chambers, cones and tori are not representable)",
                    kind_name(s)
                ),
            ));
        }
    }
    let Some((wo, wa, wr)) = wall else {
        return Err(not_repr(
            part,
            "chamber has no inward-facing cylindrical wall; model it as a closed can \
             (outer cylinder minus a coaxial inner cylinder)",
        ));
    };
    // Axial extent of the wall from its loop vertices.
    let (mut s_lo, mut s_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for fid in &wall_faces {
        let f = &topo.faces[*fid];
        for lp in std::iter::once(f.outer_loop).chain(f.inner_loops.iter().copied()) {
            for v in topo.loop_vertices(lp) {
                let s = dot(sub(p3(&topo.vertices[v].point), wo), wa);
                s_lo = s_lo.min(s);
                s_hi = s_hi.max(s);
            }
        }
    }
    if s_hi - s_lo <= tol.length_mm || !s_hi.is_finite() {
        return Err(not_repr(part, "chamber wall has no axial extent"));
    }
    // Caps: perpendicular planes at the wall ends whose face normal points
    // into the cavity (+axis at the bottom, −axis at the top).
    let cap = |s_end: f64, inward: f64| {
        planes.iter().any(|(o, n)| {
            line_angle(*n, wa) <= tol.angle_rad
                && (dot(*n, wa) * inward) > 0.0
                && (dot(sub(*o, wo), wa) - s_end).abs() <= tol.length_mm
        })
    };
    if !cap(s_lo, 1.0) || !cap(s_hi, -1.0) {
        return Err(not_repr(
            part,
            "chamber cavity is not closed by flat caps perpendicular to its axis at both ends \
             (an open tube, domed ends or a tilted cap are not representable)",
        ));
    }
    let mid = 0.5 * (s_lo + s_hi);
    Ok(Cavity {
        origin: add(wo, scale(wa, mid)),
        axis: wa,
        radius_mm: wr,
        half_height_mm: 0.5 * (s_hi - s_lo),
    })
}

/// A concentric spherical shell (or a solid ball, `inner_radius_mm = 0`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ShellFeature {
    /// Common centre, mm.
    pub center: V3,
    /// Inner radius, mm (0 for a solid ball).
    pub inner_radius_mm: f64,
    /// Outer radius, mm.
    pub outer_radius_mm: f64,
}

/// Measure a spherical shield shell.
///
/// Every face must lie on a sphere about one centre; outward faces all on
/// one radius (outer), inward faces — if any — on one smaller radius
/// (inner). Cylindrical, planar or any other face is rejected: the
/// neutronics spec is a 1-D spherical layer stack around a point source,
/// and a cylinder or slab can't be folded into it without approximating.
pub fn shell_feature(
    part: &str,
    brep: &BRepSolid,
    tol: &Tolerance,
) -> Result<ShellFeature, SimExtractError> {
    let mut center: Option<V3> = None;
    let mut outer: Option<f64> = None;
    let mut inner: Option<f64> = None;
    for (fid, f) in brep.topology.faces.iter() {
        let s = surface_of(brep, fid);
        let Some(sp) = s.as_any().downcast_ref::<SphereSurface>() else {
            return Err(not_repr(
                part,
                format!(
                    "shield part has a {} face; ShieldSpec is a 1-D spherical layer stack around \
                     a point source, so only concentric spherical shells are representable \
                     (cylindrical/box shields need a 2-D/3-D transport geometry)",
                    kind_name(s)
                ),
            ));
        };
        let c = p3(&sp.center);
        match center {
            None => center = Some(c),
            Some(c0) if norm(sub(c, c0)) > tol.length_mm => {
                return Err(not_repr(
                    part,
                    "shield part's spherical faces don't share one centre",
                ));
            }
            _ => {}
        }
        let slot = if f.orientation == Orientation::Reversed {
            &mut inner
        } else {
            &mut outer
        };
        match *slot {
            None => *slot = Some(sp.radius),
            Some(r) if (r - sp.radius).abs() > tol.length_mm => {
                return Err(not_repr(
                    part,
                    format!(
                        "shield part has spherical faces on more than one radius ({r} and {} mm) \
                         facing the same way — one part must be one shell of one material",
                        sp.radius
                    ),
                ));
            }
            _ => {}
        }
    }
    let (Some(center), Some(outer)) = (center, outer) else {
        return Err(not_repr(part, "shield part has no outward spherical face"));
    };
    let inner = inner.unwrap_or(0.0);
    if inner >= outer - tol.length_mm {
        return Err(not_repr(
            part,
            format!("inner radius {inner} mm is not inside outer radius {outer} mm"),
        ));
    }
    Ok(ShellFeature {
        center,
        inner_radius_mm: inner,
        outer_radius_mm: outer,
    })
}

// ---------------------------------------------------------------------------
// Layer 2a: DeviceSpec (charged-particle optics)
// ---------------------------------------------------------------------------

/// One electrode part and the document parameters that drive it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElectrodeSelection {
    /// Part name (root node name) or stable part id.
    pub part: String,
    /// Name of the document parameter holding the electrode potential, V.
    pub potential: String,
    /// Name of the document parameter holding the ring current,
    /// ampere-turns (`+` = CCW viewed from the chamber's `+axis`). Absent =
    /// no current. Every ring in the part carries the same value — put
    /// opposed-current rings in separate parts with a derived parameter
    /// (`"0 - shield_at"`).
    #[serde(default)]
    pub ampere_turns: Option<String>,
}

/// What to extract a [`DeviceSpec`] from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceExtraction {
    /// The chamber part (a closed can; its cavity defines the device frame).
    pub chamber: String,
    /// Name of the document parameter holding the wall potential, V.
    /// Absent = grounded (0 V).
    #[serde(default)]
    pub wall_potential: Option<String>,
    /// Electrode parts.
    pub electrodes: Vec<ElectrodeSelection>,
    /// Matching tolerances.
    #[serde(default)]
    pub tolerance: Tolerance,
}

/// The extracted device: the spec plus everything needed to run and audit it.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedDevice {
    /// The solver schema. Geometry fields are literals measured from the
    /// BRep; potentials and ampere-turns are named document parameters.
    pub spec: DeviceSpec,
    /// The document's resolved parameter environment (binds every name in
    /// `spec`).
    pub parameters: BTreeMap<String, f64>,
    /// The chamber cavity — the device frame (z is measured from
    /// `cavity.origin` along `cavity.axis`).
    pub cavity: Cavity,
    /// Source part of each ring, index-aligned with `spec.rings`.
    pub ring_parts: Vec<String>,
}

/// Evaluate `doc` and extract an axisymmetric [`DeviceSpec`] from it.
pub fn extract_device_spec(
    doc: &Document,
    sel: &DeviceExtraction,
) -> Result<ExtractedDevice, SimExtractError> {
    let scene = evaluate_document(doc, &extraction_eval_options())?;
    extract_device_spec_from_scene(doc, &scene, sel)
}

/// [`extract_device_spec`] over an already-evaluated scene (it must have
/// been evaluated without a root cache, so parts carry their BRep).
pub fn extract_device_spec_from_scene(
    doc: &Document,
    scene: &EvaluatedScene,
    sel: &DeviceExtraction,
) -> Result<ExtractedDevice, SimExtractError> {
    let tol = &sel.tolerance;
    if sel.electrodes.is_empty() {
        return Err(SimExtractError::Invalid(
            "a device needs at least one electrode part".to_string(),
        ));
    }
    let env = resolve_env(doc)?;
    if let Some(w) = &sel.wall_potential {
        require_param(&env, w, "the chamber wall potential")?;
    }
    let chamber = find_part(doc, scene, &sel.chamber)?;
    let cavity = chamber_cavity(&chamber.name, chamber.brep, tol)?;

    let mut rings: Vec<(RingSpec, String, f64, f64)> = Vec::new();
    for e in &sel.electrodes {
        if e.part == sel.chamber {
            return Err(SimExtractError::Invalid(format!(
                "part {:?} is selected as both the chamber and an electrode",
                e.part
            )));
        }
        require_param(
            &env,
            &e.potential,
            format!("the potential of electrode {:?}", e.part),
        )?;
        if let Some(at) = &e.ampere_turns {
            require_param(
                &env,
                at,
                format!("the ampere-turns of electrode {:?}", e.part),
            )?;
        }
        let part = find_part(doc, scene, &e.part)?;
        for f in wire_features(&part.name, part.brep, tol)? {
            let WireFeature::Ring {
                center,
                axis,
                ring_radius_mm,
                wire_radius_mm,
            } = f;
            let angle = line_angle(axis, cavity.axis);
            let off = off_axis(center, cavity.origin, cavity.axis);
            if angle > tol.angle_rad || off > tol.length_mm {
                return Err(SimExtractError::NonCoaxialRing {
                    part: part.name.clone(),
                    ring_radius_mm,
                    detail: format!(
                        "axis tilted {:.3e} rad, centre {:.3e} mm off the axis",
                        angle, off
                    ),
                });
            }
            let z = dot(sub(center, cavity.origin), cavity.axis);
            if ring_radius_mm + wire_radius_mm >= cavity.radius_mm {
                return Err(SimExtractError::RingOutsideChamber {
                    part: part.name.clone(),
                    detail: format!(
                        "R + r = {} mm reaches the wall at {} mm",
                        ring_radius_mm + wire_radius_mm,
                        cavity.radius_mm
                    ),
                });
            }
            if z.abs() + wire_radius_mm >= cavity.half_height_mm {
                return Err(SimExtractError::RingOutsideChamber {
                    part: part.name.clone(),
                    detail: format!(
                        "|z| + r = {} mm reaches a cap at {} mm",
                        z.abs() + wire_radius_mm,
                        cavity.half_height_mm
                    ),
                });
            }
            rings.push((
                RingSpec {
                    ring_radius_mm: PParam::Literal(ring_radius_mm),
                    z_mm: PParam::Literal(z),
                    wire_radius_mm: PParam::Literal(wire_radius_mm),
                    potential_v: PParam::Named(e.potential.clone()),
                    ampere_turns: match &e.ampere_turns {
                        Some(n) => PParam::Named(n.clone()),
                        None => PParam::Literal(0.0),
                    },
                },
                part.name.clone(),
                z,
                ring_radius_mm,
            ));
        }
    }
    rings.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.3.total_cmp(&b.3)));
    // Coaxial rings collide when their wire cross-sections overlap in the
    // (r, z) half-plane — across parts too (within one part `wire_features`
    // already refused it).
    let lit = |p: &PParam| match p {
        PParam::Literal(v) => *v,
        PParam::Named(_) => unreachable!("ring geometry is always literal here"),
    };
    for (i, a) in rings.iter().enumerate() {
        for b in &rings[i + 1..] {
            let (ra, rb) = (lit(&a.0.wire_radius_mm), lit(&b.0.wire_radius_mm));
            let dist = (a.3 - b.3).hypot(a.2 - b.2);
            if dist < ra + rb - tol.length_mm {
                return Err(SimExtractError::RingsOverlap {
                    a: a.1.clone(),
                    b: b.1.clone(),
                    detail: format!(
                        "rings at (R, z) = ({}, {}) and ({}, {}) mm are {dist:.3} mm apart \
                         with wire radii {ra} + {rb} mm",
                        a.3, a.2, b.3, b.2
                    ),
                });
            }
        }
    }
    let ring_parts = rings.iter().map(|r| r.1.clone()).collect();
    let spec = DeviceSpec {
        chamber_radius_mm: PParam::Literal(cavity.radius_mm),
        chamber_half_height_mm: PParam::Literal(cavity.half_height_mm),
        wall_potential_v: match &sel.wall_potential {
            Some(n) => PParam::Named(n.clone()),
            None => PParam::Literal(0.0),
        },
        rings: rings.into_iter().map(|r| r.0).collect(),
    };
    Ok(ExtractedDevice {
        spec,
        parameters: env,
        cavity,
        ring_parts,
    })
}

// ---------------------------------------------------------------------------
// Layer 2b: ShieldSpec (neutron transport)
// ---------------------------------------------------------------------------

/// One shield shell part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShieldLayerSelection {
    /// Part name (root node name) or stable part id.
    pub part: String,
    /// Neutronics library material; absent = the part's document material.
    #[serde(default)]
    pub material: Option<String>,
}

fn default_fill() -> String {
    "air".to_string()
}

/// What to extract a [`ShieldSpec`] from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShieldExtraction {
    /// Shell parts (any order; sorted by radius).
    pub shells: Vec<ShieldLayerSelection>,
    /// Material filling the core, the gaps between shells and the world
    /// beyond the last shell (default `"air"`).
    #[serde(default = "default_fill")]
    pub fill_material: String,
    /// The point source (rate/energy may name document parameters).
    pub source: SourceSpec,
    /// Dose detectors (radii may name document parameters).
    pub detectors: Vec<DetectorSpec>,
    /// Outer radius of the modelled world, mm. Absent = 10% past the
    /// outermost detector shell (or the last shell, whichever is larger).
    #[serde(default)]
    pub world_radius_mm: Option<f64>,
    /// Where the source sits (e.g. the device cavity origin). When given,
    /// every shell must be centred on it; absent = the shells' common centre.
    #[serde(default)]
    pub center: Option<V3>,
    /// Monte Carlo parameters.
    #[serde(default)]
    pub run: RunSpec,
    /// Matching tolerances.
    #[serde(default)]
    pub tolerance: Tolerance,
}

/// The extracted shield problem.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedShield {
    /// The solver schema (layer thicknesses are literals measured from the
    /// BRep; source/detector names bind to document parameters).
    pub spec: ShieldSpec,
    /// The document's resolved parameter environment.
    pub parameters: BTreeMap<String, f64>,
    /// The source point / common shell centre, mm.
    pub center: V3,
    /// Source part of each layer, index-aligned with `spec.layers`; `None`
    /// for a fill layer.
    pub layer_parts: Vec<Option<String>>,
}

fn n_param(
    v: &NParam,
    env: &BTreeMap<String, f64>,
    role: &str,
) -> Result<Option<f64>, SimExtractError> {
    match v {
        NParam::Literal(x) => Ok(Some(*x)),
        NParam::Named(n) => {
            require_param(env, n, role)?;
            Ok(env.get(n).copied())
        }
    }
}

fn check_material(part: &str, m: &str) -> Result<(), SimExtractError> {
    materials::by_name(m)
        .map(|_| ())
        .ok_or_else(|| SimExtractError::UnknownMaterial {
            part: part.to_string(),
            material: m.to_string(),
        })
}

/// Evaluate `doc` and extract a spherical [`ShieldSpec`] from it.
pub fn extract_shield_spec(
    doc: &Document,
    sel: &ShieldExtraction,
) -> Result<ExtractedShield, SimExtractError> {
    let scene = evaluate_document(doc, &extraction_eval_options())?;
    extract_shield_spec_from_scene(doc, &scene, sel)
}

/// [`extract_shield_spec`] over an already-evaluated scene (evaluated
/// without a root cache).
pub fn extract_shield_spec_from_scene(
    doc: &Document,
    scene: &EvaluatedScene,
    sel: &ShieldExtraction,
) -> Result<ExtractedShield, SimExtractError> {
    let tol = &sel.tolerance;
    if sel.shells.is_empty() {
        return Err(SimExtractError::Invalid(
            "a shield needs at least one shell part".to_string(),
        ));
    }
    let env = resolve_env(doc)?;
    n_param(&sel.source.rate_n_per_s, &env, "the source rate")?;
    n_param(&sel.source.energy_ev, &env, "the source energy")?;
    let mut det_extent: f64 = 0.0;
    for d in &sel.detectors {
        let r = n_param(
            &d.radius_mm,
            &env,
            &format!("detector {:?} radius", d.label),
        )?;
        let hw = n_param(
            &d.half_width_mm,
            &env,
            &format!("detector {:?} half-width", d.label),
        )?;
        if let (Some(r), Some(hw)) = (r, hw) {
            det_extent = det_extent.max(r + hw);
        }
    }
    check_material("fill", &sel.fill_material)?;

    let mut shells: Vec<(String, String, ShellFeature)> = Vec::new();
    for s in &sel.shells {
        let part = find_part(doc, scene, &s.part)?;
        let material = s.material.clone().unwrap_or_else(|| part.material.clone());
        check_material(&format!("part {:?}", part.name), &material)?;
        let feat = shell_feature(&part.name, part.brep, tol)?;
        shells.push((part.name, material, feat));
    }
    let (center, reference) = match sel.center {
        Some(c) => (c, "the source point".to_string()),
        None => (shells[0].2.center, format!("part {:?}", shells[0].0)),
    };
    for (name, _, f) in &shells {
        let offset = norm(sub(f.center, center));
        if offset > tol.length_mm {
            return Err(SimExtractError::NonConcentric {
                part: name.clone(),
                reference,
                offset_mm: offset,
            });
        }
    }
    shells.sort_by(|a, b| a.2.inner_radius_mm.total_cmp(&b.2.inner_radius_mm));
    for w in shells.windows(2) {
        if w[1].2.inner_radius_mm < w[0].2.outer_radius_mm - tol.length_mm {
            return Err(SimExtractError::ShellOverlap {
                a: w[0].0.clone(),
                b: w[1].0.clone(),
                detail: format!(
                    "{:?} ends at {} mm, {:?} starts at {} mm",
                    w[0].0, w[0].2.outer_radius_mm, w[1].0, w[1].2.inner_radius_mm
                ),
            });
        }
    }

    let mut layers: Vec<LayerSpec> = Vec::new();
    let mut layer_parts: Vec<Option<String>> = Vec::new();
    let fill = |t: f64, layers: &mut Vec<LayerSpec>, parts: &mut Vec<Option<String>>| {
        layers.push(LayerSpec {
            material: sel.fill_material.clone(),
            thickness_mm: NParam::Literal(t),
        });
        parts.push(None);
    };
    let mut r = 0.0;
    for (name, material, f) in &shells {
        let gap = f.inner_radius_mm - r;
        if gap > tol.length_mm {
            fill(gap, &mut layers, &mut layer_parts);
        }
        layers.push(LayerSpec {
            material: material.clone(),
            thickness_mm: NParam::Literal(f.outer_radius_mm - f.inner_radius_mm.max(r)),
        });
        layer_parts.push(Some(name.clone()));
        r = f.outer_radius_mm;
    }
    let world = match sel.world_radius_mm {
        Some(w) if w < r - tol.length_mm => {
            return Err(SimExtractError::Invalid(format!(
                "world_radius_mm = {w} is inside the outermost shell ({r} mm)"
            )))
        }
        Some(w) => w,
        None => r.max(1.1 * det_extent),
    };
    if world - r > tol.length_mm {
        fill(world - r, &mut layers, &mut layer_parts);
    }
    Ok(ExtractedShield {
        spec: ShieldSpec {
            layers,
            source: sel.source.clone(),
            detectors: sel.detectors.clone(),
            run: sel.run.clone(),
        },
        parameters: env,
        center,
        layer_parts,
    })
}
