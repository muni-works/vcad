//! `simulate_charged_particles` and `optimize_electrodes` over
//! `vcad-kernel-particle`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use vcad_kernel_particle as pk;

use crate::section::{self, Section};
use crate::{Hooks, ToolError};

/// Run options (wasm-compatible names and defaults).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SimOptions {
    /// Radial grid nodes.
    pub nr: usize,
    /// Axial grid nodes.
    pub nz: usize,
    /// Ensemble size.
    pub particles: usize,
    /// Pass cap.
    pub max_passes: u32,
    /// Ion current, A.
    pub ion_current_a: f64,
    /// D₂ pressure, mTorr.
    pub d2_pressure_mtorr: f64,
    /// Gas temperature, K.
    pub temperature_k: f64,
    /// Single-generation CX cross section, m² (None = CX off).
    pub cx_sigma_m2: Option<f64>,
}

impl Default for SimOptions {
    fn default() -> Self {
        Self {
            nr: 101,
            nz: 201,
            particles: 64,
            max_passes: 25,
            ion_current_a: 0.010,
            d2_pressure_mtorr: 2.0,
            temperature_k: 300.0,
            cx_sigma_m2: None,
        }
    }
}

/// What to draw.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SectionOptions {
    /// Tracks returned (an even subset of the ensemble).
    pub tracks: usize,
    /// Equipotential levels.
    pub contours: usize,
    /// Max points per track.
    pub max_points: usize,
    /// Minimum point spacing along a track, mm.
    pub spacing_mm: f64,
    /// Heatmap grid cap along the longer axis.
    pub heatmap_max: usize,
}

impl Default for SectionOptions {
    fn default() -> Self {
        Self {
            tracks: 24,
            contours: 14,
            max_points: 600,
            spacing_mm: 0.4,
            heatmap_max: 160,
        }
    }
}

/// Voltage sweep request.
#[derive(Debug, Clone, Deserialize)]
pub struct SweepOptions {
    /// Deepest potential drop per point, kV (sign ignored).
    pub kv: Vec<f64>,
    /// Grid / ensemble overrides (default: the main run's).
    pub nr: Option<usize>,
    /// Axial nodes.
    pub nz: Option<usize>,
    /// Ensemble size.
    pub particles: Option<usize>,
    /// Pass cap.
    pub max_passes: Option<u32>,
}

/// The floor … ceiling neutron-rate bracket.
#[derive(Debug, Clone, Serialize)]
pub struct Prediction {
    /// The rate the `ddn_neutron_rate` claim carries (the floor when CX
    /// is on, the ceiling when off), n/s.
    pub neutron_rate_n_per_s: f64,
    /// CX on (ion channel + fast-neutral channel), n/s. None when CX off.
    pub floor_n_per_s: Option<f64>,
    /// CX off (ions recirculate unmolested; beam-on-background), n/s.
    pub ceiling_n_per_s: f64,
    /// Whether the CX channels are in the claim.
    pub cx_included: bool,
    /// What the bracket is and is not.
    pub note: &'static str,
}

const BAND_NOTE: &str = "floor = single-generation CX on, ceiling = CX off, from the same traces (CX only re-weights, it never bends a trajectory). Both are floors on their own physics: no CX chain, no volume ionization, no beam-beam. A physics bracket, not a confidence interval.";

/// One voltage-sweep row.
#[derive(Debug, Clone, Serialize)]
pub struct SweepRow {
    /// Deepest drop, kV.
    pub kv: f64,
    /// Floor (CX on) n/s; None when CX off.
    pub floor_n_per_s: Option<f64>,
    /// Ceiling (CX off) n/s.
    pub ceiling_n_per_s: f64,
    /// Interception fraction.
    pub interception_fraction: f64,
    /// Mean core passes (censored).
    pub mean_passes: f64,
    /// Fraction censored at the pass cap.
    pub survivor_fraction: f64,
    /// Worst per-trace energy drift.
    pub max_energy_drift_rel: f64,
}

/// A device: a full ring-list [`pk::spec::DeviceSpec`], or the
/// `{"classic_fusor": {...}}` shorthand (a spherical "globe" cathode as
/// latitude rings, [`pk::device::Device::classic_fusor`]) whose fields may
/// name parameters — so one parameter can move every ring (grid radius,
/// cathode voltage), which a ring list cannot express.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DeviceInput {
    /// The shorthand.
    Classic {
        /// Its fields.
        classic_fusor: ClassicFusor,
    },
    /// A full spec.
    Spec(pk::spec::DeviceSpec),
}

/// `classic_fusor` shorthand fields.
#[derive(Debug, Clone, Deserialize)]
pub struct ClassicFusor {
    /// Chamber inner radius (and half height), mm.
    pub chamber_radius_mm: pk::spec::ParamValue,
    /// Cathode globe radius, mm.
    pub cathode_radius_mm: pk::spec::ParamValue,
    /// Latitude ring count.
    pub rings: usize,
    /// Wire radius, mm.
    pub wire_radius_mm: pk::spec::ParamValue,
    /// Cathode bias, V (negative for a fusor).
    pub cathode_v: pk::spec::ParamValue,
}

fn pv(v: &pk::spec::ParamValue, p: &BTreeMap<String, f64>) -> Result<f64, String> {
    match v {
        pk::spec::ParamValue::Literal(x) => Ok(*x),
        pk::spec::ParamValue::Named(n) => p
            .get(n)
            .copied()
            .ok_or_else(|| format!("unbound parameter {n:?}")),
    }
}

impl DeviceInput {
    /// Parse with a precise error (an untagged enum would only say "did
    /// not match any variant").
    pub fn from_json(v: &Value) -> Result<Self, ToolError> {
        if let Some(c) = v.get("classic_fusor") {
            return serde_json::from_value(c.clone())
                .map(|classic_fusor| DeviceInput::Classic { classic_fusor })
                .map_err(|e| ToolError::Invalid(format!("bad spec.classic_fusor: {e}")));
        }
        serde_json::from_value(v.clone())
            .map(DeviceInput::Spec)
            .map_err(|e| ToolError::Invalid(format!("bad spec: {e}")))
    }

    /// Resolve against parameter bindings (fail-closed).
    pub fn resolve(&self, p: &BTreeMap<String, f64>) -> Result<pk::device::Device, String> {
        match self {
            DeviceInput::Spec(s) => s.resolve(p).map_err(|e| e.to_string()),
            DeviceInput::Classic { classic_fusor: c } => {
                if c.rings == 0 || c.rings > 64 {
                    return Err(format!(
                        "classic_fusor.rings must be 1..=64 (got {})",
                        c.rings
                    ));
                }
                let chamber = pv(&c.chamber_radius_mm, p)?;
                let cathode = pv(&c.cathode_radius_mm, p)?;
                let wire = pv(&c.wire_radius_mm, p)?;
                if !(chamber > cathode + wire && cathode > wire && wire > 0.0) {
                    return Err(format!(
                        "classic_fusor needs chamber > cathode + wire > 0 and cathode > wire (got {chamber}, {cathode}, {wire})"
                    ));
                }
                Ok(pk::device::Device::classic_fusor(
                    chamber,
                    cathode,
                    c.rings,
                    wire,
                    pv(&c.cathode_v, p)?,
                ))
            }
        }
    }

    /// Every named parameter the device references.
    pub fn parameter_names(&self) -> Vec<String> {
        match self {
            DeviceInput::Spec(s) => s.parameter_roles().into_keys().collect(),
            DeviceInput::Classic { classic_fusor: c } => {
                let mut v: Vec<String> = [
                    &c.chamber_radius_mm,
                    &c.cathode_radius_mm,
                    &c.wire_radius_mm,
                    &c.cathode_v,
                ]
                .iter()
                .filter_map(|x| match x {
                    pk::spec::ParamValue::Named(n) => Some(n.clone()),
                    _ => None,
                })
                .collect();
                v.sort();
                v.dedup();
                v
            }
        }
    }
}

#[derive(Deserialize)]
struct SimArgs {
    spec: Value,
    #[serde(default)]
    parameters: BTreeMap<String, f64>,
    #[serde(default)]
    options: SimOptions,
    #[serde(default)]
    sweep: Option<SweepOptions>,
}

fn section_opts(args: &Value) -> Result<Option<SectionOptions>, ToolError> {
    match args.get("section") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(None),
        Some(Value::Bool(true)) => Ok(Some(SectionOptions::default())),
        Some(v @ Value::Object(_)) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| ToolError::Invalid(format!("bad section options: {e}"))),
        Some(_) => Err(ToolError::Invalid(
            "section must be a boolean or an object".into(),
        )),
    }
}

fn cx_model(o: &SimOptions) -> Option<pk::trace::CxModel> {
    o.cx_sigma_m2.map(|sigma| pk::trace::CxModel {
        sigma_cx_m2: sigma,
        background_deuteron_density_m3: pk::xsection::d2_deuteron_density_m3(
            o.d2_pressure_mtorr,
            o.temperature_k,
        ),
    })
}

fn bracket(stats: &pk::fom::EnsembleStats, o: &SimOptions, cx: bool) -> (Option<f64>, f64) {
    let n_d = pk::xsection::d2_deuteron_density_m3(o.d2_pressure_mtorr, o.temperature_k);
    let ceiling = pk::fom::neutron_rate_per_s(stats.mean_ddn_sigma_v_m3, o.ion_current_a, n_d);
    let floor = cx.then(|| {
        pk::fom::neutron_rate_from_counts(
            stats.mean_neutrons_ion_channel + stats.mean_neutrons_cx_channel,
            o.ion_current_a,
        )
    });
    (floor, ceiling)
}

fn check_grid(nr: usize, nz: usize, particles: usize) -> Result<(), ToolError> {
    if nr.saturating_mul(nz) > 400_000 {
        return Err(ToolError::Invalid(format!(
            "grid {nr}x{nz} is over the 400k-node cap for an interactive run"
        )));
    }
    if particles > 2_000 {
        return Err(ToolError::Invalid(format!(
            "{particles} particles is over the 2000 cap for an interactive run"
        )));
    }
    Ok(())
}

/// Solve + trace one configuration, optionally recording paths. Progress
/// covers `[p0, p1]` of the caller's range.
#[allow(clippy::too_many_arguments)]
fn run_one(
    device: &pk::device::Device,
    nr: usize,
    nz: usize,
    particles: usize,
    max_passes: u32,
    cx: Option<pk::trace::CxModel>,
    record: Option<&SectionOptions>,
    hooks: &Hooks,
    (p0, p1): (f64, f64),
    stage: &str,
) -> Result<RunOne, ToolError> {
    hooks.report(p0, &format!("{stage}: solving fields on {nr}×{nz}"));
    let sopts = pk::poisson::SolveOptions::default();
    let sol = pk::poisson::solve(device, nr, nz, &sopts)
        .map_err(|e| ToolError::Solver(format!("field solve failed: {e}")))?;
    let fields = pk::field::FieldMap::new(device, &sol);
    let topts = pk::trace::TraceOptions {
        max_passes,
        cx,
        ..Default::default()
    };
    let tracer = pk::trace::Tracer::new(device, &fields, &sol, topts);
    let n = particles.max(2);
    let mut outcomes = Vec::with_capacity(n);
    let mut paths = Vec::new();
    let solve_share = 0.15;
    for k in 0..n {
        if hooks.cancelled() {
            return Err(ToolError::Cancelled);
        }
        let pos = tracer.launch_position(k, n);
        match record {
            Some(r) => {
                let p = tracer.trace_path(
                    pk::trace::DEUTERON,
                    pos,
                    [0.0; 3],
                    r.spacing_mm,
                    r.max_points,
                );
                outcomes.push(p.outcome);
                paths.push(p);
            }
            None => outcomes.push(tracer.trace_from(pk::trace::DEUTERON, pos, [0.0; 3])),
        }
        let f = solve_share + (1.0 - solve_share) * (k + 1) as f64 / n as f64;
        hooks.report(
            p0 + (p1 - p0) * f,
            &format!("{stage}: traced {}/{n} deuterons", k + 1),
        );
    }
    let stats = pk::fom::stats(&outcomes);
    Ok(RunOne {
        stats,
        topts,
        sor_tol: sopts.tol,
        core_radius_mm: tracer.core_radius_m() * 1e3,
        shell_radius_mm: tracer.shell_radius_m() * 1e3,
        sol,
        paths,
    })
}

struct RunOne {
    stats: pk::fom::EnsembleStats,
    topts: pk::trace::TraceOptions,
    sor_tol: f64,
    core_radius_mm: f64,
    shell_radius_mm: f64,
    sol: pk::poisson::Solution,
    paths: Vec<pk::trace::TracePath>,
}

/// Every biased ring rescaled so the deepest drop is `kv` kilovolts.
pub fn scaled_to_kv(device: &pk::device::Device, kv: f64) -> pk::device::Device {
    let drop = device.max_potential_drop_v();
    let mut d = device.clone();
    if drop > 0.0 {
        let s = kv.abs() * 1e3 / drop;
        for r in &mut d.rings {
            r.potential_v = device.wall_potential_v + (r.potential_v - device.wall_potential_v) * s;
        }
    }
    d
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// `simulate_charged_particles`.
pub fn simulate(args: &Value, hooks: &Hooks) -> Result<Value, ToolError> {
    if args.get("spec").is_some_and(crate::particle3d::is_3d) {
        return crate::particle3d::simulate(args, hooks);
    }
    let a: SimArgs =
        serde_json::from_value(args.clone()).map_err(|e| ToolError::Invalid(format!("{e}")))?;
    let sec = section_opts(args)?;
    let o = &a.options;
    check_grid(o.nr, o.nz, o.particles)?;
    let spec = DeviceInput::from_json(&a.spec)?;
    let device = spec.resolve(&a.parameters).map_err(ToolError::Invalid)?;
    let cx = cx_model(o);
    let sweep_points = a.sweep.as_ref().map_or(0, |s| s.kv.len());
    let main_share = if sweep_points > 0 { 0.4 } else { 1.0 };

    let run = run_one(
        &device,
        o.nr,
        o.nz,
        o.particles,
        o.max_passes,
        cx,
        sec.as_ref(),
        hooks,
        (0.0, main_share),
        "operating point",
    )?;
    let op = pk::receipt::OperatingPoint {
        ion_current_a: o.ion_current_a,
        d2_pressure_mtorr: o.d2_pressure_mtorr,
        temperature_k: o.temperature_k,
    };
    let claim_set = pk::receipt::predicted_claims(
        &run.stats,
        &run.sol,
        &run.topts,
        run.sor_tol,
        device.max_potential_drop_v(),
        &op,
    );
    let receipt_claims = pk::receipt::design_claims(&claim_set);
    let (floor, ceiling) = bracket(&run.stats, o, cx.is_some());
    let prediction = Prediction {
        neutron_rate_n_per_s: floor.unwrap_or(ceiling),
        floor_n_per_s: floor,
        ceiling_n_per_s: ceiling,
        cx_included: cx.is_some(),
        note: BAND_NOTE,
    };

    let mut out = json!({
        "stats": to_value(&run.stats),
        "claim_set": to_value(&claim_set),
        "receipt_claims": to_value(&receipt_claims),
        "geometric_transparency": pk::fom::geometric_transparency(&device),
        "prediction": to_value(&prediction),
        "device": {
            "chamber_radius_mm": device.chamber_radius_mm,
            "chamber_half_height_mm": device.chamber_half_height_mm,
            "wall_potential_v": device.wall_potential_v,
            "max_potential_drop_v": device.max_potential_drop_v(),
            "rings": device.rings.len(),
        },
        "operating_point": to_value(&op),
    });

    if let Some(s) = &sec {
        let n = run.paths.len();
        let want = s.tracks.min(n);
        let tracks = (0..want)
            .map(|t| {
                let k = if want <= 1 {
                    0
                } else {
                    t * (n - 1) / (want - 1)
                };
                section::track(&run.paths[k])
            })
            .collect();
        let section = Section {
            frame: section::FRAME,
            r_max_mm: device.chamber_radius_mm,
            z_half_mm: device.chamber_half_height_mm,
            potential: section::potential_grid(&run.sol, s.heatmap_max),
            equipotentials: section::equipotentials(&run.sol, s.contours),
            wires: section::wires(&device),
            tracks,
            launch_shell_mm: run.shell_radius_mm,
            core_radius_mm: run.core_radius_mm,
        };
        out["section"] = to_value(&section);
    }

    if let Some(sw) = &a.sweep {
        let (nr, nz) = (sw.nr.unwrap_or(o.nr), sw.nz.unwrap_or(o.nz));
        let particles = sw.particles.unwrap_or(o.particles);
        check_grid(nr, nz, particles)?;
        let passes = sw.max_passes.unwrap_or(o.max_passes);
        let mut rows = Vec::with_capacity(sw.kv.len());
        let m = sw.kv.len() as f64;
        for (i, &kv) in sw.kv.iter().enumerate() {
            let d = scaled_to_kv(&device, kv);
            let p0 = main_share + (1.0 - main_share) * i as f64 / m;
            let p1 = main_share + (1.0 - main_share) * (i + 1) as f64 / m;
            let r = run_one(
                &d,
                nr,
                nz,
                particles,
                passes,
                cx,
                None,
                hooks,
                (p0, p1),
                &format!("sweep {:.0} kV", kv.abs()),
            )?;
            let (floor, ceiling) = bracket(&r.stats, o, cx.is_some());
            rows.push(SweepRow {
                kv: kv.abs(),
                floor_n_per_s: floor,
                ceiling_n_per_s: ceiling,
                interception_fraction: r.stats.interception_fraction,
                mean_passes: r.stats.mean_passes,
                survivor_fraction: r.stats.survivor_fraction,
                max_energy_drift_rel: r.stats.max_energy_drift_rel,
            });
        }
        out["sweep"] = json!({
            "grid": [nr, nz], "particles": particles, "max_passes": passes,
            "rows": to_value(&rows),
        });
    }
    hooks.report(1.0, "done");
    Ok(out)
}

#[derive(Deserialize)]
struct OptVariable {
    name: String,
    lo: f64,
    hi: f64,
    #[serde(default)]
    start: Option<f64>,
}

#[derive(Deserialize)]
#[serde(default)]
struct OptOptions {
    nr: usize,
    nz: usize,
    particles: usize,
    max_passes: u32,
    max_iters: usize,
    multi_start: bool,
    ion_current_a: f64,
    d2_pressure_mtorr: f64,
    temperature_k: f64,
    /// "fd" (default: every evaluation streamed) or "hybrid" (the kernel's
    /// `electrode_opt`: adjoint for potentials / ampere-turns, FD for
    /// geometry, FD fallback).
    gradient: pk::electrode_opt::GradientMode,
    adjoint_particles: Option<usize>,
    adjoint_crossings: Option<f64>,
}

impl Default for OptOptions {
    fn default() -> Self {
        Self {
            nr: 81,
            nz: 161,
            particles: 48,
            max_passes: 20,
            max_iters: 8,
            multi_start: true,
            ion_current_a: 0.010,
            d2_pressure_mtorr: 2.0,
            temperature_k: 300.0,
            gradient: pk::electrode_opt::GradientMode::Fd,
            adjoint_particles: None,
            adjoint_crossings: None,
        }
    }
}

#[derive(Deserialize)]
struct OptArgs {
    spec: Value,
    #[serde(default)]
    parameters: BTreeMap<String, f64>,
    variables: Vec<OptVariable>,
    #[serde(default)]
    options: OptOptions,
}

/// One logged objective evaluation.
#[derive(Debug, Clone, Serialize)]
pub struct Evaluation {
    /// 1-based evaluation counter across all starts.
    pub n: usize,
    /// 0-based start index.
    pub start: usize,
    /// Parameter values.
    pub params: BTreeMap<String, f64>,
    /// Objective: mean ∫σv dt per ion, m³ (0 when the candidate failed).
    pub sigma_v_m3: f64,
    /// The objective priced at the operating point, n/s (CX off).
    pub neutron_rate_n_per_s: f64,
    /// Best value seen so far (any start), n/s.
    pub best_so_far_n_per_s: f64,
}

/// `gradient: "hybrid"`: the kernel's `electrode_opt::optimize_electrodes`
/// (adjoint-priced potentials / ampere-turns), wrapped with the FD path's
/// n/s pricing. It has no per-evaluation hook, so `evaluations` is empty
/// and progress is coarse; cancellation is honoured only before it starts.
fn optimize_hybrid(a: &OptArgs, spec: &DeviceInput, hooks: &Hooks) -> Result<Value, ToolError> {
    let DeviceInput::Spec(spec) = spec else {
        return Err(ToolError::Invalid(
            "gradient \"hybrid\" needs a ring-list spec (the adjoint prices named ring potentials / ampere-turns); the classic_fusor shorthand runs with gradient \"fd\"".into(),
        ));
    };
    if hooks.cancelled() {
        return Err(ToolError::Cancelled);
    }
    let o = &a.options;
    let mut problem = pk::electrode_opt::ElectrodeOptProblem::new(
        a.variables
            .iter()
            .map(|v| pk::electrode_opt::OptVariable {
                name: v.name.clone(),
                lo: v.lo,
                hi: v.hi,
                start: v.start,
            })
            .collect(),
    );
    problem.nr = o.nr;
    problem.nz = o.nz;
    problem.particles = o.particles;
    problem.max_passes = o.max_passes;
    problem.max_iters = o.max_iters;
    problem.multi_start = o.multi_start;
    problem.gradient = pk::electrode_opt::GradientMode::Hybrid;
    if let Some(n) = o.adjoint_particles {
        problem.adjoint_particles = n;
    }
    if let Some(c) = o.adjoint_crossings {
        problem.adjoint_crossings = c;
    }
    hooks.report(0.02, "hybrid ascent (adjoint + FD) running");
    let r = pk::electrode_opt::optimize_electrodes(spec, &a.parameters, &problem)
        .map_err(ToolError::Invalid)?;
    let n_d = pk::xsection::d2_deuteron_density_m3(o.d2_pressure_mtorr, o.temperature_k);
    let price = |sv: f64| pk::fom::neutron_rate_per_s(sv, o.ion_current_a, n_d);
    let mut out = to_value(&r);
    out["best_neutron_rate_n_per_s"] = json!(price(r.best_sigma_v_m3));
    if let Some(starts) = out["starts"].as_array_mut() {
        for (s, rep) in starts.iter_mut().zip(&r.starts) {
            s["neutron_rate_n_per_s"] = json!(price(rep.value));
            s["history_n_per_s"] = json!(rep.history.iter().map(|&v| price(v)).collect::<Vec<_>>());
        }
        for (i, s) in starts.iter_mut().enumerate() {
            s["start"] = json!(i);
        }
    }
    out["evaluations"] = json!([]);
    out["evaluations_note"] = json!("hybrid mode runs inside the kernel and does not stream per-evaluation records; see starts[].iterations");
    out["objective"] = json!("tracer mean ∫σ_DDn·v dt per ion (CX off), priced as n/s at the operating point; hybrid gradients (adjoint for potentials / ampere-turns, FD for geometry, FD fallback)");
    out["operating_point"] = json!({"ion_current_a": o.ion_current_a, "d2_pressure_mtorr": o.d2_pressure_mtorr, "temperature_k": o.temperature_k});
    out["grid"] = json!([o.nr, o.nz]);
    out["particles"] = json!(o.particles);
    out["max_passes"] = json!(o.max_passes);
    out["cancelled"] = json!(false);
    hooks.report(1.0, "done");
    Ok(out)
}

/// `optimize_electrodes`.
pub fn optimize(args: &Value, hooks: &Hooks) -> Result<Value, ToolError> {
    let a: OptArgs =
        serde_json::from_value(args.clone()).map_err(|e| ToolError::Invalid(format!("{e}")))?;
    let o = &a.options;
    if a.variables.is_empty() {
        return Err(ToolError::Invalid(
            "optimize requires at least one variable".into(),
        ));
    }
    check_grid(o.nr, o.nz, o.particles)?;
    let spec = DeviceInput::from_json(&a.spec)?;
    let known = spec.parameter_names();
    for v in &a.variables {
        if !known.contains(&v.name) {
            return Err(ToolError::Invalid(format!(
                "variable {:?} is not a named parameter of the spec (named: {known:?})",
                v.name
            )));
        }
        if v.hi <= v.lo || v.hi.is_nan() || v.lo.is_nan() {
            return Err(ToolError::Invalid(format!(
                "variable {:?}: hi must exceed lo",
                v.name
            )));
        }
    }
    if o.gradient == pk::electrode_opt::GradientMode::Hybrid {
        return optimize_hybrid(&a, &spec, hooks);
    }
    let lo: Vec<f64> = a.variables.iter().map(|v| v.lo).collect();
    let hi: Vec<f64> = a.variables.iter().map(|v| v.hi).collect();
    let names: Vec<String> = a.variables.iter().map(|v| v.name.clone()).collect();
    let n_d = pk::xsection::d2_deuteron_density_m3(o.d2_pressure_mtorr, o.temperature_k);
    let price = |sv: f64| pk::fom::neutron_rate_per_s(sv, o.ion_current_a, n_d);

    let seeds: Vec<f64> = if o.multi_start {
        vec![0.2, 0.5, 0.8]
    } else {
        vec![0.5]
    };
    let explicit: Option<Vec<f64>> = a.variables.iter().map(|v| v.start).collect();
    let fd = pk::optimize::FdOptions {
        max_iters: o.max_iters,
        ..Default::default()
    };
    // Upper bound on evaluations, for the progress fraction.
    let per_start = 1 + o.max_iters * (2 * names.len() + 8);
    let budget = (per_start * seeds.len()) as f64;

    let mut evaluations: Vec<Evaluation> = Vec::new();
    let mut best_so_far = 0.0_f64;
    let mut starts = Vec::new();
    let mut best: Option<(usize, pk::optimize::FdResult)> = None;
    let mut cancelled = false;
    for (si, f) in seeds.iter().enumerate() {
        if hooks.cancelled() {
            cancelled = true;
            break;
        }
        let x0: Vec<f64> = match (&explicit, si) {
            (Some(e), 0) => e.clone(),
            _ => lo.iter().zip(&hi).map(|(l, h)| l + f * (h - l)).collect(),
        };
        let mut objective = |x: &[f64]| -> f64 {
            if hooks.cancelled() {
                return 0.0;
            }
            let mut p = a.parameters.clone();
            for (name, value) in names.iter().zip(x) {
                p.insert(name.clone(), *value);
            }
            let value = (|| {
                let device = spec.resolve(&p).ok()?;
                let sol =
                    pk::poisson::solve(&device, o.nr, o.nz, &pk::poisson::SolveOptions::default())
                        .ok()?;
                let fields = pk::field::FieldMap::new(&device, &sol);
                let topts = pk::trace::TraceOptions {
                    max_passes: o.max_passes,
                    ..Default::default()
                };
                let tracer = pk::trace::Tracer::new(&device, &fields, &sol, topts);
                Some(
                    pk::fom::stats(&tracer.launch_ensemble(pk::trace::DEUTERON, o.particles))
                        .mean_ddn_sigma_v_m3,
                )
            })()
            .unwrap_or(0.0);
            let rate = price(value);
            best_so_far = best_so_far.max(rate);
            let n = evaluations.len() + 1;
            let params: BTreeMap<String, f64> =
                names.iter().cloned().zip(x.iter().copied()).collect();
            let shown: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v:.4}")).collect();
            hooks.report(
                n as f64 / budget,
                &format!(
                    "eval {n} · start {}/{} · {} → {rate:.3e} n/s (best {best_so_far:.3e})",
                    si + 1,
                    seeds.len(),
                    shown.join(" ")
                ),
            );
            evaluations.push(Evaluation {
                n,
                start: si,
                params,
                sigma_v_m3: value,
                neutron_rate_n_per_s: rate,
                best_so_far_n_per_s: best_so_far,
            });
            value
        };
        let r = pk::optimize::maximize(&mut objective, &x0, &lo, &hi, &fd);
        starts.push(json!({
            "start": si,
            "x0": names.iter().cloned().zip(x0.iter().copied()).collect::<BTreeMap<_, _>>(),
            "params": names.iter().cloned().zip(r.x.iter().copied()).collect::<BTreeMap<_, _>>(),
            "value": r.value,
            "neutron_rate_n_per_s": price(r.value),
            "evals": r.evals,
            "history": r.history,
            "history_n_per_s": r.history.iter().map(|&v| price(v)).collect::<Vec<_>>(),
        }));
        if best.as_ref().is_none_or(|(_, b)| r.value > b.value) {
            best = Some((si, r));
        }
    }
    if hooks.cancelled() {
        cancelled = true;
    }
    if cancelled && best.is_none() {
        return Err(ToolError::Cancelled);
    }
    let (best_start, best) = best.expect("at least one start ran");
    hooks.report(1.0, "done");
    Ok(json!({
        "best_params": names.iter().cloned().zip(best.x.iter().copied()).collect::<BTreeMap<_, _>>(),
        "best_sigma_v_m3": best.value,
        "best_neutron_rate_n_per_s": price(best.value),
        "best_start": best_start,
        "evals": evaluations.len(),
        "history": best.history,
        "starts": starts,
        "evaluations": to_value(&evaluations),
        "objective": "mean ∫σ_DDn·v dt per ion (CX off), priced as n/s at the operating point; FD gradients (the discrete adjoint exists kernel-side but is not wired here)",
        "operating_point": {"ion_current_a": o.ion_current_a, "d2_pressure_mtorr": o.d2_pressure_mtorr, "temperature_k": o.temperature_k},
        "grid": [o.nr, o.nz], "particles": o.particles, "max_passes": o.max_passes,
        "cancelled": cancelled,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::with_quiet_hooks;

    /// The open-fusor POC hobby fusor (`examples/open_fusor_poc.rs` on the
    /// fusor-video branch): 76 mm chamber, 19 mm grid as 4 rings of 1 mm
    /// wire radius, 10 mA, 5 mTorr, σ_cx = 1e-19 m².
    fn poc_spec(kv: f64) -> Value {
        let d = pk::device::Device::classic_fusor(76.0, 19.0, 4, 1.0, -kv * 1e3);
        serde_json::to_value(pk::spec::DeviceSpec::from_device(&d)).unwrap()
    }

    #[test]
    fn one_cx_run_reproduces_the_poc_bracket() {
        // POC geometry/operating point at 77×153 (h = 1 mm), 96 particles,
        // −30 kV. The POC derived floor and ceiling from two separate runs
        // (CX on / off); one CX-on run must reproduce both, because CX only
        // re-weights trajectories. (The absolute POC numbers, 2.08e4 /
        // 1.80e5, came from the staggered-leapfrog tracer; #1211 made the
        // synchronized integrator the default, so they are not pinned here.)
        let opts = json!({"nr": 77, "nz": 153, "particles": 96, "max_passes": 40,
                          "ion_current_a": 0.010, "d2_pressure_mtorr": 5.0, "cx_sigma_m2": 1e-19});
        let args = json!({"spec": poc_spec(30.0), "options": opts});
        let out = with_quiet_hooks(|h| simulate(&args, h)).unwrap();
        let p = &out["prediction"];
        let floor = p["floor_n_per_s"].as_f64().unwrap();
        let ceiling = p["ceiling_n_per_s"].as_f64().unwrap();
        let mut off = opts.clone();
        off.as_object_mut().unwrap().remove("cx_sigma_m2");
        let no_cx =
            with_quiet_hooks(|h| simulate(&json!({"spec": poc_spec(30.0), "options": off}), h))
                .unwrap();
        let ceiling_sep = no_cx["prediction"]["ceiling_n_per_s"].as_f64().unwrap();
        eprintln!("POC −30 kV: floor {floor:.4e}, ceiling {ceiling:.4e} (separate CX-off {ceiling_sep:.4e})");
        assert!(
            (ceiling / ceiling_sep - 1.0).abs() < 1e-9,
            "one-run ceiling {ceiling:.6e} vs separate CX-off run {ceiling_sep:.6e}"
        );
        assert!(
            floor > 0.0 && floor < ceiling,
            "floor {floor:.4e} ceiling {ceiling:.4e}"
        );
        // Same order of magnitude as the POC row (1e4 / 1e5 band).
        assert!((5e3..1e5).contains(&floor), "floor {floor:.4e}");
        assert!((5e4..1e6).contains(&ceiling), "ceiling {ceiling:.4e}");
        // The claim carries the floor.
        let claim = out["claim_set"]["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "ddn_neutron_rate")
            .unwrap()["value"]
            .as_f64()
            .unwrap();
        assert_eq!(claim, floor);
        // wasm-compatible keys are all present.
        for k in [
            "stats",
            "claim_set",
            "receipt_claims",
            "geometric_transparency",
        ] {
            assert!(out.get(k).is_some(), "missing {k}");
        }
        assert!(out.get("section").is_none(), "section is opt-in");
    }

    #[test]
    fn section_and_sweep_are_drawable() {
        let args = json!({
            "spec": poc_spec(30.0),
            "options": {"nr": 39, "nz": 77, "particles": 8, "max_passes": 6, "cx_sigma_m2": 1e-19},
            "section": {"tracks": 4, "contours": 6},
            "sweep": {"kv": [20, 40]}
        });
        let out = with_quiet_hooks(|h| simulate(&args, h)).unwrap();
        let s = &out["section"];
        assert_eq!(s["tracks"].as_array().unwrap().len(), 4);
        assert_eq!(s["equipotentials"].as_array().unwrap().len(), 6);
        assert_eq!(s["wires"].as_array().unwrap().len(), 4);
        let pot = &s["potential"];
        assert_eq!(
            pot["values"].as_array().unwrap().len(),
            (pot["nr"].as_u64().unwrap() * pot["nz"].as_u64().unwrap()) as usize
        );
        assert!(pot["min_v"].as_f64().unwrap() < -29_000.0);
        for e in s["equipotentials"].as_array().unwrap() {
            assert!(
                !e["polylines"].as_array().unwrap().is_empty(),
                "empty level {e}"
            );
        }
        let rows = out["sweep"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let c20 = rows[0]["ceiling_n_per_s"].as_f64().unwrap();
        let c40 = rows[1]["ceiling_n_per_s"].as_f64().unwrap();
        assert!(c40 > c20, "yield rises with voltage: {c20:.3e} → {c40:.3e}");
    }

    #[test]
    fn classic_shorthand_matches_the_kernel_constructor_and_optimizes_grid_radius() {
        let short = json!({"classic_fusor": {"chamber_radius_mm": 76, "cathode_radius_mm": "grid_r",
                                             "rings": 4, "wire_radius_mm": 1.0, "cathode_v": -30000}});
        let opts = json!({"nr": 39, "nz": 77, "particles": 6, "max_passes": 4});
        let a = with_quiet_hooks(|h| {
            simulate(
                &json!({"spec": short, "parameters": {"grid_r": 19.0}, "options": opts}),
                h,
            )
        })
        .unwrap();
        let b =
            with_quiet_hooks(|h| simulate(&json!({"spec": poc_spec(30.0), "options": opts}), h))
                .unwrap();
        assert_eq!(a["stats"], b["stats"], "shorthand = Device::classic_fusor");
        let out = with_quiet_hooks(|h| optimize(&json!({
            "spec": short, "parameters": {"grid_r": 19.0},
            "variables": [{"name": "grid_r", "lo": 12.0, "hi": 30.0}],
            "options": {"nr": 39, "nz": 77, "particles": 6, "max_passes": 4, "max_iters": 1, "multi_start": false}
        }), h)).unwrap();
        let g = out["best_params"]["grid_r"].as_f64().unwrap();
        assert!((12.0..=30.0).contains(&g));
        // Unbound / impossible shorthands are invalid, not panics.
        let e = with_quiet_hooks(|h| simulate(&json!({"spec": short}), h)).unwrap_err();
        assert!(matches!(e, ToolError::Invalid(_)));
    }

    #[test]
    fn hybrid_gradient_routes_to_the_kernel_optimizer() {
        let mut spec = poc_spec(30.0);
        for r in spec["rings"].as_array_mut().unwrap() {
            r["potential_v"] = json!("v");
        }
        let opts = json!({"nr": 39, "nz": 77, "particles": 6, "max_passes": 4, "max_iters": 1,
                          "multi_start": false, "gradient": "hybrid", "adjoint_particles": 4});
        let out = with_quiet_hooks(|h| {
            optimize(
                &json!({
                    "spec": spec, "parameters": {"v": -30000.0},
                    "variables": [{"name": "v", "lo": -40000.0, "hi": -20000.0}], "options": opts
                }),
                h,
            )
        })
        .unwrap();
        assert_eq!(out["gradient"], "hybrid");
        assert!(out["best_neutron_rate_n_per_s"].as_f64().unwrap() >= 0.0);
        assert!(out["starts"][0]["history_n_per_s"].is_array());
        // The shorthand can't take hybrid gradients: a clear refusal.
        let short = json!({"classic_fusor": {"chamber_radius_mm": 76, "cathode_radius_mm": 19,
                                             "rings": 4, "wire_radius_mm": 1, "cathode_v": "v"}});
        let e = with_quiet_hooks(|h| {
            optimize(
                &json!({
                    "spec": short, "parameters": {"v": -30000.0},
                    "variables": [{"name": "v", "lo": -40000.0, "hi": -20000.0}], "options": opts
                }),
                h,
            )
        })
        .unwrap_err();
        assert!(e.to_string().contains("hybrid"), "{e}");
    }

    #[test]
    fn sweep_scaling_hits_the_requested_drop() {
        let d = pk::device::Device::classic_fusor(76.0, 19.0, 4, 1.0, -30_000.0);
        let s = scaled_to_kv(&d, 40.0);
        assert!((s.max_potential_drop_v() - 40_000.0).abs() < 1e-6);
    }

    #[test]
    fn optimizer_logs_every_evaluation_and_cancels() {
        let mut spec = poc_spec(30.0);
        for r in spec["rings"].as_array_mut().unwrap() {
            r["potential_v"] = json!("v");
        }
        let args = json!({
            "spec": spec,
            "parameters": {"v": -30000.0},
            "variables": [{"name": "v", "lo": -40000.0, "hi": -20000.0}],
            "options": {"nr": 39, "nz": 77, "particles": 6, "max_passes": 4, "max_iters": 1, "multi_start": false}
        });
        let out = with_quiet_hooks(|h| optimize(&args, h)).unwrap();
        let evals = out["evaluations"].as_array().unwrap();
        assert_eq!(evals.len() as u64, out["evals"].as_u64().unwrap());
        assert!(evals.len() >= 3, "f(x0) + a central difference at least");
        let best = out["best_neutron_rate_n_per_s"].as_f64().unwrap();
        let max_eval = evals
            .iter()
            .map(|e| e["neutron_rate_n_per_s"].as_f64().unwrap())
            .fold(0.0, f64::max);
        assert!(best <= max_eval * (1.0 + 1e-12));

        // Unknown variable names list the named parameters.
        let mut bad = args.clone();
        bad["variables"][0]["name"] = json!("grid_r");
        let e = with_quiet_hooks(|h| optimize(&bad, h)).unwrap_err();
        assert!(e.to_string().contains("\"v\""), "{e}");

        // Pre-cancelled: nothing runs.
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let progress = |_: f64, _: &str| {};
        let h = Hooks {
            progress: &progress,
            cancel: &cancel,
        };
        assert_eq!(optimize(&args, &h).unwrap_err(), ToolError::Cancelled);
    }
}
