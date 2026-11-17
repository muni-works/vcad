//! Analog Monte Carlo through a 3D [`Scene`] (M7): dose maps, room
//! return, and detector-position responses.
//!
//! Same physics contract as [`crate::transport`] in its default
//! [`crate::transport::EnergyModel::ExactKinematics`] mode — continuous
//! energy carried per neutron, nuclide sampled from its Σ_s share, exact
//! isotropic-CM elastic kinematics for both outgoing energy and lab
//! angle (now rotated in full 3D), in-group isotropic thermal
//! scattering, analog absorption. The 1D code's flux-to-dose fold uses
//! group-averaged factors; here every track is weighted by the point
//! H*(10) curve at the neutron's *actual* energy
//! ([`crate::dose::h10_psv_cm2`]), which removes the intra-group
//! spectrum assumption the 1D caveat list carries.
//!
//! Tallies (all track-length estimators, all batch statistics, all
//! [`Estimate`]s — a number without an error bar stays unrepresentable):
//!
//! - [`MeshTally`]: a regular Cartesian voxel grid laid over the scene
//!   (independent of the cells). Each flight segment is walked through
//!   the voxels with an exact 3D-DDA (Amanatides–Woo) and scored
//!   `track · h*(E)` → H*(10) per source neutron per voxel, plus total
//!   fluence. The dose map comes back voxel by voxel, each with its own
//!   RSE; voxels nothing reached report `rse = ∞` (statistics floor, not
//!   zero) rather than being filled in.
//! - [`DetectorTally`]: a *virtual* sphere at a point (does not perturb
//!   transport); scores group fluence, H*(10), and any number of
//!   [`ResponseFunction`]s folded track-by-track at the neutron's energy
//!   — the "counts per source neutron in this instrument at this spot"
//!   answer.
//! - Per-cell analog absorptions — the capture-rate map (and the
//!   capture-gamma source term the photon side needs: H(n,γ) 2.22 MeV in
//!   HDPE and concrete, Fe capture in the chamber wall).
//!
//! Books balance exactly per batch: `absorbed + leaked + truncated = N`.
//! Batches run on independent RNG streams and, off wasm, in parallel
//! threads — results are bit-identical regardless of thread count
//! because each batch owns its stream and reductions are by batch index.

use crate::groups::{group_of_energy_ev, GROUP_BOUNDS_EV, N_GROUPS, THERMAL_GROUP};
use crate::materials::LIBRARY_VERSION;
use crate::response::ResponseFunction;
use crate::rng::Rng;
use crate::scene::{dist2, dot, sub, Scene, SceneError};
use crate::tally::Estimate;
use crate::transport::RunProvenance;

/// Nudge past a boundary after a surface crossing, cm (10 nm).
const NUDGE_CM: f64 = 1.0e-6;

/// Source description for scene runs.
#[derive(Debug, Clone, PartialEq)]
pub enum SceneSource {
    /// Isotropic monoenergetic point source (position in **mm**).
    Point {
        /// Position, mm.
        position_mm: [f64; 3],
        /// Birth energy, eV (must lie in the group structure).
        energy_ev: f64,
    },
    /// Uniform isotropic fluence field inside a sphere: births on the
    /// sphere surface with a cosine-law inward direction. Inside the
    /// sphere (absent material) this produces a uniform isotropic
    /// fluence Φ = N/(πR²) per N histories — the standard way to
    /// compute an instrument's fluence response R(E) = counts/Φ.
    IsotropicFluence {
        /// Sphere center, mm.
        center_mm: [f64; 3],
        /// Sphere radius, mm.
        radius_mm: f64,
        /// Birth energy, eV.
        energy_ev: f64,
    },
}

impl SceneSource {
    fn energy_ev(&self) -> f64 {
        match self {
            SceneSource::Point { energy_ev, .. }
            | SceneSource::IsotropicFluence { energy_ev, .. } => *energy_ev,
        }
    }
}

/// A regular voxel mesh (bounds in **mm**).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MeshTally {
    /// Lower corner, mm.
    pub min_mm: [f64; 3],
    /// Upper corner, mm.
    pub max_mm: [f64; 3],
    /// Voxel counts along x, y, z.
    pub shape: [usize; 3],
}

impl MeshTally {
    /// Total voxel count.
    pub fn voxel_count(&self) -> usize {
        self.shape[0] * self.shape[1] * self.shape[2]
    }

    /// Flat index of voxel `(i, j, k)` (x fastest).
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.shape[0] * (j + self.shape[1] * k)
    }

    /// Voxel center, mm.
    pub fn center_mm(&self, i: usize, j: usize, k: usize) -> [f64; 3] {
        let ijk = [i, j, k];
        let mut c = [0.0; 3];
        for a in 0..3 {
            let w = (self.max_mm[a] - self.min_mm[a]) / self.shape[a] as f64;
            c[a] = self.min_mm[a] + (ijk[a] as f64 + 0.5) * w;
        }
        c
    }

    fn voxel_volume_cc(&self) -> f64 {
        (0..3)
            .map(|a| (self.max_mm[a] - self.min_mm[a]) * 0.1 / self.shape[a] as f64)
            .product()
    }

    fn valid(&self) -> bool {
        (0..3).all(|a| {
            self.shape[a] > 0
                && self.min_mm[a].is_finite()
                && self.max_mm[a].is_finite()
                && self.max_mm[a] > self.min_mm[a]
        }) && self.voxel_count() <= 4_000_000
    }
}

/// A virtual spherical detector (does not perturb transport).
#[derive(Debug, Clone)]
pub struct DetectorTally {
    /// Label.
    pub label: String,
    /// Center, mm.
    pub position_mm: [f64; 3],
    /// Tally-sphere radius, mm (the averaging volume; pick it near the
    /// instrument's own size).
    pub radius_mm: f64,
    /// Instrument responses folded at this position.
    pub responses: Vec<ResponseFunction>,
}

/// A complete scene run.
#[derive(Debug, Clone)]
pub struct SceneConfig {
    /// The geometry.
    pub scene: Scene,
    /// The source.
    pub source: SceneSource,
    /// Optional voxel dose map.
    pub mesh: Option<MeshTally>,
    /// Point-ish detectors.
    pub detectors: Vec<DetectorTally>,
    /// Histories per batch.
    pub histories_per_batch: usize,
    /// Batches (≥ 2).
    pub batches: usize,
    /// RNG seed.
    pub seed: u64,
    /// Collision cap per history (truncations counted, never hidden).
    pub max_collisions: u32,
}

impl SceneConfig {
    /// A config with 20 batches and a 10⁴-collision cap.
    pub fn new(scene: Scene, source: SceneSource, histories_per_batch: usize, seed: u64) -> Self {
        SceneConfig {
            scene,
            source,
            mesh: None,
            detectors: Vec::new(),
            histories_per_batch,
            batches: 20,
            seed,
            max_collisions: 10_000,
        }
    }
}

/// Scene-run configuration failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum SceneRunError {
    /// The scene failed validation.
    Scene(SceneError),
    /// Source energy outside the group structure.
    BadSourceEnergy(f64),
    /// The source position lies outside the world.
    SourceOutsideWorld,
    /// < 2 batches or zero histories.
    DegenerateStatistics,
    /// Mesh bounds/shape invalid (or > 4M voxels).
    BadMesh,
    /// Detector with a non-positive radius or non-finite position.
    BadDetector(String),
}

impl std::fmt::Display for SceneRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneRunError::Scene(e) => write!(f, "invalid scene: {e}"),
            SceneRunError::BadSourceEnergy(e) => write!(
                f,
                "source energy {e} eV outside the group structure [{:e}, {:e}] eV",
                GROUP_BOUNDS_EV[N_GROUPS], GROUP_BOUNDS_EV[0]
            ),
            SceneRunError::SourceOutsideWorld => write!(f, "source lies outside the world cell"),
            SceneRunError::DegenerateStatistics => write!(
                f,
                "need ≥ 2 batches and ≥ 1 history/batch (error bars are mandatory)"
            ),
            SceneRunError::BadMesh => write!(f, "invalid mesh (bounds, shape, or > 4M voxels)"),
            SceneRunError::BadDetector(l) => write!(f, "detector {l:?} has invalid geometry"),
        }
    }
}

impl std::error::Error for SceneRunError {}

/// The voxel dose map.
#[derive(Debug, Clone)]
pub struct MeshResult {
    /// The mesh the map is laid on.
    pub mesh: MeshTally,
    /// H*(10) per source neutron per voxel, pSv (x fastest).
    pub dose_per_source_psv: Vec<Estimate>,
    /// Total fluence per source neutron per voxel, n/cm².
    pub fluence_per_source: Vec<Estimate>,
}

impl MeshResult {
    /// Dose-rate map, µSv/h, for a physical source rate.
    pub fn dose_rate_usv_per_h(&self, source_n_per_s: f64) -> Vec<Estimate> {
        self.dose_per_source_psv
            .iter()
            .map(|e| e.scaled(source_n_per_s * 3600.0 * 1.0e-6))
            .collect()
    }
}

/// One response folded at a detector.
#[derive(Debug, Clone)]
pub struct ResponseResult {
    /// Response name.
    pub name: String,
    /// Unit of one event ("count", "bubble", "track/cm2").
    pub event_unit: String,
    /// Events per source neutron.
    pub per_source: Estimate,
}

impl ResponseResult {
    /// Events per minute for a physical source rate.
    pub fn per_minute(&self, source_n_per_s: f64) -> Estimate {
        self.per_source.scaled(source_n_per_s * 60.0)
    }

    /// Events per hour for a physical source rate.
    pub fn per_hour(&self, source_n_per_s: f64) -> Estimate {
        self.per_source.scaled(source_n_per_s * 3600.0)
    }
}

/// Per-detector results.
#[derive(Debug, Clone)]
pub struct DetectorResult {
    /// Label.
    pub label: String,
    /// H*(10) per source neutron, pSv.
    pub dose_per_source_psv: Estimate,
    /// Fluence per source neutron per group, n/cm².
    pub fluence_per_source: [Estimate; N_GROUPS],
    /// Folded instrument responses, in [`DetectorTally::responses`] order.
    pub responses: Vec<ResponseResult>,
}

impl DetectorResult {
    /// Dose rate for a physical source rate, µSv/h.
    pub fn dose_rate_usv_per_h(&self, source_n_per_s: f64) -> Estimate {
        self.dose_per_source_psv
            .scaled(source_n_per_s * 3600.0 * 1.0e-6)
    }
}

/// Everything a scene run reports.
#[derive(Debug, Clone)]
pub struct SceneResult {
    /// Voxel map, when a mesh was requested.
    pub mesh: Option<MeshResult>,
    /// Detector tallies.
    pub detectors: Vec<DetectorResult>,
    /// Analog absorptions per source neutron, per cell (scene order).
    pub absorbed_per_cell: Vec<Estimate>,
    /// Fraction leaking out of the world cell.
    pub leaked: Estimate,
    /// Max over batches of |absorbed + leaked − 1| over live histories.
    pub balance_max_dev: f64,
    /// Histories killed at the collision cap — must be 0 for claims.
    pub truncated_histories: u64,
    /// Total histories.
    pub total_histories: u64,
    /// Provenance.
    pub provenance: RunProvenance,
}

/// Per-batch raw sums.
struct BatchOut {
    mesh_dose: Vec<f64>,
    mesh_flu: Vec<f64>,
    /// Per detector: `[dose, flu_g0..flu_g4, resp_0..]` (track sums).
    det: Vec<Vec<f64>>,
    absorbed: Vec<u64>,
    leaked: u64,
    truncated: u64,
}

struct Prepared<'a> {
    cfg: &'a SceneConfig,
    mesh_min: [f64; 3],
    mesh_w: [f64; 3],
    mesh_n: [usize; 3],
    det_c: Vec<[f64; 3]>,
    det_r: Vec<f64>,
}

/// Run the scene Monte Carlo. Deterministic in `config`.
pub fn run_scene(config: &SceneConfig) -> Result<SceneResult, SceneRunError> {
    config.scene.validate().map_err(SceneRunError::Scene)?;
    let e0 = config.source.energy_ev();
    if group_of_energy_ev(e0).is_none() {
        return Err(SceneRunError::BadSourceEnergy(e0));
    }
    if config.batches < 2 || config.histories_per_batch == 0 {
        return Err(SceneRunError::DegenerateStatistics);
    }
    match &config.source {
        SceneSource::Point { position_mm, .. } => {
            let p = [
                position_mm[0] * 0.1,
                position_mm[1] * 0.1,
                position_mm[2] * 0.1,
            ];
            if config.scene.locate(p).is_none() {
                return Err(SceneRunError::SourceOutsideWorld);
            }
        }
        SceneSource::IsotropicFluence { radius_mm, .. } => {
            if !(radius_mm.is_finite() && *radius_mm > 0.0) {
                return Err(SceneRunError::SourceOutsideWorld);
            }
        }
    }
    if let Some(m) = &config.mesh {
        if !m.valid() {
            return Err(SceneRunError::BadMesh);
        }
    }
    for d in &config.detectors {
        if !(d.radius_mm.is_finite() && d.radius_mm > 0.0)
            || !d.position_mm.iter().all(|x| x.is_finite())
        {
            return Err(SceneRunError::BadDetector(d.label.clone()));
        }
    }

    let (mesh_min, mesh_w, mesh_n) = match &config.mesh {
        Some(m) => {
            let w: [f64; 3] =
                std::array::from_fn(|a| (m.max_mm[a] - m.min_mm[a]) * 0.1 / m.shape[a] as f64);
            (
                [m.min_mm[0] * 0.1, m.min_mm[1] * 0.1, m.min_mm[2] * 0.1],
                w,
                m.shape,
            )
        }
        None => ([0.0; 3], [1.0; 3], [0; 3]),
    };
    let prep = Prepared {
        cfg: config,
        mesh_min,
        mesh_w,
        mesh_n,
        det_c: config
            .detectors
            .iter()
            .map(|d| {
                [
                    d.position_mm[0] * 0.1,
                    d.position_mm[1] * 0.1,
                    d.position_mm[2] * 0.1,
                ]
            })
            .collect(),
        det_r: config.detectors.iter().map(|d| d.radius_mm * 0.1).collect(),
    };

    let outs = run_batches(&prep);

    // Reduce.
    let nb = config.batches;
    let h = config.histories_per_batch as f64;
    let mut col = vec![0.0f64; nb];
    let mut est = |f: &dyn Fn(&BatchOut) -> f64| -> Estimate {
        for (c, o) in col.iter_mut().zip(outs.iter()) {
            *c = f(o);
        }
        Estimate::from_batches(&col)
    };

    let mesh = config.mesh.as_ref().map(|m| {
        let v = m.voxel_volume_cc();
        let n = m.voxel_count();
        let mut dose = Vec::with_capacity(n);
        let mut flu = Vec::with_capacity(n);
        for i in 0..n {
            dose.push(est(&|o| o.mesh_dose[i] / (v * h)));
            flu.push(est(&|o| o.mesh_flu[i] / (v * h)));
        }
        MeshResult {
            mesh: m.clone(),
            dose_per_source_psv: dose,
            fluence_per_source: flu,
        }
    });

    let mut detectors = Vec::with_capacity(config.detectors.len());
    for (di, d) in config.detectors.iter().enumerate() {
        let v = 4.0 / 3.0 * std::f64::consts::PI * prep.det_r[di].powi(3);
        let dose = est(&|o| o.det[di][0] / (v * h));
        let mut flu = [Estimate::from_batches(&[0.0, 0.0]); N_GROUPS];
        for (g, f) in flu.iter_mut().enumerate() {
            *f = est(&|o| o.det[di][1 + g] / (v * h));
        }
        let responses = d
            .responses
            .iter()
            .enumerate()
            .map(|(k, r)| ResponseResult {
                name: r.name.clone(),
                event_unit: r.event_unit.clone(),
                per_source: est(&|o| o.det[di][1 + N_GROUPS + k] / (v * h)),
            })
            .collect();
        detectors.push(DetectorResult {
            label: d.label.clone(),
            dose_per_source_psv: dose,
            fluence_per_source: flu,
            responses,
        });
    }

    let n_cells = config.scene.cells.len();
    let absorbed_per_cell = (0..n_cells)
        .map(|c| est(&|o| o.absorbed[c] as f64 / h))
        .collect();
    let leaked = est(&|o| o.leaked as f64 / h);
    let mut balance_max_dev: f64 = 0.0;
    let mut truncated = 0u64;
    for o in &outs {
        let live = h - o.truncated as f64;
        if live > 0.0 {
            let abs: u64 = o.absorbed.iter().sum();
            let dev = ((abs + o.leaked) as f64 / live - 1.0).abs();
            balance_max_dev = balance_max_dev.max(dev);
        }
        truncated += o.truncated;
    }

    Ok(SceneResult {
        mesh,
        detectors,
        absorbed_per_cell,
        leaked,
        balance_max_dev,
        truncated_histories: truncated,
        total_histories: (nb * config.histories_per_batch) as u64,
        provenance: RunProvenance {
            seed: config.seed,
            histories_per_batch: config.histories_per_batch,
            batches: nb,
            max_collisions: config.max_collisions,
            groups: N_GROUPS,
            energy_model: "exact-kinematics-3d".to_string(),
            library: LIBRARY_VERSION.to_string(),
        },
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn run_batches(prep: &Prepared) -> Vec<BatchOut> {
    let nb = prep.cfg.batches;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(nb);
    if threads <= 1 {
        return (0..nb).map(|b| run_batch(prep, b)).collect();
    }
    let mut slots: Vec<Option<BatchOut>> = (0..nb).map(|_| None).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(&mut slots);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let b = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if b >= nb {
                    break;
                }
                let out = run_batch(prep, b);
                results.lock().expect("batch slot lock")[b] = Some(out);
            });
        }
    });
    slots
        .into_iter()
        .map(|o| o.expect("every batch ran"))
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn run_batches(prep: &Prepared) -> Vec<BatchOut> {
    (0..prep.cfg.batches).map(|b| run_batch(prep, b)).collect()
}

/// Isotropic unit vector.
fn iso_dir(rng: &mut Rng) -> [f64; 3] {
    let w = rng.uniform_mu();
    let phi = 2.0 * std::f64::consts::PI * rng.uniform();
    let s = (1.0 - w * w).max(0.0).sqrt();
    [s * phi.cos(), s * phi.sin(), w]
}

/// Rotate unit `u` by polar cosine `mu` and azimuth `phi`.
pub(crate) fn rotate_dir(u: [f64; 3], mu: f64, phi: f64) -> [f64; 3] {
    let st = (1.0 - mu * mu).max(0.0).sqrt();
    let (cp, sp) = (phi.cos(), phi.sin());
    let w = u[2];
    let out = if w.abs() > 0.999_999 {
        [st * cp, st * sp, mu * w.signum()]
    } else {
        let s = (1.0 - w * w).sqrt();
        [
            mu * u[0] + st * (u[0] * w * cp - u[1] * sp) / s,
            mu * u[1] + st * (u[1] * w * cp + u[0] * sp) / s,
            mu * w - st * s * cp,
        ]
    };
    let n = dot(out, out).sqrt();
    [out[0] / n, out[1] / n, out[2] / n]
}

fn sample_scatterer(scatterers: &[crate::materials::Scatterer], group: usize, u: f64) -> f64 {
    let mut acc = 0.0;
    let mut last = scatterers[0].mass_amu;
    for sc in scatterers {
        if sc.share[group] > 0.0 {
            last = sc.mass_amu;
            acc += sc.share[group];
            if u <= acc {
                return sc.mass_amu;
            }
        }
    }
    last
}

/// Chord length of segment `p + t·d, t ∈ [0, len]` inside sphere (c, r).
fn sphere_chord(p: [f64; 3], d: [f64; 3], len: f64, c: [f64; 3], r: f64) -> f64 {
    let oc = sub(p, c);
    let b = dot(oc, d);
    let cc = dot(oc, oc) - r * r;
    let disc = b * b - cc;
    if disc <= 0.0 {
        return 0.0;
    }
    let s = disc.sqrt();
    let t0 = (-b - s).max(0.0);
    let t1 = (-b + s).min(len);
    (t1 - t0).max(0.0)
}

impl Prepared<'_> {
    /// Score the segment `p + t·d, t ∈ [0, len]` at energy `e`, group `g`.
    #[allow(clippy::too_many_arguments)]
    fn score(
        &self,
        out: &mut BatchOut,
        p: [f64; 3],
        d: [f64; 3],
        len: f64,
        e: f64,
        g: usize,
        h: f64,
    ) {
        if len <= 0.0 {
            return;
        }
        for (di, det) in self.cfg.detectors.iter().enumerate() {
            // Cheap reject: segment's closest approach.
            let c = self.det_c[di];
            let r = self.det_r[di];
            let t = dot(sub(c, p), d).clamp(0.0, len);
            let q = [p[0] + t * d[0], p[1] + t * d[1], p[2] + t * d[2]];
            if dist2(q, c) >= r * r {
                continue;
            }
            let chord = sphere_chord(p, d, len, c, r);
            if chord <= 0.0 {
                continue;
            }
            let slot = &mut out.det[di];
            slot[0] += chord * h;
            slot[1 + g] += chord;
            for (k, resp) in det.responses.iter().enumerate() {
                slot[1 + N_GROUPS + k] += chord * resp.at(e);
            }
        }
        if self.mesh_n[0] > 0 {
            self.score_mesh(out, p, d, len, h);
        }
    }

    /// Exact 3D-DDA walk of the segment through the voxel grid.
    fn score_mesh(&self, out: &mut BatchOut, p: [f64; 3], d: [f64; 3], len: f64, h: f64) {
        let lo = self.mesh_min;
        let mut t0: f64 = 0.0;
        let mut t1: f64 = len;
        for a in 0..3 {
            let hi = lo[a] + self.mesh_w[a] * self.mesh_n[a] as f64;
            if d[a].abs() < 1.0e-300 {
                if p[a] < lo[a] || p[a] > hi {
                    return;
                }
                continue;
            }
            let (mut ta, mut tb) = ((lo[a] - p[a]) / d[a], (hi - p[a]) / d[a]);
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            t0 = t0.max(ta);
            t1 = t1.min(tb);
        }
        if t0 >= t1 {
            return;
        }
        let mut idx = [0usize; 3];
        let mut step = [0isize; 3];
        let mut t_max = [f64::INFINITY; 3];
        let mut t_delta = [f64::INFINITY; 3];
        for a in 0..3 {
            // Voxel of the entry point (nudged inside along the ray).
            let x = p[a] + (t0 + 1.0e-12 * (1.0 + t1)) * d[a];
            let fi = ((x - lo[a]) / self.mesh_w[a]).floor();
            let i = (fi.max(0.0) as usize).min(self.mesh_n[a] - 1);
            idx[a] = i;
            if d[a] > 1.0e-300 {
                step[a] = 1;
                let edge = lo[a] + (i as f64 + 1.0) * self.mesh_w[a];
                t_max[a] = (edge - p[a]) / d[a];
                t_delta[a] = self.mesh_w[a] / d[a];
            } else if d[a] < -1.0e-300 {
                step[a] = -1;
                let edge = lo[a] + i as f64 * self.mesh_w[a];
                t_max[a] = (edge - p[a]) / d[a];
                t_delta[a] = -self.mesh_w[a] / d[a];
            }
        }
        let nx = self.mesh_n[0];
        let nxy = nx * self.mesh_n[1];
        let mut t = t0;
        loop {
            let a = if t_max[0] < t_max[1] {
                if t_max[0] < t_max[2] {
                    0
                } else {
                    2
                }
            } else if t_max[1] < t_max[2] {
                1
            } else {
                2
            };
            let t_next = t_max[a].min(t1);
            let seg = t_next - t;
            if seg > 0.0 {
                let flat = idx[0] + nx * idx[1] + nxy * idx[2];
                out.mesh_dose[flat] += seg * h;
                out.mesh_flu[flat] += seg;
            }
            if t_max[a] >= t1 {
                break;
            }
            t = t_next;
            let ni = idx[a] as isize + step[a];
            if ni < 0 || ni >= self.mesh_n[a] as isize {
                break;
            }
            idx[a] = ni as usize;
            t_max[a] += t_delta[a];
        }
    }
}

fn run_batch(prep: &Prepared, batch: usize) -> BatchOut {
    let cfg = prep.cfg;
    let scene = &cfg.scene;
    let n_vox = cfg.mesh.as_ref().map_or(0, |m| m.voxel_count());
    let mut out = BatchOut {
        mesh_dose: vec![0.0; n_vox],
        mesh_flu: vec![0.0; n_vox],
        det: cfg
            .detectors
            .iter()
            .map(|d| vec![0.0; 1 + N_GROUPS + d.responses.len()])
            .collect(),
        absorbed: vec![0; scene.cells.len()],
        leaked: 0,
        truncated: 0,
    };
    let mut rng = Rng::stream(cfg.seed, batch as u64);
    let e_birth = cfg.source.energy_ev();
    let g_birth = group_of_energy_ev(e_birth).expect("validated");
    let floor = GROUP_BOUNDS_EV[N_GROUPS];

    for _ in 0..cfg.histories_per_batch {
        // Spawn.
        let (mut pos, mut dir) = match &cfg.source {
            SceneSource::Point { position_mm, .. } => (
                [
                    position_mm[0] * 0.1,
                    position_mm[1] * 0.1,
                    position_mm[2] * 0.1,
                ],
                iso_dir(&mut rng),
            ),
            SceneSource::IsotropicFluence {
                center_mm,
                radius_mm,
                ..
            } => {
                let n = iso_dir(&mut rng);
                let r = radius_mm * 0.1 * (1.0 - 1.0e-9);
                let c = [center_mm[0] * 0.1, center_mm[1] * 0.1, center_mm[2] * 0.1];
                let p = [c[0] + r * n[0], c[1] + r * n[1], c[2] + r * n[2]];
                // Cosine law about the inward normal: μ = √u.
                let mu = rng.uniform().sqrt();
                let phi = 2.0 * std::f64::consts::PI * rng.uniform();
                (p, rotate_dir([-n[0], -n[1], -n[2]], mu, phi))
            }
        };
        let mut cell = match scene.locate(pos) {
            Some(c) => c,
            None => {
                // Fluence-sphere births outside the world simply leak
                // (the sphere may be larger than the model).
                out.leaked += 1;
                continue;
            }
        };
        let mut e = e_birth;
        let mut g = g_birth;
        let mut h = crate::dose::h10_psv_cm2(e);
        let mut collisions = 0u32;

        loop {
            let mat = &scene.cells[cell].material;
            let st = mat.sigma_t[g];
            let d_coll = if st > 0.0 {
                -rng.uniform().ln() / st
            } else {
                f64::INFINITY
            };
            let d_b = scene.distance_to_surface(pos, dir, 1.0e-9);
            if d_coll.is_infinite() && d_b.is_infinite() {
                // Cannot happen inside a bounded world; never hang.
                out.truncated += 1;
                break;
            }
            if d_coll < d_b {
                prep.score(&mut out, pos, dir, d_coll, e, g, h);
                for a in 0..3 {
                    pos[a] += d_coll * dir[a];
                }
                if rng.uniform() * st <= mat.sigma_a[g] {
                    out.absorbed[cell] += 1;
                    break;
                }
                if g == THERMAL_GROUP {
                    dir = iso_dir(&mut rng);
                } else {
                    let a = sample_scatterer(&mat.scatterers, g, rng.uniform());
                    let mu_c = rng.uniform_mu();
                    let (ratio, mu_lab) = crate::scatter::elastic_outcome(a, mu_c);
                    e = (e * ratio).max(floor);
                    g = group_of_energy_ev(e).expect("clamped energy stays in the structure");
                    h = crate::dose::h10_psv_cm2(e);
                    let phi = 2.0 * std::f64::consts::PI * rng.uniform();
                    dir = rotate_dir(dir, mu_lab, phi);
                }
                collisions += 1;
                if collisions >= cfg.max_collisions {
                    out.truncated += 1;
                    break;
                }
            } else {
                prep.score(&mut out, pos, dir, d_b, e, g, h);
                for a in 0..3 {
                    pos[a] += (d_b + NUDGE_CM) * dir[a];
                }
                match scene.locate(pos) {
                    Some(c) => cell = c,
                    None => {
                        out.leaked += 1;
                        break;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::{air, Material};
    use crate::scene::{Cell, Shape};

    fn void_world(half_mm: f64) -> Scene {
        Scene::new(vec![Cell::new(
            "world",
            Shape::box_centered_mm([0.0; 3], [half_mm; 3]),
            Material::void(),
        )])
        .unwrap()
    }

    #[test]
    fn rotation_is_unit_and_preserves_polar_angle() {
        let mut rng = Rng::seeded(3);
        for _ in 0..1000 {
            let u = iso_dir(&mut rng);
            let mu = rng.uniform_mu();
            let v = rotate_dir(u, mu, std::f64::consts::TAU * rng.uniform());
            assert!((dot(v, v) - 1.0).abs() < 1e-12);
            assert!((dot(u, v) - mu).abs() < 1e-9);
        }
    }

    #[test]
    fn void_point_source_detector_matches_inverse_square() {
        // Uncollided fluence at 50 cm in vacuum: 1/(4π·50²) per source.
        let mut c = SceneConfig::new(
            void_world(2000.0),
            SceneSource::Point {
                position_mm: [0.0; 3],
                energy_ev: 2.45e6,
            },
            20_000,
            11,
        );
        c.detectors.push(DetectorTally {
            label: "d".into(),
            position_mm: [500.0, 0.0, 0.0],
            radius_mm: 50.0,
            responses: vec![],
        });
        let r = run_scene(&c).unwrap();
        // Volume-averaged 1/r² over a sphere of radius a at distance R:
        // ⟨1/r²⟩ = (3/(4a³))·[ (R²−a²)/(2R)·ln((R+a)/(R−a)) … ] — for
        // a/R = 0.1 the correction is +0.33%: well inside the tolerance.
        let expect = 1.0 / (4.0 * std::f64::consts::PI * 50.0f64.powi(2));
        let f = r.detectors[0].fluence_per_source[0];
        assert!(
            (f.mean - expect).abs() < 4.0 * f.abs_sigma() + 0.01 * expect,
            "fluence {f} vs {expect}"
        );
        assert_eq!(r.leaked.mean, 1.0);
        assert!(r.balance_max_dev < 1e-12);
    }

    #[test]
    fn mesh_conserves_total_track_length() {
        // Every history crosses the whole 1 m void cube from the center
        // to a face: total track inside the mesh (covering the cube) per
        // source = mean distance to the cube surface. The mesh's summed
        // fluence × voxel volume must equal the detector-free analytic
        // mean distance from the center of a cube of half-side a to its
        // surface along an isotropic ray: ≈ 1.1967·a (numeric constant
        // checked against a direct estimate below).
        let mut c = SceneConfig::new(
            void_world(500.0),
            SceneSource::Point {
                position_mm: [0.0; 3],
                energy_ev: 2.45e6,
            },
            5_000,
            5,
        );
        c.mesh = Some(MeshTally {
            min_mm: [-500.0; 3],
            max_mm: [500.0; 3],
            shape: [7, 9, 5],
        });
        let r = run_scene(&c).unwrap();
        let m = r.mesh.unwrap();
        let v = (100.0 / 7.0) * (100.0 / 9.0) * 20.0;
        let total: f64 = m.fluence_per_source.iter().map(|e| e.mean * v).sum();
        // Direct estimate with an independent RNG.
        let mut rng = Rng::seeded(99);
        let n = 200_000;
        let mut acc = 0.0;
        for _ in 0..n {
            let d = iso_dir(&mut rng);
            let t = (0..3)
                .map(|a| 50.0 / d[a].abs().max(1e-300))
                .fold(f64::INFINITY, f64::min);
            acc += t;
        }
        let direct = acc / n as f64;
        assert!(
            (total - direct).abs() / direct < 0.01,
            "mesh track {total} vs direct {direct}"
        );
        // Dose map: every voxel carries the 2.45 MeV factor exactly.
        let h = crate::dose::h10_psv_cm2(2.45e6);
        for (d, f) in m.dose_per_source_psv.iter().zip(&m.fluence_per_source) {
            if f.mean > 0.0 {
                assert!((d.mean / f.mean - h).abs() < 1e-9 * h);
            }
        }
    }

    #[test]
    fn analog_balance_exact_and_reproducible() {
        let scene = Scene::new(vec![
            Cell::new(
                "shield",
                Shape::sphere_mm([0.0; 3], 150.0),
                crate::materials::hdpe(),
            ),
            Cell::new("world", Shape::box_centered_mm([0.0; 3], [400.0; 3]), air()),
        ])
        .unwrap();
        let src = SceneSource::Point {
            position_mm: [0.0; 3],
            energy_ev: 2.45e6,
        };
        let mut c = SceneConfig::new(scene, src, 1_000, 42);
        c.batches = 4;
        let a = run_scene(&c).unwrap();
        let b = run_scene(&c).unwrap();
        assert_eq!(a.truncated_histories, 0);
        assert!(a.balance_max_dev < 1e-12);
        assert_eq!(a.leaked.mean, b.leaked.mean);
        assert_eq!(
            a.absorbed_per_cell[0].mean, b.absorbed_per_cell[0].mean,
            "bit-identical regardless of thread scheduling"
        );
    }

    #[test]
    fn config_fails_closed() {
        let mut c = SceneConfig::new(
            void_world(100.0),
            SceneSource::Point {
                position_mm: [0.0; 3],
                energy_ev: 14.1e6,
            },
            10,
            1,
        );
        assert_eq!(
            run_scene(&c).unwrap_err(),
            SceneRunError::BadSourceEnergy(14.1e6)
        );
        c.source = SceneSource::Point {
            position_mm: [500.0, 0.0, 0.0],
            energy_ev: 2.45e6,
        };
        assert_eq!(
            run_scene(&c).unwrap_err(),
            SceneRunError::SourceOutsideWorld
        );
        c.source = SceneSource::Point {
            position_mm: [0.0; 3],
            energy_ev: 2.45e6,
        };
        c.batches = 1;
        assert_eq!(
            run_scene(&c).unwrap_err(),
            SceneRunError::DegenerateStatistics
        );
        c.batches = 2;
        c.mesh = Some(MeshTally {
            min_mm: [0.0; 3],
            max_mm: [0.0; 3],
            shape: [1, 1, 1],
        });
        assert_eq!(run_scene(&c).unwrap_err(), SceneRunError::BadMesh);
    }
}
