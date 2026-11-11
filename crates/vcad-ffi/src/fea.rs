//! Static structural FEA for the native app (`vcad_fea_*`): faces picked in
//! the viewport become load / support regions, a material preset (or explicit
//! moduli) sets the elastic constants, and the part is meshed and solved
//! in-process through `vcad-kernel-fea`'s fail-closed refinement study.
//!
//! The finest level's nodal fields come back resampled onto the lattice's
//! cell grid in the `FieldOverlay.swift` volume layout (`dims_zyx`,
//! `origin_mm`, `voxel_mm`, cell-major `(k·ny + j)·nx + i`, plus a solid
//! mask), so the app draws them with the existing surface / slice overlay.
//!
//! Face → region: the kernel selects nodes with axis-aligned boxes, so a
//! picked face contributes the bounding box of its tessellation. Exact for
//! axis-aligned planar faces (the usual load/support pick); a curved or
//! skewed face's box can also catch nodes just inside it, which the result
//! reports so nobody mistakes it for a face-exact selection.

use std::collections::HashMap;
use std::ffi::c_char;
use std::ptr;

use serde::{Deserialize, Serialize};
use vcad_kernel::vcad_kernel_fea as fea;
use vcad_kernel::vcad_kernel_tessellate::{tessellate_brep_by_face, TessellationParams};
use vcad_kernel_tessellate::TriangleMesh;

use crate::analysis::{evaluate, parse_doc, parse_request, part_name, part_roots, shell_ordinals};
use crate::err;
use crate::json::{guard_ptr, json_out};

/// Highest finest-level lattice resolution the app will solve (the mesher's
/// ceiling is 256; this keeps an interactive solve interactive).
pub const APP_RESOLUTION_CAP: usize = 192;

/// Elastic constants for a material preset.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MaterialPreset {
    /// Preset id.
    pub id: &'static str,
    /// Display name.
    pub label: &'static str,
    /// Young's modulus, MPa.
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio.
    pub poisson: f64,
    /// Yield strength, MPa.
    pub yield_strength_mpa: f64,
}

/// Built-in presets (room-temperature handbook values).
pub const MATERIALS: &[MaterialPreset] = &[
    MaterialPreset {
        id: "aluminum_6061",
        label: "Aluminum 6061-T6",
        youngs_modulus_mpa: 69_000.0,
        poisson: 0.33,
        yield_strength_mpa: 276.0,
    },
    MaterialPreset {
        id: "steel",
        label: "Steel (1018)",
        youngs_modulus_mpa: 205_000.0,
        poisson: 0.29,
        yield_strength_mpa: 370.0,
    },
    MaterialPreset {
        id: "stainless_304",
        label: "Stainless 304",
        youngs_modulus_mpa: 193_000.0,
        poisson: 0.29,
        yield_strength_mpa: 215.0,
    },
    MaterialPreset {
        id: "titanium",
        label: "Titanium Ti-6Al-4V",
        youngs_modulus_mpa: 114_000.0,
        poisson: 0.34,
        yield_strength_mpa: 880.0,
    },
    MaterialPreset {
        id: "brass",
        label: "Brass C360",
        youngs_modulus_mpa: 97_000.0,
        poisson: 0.31,
        yield_strength_mpa: 310.0,
    },
    MaterialPreset {
        id: "pla",
        label: "PLA",
        youngs_modulus_mpa: 3_500.0,
        poisson: 0.36,
        yield_strength_mpa: 50.0,
    },
    MaterialPreset {
        id: "petg",
        label: "PETG",
        youngs_modulus_mpa: 2_100.0,
        poisson: 0.38,
        yield_strength_mpa: 50.0,
    },
    MaterialPreset {
        id: "abs",
        label: "ABS",
        youngs_modulus_mpa: 2_200.0,
        poisson: 0.35,
        yield_strength_mpa: 40.0,
    },
    MaterialPreset {
        id: "nylon",
        label: "Nylon PA12",
        youngs_modulus_mpa: 1_700.0,
        poisson: 0.39,
        yield_strength_mpa: 48.0,
    },
];

/// Best preset for a document material key (`"aluminum"`, `"pla_red"`, …).
pub fn preset_for(material: &str) -> &'static MaterialPreset {
    let m = material.to_ascii_lowercase();
    let pick = |id: &str| MATERIALS.iter().find(|p| p.id == id);
    let found = if m.contains("stainless") {
        pick("stainless_304")
    } else if m.contains("steel") || m.contains("iron") {
        pick("steel")
    } else if m.contains("titan") {
        pick("titanium")
    } else if m.contains("brass") || m.contains("bronze") {
        pick("brass")
    } else if m.contains("petg") {
        pick("petg")
    } else if m.contains("pla") {
        pick("pla")
    } else if m.contains("abs") {
        pick("abs")
    } else if m.contains("nylon") || m.contains("pa12") {
        pick("nylon")
    } else {
        None
    };
    found.unwrap_or(&MATERIALS[0])
}

/// Where a load or support acts: picked faces and/or an explicit box.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Target {
    /// Viewport face ids (shell ordinals) on the analysed part.
    pub faces: Vec<u32>,
    /// Explicit region, mm.
    pub region: Option<fea::spec::RegionBox>,
}

/// A load in the request.
#[derive(Debug, Clone, Deserialize)]
pub struct LoadReq {
    /// Where it acts.
    #[serde(flatten)]
    pub target: Target,
    /// Total force, N.
    pub force: [f64; 3],
}

/// A support in the request.
#[derive(Debug, Clone, Deserialize)]
pub struct SupportReq {
    /// Where it acts.
    #[serde(flatten)]
    pub target: Target,
    /// Fixed components (default all).
    #[serde(default = "all_fixed")]
    pub fix: [bool; 3],
}

fn all_fixed() -> [bool; 3] {
    [true; 3]
}

/// Explicit elastic constants (override the preset).
#[derive(Debug, Clone, Deserialize)]
pub struct MaterialReq {
    /// Young's modulus, MPa.
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio.
    pub poisson: f64,
    /// Yield strength, MPa.
    pub yield_strength_mpa: Option<f64>,
}

/// The app's solve request.
#[derive(Debug, Clone, Deserialize)]
pub struct SolveReq {
    /// Evaluated part to analyse.
    #[serde(default)]
    pub part_index: usize,
    /// Preset id; default: inferred from the part's material.
    #[serde(default)]
    pub material: Option<String>,
    /// Explicit constants (win over `material`).
    #[serde(default)]
    pub elastic: Option<MaterialReq>,
    /// Coarse lattice resolution (cells along the longest axis).
    #[serde(default = "default_resolution")]
    pub resolution: usize,
    /// Refinement levels (≥ 2).
    #[serde(default = "default_levels")]
    pub levels: usize,
    /// Loads.
    pub loads: Vec<LoadReq>,
    /// Supports.
    pub supports: Vec<SupportReq>,
    /// Return the cell-grid fields for the overlay.
    #[serde(default = "yes")]
    pub fields: bool,
}

fn default_resolution() -> usize {
    24
}
fn default_levels() -> usize {
    2
}
fn yes() -> bool {
    true
}

/// Fields on the lattice cell grid, `FieldOverlay.swift` volume layout.
#[derive(Debug, Clone, Serialize)]
pub struct CellField {
    /// `[nz, ny, nx]`.
    pub dims_zyx: [usize; 3],
    /// Grid origin (lattice minimum corner), mm.
    pub origin_mm: [f64; 3],
    /// Cell size, mm.
    pub voxel_mm: [f64; 3],
    /// Von Mises per cell, MPa (mean of corner nodes; 0 outside).
    pub von_mises_mpa: Vec<f32>,
    /// Displacement magnitude per cell, mm.
    pub displacement_mm: Vec<f32>,
    /// 1 where the cell is inside the part.
    pub mask: Vec<u8>,
    /// Range of von Mises over solid cells.
    pub von_mises_range: [f64; 2],
    /// Range of displacement over solid cells.
    pub displacement_range: [f64; 2],
}

/// Solve result.
#[derive(Debug, Clone, Serialize)]
pub struct SolveResult {
    /// Analysed part.
    pub part_index: usize,
    /// Part name.
    pub part_name: String,
    /// Material label used.
    pub material: String,
    /// Elastic constants used.
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio used.
    pub poisson: f64,
    /// Yield strength used.
    pub yield_strength_mpa: Option<f64>,
    /// Finest-level max von Mises, MPa (the reported answer).
    pub max_von_mises_mpa: f64,
    /// Where it occurs.
    pub max_stress_at: [f64; 3],
    /// Finest-level max displacement, mm.
    pub max_displacement_mm: f64,
    /// Where it occurs.
    pub max_displacement_at: [f64; 3],
    /// Safety factor (only when converged and a yield was given).
    pub safety_factor: Option<f64>,
    /// True when the refinement study passed its gate.
    pub converged: bool,
    /// Gate failure reasons, when not converged.
    pub reasons: Vec<String>,
    /// The full kernel study (levels, deltas, thin-wall diagnosis).
    pub study: fea::convergence::ConvergedAnalysis,
    /// Regions the picks resolved to (loads then supports), mm.
    pub load_regions: Vec<fea::spec::RegionBox>,
    /// Support regions, mm.
    pub support_regions: Vec<fea::spec::RegionBox>,
    /// Notes about approximations made (non-planar face boxes, …).
    pub notes: Vec<String>,
    /// Overlay fields, when requested.
    pub field: Option<CellField>,
}

/// Face ordinal → `(bbox, is_axis_aligned_plane)`.
type FaceBoxes = HashMap<u32, (fea::spec::RegionBox, bool)>;

/// `(bbox, is_axis_aligned_plane)` per face ordinal.
fn face_boxes(solid: &vcad_kernel::Solid) -> Result<FaceBoxes, String> {
    let brep = solid
        .as_brep()
        .ok_or("part has no B-rep faces to pick (mesh-only) — use explicit regions")?;
    let ord = shell_ordinals(brep);
    let mut out = HashMap::new();
    for (fid, _kind, m) in tessellate_brep_by_face(brep, &TessellationParams::from_segments(48)) {
        let Some(&o) = ord.get(&fid) else { continue };
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for v in m.vertices.as_chunks::<3>().0 {
            for a in 0..3 {
                min[a] = min[a].min(v[a] as f64);
                max[a] = max[a].max(v[a] as f64);
            }
        }
        if min[0].is_finite() {
            let flat = (0..3).any(|a| (max[a] - min[a]).abs() < 1e-6);
            out.insert(o, (fea::spec::RegionBox { min, max }, flat));
        }
    }
    Ok(out)
}

fn resolve_target(
    t: &Target,
    boxes: &mut Option<Result<FaceBoxes, String>>,
    solid: &vcad_kernel::Solid,
    what: &str,
    notes: &mut Vec<String>,
) -> Result<Vec<fea::spec::RegionBox>, String> {
    let mut out: Vec<_> = t.region.into_iter().collect();
    if !t.faces.is_empty() {
        let map = boxes
            .get_or_insert_with(|| face_boxes(solid))
            .as_ref()
            .map_err(Clone::clone)?;
        for f in &t.faces {
            let (b, flat) = map
                .get(f)
                .ok_or_else(|| format!("{what}: face {f} is not on this part"))?;
            if !flat {
                notes.push(format!(
                    "{what}: face {f} is not an axis-aligned plane — selected by its bounding box"
                ));
            }
            out.push(*b);
        }
    }
    if out.is_empty() {
        return Err(format!("{what}: pick at least one face"));
    }
    Ok(out)
}

/// Resample nodal fields onto the lattice cell grid.
pub fn cell_field(f: &fea::solve::NodeFields) -> Option<CellField> {
    let h = f.h_mm;
    if f.nodes.is_empty() || h.is_nan() || h <= 0.0 {
        return None;
    }
    let mut origin = [f64::INFINITY; 3];
    for n in &f.nodes {
        for a in 0..3 {
            origin[a] = origin[a].min(n[a]);
        }
    }
    let idx = |p: &[f64; 3]| [0, 1, 2].map(|a| ((p[a] - origin[a]) / h).round() as usize);
    let mut nn = [0usize; 3];
    for n in &f.nodes {
        let i = idx(n);
        for a in 0..3 {
            nn[a] = nn[a].max(i[a] + 1);
        }
    }
    let (nx, ny, nz) = (
        nn[0].saturating_sub(1).max(1),
        nn[1].saturating_sub(1).max(1),
        nn[2].saturating_sub(1).max(1),
    );
    let mut node_at = vec![u32::MAX; nn[0] * nn[1] * nn[2]];
    for (k, n) in f.nodes.iter().enumerate() {
        let i = idx(n);
        node_at[(i[2] * nn[1] + i[1]) * nn[0] + i[0]] = k as u32;
    }
    let cells = nx * ny * nz;
    let mut vm = vec![0f32; cells];
    let mut disp = vec![0f32; cells];
    let mut mask = vec![0u8; cells];
    let (mut vr, mut dr) = (
        [f64::INFINITY, f64::NEG_INFINITY],
        [f64::INFINITY, f64::NEG_INFINITY],
    );
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let mut sv = 0.0;
                let mut sd = 0.0;
                let mut all = true;
                for c in 0..8 {
                    let (di, dj, dk) = (c & 1, (c >> 1) & 1, (c >> 2) & 1);
                    let (ii, jj, kk) = (i + di, j + dj, k + dk);
                    if ii >= nn[0] || jj >= nn[1] || kk >= nn[2] {
                        all = false;
                        break;
                    }
                    let n = node_at[(kk * nn[1] + jj) * nn[0] + ii];
                    if n == u32::MAX {
                        all = false;
                        break;
                    }
                    sv += f.von_mises_mpa[n as usize];
                    sd += f.displacement_mm[n as usize];
                }
                if !all {
                    continue;
                }
                let c = (k * ny + j) * nx + i;
                let (v, d) = (sv / 8.0, sd / 8.0);
                vm[c] = v as f32;
                disp[c] = d as f32;
                mask[c] = 1;
                vr = [vr[0].min(v), vr[1].max(v)];
                dr = [dr[0].min(d), dr[1].max(d)];
            }
        }
    }
    if !vr[0].is_finite() {
        return None;
    }
    Some(CellField {
        dims_zyx: [nz, ny, nx],
        origin_mm: origin,
        voxel_mm: [h; 3],
        von_mises_mpa: vm,
        displacement_mm: disp,
        mask,
        von_mises_range: vr,
        displacement_range: dr,
    })
}

/// Build the kernel spec + surface mesh for a request (shared with tests so
/// they can compare against the kernel run directly).
pub fn prepare(
    doc: &vcad_ir::Document,
    req: &SolveReq,
) -> Result<(fea::spec::FeaSpec, TriangleMesh, SolveResultHead), String> {
    let scene = evaluate(doc)?;
    let roots = part_roots(doc, &scene);
    let part = scene.parts.get(req.part_index).ok_or_else(|| {
        format!(
            "part_index {} out of range ({} parts)",
            req.part_index,
            scene.parts.len()
        )
    })?;
    let pname = part_name(doc, roots[req.part_index], req.part_index);
    let (label, e, nu, ys) = match (&req.elastic, &req.material) {
        (Some(m), _) => (
            "custom".to_string(),
            m.youngs_modulus_mpa,
            m.poisson,
            m.yield_strength_mpa,
        ),
        (None, Some(id)) => {
            let p = MATERIALS
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| format!("unknown material preset `{id}`"))?;
            (
                p.label.to_string(),
                p.youngs_modulus_mpa,
                p.poisson,
                Some(p.yield_strength_mpa),
            )
        }
        (None, None) => {
            let p = preset_for(&part.material);
            (
                p.label.to_string(),
                p.youngs_modulus_mpa,
                p.poisson,
                Some(p.yield_strength_mpa),
            )
        }
    };
    let finest = req
        .resolution
        .saturating_mul(1usize << req.levels.saturating_sub(1).min(8))
        .min(256);
    if finest > APP_RESOLUTION_CAP {
        return Err(format!(
            "finest lattice level would be {finest} cells (cap {APP_RESOLUTION_CAP}) — lower resolution or levels"
        ));
    }
    let mut notes = Vec::new();
    let mut boxes = None;
    let solid = part.solid.as_ref();
    let need_solid = || solid.ok_or("part has no solid — use explicit regions".to_string());
    let mut loads = Vec::new();
    let mut load_regions = Vec::new();
    for (n, l) in req.loads.iter().enumerate() {
        let regions = if l.target.faces.is_empty() {
            l.target.region.into_iter().collect::<Vec<_>>()
        } else {
            resolve_target(
                &l.target,
                &mut boxes,
                need_solid()?,
                &format!("load {}", n + 1),
                &mut notes,
            )?
        };
        if regions.is_empty() {
            return Err(format!("load {}: pick at least one face", n + 1));
        }
        // The force is the total over the load, split across its regions.
        let share = 1.0 / regions.len() as f64;
        for r in regions {
            loads.push(fea::spec::Load {
                region: r,
                force: l.force.map(|c| c * share),
            });
            load_regions.push(r);
        }
    }
    let mut supports = Vec::new();
    let mut support_regions = Vec::new();
    for (n, s) in req.supports.iter().enumerate() {
        let regions = if s.target.faces.is_empty() {
            s.target.region.into_iter().collect::<Vec<_>>()
        } else {
            resolve_target(
                &s.target,
                &mut boxes,
                need_solid()?,
                &format!("support {}", n + 1),
                &mut notes,
            )?
        };
        if regions.is_empty() {
            return Err(format!("support {}: pick at least one face", n + 1));
        }
        for r in regions {
            supports.push(fea::spec::Support {
                region: r,
                fix: s.fix,
            });
            support_regions.push(r);
        }
    }
    let spec = fea::spec::FeaSpec {
        resolution: req.resolution,
        youngs_modulus_mpa: e,
        poisson: nu,
        yield_strength_mpa: ys,
        loads,
        supports,
    };
    let mut mesh = TriangleMesh::new();
    match solid {
        Some(s) => {
            let m = s.to_mesh(48);
            mesh.vertices = m.vertices;
            mesh.indices = m.indices;
        }
        None => {
            mesh.vertices = part.mesh.positions.clone();
            mesh.indices = part.mesh.indices.clone();
        }
    }
    Ok((
        spec,
        mesh,
        SolveResultHead {
            part_name: pname,
            material: label,
            load_regions,
            support_regions,
            notes,
        },
    ))
}

/// Pieces of the result known before the solve.
pub struct SolveResultHead {
    part_name: String,
    material: String,
    load_regions: Vec<fea::spec::RegionBox>,
    support_regions: Vec<fea::spec::RegionBox>,
    notes: Vec<String>,
}

/// Convergence options the app runs with.
pub fn app_convergence(levels: usize) -> fea::convergence::ConvergenceOptions {
    fea::convergence::ConvergenceOptions {
        levels,
        resolution_cap: APP_RESOLUTION_CAP,
        ..Default::default()
    }
}

/// Mesh and solve.
pub fn solve(doc: &vcad_ir::Document, req: &SolveReq) -> Result<SolveResult, String> {
    solve_with(doc, req, &mut |_| true)
}

/// Mesh and solve, reporting each study step; `on_step` returning `false`
/// cancels (the error is `"FEA study cancelled"`).
pub fn solve_with(
    doc: &vcad_ir::Document,
    req: &SolveReq,
    on_step: &mut dyn FnMut(fea::convergence::StudyStep) -> bool,
) -> Result<SolveResult, String> {
    let (spec, mesh, head) = prepare(doc, req)?;
    let (study, fields) = fea::convergence::analyze_converged_fields_with(
        &mesh,
        &spec,
        &app_convergence(req.levels),
        &fea::solve::SolveOptions::default(),
        on_step,
    )
    .map_err(|e| e.to_string())?;
    let fine = study.finest().clone();
    let reasons = match &study.verdict {
        fea::convergence::ConvergenceVerdict::Converged => Vec::new(),
        fea::convergence::ConvergenceVerdict::Unverifiable { reasons } => reasons.clone(),
    };
    Ok(SolveResult {
        part_index: req.part_index,
        part_name: head.part_name,
        material: head.material,
        youngs_modulus_mpa: spec.youngs_modulus_mpa,
        poisson: spec.poisson,
        yield_strength_mpa: spec.yield_strength_mpa,
        max_von_mises_mpa: fine.max_von_mises_mpa,
        max_stress_at: fine.max_stress_at,
        max_displacement_mm: fine.max_displacement_mm,
        max_displacement_at: fine.max_displacement_at,
        safety_factor: study.safety_factor,
        converged: study.converged(),
        reasons,
        load_regions: head.load_regions,
        support_regions: head.support_regions,
        notes: head.notes,
        field: if req.fields {
            cell_field(&fields)
        } else {
            None
        },
        study,
    })
}

// ─── C ABI ───────────────────────────────────────────────────────────────

/// Solve a static structural case on one part of a document.
///
/// `request_json`: `{"part_index", "material"?, "elastic"?, "resolution"?,
/// "levels"?, "loads": [{"faces": [ids] | "region": {min,max}, "force": [N]}],
/// "supports": [{"faces"|"region", "fix"?: [bool;3]}], "fields"?}`.
/// Returns result JSON (free with `vcad_string_free`) or null with
/// `vcad_last_error` (fail-closed: an empty pick, a thin wall the lattice
/// can't resolve, a solver that didn't converge).
#[no_mangle]
pub extern "C" fn vcad_fea_solve(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_fea_solve", || {
        let run = || -> Result<SolveResult, String> {
            let doc = parse_doc("document", doc_json)?;
            let req: SolveReq = serde_json::from_value(parse_request(request_json)?)
                .map_err(|e| format!("request: {e}"))?;
            solve(&doc, &req)
        };
        match run() {
            Ok(r) => json_out(&r),
            Err(e) => {
                err::set_error(e);
                ptr::null_mut()
            }
        }
    })
}

/// Progress hook for [`vcad_fea_solve_progress`]: `(ctx, fraction 0…1,
/// level, levels, solving)`; return `false` to cancel.
pub type VcadFeaProgressFn = extern "C" fn(
    ctx: *mut std::ffi::c_void,
    fraction: f64,
    level: u32,
    levels: u32,
    solving: bool,
) -> bool;

/// [`vcad_fea_solve`] with a per-step progress hook that can cancel (a
/// cancelled study returns null with `vcad_last_error` = "FEA study
/// cancelled"). The hook runs on the calling thread.
#[no_mangle]
pub extern "C" fn vcad_fea_solve_progress(
    doc_json: *const c_char,
    request_json: *const c_char,
    progress: Option<VcadFeaProgressFn>,
    ctx: *mut std::ffi::c_void,
) -> *mut c_char {
    err::clear_error();
    let ctx = ctx as usize;
    guard_ptr("vcad_fea_solve_progress", || {
        let run = || -> Result<SolveResult, String> {
            let doc = parse_doc("document", doc_json)?;
            let req: SolveReq = serde_json::from_value(parse_request(request_json)?)
                .map_err(|e| format!("request: {e}"))?;
            solve_with(&doc, &req, &mut |s| match progress {
                Some(f) => f(
                    ctx as *mut std::ffi::c_void,
                    s.fraction(),
                    s.level as u32,
                    s.levels as u32,
                    s.solving,
                ),
                None => true,
            })
        };
        match run() {
            Ok(r) => json_out(&r),
            Err(e) => {
                err::set_error(e);
                ptr::null_mut()
            }
        }
    })
}

/// Material presets as JSON (`[{id, label, youngs_modulus_mpa, poisson,
/// yield_strength_mpa}]`).
#[no_mangle]
pub extern "C" fn vcad_fea_materials() -> *mut c_char {
    json_out(&MATERIALS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;
    use serde_json::Value;
    use std::ffi::CString;

    /// 100 × 10 × 10 mm aluminum cantilever.
    fn bracket() -> vcad_ir::Document {
        vcad_loon::eval_vcad("[root [cube 100 10 10] \"aluminum\"]", None).unwrap()
    }

    /// Face ordinal whose box is the plane `axis = value`.
    fn face_on(doc: &vcad_ir::Document, axis: usize, value: f64) -> u32 {
        let scene = evaluate(doc).unwrap();
        let boxes = face_boxes(scene.parts[0].solid.as_ref().unwrap()).unwrap();
        *boxes
            .iter()
            .find(|(_, (b, _))| {
                (b.min[axis] - value).abs() < 1e-6 && (b.max[axis] - value).abs() < 1e-6
            })
            .expect("face")
            .0
    }

    fn cantilever_req(doc: &vcad_ir::Document) -> SolveReq {
        let fixed = face_on(doc, 0, 0.0);
        let tip = face_on(doc, 0, 100.0);
        serde_json::from_value(serde_json::json!({
            "part_index": 0,
            "resolution": 20,
            "levels": 2,
            "loads": [{ "faces": [tip], "force": [0.0, 0.0, -100.0] }],
            "supports": [{ "faces": [fixed] }],
        }))
        .unwrap()
    }

    #[test]
    fn cantilever_matches_the_kernel_reference() {
        let doc = bracket();
        let req = cantilever_req(&doc);
        let app = solve(&doc, &req).unwrap();
        // Kernel reference: the same geometry and the same regions, built by
        // hand and run straight through vcad-kernel-fea.
        let scene = evaluate(&doc).unwrap();
        let mut mesh = TriangleMesh::new();
        let m = scene.parts[0].solid.as_ref().unwrap().to_mesh(48);
        mesh.vertices = m.vertices;
        mesh.indices = m.indices;
        let spec = fea::spec::FeaSpec {
            resolution: 20,
            youngs_modulus_mpa: 69_000.0,
            poisson: 0.33,
            yield_strength_mpa: Some(276.0),
            loads: vec![fea::spec::Load {
                region: fea::spec::RegionBox {
                    min: [100.0, 0.0, 0.0],
                    max: [100.0, 10.0, 10.0],
                },
                force: [0.0, 0.0, -100.0],
            }],
            supports: vec![fea::spec::Support {
                region: fea::spec::RegionBox {
                    min: [0.0, 0.0, 0.0],
                    max: [0.0, 10.0, 10.0],
                },
                fix: [true; 3],
            }],
        };
        let reference = fea::convergence::analyze_converged(
            &mesh,
            &spec,
            &app_convergence(2),
            &fea::solve::SolveOptions::default(),
        )
        .unwrap();
        let rel = |a: f64, b: f64| (a - b).abs() / b.abs();
        assert!(rel(app.max_von_mises_mpa, reference.finest().max_von_mises_mpa) < 1e-9);
        assert!(
            rel(
                app.max_displacement_mm,
                reference.finest().max_displacement_mm
            ) < 1e-9
        );
        // And physically sane against Euler–Bernoulli: σ = F·L·c / I = 60 MPa,
        // δ = F·L³ / (3·E·I) ≈ 0.580 mm. Linear tets on a lattice are stiff and
        // smear the root concentration, so the band is generous.
        let sigma = 100.0 * 100.0 * 5.0 / (10.0 * 1000.0 / 12.0);
        let delta = 100.0 * 1e6 / (3.0 * 69_000.0 * (10.0 * 1000.0 / 12.0));
        eprintln!(
            "cantilever: σ_max {:.2} MPa (beam {:.1}), δ_max {:.4} mm (beam {:.4}), converged {}",
            app.max_von_mises_mpa, sigma, app.max_displacement_mm, delta, app.converged
        );
        assert!(app.max_von_mises_mpa > 0.4 * sigma && app.max_von_mises_mpa < 2.5 * sigma);
        assert!(app.max_displacement_mm > 0.5 * delta && app.max_displacement_mm < 1.5 * delta);
        let f = app.field.as_ref().expect("field");
        let [nz, ny, nx] = f.dims_zyx;
        assert_eq!(f.mask.len(), nx * ny * nz);
        assert!(f.mask.iter().filter(|&&m| m == 1).count() > nx * ny * nz / 2);
        assert!(
            f.von_mises_range[1] > 0.0 && f.von_mises_range[1] <= app.max_von_mises_mpa * 1.0001
        );
    }

    #[test]
    #[ignore = "resolution probe; run by hand"]
    fn resolution_probe() {
        let doc = bracket();
        for res in [12, 16, 24, 32, 48] {
            let mut req = cantilever_req(&doc);
            req.resolution = res;
            req.fields = false;
            let t = std::time::Instant::now();
            let r = solve(&doc, &req).unwrap();
            eprintln!(
                "res {res}: σ {:.2} δ {:.4} Δσ {:.3} Δδ {:.3} conv {} {:?} ({:.2}s)",
                r.max_von_mises_mpa,
                r.max_displacement_mm,
                r.study.stress_change_rel,
                r.study.displacement_change_rel,
                r.converged,
                r.reasons,
                t.elapsed().as_secs_f64()
            );
        }
    }

    #[test]
    fn empty_picks_fail_closed() {
        let doc = bracket();
        let mut req = cantilever_req(&doc);
        req.supports.clear();
        assert!(solve(&doc, &req).is_err());
        let mut req = cantilever_req(&doc);
        req.loads[0].target.faces = vec![999];
        assert!(solve(&doc, &req).unwrap_err().contains("not on this part"));
    }

    #[test]
    fn c_abi_round_trip() {
        let doc = bracket();
        let req = serde_json::json!({
            "part_index": 0, "resolution": 10, "levels": 2, "fields": false,
            "loads": [{ "region": { "min": [100,0,0], "max": [100,10,10] }, "force": [0,0,-50] }],
            "supports": [{ "region": { "min": [0,0,0], "max": [0,10,10] } }],
        });
        let dj = CString::new(serde_json::to_string(&doc).unwrap()).unwrap();
        let rj = CString::new(req.to_string()).unwrap();
        let out: Value =
            serde_json::from_str(&take(vcad_fea_solve(dj.as_ptr(), rj.as_ptr()))).unwrap();
        assert!(out["max_von_mises_mpa"].as_f64().unwrap() > 0.0);
        assert_eq!(out["material"], "Aluminum 6061-T6");
        let mats: Value = serde_json::from_str(&take(vcad_fea_materials())).unwrap();
        assert!(mats.as_array().unwrap().len() >= 5);
    }
}
