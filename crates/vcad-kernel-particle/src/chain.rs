//! The charge-exchange chain: a two-sided, falsifiable D-D yield prediction
//! for a gas-fed (glow-discharge) fusor at the operating point an operator
//! actually reads — **supply current, voltage, pressure**.
//!
//! # Why the old bracket could not lose
//!
//! Before this module the crate priced a fusor two ways: CX off (ions
//! recirculate unmolested — unphysical at mTorr) and CX on, single
//! generation (each charge exchange kills the ion and nothing replaces it).
//! Both are floors on their own physics, and the ion current was a free
//! input. A measurement could not violate that.
//!
//! # The model
//!
//! Every ion the discharge produces is a **birth** somewhere in the gas.
//! A born ion falls down the well from its birth potential, fusing with
//! the background as it goes, until it charge-exchanges, strikes a cathode
//! wire, or reaches the wall. Each collision begets more births:
//!
//! - **charge exchange** `D_m⁺ + D₂ → D_m(fast) + D₂⁺(slow)` — the fast
//!   neutral flies straight at full energy (fusing in the gas, then either
//!   striking a cathode wire — secondary electrons — or burying itself in
//!   the wall — wall-embedded fusion), and a **cold D₂⁺ is born where the
//!   event happened** and re-accelerates from that potential;
//! - **ion- and fast-neutral-impact ionization** of D₂ — births a cold
//!   ion and an electron, while the projectile continues;
//! - **electron-impact ionization** by the electrons the discharge expels
//!   (cathode secondaries at full energy, ionization electrons from their
//!   birth potential) — traced in the same field.
//!
//! Births depend only on position, so they are deposited on the (r, z)
//! grid, and the next **generation** is launched from that map. Power
//! iteration over generations converges to the discharge's fundamental
//! source mode (the analogue of a k-eigenvalue in neutron transport); the
//! per-generation multiplication `k` is reported as a model-consistency
//! diagnostic (a self-sustained discharge has `k ≈ 1`).
//!
//! **Species.** Born ions are D₂⁺ (plus a stated D⁺ fraction from
//! dissociative ionization); D₂⁺ converts to D₃⁺ at low energy (HYDHEL
//! 4.3.3). In a pure electrostatic field the trajectory of a singly
//! charged ion launched at rest is **independent of its mass** (only the
//! clock scales, by √m), so one trajectory carries a weight per species
//! and every collision rate is applied per unit path length. A `D_m⁺` of
//! lab energy `E` fuses as `m` deuterons at `E/m` each. This exactness
//! needs `B = 0`; devices with coil current are refused
//! ([`ChainError::Magnetics`]).
//!
//! **Supply-current partition.** In steady state every ion eventually
//! lands on the cathode (or, rarely, the wall). The supply sees, per born
//! ion, the cathode ion arrivals plus the secondary electrons those
//! arrivals (and fast neutrals striking the cathode) knock out:
//! `I_supply = e·S·(c_ion + γ₁₅·u_se)`, where `S` is the birth rate. So the
//! meter reading fixes `S`, and the neutron rate is `S·Y` — no free ion
//! current.
//!
//! All cross sections are cited in [`crate::xsection`]. The two-sided band
//! is assembled by [`predict`]; see `docs/particle-optics-m0.md` (M7).

use serde::{Deserialize, Serialize};

use crate::constants::ELEMENTARY_CHARGE;
use crate::device::Device;
use crate::field::FieldMap;
use crate::poisson::{Solution, SolveOptions};
use crate::trace::{Fate, StepEvent, TraceOptions, Tracer, DEUTERON, ELECTRON};
use crate::xsection as xs;

/// Errors from the chain.
#[derive(Debug, Clone, PartialEq)]
pub enum ChainError {
    /// The device carries coil current: trajectories are no longer
    /// mass-independent, so the one-trajectory species vector is invalid.
    Magnetics,
    /// The device has no accelerating potential.
    NoWell,
    /// A Poisson / space-charge solve failed.
    Solve(String),
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::Magnetics => write!(
                f,
                "chain requires B = 0 (mass-independent trajectories); device has coil current"
            ),
            ChainError::NoWell => write!(f, "device has no potential well"),
            ChainError::Solve(e) => write!(f, "solve failed: {e}"),
        }
    }
}

impl std::error::Error for ChainError {}

/// Where generation 0 is born.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InitialSource {
    /// Uniform per unit volume over the free (non-conductor) gas.
    UniformVolume,
    /// A thin shell just inside the chamber wall (every ion gets the full
    /// drop — the most energetic possible source shape).
    EdgeShell,
}

/// Physics inputs and numerics for one chain run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChainOptions {
    /// D₂ pressure, mTorr.
    pub pressure_mtorr: f64,
    /// Gas temperature, K.
    pub temperature_k: f64,
    /// Multiplier on every charge-exchange cross section (data band).
    pub sigma_cx_scale: f64,
    /// Multiplier on every ion/neutral-impact ionization cross section.
    pub sigma_ion_scale: f64,
    /// Fraction of born ions that are atomic D⁺ (rest D₂⁺).
    pub born_atomic_fraction: f64,
    /// Whether D₂⁺ converts to D₃⁺ at low energy.
    pub d3_formation: bool,
    /// Whether fast D₂⁺/D₃⁺ break up on the gas (collision-induced
    /// dissociation → a fast D⁺ fragment traced on its own trajectory plus
    /// a fast neutral fragment).
    pub molecular_breakup: bool,
    /// Multiplier on the collision-induced-dissociation cross sections.
    pub sigma_cid_scale: f64,
    /// Read the D₃⁺ dissociation fit in the lab instead of the
    /// centre-of-mass frame (see [`xs::cid_d3plus_m2`]).
    pub d3_cid_lab_frame: bool,
    /// γ_SE at 15 keV/amu used for the *electron launches* (the current
    /// partition rescales γ analytically; see [`ChainResult::rate_n_per_s`]).
    pub gamma_se_15kev: f64,
    /// Ion traces per charge-exchange wave.
    pub particles: usize,
    /// Stratum offset of the deterministic systematic sampler, in (0, 1).
    /// Re-running with a different offset measures sampling scatter.
    pub sample_offset: f64,
    /// Stop following charge-exchange waves once the relaunch weight per
    /// unit source falls below this (the remainder is reported as
    /// truncated).
    pub wave_tol: f64,
    /// Hard cap on charge-exchange waves per generation.
    pub max_waves: usize,
    /// Electron traces per generation per electron source (cathode
    /// secondaries; ionization electrons).
    pub electrons: usize,
    /// Maximum generations.
    pub max_generations: usize,
    /// Minimum generations before the convergence test applies.
    pub min_generations: usize,
    /// Generations averaged for the reported mode.
    pub average_generations: usize,
    /// Converged when the neutrons-per-supply-charge figure moves by less
    /// than this (relative) between successive generations.
    pub generation_tol: f64,
    /// Terminate an ion trace once its total species weight falls below
    /// this; the residual is reported as censored weight.
    pub weight_floor: f64,
    /// Accumulated fast-neutral weight that triggers one straight-line
    /// neutral march (quadrature of the continuous CX source).
    pub neutral_quantum: f64,
    /// Births whose available drop is below this fraction of the well are
    /// trapped in the cathode interior: tallied as zero-energy cathode
    /// arrivals without tracing (they cannot fuse).
    pub cold_cut_fraction: f64,
    /// Generation-0 source shape.
    pub initial_source: InitialSource,
    /// Base trace options (integrator, step control). `max_passes` is a
    /// safety net only — ion lifetimes are bounded by the weight floor.
    pub trace: TraceOptions,
    /// Worker threads (0 = available parallelism).
    pub threads: usize,
}

impl Default for ChainOptions {
    fn default() -> Self {
        Self {
            pressure_mtorr: 5.0,
            temperature_k: 300.0,
            sigma_cx_scale: 1.0,
            sigma_ion_scale: 1.0,
            born_atomic_fraction: 0.05,
            d3_formation: true,
            molecular_breakup: true,
            sigma_cid_scale: 1.0,
            d3_cid_lab_frame: false,
            gamma_se_15kev: xs::GAMMA_SE_15KEV_CENTRAL,
            particles: 128,
            sample_offset: 0.5,
            wave_tol: 2.0e-3,
            max_waves: 40,
            electrons: 32,
            max_generations: 6,
            min_generations: 4,
            average_generations: 3,
            generation_tol: 0.05,
            weight_floor: 1.0e-3,
            neutral_quantum: 0.02,
            cold_cut_fraction: 0.02,
            initial_source: InitialSource::UniformVolume,
            trace: TraceOptions {
                max_passes: 400,
                step_fraction: 0.125,
                time_budget_factor: 4.0,
                ..TraceOptions::default()
            },
            threads: 0,
        }
    }
}

/// Tallies per unit **ionization source** (one ion created by ionization,
/// followed through every charge-exchange relaunch it leads to) for one
/// generation, or the averaged mode.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Tally {
    /// D-D neutrons from ions fusing with the gas, per born ion.
    pub neutrons_ion: f64,
    /// D-D neutrons from fast CX neutrals fusing with the gas.
    pub neutrons_fast_neutral: f64,
    /// Wall-embedded neutrons per born ion **per unit relative wall
    /// loading** (loading relative to the calibrated Ti-drive-in target of
    /// [`crate::beam_target`]).
    pub neutrons_wall_per_loading: f64,
    /// Ion arrivals at the cathode, per born ion.
    pub cathode_ions: f64,
    /// Secondary electrons per born ion from cathode ion arrivals, per
    /// unit γ₁₅ (multiply by γ_SE at 15 keV/amu).
    pub cathode_se_ions_per_gamma: f64,
    /// Secondary electrons per born ion from fast neutrals striking the
    /// cathode, per unit γ₁₅.
    pub cathode_se_neutrals_per_gamma: f64,
    /// Ion arrivals at the wall, per born ion.
    pub wall_ions: f64,
    /// Ionization births per unit source (impact + electron) — the next
    /// generation's source strength.
    pub births: f64,
    /// Charge-exchange relaunches per unit source (each a cold D₂⁺ that
    /// replaces a fast ion; number-conserving, not multiplication).
    pub births_cx: f64,
    /// Of which ion/neutral-impact ionization.
    pub births_impact: f64,
    /// Of which electron-impact ionization.
    pub births_electron: f64,
    /// Weight still alive at the floor/budget (censored), per unit source.
    pub censored: f64,
    /// Relaunch weight left when the wave loop stopped (truncated).
    pub truncated: f64,
    /// Charge-exchange waves followed.
    pub waves: f64,
    /// Molecular-ion breakups per unit source.
    pub breakups: f64,
    /// Sustainment `M` = ionization births per ion lost to cathode or
    /// wall. A self-sustained discharge has `M = 1`; `M < 1` means the
    /// modeled processes do not create all the ions the meter says exist,
    /// and the shortfall's birthplace is a stated model-form unknown.
    pub sustainment: f64,
    /// Fraction of births trapped cold in the cathode interior.
    pub cold_fraction: f64,
    /// Mean ion energy at cathode impact, eV (diagnostic).
    pub mean_cathode_energy_ev: f64,
    /// Worst ion-trace energy drift (integration quality).
    pub max_energy_drift_rel: f64,
    /// Mean ion-trace energy drift.
    pub mean_energy_drift_rel: f64,
}

impl Tally {
    /// Total neutrons per born ion at relative wall loading `loading`.
    pub fn neutrons(&self, loading: f64) -> f64 {
        self.neutrons_ion + self.neutrons_fast_neutral + loading * self.neutrons_wall_per_loading
    }

    /// Supply charges (units of e) per born ion at γ₁₅ = `gamma`.
    pub fn supply_charges(&self, gamma: f64) -> f64 {
        self.cathode_ions
            + gamma * (self.cathode_se_ions_per_gamma + self.cathode_se_neutrals_per_gamma)
    }

    /// Neutrons per supply electron-charge — the figure that turns a meter
    /// reading into a neutron rate.
    pub fn neutrons_per_supply_charge(&self, gamma: f64, loading: f64) -> f64 {
        let c = self.supply_charges(gamma);
        if c > 0.0 {
            self.neutrons(loading) / c
        } else {
            0.0
        }
    }

    fn scaled_add(&mut self, o: &Tally, s: f64) {
        self.neutrons_ion += s * o.neutrons_ion;
        self.neutrons_fast_neutral += s * o.neutrons_fast_neutral;
        self.neutrons_wall_per_loading += s * o.neutrons_wall_per_loading;
        self.cathode_ions += s * o.cathode_ions;
        self.cathode_se_ions_per_gamma += s * o.cathode_se_ions_per_gamma;
        self.cathode_se_neutrals_per_gamma += s * o.cathode_se_neutrals_per_gamma;
        self.wall_ions += s * o.wall_ions;
        self.births += s * o.births;
        self.births_cx += s * o.births_cx;
        self.births_impact += s * o.births_impact;
        self.births_electron += s * o.births_electron;
        self.censored += s * o.censored;
        self.truncated += s * o.truncated;
        self.waves += s * o.waves;
        self.breakups += s * o.breakups;
        self.sustainment += s * o.sustainment;
        self.cold_fraction += s * o.cold_fraction;
        self.mean_cathode_energy_ev += s * o.mean_cathode_energy_ev;
        self.max_energy_drift_rel = self.max_energy_drift_rel.max(o.max_energy_drift_rel);
        self.mean_energy_drift_rel += s * o.mean_energy_drift_rel;
    }
}

/// Result of one chain run.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainResult {
    /// Per-generation tallies (generation 0 first).
    pub generations: Vec<Tally>,
    /// Average of the last `average_generations` generations — the mode.
    pub mode: Tally,
    /// Relative spread (max−min)/mean of neutrons-per-supply-charge over
    /// the averaged generations at the run's γ — the stochastic +
    /// generation-convergence uncertainty.
    pub mode_spread_rel: f64,
    /// Whether the generation test passed within the budget.
    pub converged: bool,
    /// Steady population dwell per unit ionization source, seconds per
    /// node (all CX waves, species clocks included) — the space-charge
    /// input.
    pub dwell_per_birth: Vec<f64>,
    /// Final normalized ionization-source map (the fixed point), per node.
    pub source: Vec<f64>,
}

impl ChainResult {
    /// Neutron rate, n/s, for a supply-current reading.
    pub fn rate_n_per_s(&self, supply_current_a: f64, gamma_15kev: f64, wall_loading: f64) -> f64 {
        self.birth_rate_per_s(supply_current_a, gamma_15kev) * self.mode.neutrons(wall_loading)
    }

    /// Ionization births per second implied by a supply-current reading.
    pub fn birth_rate_per_s(&self, supply_current_a: f64, gamma_15kev: f64) -> f64 {
        let c = self.mode.supply_charges(gamma_15kev);
        if c > 0.0 {
            supply_current_a / (ELEMENTARY_CHARGE * c)
        } else {
            0.0
        }
    }

    /// Ion current reaching the cathode, amperes, for a supply reading —
    /// the partition the old API took as a free input.
    pub fn cathode_ion_current_a(&self, supply_current_a: f64, gamma_15kev: f64) -> f64 {
        self.birth_rate_per_s(supply_current_a, gamma_15kev)
            * ELEMENTARY_CHARGE
            * self.mode.cathode_ions
    }
}

/// Grid context shared by every trace in a run.
struct Ctx<'a> {
    sol: &'a Solution,
    tracer: Tracer<'a>,
    etracer: Tracer<'a>,
    n_d: f64,
    n_mol: f64,
    phi_min: f64,
    drop: f64,
    wires: Vec<(f64, f64, f64)>,
    march_ds: f64,
    opts: ChainOptions,
}

impl Ctx<'_> {
    fn node(&self, p: [f64; 3]) -> Option<usize> {
        let (nr, nz, dr, dz, z_half) = (
            self.sol.nr,
            self.sol.nz,
            self.sol.dr,
            self.sol.dz,
            self.sol.z_half,
        );
        let i = ((p[0] * p[0] + p[1] * p[1]).sqrt() / dr).round() as usize;
        let j = ((p[2] + z_half) / dz).round();
        if j < 0.0 {
            return None;
        }
        let j = j as usize;
        (i < nr && j < nz).then_some(i * nz + j)
    }

    fn sigma_cx(&self, m: usize, e_amu: f64) -> f64 {
        self.opts.sigma_cx_scale
            * if m == 1 {
                xs::cx_atomic_m2(e_amu)
            } else {
                xs::cx_molecular_m2(e_amu)
            }
    }

    fn sigma_ion(&self, m: usize, e_amu: f64) -> f64 {
        self.opts.sigma_ion_scale
            * if m == 1 {
                xs::ion_atomic_m2(e_amu)
            } else {
                xs::ion_molecular_m2(e_amu)
            }
    }
}

/// D-D neutron cross section for a `D_m` projectile of lab energy `e_ev`
/// on a stationary deuteron, per projectile (m deuterons at E/m each).
fn sigma_ddn_projectile(m: usize, e_ev: f64) -> f64 {
    let mf = m as f64;
    mf * xs::dd_n_sigma_m2(0.5 * e_ev / mf * 1e-3)
}

/// Thick-target wall yield per projectile per unit relative loading.
fn wall_yield_per_loading(m: usize, e_ev: f64) -> f64 {
    let mf = m as f64;
    mf * crate::beam_target::yield_per_ion(e_ev / mf * 1e-3)
}

/// Per-thread accumulator for one CX wave.
struct Acc {
    t: Tally,
    /// Charge-exchange relaunch weight per node (next wave's source).
    cx_births: Vec<f64>,
    /// Ionization births per node (ion + electron pairs) — the next outer
    /// generation's source.
    ion_births: Vec<f64>,
    /// Ionization births whose electron still has to be traced.
    e_births: Vec<f64>,
    e_launch: Vec<([f64; 3], f64)>,
    dwell: Vec<f64>,
    cathode_energy_w: f64,
    drift_sum: f64,
    traced: usize,
}

impl Acc {
    fn new(n: usize) -> Self {
        Self {
            t: Tally::default(),
            cx_births: vec![0.0; n],
            ion_births: vec![0.0; n],
            e_births: vec![0.0; n],
            e_launch: Vec::new(),
            dwell: vec![0.0; n],
            cathode_energy_w: 0.0,
            drift_sum: 0.0,
            traced: 0,
        }
    }

    fn merge(&mut self, o: Acc) {
        self.t.scaled_add(&o.t, 1.0);
        for (dst, src) in [
            (&mut self.cx_births, &o.cx_births),
            (&mut self.ion_births, &o.ion_births),
            (&mut self.e_births, &o.e_births),
            (&mut self.dwell, &o.dwell),
        ] {
            if dst.len() == src.len() {
                for (a, b) in dst.iter_mut().zip(src) {
                    *a += b;
                }
            }
        }
        self.e_launch.extend(o.e_launch);
        self.cathode_energy_w += o.cathode_energy_w;
        self.drift_sum += o.drift_sum;
        self.traced += o.traced;
    }

    fn ionization_birth(&mut self, node: Option<usize>, b: f64, with_electron: bool) {
        if let Some(id) = node {
            self.ion_births[id] += b;
            if with_electron {
                self.e_births[id] += b;
            }
        }
    }
}

/// March a fast neutral straight from `p` along `dir` to the first wire or
/// the wall. `w[m]` is the weight of `D_m` neutrals at lab energy `e_ev`.
fn neutral_march(ctx: &Ctx, acc: &mut Acc, p: [f64; 3], dir: [f64; 3], e_ev: f64, w: [f64; 4]) {
    let total: f64 = w[1..].iter().sum();
    if total <= 0.0 || e_ev <= 0.0 {
        return;
    }
    let l_exit = ctx.tracer.exit_distance_m(p, dir);
    let n_steps = ((l_exit / ctx.march_ds).ceil() as usize).max(1);
    let ds = l_exit / n_steps as f64;
    let mut sig_f = 0.0;
    let mut sig_i = 0.0;
    for (m, wm) in w.iter().enumerate().skip(1) {
        if *wm > 0.0 {
            let e_amu = e_ev / (2.0 * m as f64);
            sig_f += wm * sigma_ddn_projectile(m, e_ev);
            // Fast-neutral impact ionization ≈ ion impact at equal
            // velocity (stated approximation; see xsection).
            sig_i += wm * ctx.sigma_ion(m, e_amu);
        }
    }
    let mut blocked = false;
    let mut travelled = 0.0;
    for k in 0..n_steps {
        let s = (k as f64 + 0.5) * ds;
        let q = [p[0] + dir[0] * s, p[1] + dir[1] * s, p[2] + dir[2] * s];
        let r = (q[0] * q[0] + q[1] * q[1]).sqrt();
        if ctx
            .wires
            .iter()
            .any(|&(r0, z0, a)| (r - r0).powi(2) + (q[2] - z0).powi(2) <= a * a)
        {
            blocked = true;
            break;
        }
        travelled += ds;
        let b = sig_i * ctx.n_mol * ds;
        if b > 0.0 {
            acc.ionization_birth(ctx.node(q), b, true);
            acc.t.births_impact += b;
        }
    }
    acc.t.neutrons_fast_neutral += sig_f * ctx.n_d * travelled;
    let mut unit_gamma = 0.0;
    let mut wall = 0.0;
    for (m, wm) in w.iter().enumerate().skip(1) {
        if *wm > 0.0 {
            unit_gamma += wm * xs::gamma_se(e_ev, m as u32, 1.0);
            wall += wm * wall_yield_per_loading(m, e_ev);
        }
    }
    if blocked {
        acc.t.cathode_se_neutrals_per_gamma += unit_gamma;
    } else {
        acc.t.neutrons_wall_per_loading += wall;
    }
}

/// A fast D⁺ breakup fragment to trace on its own trajectory.
struct Fragment {
    pos: [f64; 3],
    vel: [f64; 3],
    weight: f64,
}

/// Trace one ion: species weights `w0` (indexed by deuteron count) on one
/// trajectory from `pos` with velocity `vel` (a D⁺-mass velocity; the
/// species vector is only exact for ions launched at rest — fragments
/// launch as pure D⁺). Breakup fragments are traced recursively.
fn trace_ion(ctx: &Ctx, acc: &mut Acc, pos: [f64; 3], vel: [f64; 3], w0: [f64; 4], depth: u32) {
    let w_init: f64 = w0[1..].iter().sum();
    if w_init <= 0.0 {
        return;
    }
    let mut w = w0;
    let mut pending = [0.0_f64; 4];
    let mut frag_ion = 0.0_f64;
    let mut frag_n = [0.0_f64; 4];
    let mut frags: Vec<Fragment> = Vec::new();
    let mut last_dir = [0.0, 0.0, 0.0];
    let mut last_vel = vel;
    let mut last_e = 0.0;
    let mut last_p = pos;
    let floor = ctx.opts.weight_floor * w_init;
    let quantum = ctx.opts.neutral_quantum * w_init;
    let (n_d, n_mol) = (ctx.n_d, ctx.n_mol);
    let breakup = ctx.opts.molecular_breakup;

    // Flush accumulated CX neutrals (at the parent energy) and breakup
    // products (D⁺ fragment queued for tracing; neutral fragment marched).
    let flush = |acc: &mut Acc,
                 pending: &mut [f64; 4],
                 frag_ion: &mut f64,
                 frag_n: &mut [f64; 4],
                 frags: &mut Vec<Fragment>,
                 p: [f64; 3],
                 v: [f64; 3],
                 dir: [f64; 3],
                 e: f64| {
        if pending[1..].iter().sum::<f64>() > 0.0 {
            neutral_march(ctx, acc, p, dir, e, *pending);
            *pending = [0.0; 4];
        }
        // Neutral fragments: from D₂⁺ a D at E/2, from D₃⁺ a D₂ at 2E/3.
        if frag_n[2] > 0.0 {
            neutral_march(ctx, acc, p, dir, 0.5 * e, [0.0, frag_n[2], 0.0, 0.0]);
        }
        if frag_n[3] > 0.0 {
            neutral_march(ctx, acc, p, dir, 2.0 * e / 3.0, [0.0, 0.0, frag_n[3], 0.0]);
        }
        *frag_n = [0.0; 4];
        if *frag_ion > 0.0 {
            frags.push(Fragment {
                pos: p,
                vel: v,
                weight: *frag_ion,
            });
            *frag_ion = 0.0;
        }
    };

    let mut observer = |ev: &StepEvent| -> bool {
        let e = ev.kinetic_ev.max(0.0);
        let ds = ev.ds_m;
        let speed = (ev.vel[0] * ev.vel[0] + ev.vel[1] * ev.vel[1] + ev.vel[2] * ev.vel[2]).sqrt();
        let node = ctx.node(ev.pos);
        let mut cx_here = 0.0;
        let mut dwell_here = 0.0;
        // Kinetic energy of each species on this trajectory: for a
        // launch-at-rest trajectory all species share e(φ_b − φ); the
        // tracer reports it for D⁺ mass, which is exact for every
        // species when the launch velocity is zero.
        for m in 1..=3 {
            let wm = w[m];
            if wm <= 0.0 {
                continue;
            }
            let mf = m as f64;
            dwell_here += wm * mf.sqrt() * ev.dt_s;
            let e_amu = e / (2.0 * mf);
            acc.t.neutrons_ion += wm * n_d * sigma_ddn_projectile(m, e) * ds;
            let p_cx = wm * (1.0 - (-n_mol * ctx.sigma_cx(m, e_amu) * ds).exp());
            let p_ion = wm * n_mol * ctx.sigma_ion(m, e_amu) * ds;
            w[m] -= p_cx;
            pending[m] += p_cx;
            cx_here += p_cx;
            acc.t.births_cx += p_cx;
            if p_ion > 0.0 {
                acc.t.births_impact += p_ion;
                acc.ionization_birth(node, p_ion, true);
            }
            if m >= 2 && breakup {
                let s_cid = ctx.opts.sigma_cid_scale
                    * if m == 2 {
                        xs::cid_d2plus_m2(e_amu)
                    } else {
                        xs::cid_d3plus_m2(e_amu, ctx.opts.d3_cid_lab_frame)
                    };
                if s_cid > 0.0 {
                    let dw = w[m] * (1.0 - (-n_mol * s_cid * ds).exp());
                    w[m] -= dw;
                    frag_ion += dw;
                    frag_n[m] += dw;
                    acc.t.breakups += dw;
                }
            }
            if m == 2 && ctx.opts.d3_formation {
                let s3 = xs::d3_formation_m2(e_amu);
                if s3 > 0.0 {
                    let dw = w[2] * (1.0 - (-n_mol * s3 * ds).exp());
                    w[2] -= dw;
                    w[3] += dw;
                }
            }
        }
        if let Some(id) = node {
            acc.cx_births[id] += cx_here;
            acc.dwell[id] += dwell_here;
        }
        if speed > 0.0 {
            last_dir = [ev.vel[0] / speed, ev.vel[1] / speed, ev.vel[2] / speed];
        }
        last_vel = ev.vel;
        last_e = e;
        last_p = ev.pos;
        let fr: f64 = frag_n[1..].iter().sum();
        if pending[1..].iter().sum::<f64>() + fr >= quantum {
            flush(
                acc,
                &mut pending,
                &mut frag_ion,
                &mut frag_n,
                &mut frags,
                ev.pos,
                ev.vel,
                last_dir,
                e,
            );
        }
        w[1..].iter().sum::<f64>() >= floor
    };
    let out = ctx.tracer.trace_observed(DEUTERON, pos, vel, &mut observer);
    flush(
        acc,
        &mut pending,
        &mut frag_ion,
        &mut frag_n,
        &mut frags,
        last_p,
        last_vel,
        last_dir,
        last_e,
    );

    let alive: f64 = w[1..].iter().sum();
    let v = out.final_vel;
    let e_final =
        0.5 * DEUTERON.mass_kg * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) / ELEMENTARY_CHARGE;
    acc.drift_sum += out.energy_drift_rel;
    acc.t.max_energy_drift_rel = acc.t.max_energy_drift_rel.max(out.energy_drift_rel);
    acc.traced += 1;
    match out.fate {
        Fate::Wire(k) => {
            acc.t.cathode_ions += alive;
            acc.cathode_energy_w += alive * e_final;
            let mut unit = 0.0;
            for (m, wm) in w.iter().enumerate().skip(1) {
                unit += wm * xs::gamma_se(e_final, m as u32, 1.0);
            }
            acc.t.cathode_se_ions_per_gamma += unit;
            // Secondary electrons leave the wire surface outward.
            let (r0, z0, a) = ctx.wires[k];
            let p = out.final_pos;
            let r = (p[0] * p[0] + p[1] * p[1]).sqrt().max(1e-12);
            let (dr, dz) = (r - r0, p[2] - z0);
            let d = (dr * dr + dz * dz).sqrt().max(1e-12);
            let s = 1.3 * a / d;
            let rn = (r0 + dr * s).max(0.0);
            let zn = z0 + dz * s;
            let launch = [rn * p[0] / r, rn * p[1] / r, zn];
            acc.e_launch.push((launch, unit * ctx.opts.gamma_se_15kev));
        }
        Fate::Wall => {
            acc.t.wall_ions += alive;
            for (m, wm) in w.iter().enumerate().skip(1) {
                acc.t.neutrons_wall_per_loading += wm * wall_yield_per_loading(m, e_final);
            }
        }
        Fate::Survived => {
            acc.t.censored += alive;
        }
    }

    for f in frags {
        if depth >= 3 {
            // Deep fragment chains are negligible; keep the books closed.
            acc.t.censored += f.weight;
            continue;
        }
        trace_ion(ctx, acc, f.pos, f.vel, [0.0, f.weight, 0.0, 0.0], depth + 1);
    }
}

/// Trace one electron of weight `w0` from rest at `pos`, depositing
/// electron-impact ionization births (their own electrons are not
/// followed — electron multiplication per electron is ≲10⁻²).
fn trace_electron(ctx: &Ctx, acc: &mut Acc, pos: [f64; 3], w0: f64) {
    if w0 <= 0.0 {
        return;
    }
    let n_mol = ctx.n_mol;
    let mut observer = |ev: &StepEvent| -> bool {
        let b = w0 * n_mol * xs::e_ionization_d2_m2(ev.kinetic_ev) * ev.ds_m;
        if b > 0.0 {
            acc.ionization_birth(ctx.node(ev.pos), b, false);
            acc.t.births_electron += b;
        }
        true
    };
    ctx.etracer
        .trace_observed(ELECTRON, pos, [0.0; 3], &mut observer);
}

/// Deterministic systematic sample of `n` node indices from weights, with
/// stratum offset `offset ∈ (0, 1)`.
fn systematic_sample(weights: &[f64], n: usize, offset: f64) -> Vec<usize> {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 || n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    let mut cum = 0.0;
    let mut k = 0;
    for (id, w) in weights.iter().enumerate() {
        cum += w / total;
        while k < n && (k as f64 + offset) / n as f64 <= cum {
            out.push(id);
            k += 1;
        }
    }
    while out.len() < n {
        out.push(weights.iter().rposition(|w| *w > 0.0).unwrap_or(0));
    }
    out
}

/// Position of a node with a deterministic in-cell offset (sample `k`).
fn node_position(sol: &Solution, id: usize, k: usize, offset: f64) -> [f64; 3] {
    let (nz, dr, dz) = (sol.nz, sol.dr, sol.dz);
    let (i, j) = (id / nz, id % nz);
    let g1 = ((k as f64 + offset) * 0.618_033_988_75).fract() - 0.5;
    let g2 = ((k as f64 + offset) * 0.754_877_666_25).fract() - 0.5;
    let r = (i as f64 * dr + 0.8 * g1 * dr).abs();
    let z = j as f64 * dz - sol.z_half + 0.8 * g2 * dz;
    [r, 0.0, z]
}

fn node_volume(sol: &Solution, id: usize) -> f64 {
    let i = id / sol.nz;
    let r_eff = if i == 0 {
        0.25 * sol.dr
    } else {
        i as f64 * sol.dr
    };
    2.0 * std::f64::consts::PI * r_eff * sol.dr * sol.dz
}

fn initial_source(sol: &Solution, device: &Device, shape: InitialSource) -> Vec<f64> {
    let n = sol.nr * sol.nz;
    let r_wall = device.chamber_radius_mm * 1e-3;
    let z_wall = device.chamber_half_height_mm * 1e-3;
    (0..n)
        .map(|id| {
            if sol.fixed[id] {
                return 0.0;
            }
            match shape {
                InitialSource::UniformVolume => node_volume(sol, id),
                InitialSource::EdgeShell => {
                    let (i, j) = (id / sol.nz, id % sol.nz);
                    let r = i as f64 * sol.dr;
                    let z = j as f64 * sol.dz - sol.z_half;
                    let s = (r * r + z * z).sqrt();
                    let shell = 0.85 * r_wall.min(z_wall);
                    if (s - shell).abs() <= 1.0 * sol.dr.max(sol.dz) {
                        node_volume(sol, id)
                    } else {
                        0.0
                    }
                }
            }
        })
        .collect()
}

fn run_parallel<F>(n_items: usize, threads: usize, cells: usize, work: F) -> Acc
where
    F: Fn(usize, &mut Acc) + Sync,
{
    let threads = if threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        threads
    }
    .clamp(1, n_items.max(1));
    let mut total = Acc::new(cells);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let work = &work;
                scope.spawn(move || {
                    let mut acc = Acc::new(cells);
                    let mut k = t;
                    while k < n_items {
                        work(k, &mut acc);
                        k += threads;
                    }
                    acc
                })
            })
            .collect();
        for h in handles {
            total.merge(h.join().expect("chain worker panicked"));
        }
    });
    total
}

/// Trace the electrons a wave produced: cathode secondaries (subsampled,
/// weight-preserving) and the electrons of ionization births.
fn trace_wave_electrons(ctx: &Ctx, acc: &mut Acc, cells: usize, offset: f64) {
    let ne = ctx.opts.electrons;
    let launches = std::mem::take(&mut acc.e_launch);
    let e_src = std::mem::replace(&mut acc.e_births, vec![0.0; cells]);
    if ne == 0 {
        return;
    }
    let w_tot: f64 = launches.iter().map(|l| l.1).sum();
    if w_tot > 0.0 && !launches.is_empty() {
        let stride = launches.len().div_ceil(ne).max(1);
        let picked: Vec<_> = launches.iter().step_by(stride).collect();
        let wsum: f64 = picked.iter().map(|l| l.1).sum();
        let scale = if wsum > 0.0 { w_tot / wsum } else { 0.0 };
        let e1 = run_parallel(picked.len(), ctx.opts.threads, cells, |k, a| {
            trace_electron(ctx, a, picked[k].0, picked[k].1 * scale);
        });
        acc.merge(e1);
    }
    let e_tot: f64 = e_src.iter().sum();
    if e_tot > 0.0 {
        let picks = systematic_sample(&e_src, ne, offset);
        let we = e_tot / picks.len().max(1) as f64;
        let e2 = run_parallel(picks.len(), ctx.opts.threads, cells, |k, a| {
            trace_electron(ctx, a, node_position(ctx.sol, picks[k], k, offset), we);
        });
        acc.merge(e2);
    }
}

/// Follow every charge-exchange generation from a normalized ionization
/// source `q` until the remaining relaunch weight falls below
/// `wave_tol`. Returns the per-unit-q accumulator and the truncated
/// weight.
fn summed_cx_chain(ctx: &Ctx, q: &[f64], cells: usize, salt: usize) -> (Acc, f64, usize) {
    let n = ctx.opts.particles.max(2);
    let off = ctx.opts.sample_offset;
    let cut = ctx.opts.cold_cut_fraction * ctx.drop;
    let fa = ctx.opts.born_atomic_fraction.clamp(0.0, 1.0);
    let mut total = Acc::new(cells);
    let mut src: Vec<f64> = q.to_vec();
    let mut weight = 1.0_f64;
    let mut waves = 0;
    while weight > ctx.opts.wave_tol && waves < ctx.opts.max_waves {
        let picks = systematic_sample(&src, n, off);
        if picks.is_empty() {
            break;
        }
        let w0 = weight / picks.len() as f64;
        let mut acc = run_parallel(picks.len(), ctx.opts.threads, cells, |k, acc| {
            let id = picks[k];
            if ctx.sol.phi[id] - ctx.phi_min < cut {
                // Trapped in the cathode interior: lands on the cathode at
                // ≲ cut energy, never fuses, γ ≈ 0.
                acc.t.cathode_ions += w0;
                acc.t.cold_fraction += w0;
                return;
            }
            let pos = node_position(ctx.sol, id, k + salt * 7919 + waves * 104_729, off);
            trace_ion(
                ctx,
                acc,
                pos,
                [0.0; 3],
                [0.0, w0 * fa, w0 * (1.0 - fa), 0.0],
                0,
            );
        });
        trace_wave_electrons(ctx, &mut acc, cells, off);
        let next = std::mem::replace(&mut acc.cx_births, vec![0.0; cells]);
        total.merge(acc);
        let mut next = next;
        for (b, f) in next.iter_mut().zip(&ctx.sol.fixed) {
            if *f {
                *b = 0.0;
            }
        }
        weight = next.iter().sum();
        src = next;
        waves += 1;
    }
    (total, weight.max(0.0), waves)
}

/// Run the chain on a solved device: the summed CX chain from an
/// ionization source, iterated to the source's fixed point.
///
/// `solution` may carry a space-charge-corrected potential (see
/// [`predict`]); the device supplies wire geometry and the wall.
pub fn run(
    device: &Device,
    solution: &Solution,
    opts: &ChainOptions,
) -> Result<ChainResult, ChainError> {
    run_from(device, solution, opts, None)
}

/// [`run`] warm-started from an ionization-source map `q0` (e.g. a
/// previous run's [`ChainResult::source`]) instead of
/// [`ChainOptions::initial_source`]. Knob reruns start from the central
/// fixed point so they need only a couple of generations and share its
/// sample points (common random numbers).
pub fn run_from(
    device: &Device,
    solution: &Solution,
    opts: &ChainOptions,
    q0: Option<&[f64]>,
) -> Result<ChainResult, ChainError> {
    if device.rings.iter().any(|r| r.ampere_turns != 0.0) {
        return Err(ChainError::Magnetics);
    }
    let fields = FieldMap::new(device, solution);
    let phi_min = solution.phi.iter().cloned().fold(f64::INFINITY, f64::min);
    let drop = -phi_min;
    if drop <= 0.0 {
        return Err(ChainError::NoWell);
    }
    let tracer = Tracer::new(device, &fields, solution, opts.trace);
    let etracer = Tracer::new(
        device,
        &fields,
        solution,
        TraceOptions {
            max_passes: 4,
            ..opts.trace
        },
    );
    let wires = tracer.wire_tori();
    let min_a = wires.iter().map(|w| w.2).fold(f64::INFINITY, f64::min);
    let h = solution.dr.min(solution.dz);
    let ctx = Ctx {
        sol: solution,
        n_d: xs::d2_deuteron_density_m3(opts.pressure_mtorr, opts.temperature_k),
        n_mol: xs::d2_molecule_density_m3(opts.pressure_mtorr, opts.temperature_k),
        phi_min,
        drop,
        march_ds: (0.5 * min_a).min(h).max(1e-6),
        wires,
        tracer,
        etracer,
        opts: *opts,
    };
    let cells = solution.nr * solution.nz;
    let mut q = match q0 {
        Some(q0) if q0.len() == cells => q0.to_vec(),
        _ => initial_source(solution, device, opts.initial_source),
    };
    let q_tot: f64 = q.iter().sum::<f64>().max(1e-300);
    q.iter_mut().for_each(|x| *x /= q_tot);
    let mut generations: Vec<Tally> = Vec::new();
    let mut dwells: Vec<Vec<f64>> = Vec::new();
    let mut converged = false;
    let gamma = opts.gamma_se_15kev;

    for g in 0..opts.max_generations.max(1) {
        let (acc, truncated, waves) = summed_cx_chain(&ctx, &q, cells, g);
        let mut t = acc.t;
        t.births = t.births_impact + t.births_electron;
        t.truncated = truncated;
        t.waves = waves as f64;
        let lost = t.cathode_ions + t.wall_ions;
        t.sustainment = if lost > 0.0 { t.births / lost } else { 0.0 };
        t.mean_cathode_energy_ev = if t.cathode_ions > 0.0 {
            acc.cathode_energy_w / t.cathode_ions
        } else {
            0.0
        };
        t.mean_energy_drift_rel = if acc.traced > 0 {
            acc.drift_sum / acc.traced as f64
        } else {
            0.0
        };
        generations.push(t);
        dwells.push(acc.dwell);
        let mut next = acc.ion_births;
        for (b, f) in next.iter_mut().zip(&solution.fixed) {
            if *f {
                *b = 0.0;
            }
        }
        let s: f64 = next.iter().sum();
        if s <= 0.0 {
            break;
        }
        next.iter_mut().for_each(|x| *x /= s);
        q = next;
        if g + 1 >= opts.min_generations.max(2) {
            let a = generations[g].neutrons_per_supply_charge(gamma, 0.0);
            let b = generations[g - 1].neutrons_per_supply_charge(gamma, 0.0);
            if a > 0.0 && ((a - b) / a).abs() < opts.generation_tol {
                converged = true;
                break;
            }
        }
    }

    let k = opts.average_generations.clamp(1, generations.len());
    let tail = &generations[generations.len() - k..];
    let mut mode = Tally::default();
    for t in tail {
        mode.scaled_add(t, 1.0 / k as f64);
    }
    mode.max_energy_drift_rel = generations
        .iter()
        .map(|t| t.max_energy_drift_rel)
        .fold(0.0, f64::max);
    let figs: Vec<f64> = tail
        .iter()
        .map(|t| t.neutrons_per_supply_charge(gamma, 0.0))
        .collect();
    let mean = figs.iter().sum::<f64>() / figs.len() as f64;
    let spread = if mean > 0.0 && figs.len() > 1 {
        (figs.iter().cloned().fold(f64::MIN, f64::max)
            - figs.iter().cloned().fold(f64::MAX, f64::min))
            / mean
    } else {
        0.0
    };
    let mut dwell = vec![0.0; cells];
    for d in &dwells[dwells.len() - k..] {
        for (a, b) in dwell.iter_mut().zip(d) {
            *a += b / k as f64;
        }
    }
    Ok(ChainResult {
        generations,
        mode,
        mode_spread_rel: spread,
        converged,
        dwell_per_birth: dwell,
        source: q,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// The two-sided prediction.
// ─────────────────────────────────────────────────────────────────────────

/// What an operator reads, plus the uncertainty of each reading.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OperatorReading {
    /// Supply (HV meter) current, A.
    pub supply_current_a: f64,
    /// Relative uncertainty of the current reading (±, ~2σ).
    pub supply_current_rel: f64,
    /// Cathode voltage, V (negative).
    pub cathode_voltage_v: f64,
    /// Relative uncertainty of the voltage reading.
    pub voltage_rel: f64,
    /// D₂ pressure, mTorr.
    pub pressure_mtorr: f64,
    /// Relative uncertainty of the pressure reading (gas-corrected gauge).
    pub pressure_rel: f64,
    /// Gas temperature, K.
    pub temperature_k: f64,
}

impl OperatorReading {
    /// A hobby-fusor reading at `supply_a`, `volts`, `mtorr` with typical
    /// meter tolerances (2% current, 2% voltage, 15% pressure on a
    /// D₂-corrected Pirani/capacitance gauge).
    pub fn hobby(supply_a: f64, volts: f64, mtorr: f64) -> Self {
        Self {
            supply_current_a: supply_a,
            supply_current_rel: 0.02,
            cathode_voltage_v: volts,
            voltage_rel: 0.02,
            pressure_mtorr: mtorr,
            pressure_rel: 0.15,
            temperature_k: 300.0,
        }
    }
}

/// Model-form inputs that are *assumed*, each with the range the band
/// carries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelForm {
    /// γ_SE at 15 keV/amu: central, low, high.
    pub gamma_se_15kev: [f64; 3],
    /// Born atomic (D⁺) fraction: central, low, high.
    pub born_atomic_fraction: [f64; 3],
    /// Wall deuterium loading relative to a Ti drive-in target: central,
    /// low, high.
    pub wall_loading: [f64; 3],
    /// Relative uncertainty applied to every CX and ionization cross
    /// section (±).
    pub atomic_data_rel: f64,
}

impl Default for ModelForm {
    fn default() -> Self {
        Self {
            gamma_se_15kev: [
                xs::GAMMA_SE_15KEV_CENTRAL,
                xs::GAMMA_SE_15KEV_LOW,
                xs::GAMMA_SE_15KEV_HIGH,
            ],
            born_atomic_fraction: [0.05, 0.0, 0.3],
            wall_loading: [0.003, 0.0, 0.02],
            atomic_data_rel: xs::ATOMIC_DATA_REL_UNCERTAINTY,
        }
    }
}

/// How the prediction is computed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PredictNumerics {
    /// Fine grid (radial, axial nodes) for the central value.
    pub grid: (usize, usize),
    /// Coarse grid for the discretization estimate (`None` skips it).
    pub coarse_grid: Option<(usize, usize)>,
    /// Poisson options.
    pub solve: SolveOptions,
    /// Chain options (physics fields are overwritten from the reading).
    pub chain: ChainOptions,
    /// Self-consistent space-charge iterations (0 = vacuum only).
    pub space_charge_iterations: usize,
    /// Under-relaxation for the space-charge density update.
    pub space_charge_relax: f64,
    /// Whether to rerun with half the integrator step for the
    /// time-discretization estimate.
    pub step_check: bool,
}

impl Default for PredictNumerics {
    fn default() -> Self {
        Self {
            grid: (305, 609),
            coarse_grid: Some((153, 305)),
            solve: SolveOptions::default(),
            chain: ChainOptions::default(),
            space_charge_iterations: 3,
            space_charge_relax: 0.5,
            step_check: true,
        }
    }
}

/// Which bucket a band component falls in (each bucket combines
/// differently — see [`YieldPrediction`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BandKind {
    /// Discretization / sampling: combined in quadrature (log space).
    Numerical,
    /// Measured inputs and cited data: combined in quadrature (log space).
    Input,
    /// Assumptions with no single right answer: envelope — the excursions
    /// are **added** in log space, never quadrature.
    ModelForm,
}

/// One component of the band: the rate at the low and high setting of one
/// knob, as multiplicative factors on the central rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BandComponent {
    /// Knob name.
    pub name: String,
    /// Bucket.
    pub kind: BandKind,
    /// rate(low setting) / central.
    pub factor_low_setting: f64,
    /// rate(high setting) / central.
    pub factor_high_setting: f64,
    /// What was varied, in words.
    pub note: String,
    /// True when the factors come from a stochastic rerun: its
    /// excursion is reduced by the measured sampling scatter in
    /// quadrature before combining (an excursion below the scatter is
    /// noise, not a band term).
    pub rerun: bool,
}

impl BandComponent {
    /// Downward and upward log excursions (both ≥ 0).
    pub fn log_excursions(&self) -> (f64, f64) {
        let a = self.factor_low_setting.max(1e-300).ln();
        let b = self.factor_high_setting.max(1e-300).ln();
        ((-a.min(b)).max(0.0), a.max(b).max(0.0))
    }
}

/// The two-sided D-D neutron-rate prediction.
///
/// Combination rule (stated, not tuned): within the numerical and input
/// buckets, log-excursions add in quadrature (independent errors); the
/// model-form bucket is an envelope — its excursions add linearly; the
/// three bucket totals then add linearly. `lower = central·e^{−D}`,
/// `upper = central·e^{+U}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YieldPrediction {
    /// The reading the prediction is priced at.
    pub reading: OperatorReading,
    /// Central rate, n/s.
    pub central_n_per_s: f64,
    /// Lower edge of the band, n/s.
    pub lower_n_per_s: f64,
    /// Upper edge of the band, n/s.
    pub upper_n_per_s: f64,
    /// Every component.
    pub components: Vec<BandComponent>,
    /// The central mode tallies (after space charge).
    pub mode: Tally,
    /// Vacuum-field (no space charge) central rate, n/s.
    pub vacuum_n_per_s: f64,
    /// Space-charge potential peak / well depth at the final iterate.
    pub space_charge_ratio: f64,
    /// Ion current at the cathode implied by the supply reading, A.
    pub cathode_ion_current_a: f64,
    /// Generations used and whether the central chain converged.
    pub generations: usize,
    /// Central chain convergence flag.
    pub converged: bool,
    /// Fraction of the neutron rate by channel: [ion-gas, fast-neutral-gas,
    /// wall-embedded].
    pub channel_fractions: [f64; 3],
    /// Grid used for the central value.
    pub grid: [usize; 2],
    /// Measured sampling scatter (log), subtracted in quadrature from
    /// every rerun component.
    pub sampling_noise_log: f64,
    /// SOR tolerance and sweeps of the central Poisson solve.
    pub sor: (f64, usize),
    /// Ion traces per CX wave and the safety pass cap.
    pub ensemble: (usize, u32),
}

fn chain_opts_for(base: &ChainOptions, reading: &OperatorReading, mf: &ModelForm) -> ChainOptions {
    ChainOptions {
        pressure_mtorr: reading.pressure_mtorr,
        temperature_k: reading.temperature_k,
        gamma_se_15kev: mf.gamma_se_15kev[0],
        born_atomic_fraction: mf.born_atomic_fraction[0],
        ..*base
    }
}

fn solve_device(
    template: &Device,
    volts: f64,
    grid: (usize, usize),
    sopts: &SolveOptions,
) -> Result<(Device, Solution), ChainError> {
    let mut device = template.clone();
    let v0 = template
        .rings
        .iter()
        .map(|r| r.potential_v)
        .fold(0.0_f64, |a, b| if b.abs() > a.abs() { b } else { a });
    let scale = if v0 != 0.0 { volts / v0 } else { 1.0 };
    for r in &mut device.rings {
        r.potential_v *= scale;
    }
    let sol = crate::poisson::solve(&device, grid.0, grid.1, sopts)
        .map_err(|e| ChainError::Solve(format!("{e:?}")))?;
    Ok((device, sol))
}

/// Chain + self-consistent space charge at one setting. Returns
/// (sc result, vacuum result, final ratio).
fn chain_with_space_charge(
    device: &Device,
    sol: &Solution,
    copts: &ChainOptions,
    reading: &OperatorReading,
    gamma: f64,
    numerics: &PredictNumerics,
) -> Result<(ChainResult, ChainResult, f64), ChainError> {
    let vacuum = run(device, sol, copts)?;
    let mut current = vacuum.clone();
    let mut ratio = 0.0;
    let mut rho: Option<Vec<f64>> = None;
    let well = sol.phi.iter().fold(0.0_f64, |a, b| a.max(-b)).max(1e-30);
    for _ in 0..numerics.space_charge_iterations {
        let s_rate = current.birth_rate_per_s(reading.supply_current_a, gamma);
        let rho_new = crate::space_charge::rho_from_dwell(sol, &current.dwell_per_birth, s_rate);
        let relaxed = match rho {
            None => rho_new,
            Some(old) => old
                .iter()
                .zip(&rho_new)
                .map(|(o, n)| {
                    (1.0 - numerics.space_charge_relax) * o + numerics.space_charge_relax * n
                })
                .collect(),
        };
        let phi_sc = crate::space_charge::grounded_potential(sol, &relaxed, &numerics.solve)
            .map_err(|e| ChainError::Solve(format!("{e:?}")))?;
        ratio = phi_sc.iter().fold(0.0_f64, |a, b| a.max(b.abs())) / well;
        let mut total = sol.clone();
        for (t, s) in total.phi.iter_mut().zip(&phi_sc) {
            *t += s;
        }
        current = run(device, &total, copts)?;
        rho = Some(relaxed);
    }
    Ok((current, vacuum, ratio))
}

/// Predict the D-D neutron rate with a two-sided band for a fusor
/// `device` (its ring potentials are rescaled to the reading's voltage)
/// at an operator `reading`.
pub fn predict(
    device: &Device,
    reading: &OperatorReading,
    model: &ModelForm,
    numerics: &PredictNumerics,
) -> Result<YieldPrediction, ChainError> {
    let g = model.gamma_se_15kev;
    let wl = model.wall_loading;
    let base = chain_opts_for(&numerics.chain, reading, model);
    let rate = |r: &ChainResult, current: f64, gamma: f64, loading: f64| {
        r.rate_n_per_s(current, gamma, loading)
    };

    // Central: fine grid, self-consistent space charge.
    let (dev_c, sol_c) = solve_device(
        device,
        reading.cathode_voltage_v,
        numerics.grid,
        &numerics.solve,
    )?;
    let (central, vacuum, sc_ratio) =
        chain_with_space_charge(&dev_c, &sol_c, &base, reading, g[0], numerics)?;
    // Variants are evaluated in the space-charge-corrected field of the
    // central run (the correction is carried, not re-derived per knob).
    let sc_sol = if numerics.space_charge_iterations > 0 {
        let s_rate = central.birth_rate_per_s(reading.supply_current_a, g[0]);
        let rho = crate::space_charge::rho_from_dwell(&sol_c, &central.dwell_per_birth, s_rate);
        let phi_sc = crate::space_charge::grounded_potential(&sol_c, &rho, &numerics.solve)
            .map_err(|e| ChainError::Solve(format!("{e:?}")))?;
        let mut s = sol_c.clone();
        for (t, d) in s.phi.iter_mut().zip(&phi_sc) {
            *t += d;
        }
        s
    } else {
        sol_c.clone()
    };
    let c0 = rate(&central, reading.supply_current_a, g[0], wl[0]);
    // Knob reruns: warm-started from the central fixed point.
    let warm = ChainOptions {
        max_generations: 2,
        min_generations: 2,
        average_generations: 2,
        ..base
    };
    let q_c = central.source.clone();
    let rerun = |o: &ChainOptions| run_from(&dev_c, &sc_sol, o, Some(&q_c));
    // Knob reruns are compared against a reference run in the *same*
    // field with the same deterministic sample points (common random
    // numbers), not against the central iterate.
    let reference = rerun(&warm)?;
    let r_ref = rate(&reference, reading.supply_current_a, g[0], wl[0]).max(1e-300);
    let ratio_of = |r: &ChainResult| rate(r, reading.supply_current_a, g[0], wl[0]) / r_ref;
    // Sampling scatter: the same run with the stratified sampler shifted.
    // Sampling scatter: RMS log-ratio of two runs with the stratified
    // sampler shifted to either side of the central offset.
    let mut noise2 = 0.0;
    for off in [0.2, 0.8] {
        let shifted = rerun(&ChainOptions {
            sample_offset: off,
            ..warm
        })?;
        noise2 += 0.5 * ratio_of(&shifted).ln().powi(2);
    }
    let noise = noise2.sqrt();
    let mut comps: Vec<BandComponent> = Vec::new();
    let mut push = |name: &str, kind: BandKind, lo: f64, hi: f64, note: String| {
        let rerun = matches!(
            name,
            "pressure"
                | "voltage"
                | "sigma_cx"
                | "sigma_ionization"
                | "sigma_breakup"
                | "born_species"
                | "d3_formation"
                | "d3_breakup_frame"
                | "grid"
                | "integrator_step"
        );
        comps.push(BandComponent {
            name: name.to_string(),
            kind,
            factor_low_setting: lo,
            factor_high_setting: hi,
            note,
            rerun,
        });
    };

    // ── numerical ──────────────────────────────────────────────────────
    let e_samp = noise.max(0.5 * central.mode_spread_rel);
    push(
        "sampling",
        BandKind::Numerical,
        (-e_samp).exp(),
        e_samp.exp(),
        format!(
            "max(shifted-sampler scatter {:.3}, half the generation spread {:.3}) over {} \
             particles per wave; converged = {}",
            noise,
            0.5 * central.mode_spread_rel,
            numerics.chain.particles,
            central.converged
        ),
    );
    if let Some(cg) = numerics.coarse_grid {
        let (dev_g, sol_g) = solve_device(device, reading.cathode_voltage_v, cg, &numerics.solve)?;
        let r = run(&dev_g, &sol_g, &base)?;
        let vac_fine = rate(&vacuum, reading.supply_current_a, g[0], wl[0]);
        let f = rate(&r, reading.supply_current_a, g[0], wl[0]) / vac_fine.max(1e-300);
        let e = f.ln().abs();
        push(
            "grid",
            BandKind::Numerical,
            (-e).exp(),
            e.exp(),
            format!(
                "vacuum chain at {}x{} vs {}x{}: ratio {f:.3} (symmetrized)",
                cg.0, cg.1, numerics.grid.0, numerics.grid.1
            ),
        );
    }
    if numerics.step_check {
        let half = ChainOptions {
            trace: TraceOptions {
                step_fraction: 0.5 * warm.trace.step_fraction,
                ..warm.trace
            },
            ..warm
        };
        let r = rerun(&half)?;
        let f = ratio_of(&r);
        let e = f.ln().abs();
        push(
            "integrator_step",
            BandKind::Numerical,
            (-e).exp(),
            e.exp(),
            format!("half step_fraction: ratio {f:.3} (symmetrized; includes sampling noise)"),
        );
    }

    // ── inputs ─────────────────────────────────────────────────────────
    push(
        "supply_current",
        BandKind::Input,
        1.0 - reading.supply_current_rel,
        1.0 + reading.supply_current_rel,
        format!(
            "meter ±{:.0}% (rate is linear in the reading)",
            100.0 * reading.supply_current_rel
        ),
    );
    {
        let mut lo_hi = [0.0; 2];
        for (k, f) in [
            1.0 / (1.0 + reading.pressure_rel),
            1.0 + reading.pressure_rel,
        ]
        .iter()
        .enumerate()
        {
            let o = ChainOptions {
                pressure_mtorr: reading.pressure_mtorr * f,
                ..warm
            };
            lo_hi[k] = ratio_of(&rerun(&o)?);
        }
        push(
            "pressure",
            BandKind::Input,
            lo_hi[0],
            lo_hi[1],
            format!(
                "gauge ±{:.0}% (full chain rerun)",
                100.0 * reading.pressure_rel
            ),
        );
    }
    {
        let v_hi = reading.cathode_voltage_v * (1.0 + reading.voltage_rel);
        let (dv, sv) = solve_device(device, v_hi, numerics.grid, &numerics.solve)?;
        // Both sides warm-started from the central source on the same
        // grid (common sample points), vacuum fields.
        let at = |d: &Device, s: &Solution| -> Result<f64, ChainError> {
            Ok(rate(
                &run_from(d, s, &warm, Some(&q_c))?,
                reading.supply_current_a,
                g[0],
                wl[0],
            ))
        };
        let f = at(&dv, &sv)? / at(&dev_c, &sol_c)?.max(1e-300);
        let e = f.ln().abs();
        push(
            "voltage",
            BandKind::Input,
            (-e).exp(),
            e.exp(),
            format!(
                "meter ±{:.0}%: vacuum ratio {f:.3} at +, symmetrized",
                100.0 * reading.voltage_rel
            ),
        );
    }
    for (name, which) in [
        ("sigma_cx", 0),
        ("sigma_ionization", 1),
        ("sigma_breakup", 2),
    ] {
        let mut lo_hi = [0.0; 2];
        for (k, f) in [1.0 - model.atomic_data_rel, 1.0 + model.atomic_data_rel]
            .iter()
            .enumerate()
        {
            let o = match which {
                0 => ChainOptions {
                    sigma_cx_scale: *f,
                    ..warm
                },
                1 => ChainOptions {
                    sigma_ion_scale: *f,
                    ..warm
                },
                _ => ChainOptions {
                    sigma_cid_scale: *f,
                    ..warm
                },
            };
            lo_hi[k] = ratio_of(&rerun(&o)?);
        }
        push(
            name,
            BandKind::Input,
            lo_hi[0],
            lo_hi[1],
            format!(
                "cited fits ×(1±{:.2}) (full chain rerun)",
                model.atomic_data_rel
            ),
        );
    }

    // ── model form ─────────────────────────────────────────────────────
    push(
        "gamma_se",
        BandKind::ModelForm,
        rate(&central, reading.supply_current_a, g[1], wl[0]) / c0,
        rate(&central, reading.supply_current_a, g[2], wl[0]) / c0,
        format!(
            "γ_SE at 15 keV/amu {:.2}…{:.2} (central {:.2}); changes the supply→ion partition",
            g[1], g[2], g[0]
        ),
    );
    push(
        "wall_loading",
        BandKind::ModelForm,
        rate(&central, reading.supply_current_a, g[0], wl[1]) / c0,
        rate(&central, reading.supply_current_a, g[0], wl[2]) / c0,
        format!(
            "wall D loading {:.3}…{:.3} of a Ti drive-in target (central {:.3}); cathode assumed \
             hot (no retained D)",
            wl[1], wl[2], wl[0]
        ),
    );
    {
        let fa = model.born_atomic_fraction;
        let mut lo_hi = [0.0; 2];
        for (k, f) in [fa[1], fa[2]].iter().enumerate() {
            let o = ChainOptions {
                born_atomic_fraction: *f,
                ..warm
            };
            lo_hi[k] = ratio_of(&rerun(&o)?);
        }
        push(
            "born_species",
            BandKind::ModelForm,
            lo_hi[0],
            lo_hi[1],
            format!(
                "born D⁺ fraction {:.2}…{:.2} (central {:.2}; rest D₂⁺)",
                fa[1], fa[2], fa[0]
            ),
        );
    }
    {
        let o = ChainOptions {
            d3_formation: !warm.d3_formation,
            ..warm
        };
        let f = ratio_of(&rerun(&o)?);
        push(
            "d3_formation",
            BandKind::ModelForm,
            f.min(1.0),
            f.max(1.0),
            format!(
                "D₂⁺→D₃⁺ conversion toggled (ratio {f:.3}); D₃⁺ collision data borrowed from D₂⁺"
            ),
        );
    }
    {
        let o = ChainOptions {
            d3_cid_lab_frame: !warm.d3_cid_lab_frame,
            ..warm
        };
        let f = ratio_of(&rerun(&o)?);
        push(
            "d3_breakup_frame",
            BandKind::ModelForm,
            f.min(1.0),
            f.max(1.0),
            format!(
                "D₃⁺ dissociation fit (Jül-4105 Eq. 243) read in the lab instead of the CM frame \
                 (ratio {f:.3})"
            ),
        );
    }
    {
        // Source shape: the modeled births sustain k per born ion; the
        // (1−k) remainder (or excess) comes from processes the model does
        // not carry. Bound that fraction between an interior-uniform and
        // an edge-born source.
        let kk = central.mode.sustainment;
        let f_unmodeled = (1.0 - kk).abs().min(1.0);
        let single = |shape: InitialSource| -> Result<Tally, ChainError> {
            let o = ChainOptions {
                initial_source: shape,
                max_generations: 1,
                min_generations: 1,
                average_generations: 1,
                ..base
            };
            Ok(run(&dev_c, &sc_sol, &o)?.mode)
        };
        let uni = single(InitialSource::UniformVolume)?;
        let edge = single(InitialSource::EdgeShell)?;
        let mix = |alt: &Tally| -> f64 {
            let m = &central.mode;
            let y = (1.0 - f_unmodeled) * m.neutrons(wl[0]) + f_unmodeled * alt.neutrons(wl[0]);
            let c = (1.0 - f_unmodeled) * m.supply_charges(g[0])
                + f_unmodeled * alt.supply_charges(g[0]);
            (y / c) / m.neutrons_per_supply_charge(g[0], wl[0]).max(1e-300)
        };
        let (a, b) = (mix(&uni), mix(&edge));
        push(
            "source_shape",
            BandKind::ModelForm,
            a.min(b).min(1.0),
            a.max(b).max(1.0),
            format!(
                "modeled sustainment M = {kk:.3} (ionization births per ion lost); the unmodeled \
                 |1−M| = {f_unmodeled:.3} of ion production bounded between uniform-volume and \
                 edge-born sources"
            ),
        );
    }
    {
        let v = rate(&vacuum, reading.supply_current_a, g[0], wl[0]) / c0;
        push(
            "space_charge",
            BandKind::ModelForm,
            v.min(1.0),
            v.max(1.0),
            format!(
                "central carries the self-consistent ion space charge (peak/well {sc_ratio:.3}); \
                 electron neutralization could restore the vacuum field (ratio {v:.3})"
            ),
        );
    }

    // ── combine ────────────────────────────────────────────────────────
    let mut down = 0.0;
    let mut up = 0.0;
    let denoise = |c: &BandComponent| -> (f64, f64) {
        let (d, u) = c.log_excursions();
        if c.rerun {
            let n2 = noise * noise;
            ((d * d - n2).max(0.0).sqrt(), (u * u - n2).max(0.0).sqrt())
        } else {
            (d, u)
        }
    };
    for kind in [BandKind::Numerical, BandKind::Input] {
        let (mut d2, mut u2) = (0.0, 0.0);
        for c in comps.iter().filter(|c| c.kind == kind) {
            let (d, u) = denoise(c);
            d2 += d * d;
            u2 += u * u;
        }
        down += d2.sqrt();
        up += u2.sqrt();
    }
    for c in comps.iter().filter(|c| c.kind == BandKind::ModelForm) {
        let (d, u) = denoise(c);
        down += d;
        up += u;
    }
    let m = &central.mode;
    let tot = m.neutrons(wl[0]).max(1e-300);
    Ok(YieldPrediction {
        reading: *reading,
        central_n_per_s: c0,
        lower_n_per_s: c0 * (-down).exp(),
        upper_n_per_s: c0 * up.exp(),
        components: comps,
        mode: *m,
        vacuum_n_per_s: rate(&vacuum, reading.supply_current_a, g[0], wl[0]),
        space_charge_ratio: sc_ratio,
        cathode_ion_current_a: central.cathode_ion_current_a(reading.supply_current_a, g[0]),
        generations: central.generations.len(),
        converged: central.converged,
        channel_fractions: [
            m.neutrons_ion / tot,
            m.neutrons_fast_neutral / tot,
            wl[0] * m.neutrons_wall_per_loading / tot,
        ],
        grid: [numerics.grid.0, numerics.grid.1],
        sampling_noise_log: noise,
        sor: (numerics.solve.tol, sol_c.sweeps),
        ensemble: (numerics.chain.particles, numerics.chain.trace.max_passes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poisson::solve;

    fn small() -> (Device, Solution) {
        let device = Device::classic_fusor(76.0, 19.0, 4, 1.0, -30_000.0);
        let sol = solve(&device, 61, 121, &SolveOptions::default()).unwrap();
        (device, sol)
    }

    fn quick() -> ChainOptions {
        ChainOptions {
            particles: 16,
            electrons: 4,
            max_generations: 3,
            min_generations: 2,
            average_generations: 2,
            generation_tol: 0.1,
            trace: TraceOptions {
                max_passes: 200,
                step_fraction: 0.25,
                time_budget_factor: 4.0,
                ..TraceOptions::default()
            },
            ..ChainOptions::default()
        }
    }

    #[test]
    fn chain_produces_a_finite_mode_with_every_channel_alive() {
        let (device, sol) = small();
        let r = run(&device, &sol, &quick()).unwrap();
        let m = r.mode;
        assert!(m.neutrons_ion > 0.0, "ion channel dead: {m:?}");
        assert!(
            m.neutrons_fast_neutral > 0.0,
            "fast-neutral channel dead: {m:?}"
        );
        assert!(
            m.neutrons_wall_per_loading > 0.0,
            "wall channel dead: {m:?}"
        );
        assert!(
            m.cathode_ions > 0.2,
            "most ions must land on the cathode: {m:?}"
        );
        assert!(m.births_cx > 0.0 && m.births_impact > 0.0, "{m:?}");
        assert!(m.births_electron > 0.0, "electron ionization dead: {m:?}");
        assert!(m.censored < 0.05, "censoring must be negligible: {m:?}");
        assert!(m.truncated < 0.01, "CX waves must be followed out: {m:?}");
        assert!(m.sustainment > 0.0, "{m:?}");
        // Ion number is conserved per unit source: CX relaunches and
        // breakups replace ions, so every source ion ends exactly once
        // (cathode, wall, censored, or still in the truncated wave).
        let ends = m.cathode_ions + m.wall_ions + m.censored + m.truncated;
        assert!((ends - 1.0).abs() < 0.05, "ion bookkeeping leaks: {ends}");
        let rate = r.rate_n_per_s(0.010, xs::GAMMA_SE_15KEV_CENTRAL, 0.0);
        assert!(rate > 1e2 && rate < 1e8, "rate {rate:e} n/s implausible");
    }

    #[test]
    fn following_cx_waves_adds_yield_over_the_single_generation_floor() {
        // The old floor killed an ion at its first charge exchange. Following
        // the relaunched cold ions can only add neutrons per source ion.
        let (device, sol) = small();
        let o = ChainOptions {
            max_generations: 1,
            min_generations: 1,
            average_generations: 1,
            ..quick()
        };
        let chain = run(&device, &sol, &o).unwrap();
        let single = run(&device, &sol, &ChainOptions { max_waves: 1, ..o }).unwrap();
        assert!(chain.mode.waves > 1.5, "{:?}", chain.mode);
        assert!(
            single.mode.truncated > 0.05,
            "one wave must leave relaunch weight"
        );
        assert!(
            chain.mode.neutrons(0.0) > single.mode.neutrons(0.0),
            "chain {:e} vs single {:e}",
            chain.mode.neutrons(0.0),
            single.mode.neutrons(0.0)
        );
    }

    #[test]
    fn higher_gamma_means_fewer_ions_per_amp() {
        let (device, sol) = small();
        let r = run(&device, &sol, &quick()).unwrap();
        let lo = r.rate_n_per_s(0.01, 0.6, 0.0);
        let hi = r.rate_n_per_s(0.01, 2.4, 0.0);
        assert!(
            lo > hi,
            "more secondary electrons per ion → fewer ions per amp"
        );
        let ic = r.cathode_ion_current_a(0.01, 1.2);
        assert!(
            ic > 0.0 && ic < 0.01,
            "cathode ion current {ic} must be a fraction of supply"
        );
    }

    #[test]
    fn magnetized_devices_are_refused() {
        let device = Device::shielded_two_ring(120.0, 40.0, 22.0, 4.0, -5_000.0, 1_000.0);
        let sol = solve(&device, 41, 81, &SolveOptions::default()).unwrap();
        assert_eq!(
            run(&device, &sol, &quick()).unwrap_err(),
            ChainError::Magnetics
        );
    }

    #[test]
    fn prediction_is_two_sided_and_can_be_violated() {
        let device = Device::classic_fusor(76.0, 19.0, 4, 1.0, -30_000.0);
        let reading = OperatorReading::hobby(0.010, -30_000.0, 5.0);
        let numerics = PredictNumerics {
            grid: (61, 121),
            coarse_grid: None,
            chain: quick(),
            space_charge_iterations: 1,
            step_check: false,
            ..PredictNumerics::default()
        };
        let p = predict(&device, &reading, &ModelForm::default(), &numerics).unwrap();
        // Structure only on this tiny grid — width belongs to the
        // production run (examples/open_fusor_band.rs).
        assert!(p.lower_n_per_s > 0.0);
        assert!(p.lower_n_per_s < p.central_n_per_s && p.central_n_per_s < p.upper_n_per_s);
        let f: f64 = p.channel_fractions.iter().sum();
        assert!((f - 1.0).abs() < 1e-9);
        assert!(p.components.iter().any(|c| c.kind == BandKind::ModelForm));
        // The banded claim can lose: just outside → Violated, inside → Holds.
        let set = crate::receipt::yield_band_claims(&p);
        let m = |v: f64| crate::receipt::Measurement {
            name: "ddn_neutron_rate".into(),
            value: v,
            uncertainty: 0.0,
            instrument: "test".into(),
            band_factor: 1e9, // ignored: the claim owns its band
        };
        let verdict = |v: f64| crate::receipt::compare(&set, &[m(v)]).unwrap().entries[0].verdict;
        use crate::receipt::Verdict;
        assert_eq!(verdict(p.central_n_per_s), Verdict::Holds);
        assert_eq!(verdict(0.99 * p.lower_n_per_s), Verdict::Violated);
        assert_eq!(verdict(1.01 * p.upper_n_per_s), Verdict::Violated);
    }
}
