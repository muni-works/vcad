//! Document → [`AxisymSpec`]: revolved magnetic parts (steel pole piece,
//! top plate, T-yoke, ring magnet, voice coil) become the annuli of an
//! axisymmetric magnetostatic device.
//!
//! **Acceptance rule (sufficient for axisymmetry).** A selected part is
//! accepted only if every face lies on a cylinder coaxial with the common
//! motor axis or on a plane perpendicular to it. With nothing else
//! present, a face can only be bounded by circles, so the solid is a solid
//! of revolution whose meridian section is a rectilinear (r, z) polygon.
//! Cones (chamfers), tori (fillets), spheres, off-axis holes, flats and
//! keyways all violate the rule and are refused by part name — nothing is
//! approximated.
//!
//! **Section → annuli.** The polygon is slabbed on its r and z breakpoints
//! (cylinder radii and the axis; plane heights and cylinder ends) and each
//! slab cell is classified by the parity of the cylinder walls crossed on
//! a ray from its centre toward +r. Inside cells merge into maximal
//! rectangles (runs along r, then identical runs stacked along z). A
//! T-yoke becomes two annuli (back plate + pole). As a final guard,
//! `Σ π(r₁² − r₀²)·h` must match the part's tessellated volume.
//!
//! **Magnetic properties** come from a small fail-closed material table
//! keyed by the part's document material (or a per-part `material`
//! override), with per-part annotations in the selection overriding any
//! field. Electrical drive (turns, current) and magnet strength may be
//! named document parameters, carried as
//! [`ParamValue::Named`](vcad_kernel_em::spec::ParamValue) so scrubbing
//! the parameter re-prices the field solve. An unknown material with no
//! explicit annotation is an error — stainless 304 is non-magnetic and
//! "steel" alone doesn't say which, so it is not in the table.
//!
//! **Grid advice.** Materials are sampled at cell centres, so the grid
//! must resolve the thinnest steel/magnet feature or air gap. The
//! extraction reports the thinnest material-boundary spacing and the node
//! counts that put `cells_per_feature` cells across it; a domain whose
//! advised grid exceeds the solver's 4M-node cap is refused rather than
//! solved on a grid that would close the gap. Coils are exempt — their
//! sources deposit by exact area overlap.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vcad_ir::Document;
use vcad_kernel::vcad_kernel_primitives::BRepSolid;
use vcad_kernel_em::grid::Bc;
use vcad_kernel_em::spec::{
    AxisymMagnetSpec, AxisymSpec, CoilSpec, MaterialSpec, ParamValue, RectSpec,
};
use vcad_kernel_geom::{CylinderSurface, Plane};

use super::{
    canonical_axis, d3, dot, find_part, kind_name, line_angle, not_repr, off_axis, p3, resolve_env,
    scale, sub, surface_of, SimExtractError, Tolerance, V3,
};
use crate::{evaluate_document, EvaluatedScene};

/// Largest node count the advised grid may reach (the MCP tier's cap).
pub const MAX_GRID_NODES: usize = 4_000_000;

/// What a part does in the magnetic circuit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagRole {
    /// Soft magnetic material (μ_r, optional saturation J_s).
    Steel,
    /// Axially magnetized permanent magnet (B_r, recoil μ_r).
    Magnet,
    /// Winding of revolution (turns × current), non-magnetic.
    Coil,
}

/// Which way an axial magnet's north face points along the motor axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum North {
    /// North face toward +axis (the axis is oriented with its largest
    /// component positive, so +Z for a Z-up motor).
    #[default]
    #[serde(rename = "+axis")]
    PlusAxis,
    /// North face toward −axis.
    #[serde(rename = "-axis")]
    MinusAxis,
}

/// One selected part, with optional annotations overriding the material
/// table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagPartSelection {
    /// Part name or part id.
    pub part: String,
    /// Material key for the table lookup (default: the part's document
    /// material).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// Role override (required when the material isn't in the table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<MagRole>,
    /// Relative permeability (steel: initial μ_r; magnet: recoil μ_r).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mu_r: Option<ParamValue>,
    /// Steel saturation polarization J_s, tesla (`null`/absent with a
    /// table material keeps the table value; set `linear: true` to drop it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_t: Option<ParamValue>,
    /// Force a linear steel model even when the table gives J_s.
    #[serde(default)]
    pub linear: bool,
    /// Magnet remanence magnitude, tesla.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub br_t: Option<ParamValue>,
    /// Magnet polarity.
    #[serde(default)]
    pub north: North,
    /// Coil turns over the cross-section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<ParamValue>,
    /// Coil current per turn, A (default 0 — a Bl / gap-flux probe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_a: Option<ParamValue>,
}

/// Domain sizing: by default the motor's bounding (r, z) box padded by
/// `pad_factor` × its larger extent on every open side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DomainSelection {
    /// Padding as a multiple of the motor's larger extent. Default 1.0.
    pub pad_factor: f64,
    /// Absolute outer radius, mm (overrides the padding).
    pub r_max_mm: Option<f64>,
    /// Absolute lower axial bound, mm along the axis.
    pub z_min_mm: Option<f64>,
    /// Absolute upper axial bound, mm along the axis.
    pub z_max_mm: Option<f64>,
    /// Grid cells across the thinnest material feature. Default 4.
    pub cells_per_feature: f64,
}

impl Default for DomainSelection {
    fn default() -> Self {
        Self {
            pad_factor: 1.0,
            r_max_mm: None,
            z_min_mm: None,
            z_max_mm: None,
            cells_per_feature: 4.0,
        }
    }
}

/// Which parts of a document form the axisymmetric magnetic device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxisymMagExtraction {
    /// The parts (steel, magnets, coils). Unselected parts are air.
    pub parts: Vec<MagPartSelection>,
    /// Domain sizing.
    #[serde(default)]
    pub domain: DomainSelection,
    /// Matching tolerances.
    #[serde(default)]
    pub tolerance: Tolerance,
}

/// The motor axis in world coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MotorAxis {
    /// The axis point closest to the world origin, mm. Spec `z` is the
    /// signed distance along `direction` from this point.
    pub origin: V3,
    /// Unit direction (largest component positive).
    pub direction: V3,
}

/// One part's contribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedMagPart {
    /// Part name as selected.
    pub part: String,
    /// Material key used for the table lookup.
    pub material: String,
    /// Role in the circuit.
    pub role: MagRole,
    /// Its meridian section as `[r_inner, r_outer, z_min, z_max]` mm.
    pub annuli: Vec<[f64; 4]>,
    /// Indices into the spec's `coils` / `materials` / `magnets` list
    /// (by role) that this part emitted.
    pub spec_indices: Vec<usize>,
    /// Revolved volume of the annuli vs the tessellated part volume.
    pub volume_mm3: [f64; 2],
}

/// Advised solve grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridAdvice {
    /// Thinnest steel/magnet feature or air gap, mm.
    pub thinnest_feature_mm: f64,
    /// Advised node spacing, mm.
    pub dx_mm: f64,
    /// Radial nodes.
    pub nx: usize,
    /// Axial nodes.
    pub ny: usize,
}

/// Result of [`extract_axisym_mag_spec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedAxisymMag {
    /// The device, ready for `AxisymSpec::resolve(&parameters)`.
    pub spec: AxisymSpec,
    /// The document's resolved parameter environment.
    pub parameters: BTreeMap<String, f64>,
    /// The common axis.
    pub axis: MotorAxis,
    /// Per-part breakdown.
    pub parts: Vec<ExtractedMagPart>,
    /// Advised grid.
    pub grid: GridAdvice,
}

// ---------------------------------------------------------------------------
// Material table
// ---------------------------------------------------------------------------

/// Table entry: (role, μ_r, J_s for steel / B_r for magnets).
fn table(key: &str) -> Option<(MagRole, f64, f64)> {
    let k = key.to_ascii_lowercase();
    Some(match k.as_str() {
        // Low-carbon steels: initial μ_r ~1000, J_s ≈ 2.0 T (annealed
        // 1008–1018; the speaker-motor default).
        "steel-1008" | "steel-1010" | "steel-1018" | "low-carbon-steel" | "mild-steel" => {
            (MagRole::Steel, 1000.0, 2.0)
        }
        "iron" | "pure-iron" => (MagRole::Steel, 5000.0, 2.15),
        // Hard ferrites (C8 ≈ Y30), sintered NdFeB grades: B_r typical.
        "ferrite-y30" | "ferrite-c8" | "ferrite" => (MagRole::Magnet, 1.05, 0.39),
        "ferrite-y35" => (MagRole::Magnet, 1.05, 0.42),
        "ndfeb-n35" => (MagRole::Magnet, 1.05, 1.19),
        "ndfeb-n42" => (MagRole::Magnet, 1.05, 1.30),
        "ndfeb-n52" => (MagRole::Magnet, 1.05, 1.44),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

struct RevSection {
    cylinders: Vec<(f64, f64, f64)>, // (r, z_lo, z_hi), deduplicated
    planes: Vec<f64>,                // z of perpendicular planes
}

/// Collect the axis-perpendicular planes and coaxial cylinders of a part,
/// refusing any other face. `axis` is fixed by the first cylinder seen in
/// any part.
fn rev_section(
    part: &str,
    brep: &BRepSolid,
    axis: &mut Option<(V3, V3)>,
    tol: &Tolerance,
) -> Result<RevSection, SimExtractError> {
    let topo = &brep.topology;
    let mut cyl_faces: Vec<(V3, V3, f64, vcad_kernel_topo::FaceId)> = Vec::new();
    let mut plane_faces: Vec<(V3, V3)> = Vec::new();
    for (fid, _) in topo.faces.iter() {
        let s = surface_of(brep, fid);
        let any = s.as_any();
        if let Some(c) = any.downcast_ref::<CylinderSurface>() {
            cyl_faces.push((p3(&c.center), canonical_axis(d3(&c.axis)), c.radius, fid));
        } else if let Some(p) = any.downcast_ref::<Plane>() {
            plane_faces.push((p3(&p.origin), d3(&p.normal_dir)));
        } else {
            return Err(not_repr(
                part,
                format!(
                    "has a {} face; only coaxial cylinders and axis-perpendicular planes are \
                     representable as axisymmetric annuli (remove chamfers/fillets or model them \
                     as steps)",
                    kind_name(s)
                ),
            ));
        }
    }
    if axis.is_none() {
        if let Some((o, a, _, _)) = cyl_faces.first() {
            // Foot of the perpendicular from the world origin.
            let foot = sub(*o, scale(*a, dot(*o, *a)));
            *axis = Some((foot, *a));
        }
    }
    let Some((ao, aa)) = *axis else {
        return Err(not_repr(
            part,
            "has no cylindrical face, so no axis of revolution can be measured",
        ));
    };
    let mut cylinders: Vec<(f64, f64, f64)> = Vec::new();
    for (o, a, r, fid) in &cyl_faces {
        if line_angle(*a, aa) > tol.angle_rad || off_axis(*o, ao, aa) > tol.length_mm {
            return Err(SimExtractError::NotRepresentable {
                part: part.to_string(),
                detail: format!(
                    "cylindrical face (r = {r} mm) is not coaxial with the motor axis (off by \
                     {:.3e} mm / {:.3e} rad); off-axis holes and pins are not axisymmetric",
                    off_axis(*o, ao, aa),
                    line_angle(*a, aa)
                ),
            });
        }
        let f = &topo.faces[*fid];
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for lp in std::iter::once(f.outer_loop).chain(f.inner_loops.iter().copied()) {
            for v in topo.loop_vertices(lp) {
                let z = dot(sub(p3(&topo.vertices[v].point), ao), aa);
                lo = lo.min(z);
                hi = hi.max(z);
            }
        }
        if !(hi - lo > tol.length_mm) {
            return Err(not_repr(
                part,
                format!("cylinder r = {r} mm has no axial extent"),
            ));
        }
        // Seam-split halves of one wall share (r, lo, hi): count once.
        let dup = cylinders.iter().any(|&(r2, l2, h2)| {
            (r2 - r).abs() <= tol.length_mm
                && (l2 - lo).abs() <= tol.length_mm
                && (h2 - hi).abs() <= tol.length_mm
        });
        if !dup {
            cylinders.push((*r, lo, hi));
        }
    }
    let mut planes = Vec::new();
    for (o, n) in &plane_faces {
        if line_angle(*n, aa) > tol.angle_rad {
            return Err(not_repr(
                part,
                "has a planar face that is not perpendicular to the motor axis (a flat, slot or \
                 keyway); not axisymmetric",
            ));
        }
        planes.push(dot(sub(*o, ao), aa));
    }
    Ok(RevSection { cylinders, planes })
}

fn dedup_sorted(mut v: Vec<f64>, tol: f64) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    let mut out: Vec<f64> = Vec::with_capacity(v.len());
    for x in v {
        if out.last().is_none_or(|l| x - l > tol) {
            out.push(x);
        }
    }
    out
}

/// Meridian section → maximal rectangles `[r0, r1, z0, z1]`.
fn section_annuli(sec: &RevSection, tol: f64) -> Vec<[f64; 4]> {
    let mut rs = vec![0.0];
    let mut zs = sec.planes.clone();
    for &(r, lo, hi) in &sec.cylinders {
        rs.push(r);
        zs.push(lo);
        zs.push(hi);
    }
    let rs = dedup_sorted(rs, tol);
    let zs = dedup_sorted(zs, tol);
    let inside = |rc: f64, zc: f64| {
        sec.cylinders
            .iter()
            .filter(|&&(r, lo, hi)| r > rc && lo < zc && zc < hi)
            .count()
            % 2
            == 1
    };
    // Runs along r per z-row.
    let mut rows: Vec<Vec<(f64, f64)>> = Vec::new();
    for zw in zs.windows(2) {
        let zc = 0.5 * (zw[0] + zw[1]);
        let mut runs: Vec<(f64, f64)> = Vec::new();
        for rw in rs.windows(2) {
            let rc = 0.5 * (rw[0] + rw[1]);
            if inside(rc, zc) {
                match runs.last_mut() {
                    Some(last) if (last.1 - rw[0]).abs() <= tol => last.1 = rw[1],
                    _ => runs.push((rw[0], rw[1])),
                }
            }
        }
        rows.push(runs);
    }
    // Stack identical runs along z.
    let mut out: Vec<[f64; 4]> = Vec::new();
    let mut open: Vec<[f64; 4]> = Vec::new();
    for (j, runs) in rows.iter().enumerate() {
        let (z0, z1) = (zs[j], zs[j + 1]);
        let mut next: Vec<[f64; 4]> = Vec::new();
        for &(r0, r1) in runs {
            if let Some(pos) = open.iter().position(|a| {
                (a[0] - r0).abs() <= tol && (a[1] - r1).abs() <= tol && (a[3] - z0).abs() <= tol
            }) {
                let mut a = open.swap_remove(pos);
                a[3] = z1;
                next.push(a);
            } else {
                next.push([r0, r1, z0, z1]);
            }
        }
        out.append(&mut open);
        open = next;
    }
    out.append(&mut open);
    out.sort_by(|a, b| a[2].total_cmp(&b[2]).then(a[0].total_cmp(&b[0])));
    out
}

fn revolved_volume(annuli: &[[f64; 4]]) -> f64 {
    annuli
        .iter()
        .map(|a| std::f64::consts::PI * (a[1] * a[1] - a[0] * a[0]) * (a[3] - a[2]))
        .sum()
}

fn brep_volume(brep: &BRepSolid) -> f64 {
    let mesh = vcad_kernel_tessellate::tessellate_brep(brep, 256);
    let v = &mesh.vertices;
    let p = |i: u32| {
        let i = i as usize * 3;
        [v[i] as f64, v[i + 1] as f64, v[i + 2] as f64]
    };
    let mut vol = 0.0;
    for t in mesh.indices.chunks_exact(3) {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        vol += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
    }
    (vol / 6.0).abs()
}

fn literal(v: f64) -> ParamValue {
    ParamValue::Literal(v)
}

fn rect(a: &[f64; 4]) -> RectSpec {
    RectSpec {
        x_min_mm: literal(a[0]),
        x_max_mm: literal(a[1]),
        y_min_mm: literal(a[2]),
        y_max_mm: literal(a[3]),
    }
}

fn check_param(
    v: &ParamValue,
    env: &BTreeMap<String, f64>,
    part: &str,
    role: &str,
) -> Result<(), SimExtractError> {
    if let ParamValue::Named(n) = v {
        if !env.contains_key(n) {
            return Err(SimExtractError::UnknownParameter {
                name: n.clone(),
                role: format!("{role} of part {part:?}"),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Evaluate `doc` and extract an [`AxisymSpec`] from the selected parts.
pub fn extract_axisym_mag_spec(
    doc: &Document,
    sel: &AxisymMagExtraction,
) -> Result<ExtractedAxisymMag, SimExtractError> {
    let scene = evaluate_document(doc, &super::extraction_eval_options())?;
    extract_axisym_mag_spec_from_scene(doc, &scene, sel)
}

/// [`extract_axisym_mag_spec`] on an already-evaluated scene.
pub fn extract_axisym_mag_spec_from_scene(
    doc: &Document,
    scene: &EvaluatedScene,
    sel: &AxisymMagExtraction,
) -> Result<ExtractedAxisymMag, SimExtractError> {
    let tol = &sel.tolerance;
    if sel.parts.is_empty() {
        return Err(SimExtractError::Invalid(
            "an axisymmetric magnetic device needs at least one part".into(),
        ));
    }
    let d = &sel.domain;
    if !(d.pad_factor >= 0.0) || !(d.cells_per_feature >= 1.0) {
        return Err(SimExtractError::Invalid(
            "domain.pad_factor must be ≥ 0 and domain.cells_per_feature ≥ 1".into(),
        ));
    }
    let env = resolve_env(doc)?;
    let mut axis: Option<(V3, V3)> = None;
    let mut spec = AxisymSpec {
        r_max_mm: literal(0.0),
        z_min_mm: literal(0.0),
        z_max_mm: literal(0.0),
        coils: Vec::new(),
        materials: Vec::new(),
        magnets: Vec::new(),
        bc_r_outer: Bc::Zero,
        bc_z_low: Bc::Zero,
        bc_z_high: Bc::Zero,
    };
    let mut parts_out = Vec::new();
    // Material/magnet breakpoints (for grid advice) and the device bbox.
    let mut mat_r: Vec<f64> = Vec::new();
    let mut mat_z: Vec<f64> = Vec::new();
    let (mut r_hi, mut z_lo, mut z_hi) = (0.0_f64, f64::INFINITY, f64::NEG_INFINITY);
    let mut seen = std::collections::BTreeSet::new();

    for ps in &sel.parts {
        if !seen.insert(ps.part.clone()) {
            return Err(SimExtractError::Invalid(format!(
                "part {:?} is selected twice",
                ps.part
            )));
        }
        let part = find_part(doc, scene, &ps.part)?;
        let material = ps.material.clone().unwrap_or_else(|| part.material.clone());
        let row = table(&material);
        let role = match (ps.role, row) {
            (Some(r), _) => r,
            (None, Some((r, _, _))) => r,
            (None, None) => {
                return Err(not_repr(
                    &part.name,
                    format!(
                        "material {material:?} is not in the magnetic material table and no \
                         `role` is given; annotate the part (role + mu_r/js_t, br_t or turns) or \
                         use a table key (steel-1010, iron, ferrite-y30, ndfeb-n42, …)"
                    ),
                ))
            }
        };
        // Table values only apply when the table row has the same role.
        let row = row.filter(|(r, _, _)| *r == role);
        let sec = rev_section(&part.name, part.brep, &mut axis, tol)?;
        let annuli = section_annuli(&sec, tol.length_mm.max(1e-9));
        if annuli.is_empty() {
            return Err(not_repr(&part.name, "meridian section is empty"));
        }
        let v_rev = revolved_volume(&annuli);
        let v_brep = brep_volume(part.brep);
        if (v_rev - v_brep).abs() > 0.01 * v_brep.max(1e-9) {
            return Err(not_repr(
                &part.name,
                format!(
                    "revolved section volume {v_rev:.4} mm³ does not match the part volume \
                     {v_brep:.4} mm³ (>1%); the part is not the solid of revolution its faces \
                     suggest"
                ),
            ));
        }
        for a in &annuli {
            r_hi = r_hi.max(a[1]);
            z_lo = z_lo.min(a[2]);
            z_hi = z_hi.max(a[3]);
        }
        let mut spec_indices = Vec::new();
        match role {
            MagRole::Steel => {
                let mu_r = ps
                    .mu_r
                    .clone()
                    .or(row.map(|r| literal(r.1)))
                    .ok_or_else(|| {
                        not_repr(&part.name, "steel part needs `mu_r` (not in the table)")
                    })?;
                let js_t = if ps.linear {
                    None
                } else {
                    ps.js_t.clone().or(row.map(|r| literal(r.2)))
                };
                check_param(&mu_r, &env, &part.name, "mu_r")?;
                if let Some(js) = &js_t {
                    check_param(js, &env, &part.name, "js_t")?;
                }
                for a in &annuli {
                    spec_indices.push(spec.materials.len());
                    spec.materials.push(MaterialSpec {
                        region: rect(a),
                        mu_r: mu_r.clone(),
                        js_t: js_t.clone(),
                    });
                    mat_r.extend([a[0], a[1]]);
                    mat_z.extend([a[2], a[3]]);
                }
            }
            MagRole::Magnet => {
                let br = ps
                    .br_t
                    .clone()
                    .or(row.map(|r| literal(r.2)))
                    .ok_or_else(|| {
                        not_repr(&part.name, "magnet part needs `br_t` (not in the table)")
                    })?;
                let mu_r = ps
                    .mu_r
                    .clone()
                    .or(row.map(|r| literal(r.1)))
                    .unwrap_or(literal(1.05));
                check_param(&br, &env, &part.name, "br_t")?;
                check_param(&mu_r, &env, &part.name, "mu_r")?;
                if ps.north == North::MinusAxis && matches!(br, ParamValue::Named(_)) {
                    return Err(not_repr(
                        &part.name,
                        "north = \"-axis\" with a named br_t can't be negated in the spec; bind a \
                         negative parameter and use north = \"+axis\"",
                    ));
                }
                let br_z = match (&br, ps.north) {
                    (ParamValue::Literal(v), North::MinusAxis) => literal(-v),
                    _ => br.clone(),
                };
                for a in &annuli {
                    spec_indices.push(spec.magnets.len());
                    spec.magnets.push(AxisymMagnetSpec {
                        region: rect(a),
                        br_z_t: br_z.clone(),
                        mu_r: mu_r.clone(),
                    });
                    mat_r.extend([a[0], a[1]]);
                    mat_z.extend([a[2], a[3]]);
                }
            }
            MagRole::Coil => {
                if annuli.len() != 1 {
                    return Err(not_repr(
                        &part.name,
                        format!(
                            "a coil must be a single rectangular winding section (found {} \
                             annuli) — turns can't be apportioned between them",
                            annuli.len()
                        ),
                    ));
                }
                let turns = ps
                    .turns
                    .clone()
                    .ok_or_else(|| not_repr(&part.name, "coil part needs `turns`"))?;
                let current = ps.current_a.clone().unwrap_or(literal(0.0));
                check_param(&turns, &env, &part.name, "turns")?;
                check_param(&current, &env, &part.name, "current_a")?;
                spec_indices.push(spec.coils.len());
                spec.coils.push(CoilSpec {
                    region: rect(&annuli[0]),
                    turns,
                    current_a: current,
                });
            }
        }
        parts_out.push(ExtractedMagPart {
            part: part.name.clone(),
            material,
            role,
            annuli,
            spec_indices,
            volume_mm3: [v_rev, v_brep],
        });
    }

    // Overlap between different parts' material/magnet annuli is a
    // modelling error the solver would silently resolve by "last wins".
    let regions: Vec<(&str, [f64; 4])> = parts_out
        .iter()
        .filter(|p| p.role != MagRole::Coil)
        .flat_map(|p| p.annuli.iter().map(move |a| (p.part.as_str(), *a)))
        .collect();
    for (i, (pa, a)) in regions.iter().enumerate() {
        for (pb, b) in &regions[i + 1..] {
            if pa == pb {
                continue;
            }
            let ov = |l0: f64, h0: f64, l1: f64, h1: f64| h0.min(h1) - l0.max(l1);
            if ov(a[0], a[1], b[0], b[1]) > tol.length_mm
                && ov(a[2], a[3], b[2], b[3]) > tol.length_mm
            {
                return Err(SimExtractError::Invalid(format!(
                    "parts {pa:?} and {pb:?} overlap in the meridian section; the device needs \
                     disjoint magnetic regions"
                )));
            }
        }
    }

    let (ao, aa) = axis.expect("at least one part measured");
    let extent = r_hi.max(z_hi - z_lo);
    let pad = d.pad_factor * extent;
    let r_max = d.r_max_mm.unwrap_or(r_hi + pad);
    let zmin = d.z_min_mm.unwrap_or(z_lo - pad);
    let zmax = d.z_max_mm.unwrap_or(z_hi + pad);
    if r_max <= r_hi || zmin >= z_lo || zmax <= z_hi {
        return Err(SimExtractError::Invalid(format!(
            "domain (r ≤ {r_max}, z ∈ [{zmin}, {zmax}]) does not enclose the device \
             (r ≤ {r_hi}, z ∈ [{z_lo}, {z_hi}])"
        )));
    }
    spec.r_max_mm = literal(r_max);
    spec.z_min_mm = literal(zmin);
    spec.z_max_mm = literal(zmax);

    // Thinnest feature: smallest spacing between distinct material
    // breakpoints along each direction (includes air gaps between parts).
    let spacing = |v: Vec<f64>| {
        dedup_sorted(v, tol.length_mm.max(1e-9))
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(f64::INFINITY, f64::min)
    };
    let mut rbp = mat_r.clone();
    rbp.push(0.0);
    let thinnest = spacing(rbp).min(spacing(mat_z));
    let thinnest = if thinnest.is_finite() {
        thinnest
    } else {
        extent
    };
    let dx = thinnest / d.cells_per_feature;
    let nx = (r_max / dx).ceil() as usize + 1;
    let ny = ((zmax - zmin) / dx).ceil() as usize + 1;
    if nx.saturating_mul(ny) > MAX_GRID_NODES {
        return Err(SimExtractError::Invalid(format!(
            "resolving the thinnest feature ({thinnest:.4} mm) with {} cells needs a {nx}×{ny} \
             grid (> {MAX_GRID_NODES} nodes) on this domain; shrink the domain (domain.pad_factor \
             / explicit bounds) or lower domain.cells_per_feature — refusing to solve on a grid \
             that would close the gap",
            d.cells_per_feature
        )));
    }

    Ok(ExtractedAxisymMag {
        spec,
        parameters: env,
        axis: MotorAxis {
            origin: ao,
            direction: aa,
        },
        parts: parts_out,
        grid: GridAdvice {
            thinnest_feature_mm: thinnest,
            dx_mm: dx,
            nx,
            ny,
        },
    })
}
