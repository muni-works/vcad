//! How parts sound.
//!
//! The bridge between a vcad document and its acoustics:
//!
//! - [`part_modes`] — natural frequencies of any evaluated part (the solid
//!   modal solver in [`vcad_kernel_fea::modal`]), each named as a note, with
//!   its mode shape interpolated onto the part's **render vertices** so a
//!   viewer can animate the part flexing and draw its still regions.
//! - [`strike_gains`] — how loudly each mode rings when the part is struck
//!   at a vertex in a direction: the mode shape at the strike point,
//!   filtered by the mallet's contact spectrum. Struck on a node, a mode
//!   stays silent; struck at an antinode, it rings out.
//! - [`solve_pitch_lock`] — the pitch constraint: hold mode `n` of a part at
//!   a target frequency by solving one document parameter (Newton on
//!   `ln f` vs `ln p` through the real geometry). A design drawn with a
//!   length parameter and a pitch lock retunes itself when its width,
//!   thickness or material change.
//!
//! Materials come from the part's material key (aluminium, steel, brass,
//! bronze, copper, titanium, PLA, PETG, ABS, nylon, glass, wood) — elastic
//! constants are not in the IR yet, so an unknown key falls back to
//! aluminium and the result says so.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use vcad_eval::{evaluate_document, EvalOptions, EvaluatedScene};
use vcad_ir::Document;
use vcad_kernel_fea::modal::{modal_analysis, ModalResult, ModalSpec};
use vcad_kernel_tessellate::TriangleMesh;

/// Elastic constants for a part.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SoundMaterial {
    /// Display name.
    pub name: &'static str,
    /// Young's modulus, MPa.
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio.
    pub poisson: f64,
    /// Density, kg/m³.
    pub density_kg_m3: f64,
}

const MATERIALS: &[(&[&str], SoundMaterial)] = &[
    (
        &["stainless"],
        SoundMaterial {
            name: "stainless steel",
            youngs_modulus_mpa: 193_000.0,
            poisson: 0.29,
            density_kg_m3: 8000.0,
        },
    ),
    (
        &["steel", "iron"],
        SoundMaterial {
            name: "steel",
            youngs_modulus_mpa: 200_000.0,
            poisson: 0.29,
            density_kg_m3: 7850.0,
        },
    ),
    (
        &["brass"],
        SoundMaterial {
            name: "brass",
            youngs_modulus_mpa: 97_000.0,
            poisson: 0.34,
            density_kg_m3: 8500.0,
        },
    ),
    (
        &["bronze"],
        SoundMaterial {
            name: "bronze",
            youngs_modulus_mpa: 110_000.0,
            poisson: 0.34,
            density_kg_m3: 8800.0,
        },
    ),
    (
        &["copper"],
        SoundMaterial {
            name: "copper",
            youngs_modulus_mpa: 117_000.0,
            poisson: 0.34,
            density_kg_m3: 8960.0,
        },
    ),
    (
        &["titan"],
        SoundMaterial {
            name: "titanium",
            youngs_modulus_mpa: 114_000.0,
            poisson: 0.34,
            density_kg_m3: 4430.0,
        },
    ),
    (
        &["petg"],
        SoundMaterial {
            name: "PETG",
            youngs_modulus_mpa: 2_100.0,
            poisson: 0.38,
            density_kg_m3: 1270.0,
        },
    ),
    (
        &["pla"],
        SoundMaterial {
            name: "PLA",
            youngs_modulus_mpa: 3_500.0,
            poisson: 0.36,
            density_kg_m3: 1240.0,
        },
    ),
    (
        &["abs"],
        SoundMaterial {
            name: "ABS",
            youngs_modulus_mpa: 2_200.0,
            poisson: 0.35,
            density_kg_m3: 1050.0,
        },
    ),
    (
        &["nylon", "pa12"],
        SoundMaterial {
            name: "nylon",
            youngs_modulus_mpa: 2_700.0,
            poisson: 0.39,
            density_kg_m3: 1140.0,
        },
    ),
    (
        &["glass"],
        SoundMaterial {
            name: "glass",
            youngs_modulus_mpa: 70_000.0,
            poisson: 0.22,
            density_kg_m3: 2500.0,
        },
    ),
    (
        &["wood", "maple", "rosewood", "oak"],
        SoundMaterial {
            name: "wood",
            youngs_modulus_mpa: 12_000.0,
            poisson: 0.35,
            density_kg_m3: 700.0,
        },
    ),
    (&["alum", "6061", "7075"], ALUMINIUM),
];

const ALUMINIUM: SoundMaterial = SoundMaterial {
    name: "aluminium",
    youngs_modulus_mpa: 68_900.0,
    poisson: 0.33,
    density_kg_m3: 2700.0,
};

/// Look a material up by key or display name. `None` when nothing matches.
pub fn material_for_key(key: &str) -> Option<SoundMaterial> {
    let k = key.to_ascii_lowercase();
    MATERIALS
        .iter()
        .find(|(keys, _)| keys.iter().any(|needle| k.contains(needle)))
        .map(|(_, m)| *m)
}

/// The material of visible part `part` (key, then its display name).
fn part_material(doc: &Document, part: usize) -> (SoundMaterial, bool) {
    let Some(entry) = visible_roots(doc).nth(part) else {
        return (ALUMINIUM, false);
    };
    let key = &entry.material;
    let display = doc
        .materials
        .get(key)
        .map(|m| m.name.as_str())
        .unwrap_or("");
    match material_for_key(key).or_else(|| material_for_key(display)) {
        Some(m) => (m, true),
        None => (ALUMINIUM, false),
    }
}

fn visible_roots(doc: &Document) -> impl Iterator<Item = &vcad_ir::SceneEntry> {
    doc.roots.iter().filter(|e| e.visible != Some(false))
}

/// Name of visible part `part` (its root node's name, else `part<i>`).
pub fn part_name(doc: &Document, part: usize) -> String {
    visible_roots(doc)
        .nth(part)
        .and_then(|e| doc.nodes.get(&e.root))
        .and_then(|n| n.name.clone())
        .unwrap_or_else(|| format!("part{part}"))
}

/// Index of the visible part named `name`.
pub fn part_index(doc: &Document, name: &str) -> Option<usize> {
    (0..visible_roots(doc).count()).find(|&i| part_name(doc, i) == name)
}

/// Mesh fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Fast enough to answer a click (~0.1–1 s): two cells through the
    /// thinnest wall, ≤ ~6k cells.
    #[default]
    Preview,
    /// Converged numbers: three cells through the thinnest wall, ≤ ~40k cells.
    Fine,
}

/// Request for [`part_modes`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModesRequest {
    /// Visible part index.
    pub part: usize,
    /// Elastic modes wanted (default 6).
    #[serde(default = "default_modes")]
    pub modes: usize,
    /// Mesh fidelity.
    #[serde(default)]
    pub quality: Quality,
    /// Material override (key or name), else the part's own.
    #[serde(default)]
    pub material: Option<String>,
    /// Interpolate mode shapes onto render vertices (default true).
    #[serde(default = "yes")]
    pub shapes: bool,
}

fn default_modes() -> usize {
    6
}
fn yes() -> bool {
    true
}

/// One mode of a part.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartMode {
    /// Natural frequency, Hz.
    pub hz: f64,
    /// Nearest note (12-TET, A4 = 440).
    pub note: String,
    /// Offset from that note, cents.
    pub cents: f64,
    /// Displacement per animation-mesh vertex (x, y, z interleaved), max |u| = 1.
    /// Empty when shapes were not requested.
    pub shape: Vec<f32>,
}

/// The modes of one part.
#[derive(Debug, Clone, Serialize)]
pub struct PartModes {
    /// Visible part index.
    pub part: usize,
    /// Part name.
    pub name: String,
    /// Material used.
    pub material: SoundMaterial,
    /// False when the part's material key was not recognized and aluminium
    /// was assumed.
    pub material_known: bool,
    /// Elastic modes, ascending.
    pub modes: Vec<PartMode>,
    /// Animation mesh the shapes live on: the part's render mesh with every
    /// triangle subdivided until its edges are no longer than one solver cell
    /// (a box's render mesh has only corner vertices, which can't bend).
    /// Positions in mm, the document's kernel frame.
    pub positions: Vec<f32>,
    /// Animation mesh triangles.
    pub indices: Vec<u32>,
    /// Animation mesh vertex count (= positions.len() / 3).
    pub vertices: usize,
    /// Solver cell size, mm.
    pub pitch_mm: [f64; 3],
    /// Solver DOFs.
    pub dofs: usize,
    /// Part mass, kg.
    pub mass_kg: f64,
    /// Wall time, ms.
    pub ms: f64,
}

/// Sound-analysis failures.
#[derive(Debug, Clone, PartialEq)]
pub enum SoundError {
    /// The document did not evaluate.
    Eval(String),
    /// No such part.
    NoPart(String),
    /// The modal solve failed.
    Modal(String),
    /// Bad pitch-lock request.
    Lock(String),
}

impl std::fmt::Display for SoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoundError::Eval(s) => write!(f, "evaluation failed: {s}"),
            SoundError::NoPart(s) => write!(f, "no part {s}"),
            SoundError::Modal(s) => write!(f, "modal analysis failed: {s}"),
            SoundError::Lock(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for SoundError {}

/// Evaluate a document for sound analysis.
pub fn evaluate(doc: &Document) -> Result<EvaluatedScene, SoundError> {
    let opts = EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    };
    evaluate_document(doc, &opts).map_err(|e| SoundError::Eval(e.to_string()))
}

fn tri_mesh(positions: &[f32], indices: &[u32]) -> TriangleMesh {
    let mut m = TriangleMesh::new();
    m.vertices = positions.to_vec();
    m.indices = indices.to_vec();
    m
}

/// Cell size for a part: enough cells through its thinnest wall, capped so
/// the solve stays interactive.
fn pitch_for(mesh: &TriangleMesh, quality: Quality) -> (usize, [f64; 3]) {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for v in mesh.vertices.as_chunks::<3>().0.iter() {
        for a in 0..3 {
            lo[a] = lo[a].min(v[a] as f64);
            hi[a] = hi[a].max(v[a] as f64);
        }
    }
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let longest = size.iter().cloned().fold(0.0, f64::max).max(1e-6);
    let thin = vcad_kernel_fea::mesh::wall_thickness(mesh, 24)
        .map(|t| t.p05_mm.max(t.min_mm))
        .unwrap_or_else(|_| size.iter().cloned().fold(f64::INFINITY, f64::min));
    let (through, cap) = match quality {
        Quality::Preview => (2.0, 6_000.0),
        Quality::Fine => (3.0, 40_000.0),
    };
    let mut h = (thin / through).max(longest / 160.0);
    // Cell budget: volume of the bbox / h³ overestimates; scale up h until
    // the bbox cell count fits (the solid fills less, so this is safe).
    let bbox_cells = |h: f64| {
        size.iter()
            .map(|s| (s / h).ceil().max(1.0))
            .product::<f64>()
    };
    while bbox_cells(h) > cap * 4.0 {
        h *= 1.15;
    }
    let res = (longest / h).round().max(2.0) as usize;
    (res, [h, h, h])
}

/// Natural frequencies and (optionally) render-vertex mode shapes of a part.
pub fn part_modes(
    doc: &Document,
    scene: &EvaluatedScene,
    req: &ModesRequest,
) -> Result<PartModes, SoundError> {
    let t0 = std::time::Instant::now();
    let part = scene
        .parts
        .get(req.part)
        .ok_or_else(|| SoundError::NoPart(format!("index {}", req.part)))?;
    let (material, material_known) = match req.material.as_deref() {
        Some(k) => material_for_key(k).map_or((ALUMINIUM, false), |m| (m, true)),
        None => part_material(doc, req.part),
    };
    let mesh = tri_mesh(&part.mesh.positions, &part.mesh.indices);
    let (resolution, pitch) = pitch_for(&mesh, req.quality);
    let spec = ModalSpec {
        resolution,
        max_pitch_mm: Some(pitch),
        youngs_modulus_mpa: material.youngs_modulus_mpa,
        poisson: material.poisson,
        density_kg_m3: material.density_kg_m3,
        supports: vec![],
        modes: req.modes.clamp(1, 24),
    };
    let r = modal_analysis(&mesh, &spec).map_err(|e| SoundError::Modal(e.to_string()))?;
    let (anim_pos, anim_idx) = if req.shapes {
        refine(
            &part.mesh.positions,
            &part.mesh.indices,
            r.pitch[0].min(r.pitch[1]).min(r.pitch[2]),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let shapes = if req.shapes {
        to_render_vertices(&r, &anim_pos)
    } else {
        vec![Vec::new(); r.modes.len()]
    };
    let modes = r
        .modes
        .iter()
        .zip(shapes)
        .map(|(m, shape)| {
            let (note, cents) = vcad_kernel_acoustics::instrument::nearest_note(m.hz);
            PartMode {
                hz: m.hz,
                note,
                cents,
                shape,
            }
        })
        .collect();
    Ok(PartModes {
        part: req.part,
        name: part_name(doc, req.part),
        material,
        material_known,
        modes,
        vertices: anim_pos.len() / 3,
        positions: anim_pos,
        indices: anim_idx,
        pitch_mm: r.pitch,
        dofs: r.dofs,
        mass_kg: r.mass_kg,
        ms: t0.elapsed().as_secs_f64() * 1e3,
    })
}

/// Subdivide triangles (longest-edge midpoint splits, shared across
/// neighbours) until every edge is at most `max_edge` mm, capped at ~200k
/// triangles.
fn refine(positions: &[f32], indices: &[u32], max_edge: f64) -> (Vec<f32>, Vec<u32>) {
    let mut pos: Vec<[f32; 3]> = positions
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    let mut tris: Vec<[u32; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
    let len2 = |a: [f32; 3], b: [f32; 3]| {
        let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f64
    };
    let lim = max_edge * max_edge;
    for _ in 0..24 {
        if tris.len() > 200_000 {
            break;
        }
        let mut changed = false;
        let mut next = Vec::with_capacity(tris.len() * 2);
        for t in tris {
            let e = [
                len2(pos[t[0] as usize], pos[t[1] as usize]),
                len2(pos[t[1] as usize], pos[t[2] as usize]),
                len2(pos[t[2] as usize], pos[t[0] as usize]),
            ];
            let (k, worst) = e
                .iter()
                .enumerate()
                .fold((0, 0.0), |b, (i, &v)| if v > b.1 { (i, v) } else { b });
            if worst <= lim {
                next.push(t);
                continue;
            }
            changed = true;
            let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
            let key = (a.min(b), a.max(b));
            let m = *mid.entry(key).or_insert_with(|| {
                let (pa, pb) = (pos[a as usize], pos[b as usize]);
                pos.push([
                    (pa[0] + pb[0]) / 2.0,
                    (pa[1] + pb[1]) / 2.0,
                    (pa[2] + pb[2]) / 2.0,
                ]);
                (pos.len() - 1) as u32
            });
            next.push([a, m, c]);
            next.push([m, b, c]);
        }
        tris = next;
        if !changed {
            break;
        }
    }
    (
        pos.into_iter().flatten().collect(),
        tris.into_iter().flatten().collect(),
    )
}

/// Interpolate lattice-node mode shapes onto render vertices: average of the
/// lattice nodes at the corners of the vertex's cell that exist (the
/// staircased solid may not cover every corner), widening the search one
/// cell when none do.
fn to_render_vertices(r: &ModalResult, positions: &[f32]) -> Vec<Vec<f32>> {
    let p = r.pitch;
    let mut lo = [f64::INFINITY; 3];
    for x in &r.nodes {
        for a in 0..3 {
            lo[a] = lo[a].min(x[a]);
        }
    }
    let key = |x: &[f64; 3]| -> (i64, i64, i64) {
        (
            ((x[0] - lo[0]) / p[0]).round() as i64,
            ((x[1] - lo[1]) / p[1]).round() as i64,
            ((x[2] - lo[2]) / p[2]).round() as i64,
        )
    };
    let lattice: HashMap<(i64, i64, i64), usize> = r
        .nodes
        .iter()
        .enumerate()
        .map(|(i, x)| (key(x), i))
        .collect();
    // For each render vertex: (node index, weight) list.
    let weights: Vec<Vec<(usize, f64)>> = positions
        .as_chunks::<3>()
        .0
        .iter()
        .map(|v| {
            let g = [
                (v[0] as f64 - lo[0]) / p[0],
                (v[1] as f64 - lo[1]) / p[1],
                (v[2] as f64 - lo[2]) / p[2],
            ];
            for reach in 0..3i64 {
                let mut out = Vec::new();
                let base = [
                    g[0].floor() as i64,
                    g[1].floor() as i64,
                    g[2].floor() as i64,
                ];
                for dx in -reach..=1 + reach {
                    for dy in -reach..=1 + reach {
                        for dz in -reach..=1 + reach {
                            let k = (base[0] + dx, base[1] + dy, base[2] + dz);
                            if let Some(&n) = lattice.get(&k) {
                                let d2 = (k.0 as f64 - g[0]).powi(2)
                                    + (k.1 as f64 - g[1]).powi(2)
                                    + (k.2 as f64 - g[2]).powi(2);
                                out.push((n, 1.0 / (d2 + 1e-3)));
                            }
                        }
                    }
                }
                if !out.is_empty() {
                    let s: f64 = out.iter().map(|w| w.1).sum();
                    out.iter_mut().for_each(|w| w.1 /= s);
                    return out;
                }
            }
            Vec::new()
        })
        .collect();
    r.modes
        .iter()
        .map(|m| {
            let mut out = vec![0f32; positions.len()];
            for (vi, ws) in weights.iter().enumerate() {
                for &(n, w) in ws {
                    for a in 0..3 {
                        out[3 * vi + a] += (w * m.shape[3 * n + a]) as f32;
                    }
                }
            }
            let peak = out.iter().fold(0f32, |a, x| a.max(x.abs()));
            if peak > 0.0 {
                out.iter_mut().for_each(|x| *x /= peak);
            }
            out
        })
        .collect()
}

/// Relative loudness of each mode when the part is struck at render vertex
/// `vertex` along `direction` (unit, mm frame) with a mallet of contact time
/// `contact_ms`: |φ(vertex)·direction| × the half-sine contact spectrum.
/// Normalized so the loudest mode is 1.
pub fn strike_gains(
    modes: &PartModes,
    vertex: usize,
    direction: [f64; 3],
    contact_ms: f64,
) -> Vec<f64> {
    let mut g: Vec<f64> = modes
        .modes
        .iter()
        .map(|m| {
            let s = &m.shape;
            if s.len() < 3 * (vertex + 1) {
                return 0.0;
            }
            let u = [
                s[3 * vertex] as f64,
                s[3 * vertex + 1] as f64,
                s[3 * vertex + 2] as f64,
            ];
            let along = (u[0] * direction[0] + u[1] * direction[1] + u[2] * direction[2]).abs();
            along * vcad_kernel_acoustics::strike::mallet_spectrum(m.hz, contact_ms)
        })
        .collect();
    let top = g.iter().cloned().fold(0.0, f64::max);
    if top > 0.0 {
        g.iter_mut().for_each(|x| *x /= top);
    }
    g
}

// ─── Pitch lock ───────────────────────────────────────────────────────────

pub use vcad_ir::instrument::PitchLock;

/// One Newton step of a pitch solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockStep {
    /// Parameter value.
    pub value: f64,
    /// Mode frequency there, Hz.
    pub hz: f64,
    /// Error vs target, cents.
    pub cents: f64,
}

/// Result of [`solve_pitch_lock`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockReport {
    /// Solved parameter value.
    pub value: f64,
    /// Parameter value before.
    pub before: f64,
    /// Final mode frequency, Hz.
    pub hz: f64,
    /// Final error, cents.
    pub cents: f64,
    /// Within tolerance.
    pub converged: bool,
    /// Iterates.
    pub steps: Vec<LockStep>,
}

fn param_value(doc: &Document, name: &str) -> Result<f64, SoundError> {
    let env = vcad_ir::parameters::resolve_parameters(&doc.parameters)
        .map_err(|e| SoundError::Lock(format!("parameters: {e}")))?;
    env.get(name)
        .copied()
        .ok_or_else(|| SoundError::Lock(format!("no parameter {name:?} to drive")))
}

fn with_param(doc: &Document, name: &str, v: f64) -> Document {
    let mut d = doc.clone();
    if let Some(p) = d.parameters.get_mut(name) {
        p.value = vcad_ir::parameters::Expr::Number(v);
    }
    d
}

fn mode_hz(doc: &Document, part: &str, mode: usize) -> Result<f64, SoundError> {
    let idx = part_index(doc, part).ok_or_else(|| SoundError::NoPart(part.to_string()))?;
    let scene = evaluate(doc)?;
    let pm = part_modes(
        doc,
        &scene,
        &ModesRequest {
            part: idx,
            modes: mode,
            quality: Quality::Fine,
            material: None,
            shapes: false,
        },
    )?;
    pm.modes
        .get(mode - 1)
        .map(|m| m.hz)
        .ok_or_else(|| SoundError::Modal(format!("mode {mode} not found")))
}

/// Solve `lock.drives` so mode `lock.mode` of `lock.part` rings at
/// `lock.target_hz`. Newton on `ln f(ln p)` with a central-difference slope
/// through the full evaluate → mesh → modal pipeline; steps are damped to
/// ±25 % and respect the parameter's min/max. Returns the patched document.
pub fn solve_pitch_lock(
    doc: &Document,
    lock: &PitchLock,
) -> Result<(Document, LockReport), SoundError> {
    if lock.target_hz.is_nan() || lock.target_hz <= 0.0 || lock.mode == 0 {
        return Err(SoundError::Lock(
            "target_hz must be positive and mode ≥ 1".into(),
        ));
    }
    let before = param_value(doc, &lock.drives)?;
    if before <= 0.0 {
        return Err(SoundError::Lock(format!(
            "parameter {:?} is {before}; a pitch lock drives a positive dimension",
            lock.drives
        )));
    }
    let bounds = doc
        .parameters
        .get(&lock.drives)
        .map(|p| (p.min, p.max))
        .unwrap_or((None, None));
    let clamp = |v: f64| {
        let v = bounds.0.map_or(v, |lo| v.max(lo));
        bounds.1.map_or(v, |hi| v.min(hi))
    };
    let cents = |hz: f64| 1200.0 * (hz / lock.target_hz).log2();
    let mut x = before;
    let mut steps = Vec::new();
    let mut hz = mode_hz(doc, &lock.part, lock.mode)?;
    steps.push(LockStep {
        value: x,
        hz,
        cents: cents(hz),
    });
    for _ in 0..12 {
        if cents(hz).abs() <= lock.tolerance_cents {
            break;
        }
        let h = 0.01;
        let up = mode_hz(
            &with_param(doc, &lock.drives, x * (1.0 + h)),
            &lock.part,
            lock.mode,
        )?;
        let dn = mode_hz(
            &with_param(doc, &lock.drives, x * (1.0 - h)),
            &lock.part,
            lock.mode,
        )?;
        let slope = (up.ln() - dn.ln()) / ((1.0 + h).ln() - (1.0 - h).ln());
        if !slope.is_finite() || slope.abs() < 1e-6 {
            return Err(SoundError::Lock(format!(
                "parameter {:?} does not change mode {} of {} — pick a dimension of that part",
                lock.drives, lock.mode, lock.part
            )));
        }
        let dln = ((lock.target_hz / hz).ln() / slope).clamp(-0.223, 0.223);
        x = clamp(x * dln.exp());
        hz = mode_hz(&with_param(doc, &lock.drives, x), &lock.part, lock.mode)?;
        steps.push(LockStep {
            value: x,
            hz,
            cents: cents(hz),
        });
    }
    let c = cents(hz);
    Ok((
        with_param(doc, &lock.drives, x),
        LockReport {
            value: x,
            before,
            hz,
            cents: c,
            converged: c.abs() <= lock.tolerance_cents,
            steps,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_doc(len: f64, material: &str) -> Document {
        let src = format!(
            "[defparam len {len} :unit \"mm\" :min 40 :max 300]\n[let bar [cube len 25 6]]\n[root bar \"{material}\"]\n"
        );
        vcad_loon::eval_vcad(&src, None).expect("loon")
    }

    #[test]
    fn materials_resolve_by_key() {
        assert_eq!(material_for_key("aluminum").unwrap().name, "aluminium");
        assert_eq!(material_for_key("Brass C360").unwrap().name, "brass");
        assert_eq!(material_for_key("pla").unwrap().name, "PLA");
        assert!(material_for_key("unobtainium").is_none());
    }

    #[test]
    fn bar_part_rings_near_beam_theory_and_shapes_align() {
        let doc = bar_doc(131.32, "aluminum");
        let scene = evaluate(&doc).unwrap();
        let pm = part_modes(
            &doc,
            &scene,
            &ModesRequest {
                part: 0,
                modes: 3,
                quality: Quality::Preview,
                material: None,
                shapes: true,
            },
        )
        .unwrap();
        assert!(pm.material_known);
        assert_eq!(pm.name, "bar");
        // Beam solver: 1793 Hz (Timoshenko); the solid converges to ~1796.
        let f1 = pm.modes[0].hz;
        assert!((f1 / 1796.3 - 1.0).abs() < 0.01, "f1 {f1}");
        assert_eq!(pm.modes[0].shape.len(), 3 * pm.vertices);
        // Fundamental: ends move most, middle second-most, nodes near 0.224 L barely.
        let pos = &pm.positions;
        let amp_at = |xf: f32| {
            let mut best = (f32::INFINITY, 0.0f32);
            for (i, v) in pos.as_chunks::<3>().0.iter().enumerate() {
                let d = (v[0] - xf).abs();
                if d < best.0 {
                    best = (d, pm.modes[0].shape[3 * i + 2].abs());
                }
            }
            best.1
        };
        assert!(amp_at(0.0) > 0.8);
        assert!(
            amp_at(131.32 * 0.2242) < 0.15,
            "node amp {}",
            amp_at(131.32 * 0.2242)
        );
    }

    #[test]
    fn striking_a_node_silences_the_fundamental() {
        let doc = bar_doc(131.32, "aluminum");
        let scene = evaluate(&doc).unwrap();
        let pm = part_modes(
            &doc,
            &scene,
            &ModesRequest {
                part: 0,
                modes: 3,
                quality: Quality::Preview,
                material: None,
                shapes: true,
            },
        )
        .unwrap();
        let pos = &pm.positions;
        let near = |x: f32| {
            pos.as_chunks::<3>()
                .0
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    ((a.1[0] - x).abs() + (a.1[2] - 6.0).abs())
                        .partial_cmp(&((b.1[0] - x).abs() + (b.1[2] - 6.0).abs()))
                        .unwrap()
                })
                .unwrap()
                .0
        };
        let end = strike_gains(&pm, near(0.0), [0.0, 0.0, 1.0], 1.0);
        let node = strike_gains(&pm, near(131.32 * 0.2242), [0.0, 0.0, 1.0], 1.0);
        assert!(end[0] > 0.5, "end strike rings the fundamental: {end:?}");
        assert!(
            node[0] < 0.2 * end[0].max(1e-9) || node[0] < 0.2,
            "node strike: {node:?}"
        );
    }

    #[test]
    fn pitch_lock_solves_length_to_a_note() {
        // A plain 25×6 6061 bar rings G6 at ~140.7 mm; start short (sharp).
        let doc = bar_doc(120.0, "aluminum");
        let lock = PitchLock {
            part: "bar".into(),
            mode: 1,
            target_hz: 1567.98,
            note: Some("G6".into()),
            drives: "len".into(),
            tolerance_cents: 1.0,
        };
        let (solved, rep) = solve_pitch_lock(&doc, &lock).unwrap();
        assert!(rep.converged, "{rep:?}");
        assert!(
            (rep.value - 140.7).abs() < 1.5,
            "G6 plain bar length {}",
            rep.value
        );
        assert!(rep.steps.len() <= 6, "{} steps", rep.steps.len());
        assert!((param_value(&solved, "len").unwrap() - rep.value).abs() < 1e-9);
        // Change the material: brass rings lower; the same lock re-solves.
        let brass = bar_doc(rep.value, "brass");
        let (_, rep2) = solve_pitch_lock(&brass, &lock).unwrap();
        assert!(rep2.converged && rep2.value < rep.value, "brass {rep2:?}");
    }
}
