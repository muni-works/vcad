//! Boris-pusher particle tracing through a solved device.
//!
//! Particles are integrated with the standard Boris scheme (exact energy
//! behavior in static E, exact gyration in static B), with adaptive time
//! steps: displacement per step is capped at a fraction of the grid
//! spacing, and steps subdivide automatically where the coil field is
//! strong so gyration stays resolved.
//!
//! A trace ends when the particle hits a wire ring, reaches the chamber
//! wall, or survives to its pass/time budget. Passes through the central
//! core are counted — the recirculation statistic that grid shielding is
//! supposed to improve.

use crate::device::Device;
use crate::field::{FieldMap, FieldSampler};
use crate::poisson::Solution;

/// A charged species.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Species {
    /// Rest mass, kg.
    pub mass_kg: f64,
    /// Charge, C (signed).
    pub charge_c: f64,
}

/// Deuteron (D⁺).
pub const DEUTERON: Species = Species {
    mass_kg: crate::constants::DEUTERON_MASS,
    charge_c: crate::constants::ELEMENTARY_CHARGE,
};

/// Electron.
pub const ELECTRON: Species = Species {
    mass_kg: crate::constants::ELECTRON_MASS,
    charge_c: -crate::constants::ELEMENTARY_CHARGE,
};

/// Sinusoidal modulation of **all** electrode potentials together:
/// `V(t) = V₀ · (1 + m·sin(2πft + φ))`.
///
/// Because the wall sits at 0 V and Dirichlet problems superpose, scaling
/// every electrode by the same factor scales the whole field exactly — no
/// re-solve needed. This is the POPS/RF-drive seam: tune `frequency_hz`
/// to the ion bounce frequency and the drive pumps the radial
/// oscillation (energy is deliberately not conserved under drive).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Drive {
    /// Fractional modulation depth m (0 = no drive).
    pub modulation: f64,
    /// Drive frequency, Hz.
    pub frequency_hz: f64,
    /// Phase at t = 0, radians.
    pub phase_rad: f64,
}

impl Drive {
    /// The field scale factor at time `t`.
    #[inline]
    pub fn scale(&self, t_s: f64) -> f64 {
        1.0 + self.modulation
            * (2.0 * std::f64::consts::PI * self.frequency_hz * t_s + self.phase_rad).sin()
    }
}

/// How a trace ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Intercepted by electrode `k` (index into `Device::rings`, or
    /// `Device3d::wires` for 3D tracers).
    Wire(usize),
    /// Reached the chamber wall or an end cap.
    Wall,
    /// Still alive at the pass/time/step budget (censored).
    Survived,
}

/// Result of tracing one particle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceOutcome {
    /// Terminal event.
    pub fate: Fate,
    /// Number of entries into the central core.
    pub core_passes: u32,
    /// Flight time, s.
    pub time_s: f64,
    /// Integrator steps (including substeps).
    pub steps: u64,
    /// Worst relative energy drift observed in the far-field region,
    /// normalized by the deepest potential drop. Integration-quality
    /// diagnostic; should be ≪ 1.
    pub energy_drift_rel: f64,
    /// cos θ of the launch direction (θ from +z), for post-hoc binning.
    pub launch_cos_theta: f64,
    /// Path integral ∫ σ_DDn(E)·v dt over the whole trace, m³ — the
    /// beam-on-background D(d,n)³He reaction volume with **no
    /// charge-exchange attrition** (kept raw for cross-config
    /// comparability). Multiply by target deuteron density for expected
    /// neutrons per ion; zero for non-deuteron species.
    pub ddn_sigma_v_m3: f64,
    /// Path integral ∫ σ_DDp(E)·v dt (the proton branch), m³ — with the
    /// neutron branch this prices total fusion power for the receipt.
    pub ddp_sigma_v_m3: f64,
    /// Expected D-D neutrons per injected ion from the surviving-ion
    /// channel (CX-survival-weighted). Zero unless a [`CxModel`] is set.
    pub neutrons_ion_channel: f64,
    /// Expected D-D neutrons per injected ion from fast neutrals born at
    /// charge-exchange events (straight-line continuation at full energy).
    /// Zero unless a [`CxModel`] is set.
    pub neutrons_cx_channel: f64,
    /// Position at termination, m (the impact point for wire/wall fates).
    pub final_pos: [f64; 3],
    /// Velocity at termination, m/s (synchronized with `final_pos`).
    pub final_vel: [f64; 3],
}

/// Time integrator for [`Tracer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Integrator {
    /// Synchronized kick–rotate–drift–rotate–kick (velocity-Verlet form of
    /// Boris): positions and velocities live at the same instant, so the
    /// energy diagnostic is exact and the scheme stays second order when
    /// the adaptive step changes from one step to the next. Default.
    Synchronized,
    /// The original staggered Boris leapfrog (velocity at half steps).
    /// First order under per-step adaptive `dt` — kept for bit-for-bit
    /// comparison with pre-M7 results.
    Leapfrog,
}

/// One integrator step, as seen by a [`Tracer::trace_observed`] observer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepEvent {
    /// Position after the step, m.
    pub pos: [f64; 3],
    /// Velocity after the step (synchronized), m/s.
    pub vel: [f64; 3],
    /// Step duration, s.
    pub dt_s: f64,
    /// Path length of the step, m.
    pub ds_m: f64,
    /// Kinetic energy after the step, eV.
    pub kinetic_ev: f64,
    /// Elapsed flight time, s.
    pub time_s: f64,
}

/// A traced particle with its recorded flight path.
#[derive(Debug, Clone, PartialEq)]
pub struct TracePath {
    /// The trace's outcome (identical to an unrecorded trace).
    pub outcome: TraceOutcome,
    /// Flight path, millimetres, Cartesian xyz.
    pub points_mm: Vec<[f64; 3]>,
}

/// Spacing-gated, self-decimating polyline recorder (see
/// [`Tracer::trace_path`]).
#[derive(Debug)]
struct PathRecorder {
    spacing2: f64,
    max_points: usize,
    points: Vec<[f64; 3]>,
}

impl PathRecorder {
    fn new(spacing_mm: f64, max_points: usize) -> Self {
        let s = spacing_mm.max(1e-6);
        Self {
            spacing2: s * s,
            max_points: max_points.max(4),
            points: Vec::new(),
        }
    }

    fn push(&mut self, p_m: [f64; 3]) {
        let p = [p_m[0] * 1e3, p_m[1] * 1e3, p_m[2] * 1e3];
        if let Some(last) = self.points.last() {
            let d = [p[0] - last[0], p[1] - last[1], p[2] - last[2]];
            if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < self.spacing2 {
                return;
            }
        }
        if self.points.len() >= self.max_points {
            // Keep the first point and every other one after it.
            let kept: Vec<[f64; 3]> = self.points.iter().copied().step_by(2).collect();
            self.points = kept;
            self.spacing2 *= 4.0;
        }
        self.points.push(p);
    }

    fn end(&mut self, p_m: [f64; 3]) {
        let p = [p_m[0] * 1e3, p_m[1] * 1e3, p_m[2] * 1e3];
        if self.points.last() != Some(&p) {
            if self.points.len() >= self.max_points {
                self.points.pop();
            }
            self.points.push(p);
        }
    }

    fn finish(self) -> Vec<[f64; 3]> {
        self.points
    }
}

/// Charge-exchange model: constant cross-section approximation for
/// D⁺ + D₂ → fast D⁰ + slow D₂⁺ against a uniform background gas.
///
/// σ_cx for deuterons on D₂ is of order 1×10⁻¹⁹ m² across the 1–30 keV
/// band (falling slowly with energy); supply your own value — tabulated
/// energy-dependent data is a receipts-milestone upgrade. With a model
/// present, traces report **expected neutrons per injected ion** in two
/// channels: the surviving-ion channel (beam-on-background fusion,
/// weighted by the probability the ion has not yet charge-exchanged) and
/// the fast-neutral channel (the CX product flies straight at full energy
/// and keeps fusing until it exits). The CX chain (each event also births
/// a cold ion that re-accelerates) is not modeled here, so both channels
/// are floors.
///
/// **Superseded for yield predictions by [`crate::chain`]** (CX chain,
/// energy-dependent cited σ, molecular species, ionization source, supply
/// partition). Note the convention: this legacy model multiplies σ_cx by
/// the *deuteron* density, while the CX target is the D₂ *molecule*
/// (half that) — at a physical σ_cx it double-counts attrition. Kept
/// unchanged because the wasm/MCP `simulate_charged_particles` surface
/// passes these fields.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CxModel {
    /// Charge-exchange cross section, m².
    pub sigma_cx_m2: f64,
    /// Background deuteron density, m⁻³ (see
    /// [`crate::xsection::d2_deuteron_density_m3`]).
    pub background_deuteron_density_m3: f64,
}

/// Tracing options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceOptions {
    /// Stop after this many core passes (censor as [`Fate::Survived`]).
    pub max_passes: u32,
    /// Displacement per step as a fraction of the grid spacing.
    pub step_fraction: f64,
    /// Launch shell radius as a fraction of the smaller chamber dimension.
    pub launch_shell_fraction: f64,
    /// Launch directions span cos θ ∈ [−this, +this].
    pub launch_cos_max: f64,
    /// Flight-time budget in units of `max_passes` ideal crossing times.
    pub time_budget_factor: f64,
    /// Optional charge-exchange model; `None` leaves the CX channels zero.
    pub cx: Option<CxModel>,
    /// Optional RF drive on the electrode potentials; `None` = static
    /// fields. Under drive the energy-drift diagnostic is not
    /// accumulated (the drive does work by design).
    pub drive: Option<Drive>,
    /// Time integrator (default [`Integrator::Synchronized`]).
    pub integrator: Integrator,
    /// Cap on the fractional speed change per step, `|a|·dt ≤ f·|v|`
    /// (applied where the particle is already moving). Bounds the local
    /// error where the field is steep but the particle is not near a wire
    /// mask — the far-field complement of the near-wire refinement.
    pub accel_fraction: f64,
    /// Minimum near-wire step factor (the step shrinks linearly with
    /// distance to the wire surface down to this fraction of the base
    /// step).
    pub near_wire_min_factor: f64,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            max_passes: 40,
            step_fraction: 0.25,
            launch_shell_fraction: 0.85,
            launch_cos_max: 0.95,
            time_budget_factor: 12.0,
            cx: None,
            drive: None,
            integrator: Integrator::Synchronized,
            accel_fraction: 0.05,
            near_wire_min_factor: 0.1,
        }
    }
}

/// Collision and step-control geometry for the tracer — everything it
/// needs besides the field. [`Tracer::new`] builds the axisymmetric
/// ring + cylinder geometry; 3D devices supply their own (see
/// [`crate::bem::WireField`]). Positions are SI meters.
pub trait TraceGeometry: std::fmt::Debug + Send + Sync {
    /// Index of the electrode whose conductor contains `p`, if any.
    fn wire_hit(&self, p: [f64; 3]) -> Option<usize>;
    /// Whether `p` has reached the chamber wall.
    fn at_wall(&self, p: [f64; 3]) -> bool;
    /// Near-wire step-refinement ratio: the smallest `(d − a)/(6a)` over
    /// electrodes (`d` = distance to the wire axis), or ≥ 1 far from all
    /// of them. The tracer scales its step by this, clamped to [0.1, 1].
    fn near_wire_ratio(&self, p: [f64; 3]) -> f64;
    /// Straight-line distance from `p` along `v` to the chamber wall —
    /// fast-neutral continuation after charge exchange.
    fn exit_distance(&self, p: [f64; 3], v: [f64; 3]) -> f64;
}

/// Length and energy scales for a tracer built with
/// [`Tracer::with_geometry`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceScales {
    /// Field-resolution length, m: the base step is
    /// `step_fraction × step_length_m / speed`.
    pub step_length_m: f64,
    /// Core radius for pass counting, m.
    pub core_radius_m: f64,
    /// Launch shell radius, m.
    pub shell_radius_m: f64,
    /// Deepest electrode-to-wall potential drop, V (velocity scale).
    pub potential_drop_v: f64,
}

#[derive(Debug, Clone, Copy)]
struct WireHit {
    r0: f64,
    z0: f64,
    a: f64,
    a2: f64,
}

/// The axisymmetric device geometry: coaxial wire rings in a closed
/// cylinder.
#[derive(Debug, Clone)]
struct AxisymGeometry {
    wires: Vec<WireHit>,
    r_wall: f64,
    z_wall: f64,
}

impl TraceGeometry for AxisymGeometry {
    fn wire_hit(&self, p: [f64; 3]) -> Option<usize> {
        let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
        for (k, w) in self.wires.iter().enumerate() {
            let dr = r - w.r0;
            let dz = p[2] - w.z0;
            if dr * dr + dz * dz <= w.a2 {
                return Some(k);
            }
        }
        None
    }

    fn at_wall(&self, p: [f64; 3]) -> bool {
        let r2 = p[0] * p[0] + p[1] * p[1];
        r2 > self.r_wall * self.r_wall || p[2].abs() > self.z_wall
    }

    fn near_wire_ratio(&self, p: [f64; 3]) -> f64 {
        let r_now = (p[0] * p[0] + p[1] * p[1]).sqrt();
        let mut factor = 1.0_f64;
        for w in &self.wires {
            let drw = r_now - w.r0;
            let dzw = p[2] - w.z0;
            let ratio = ((drw * drw + dzw * dzw).sqrt() - w.a) / (6.0 * w.a);
            if ratio < factor {
                factor = ratio;
            }
        }
        factor
    }

    /// Straight-line distance from `p` along `v` to the chamber boundary
    /// (cylinder wall or either end cap), meters.
    fn exit_distance(&self, p: [f64; 3], v: [f64; 3]) -> f64 {
        let speed = dot(v, v).sqrt();
        if speed < 1e-12 {
            return 0.0;
        }
        let d = scale(v, 1.0 / speed);
        // Cylinder r = r_wall in the xy plane.
        let a = d[0] * d[0] + d[1] * d[1];
        let t_cyl = if a > 1e-18 {
            let b = p[0] * d[0] + p[1] * d[1];
            let c = p[0] * p[0] + p[1] * p[1] - self.r_wall * self.r_wall;
            let disc = b * b - a * c;
            if disc > 0.0 {
                let t = (-b + disc.sqrt()) / a;
                if t > 0.0 {
                    t
                } else {
                    f64::INFINITY
                }
            } else {
                f64::INFINITY
            }
        } else {
            f64::INFINITY
        };
        // End caps z = ±z_wall.
        let t_cap = if d[2] > 1e-18 {
            (self.z_wall - p[2]) / d[2]
        } else if d[2] < -1e-18 {
            (-self.z_wall - p[2]) / d[2]
        } else {
            f64::INFINITY
        };
        t_cyl.min(t_cap.max(0.0)).max(0.0)
    }
}

/// Prepared tracer for one device + field.
#[derive(Debug)]
pub struct Tracer<'a> {
    fields: &'a dyn FieldSampler,
    geometry: Box<dyn TraceGeometry>,
    /// (nr, nz, dr, dz, z_half) of the axisymmetric grid, for dwell
    /// deposits; `None` for tracers without one.
    grid: Option<(usize, usize, f64, f64, f64)>,
    /// Axisymmetric wire tori `(r0, z0, a)` (empty for generic geometry).
    axisym_tori: Vec<(f64, f64, f64)>,
    core_radius: f64,
    shell_radius: f64,
    drop_v: f64,
    h: f64,
    opts: TraceOptions,
}

const MAX_STEPS: u64 = 20_000_000;

impl<'a> Tracer<'a> {
    /// Prepare a tracer. `solution` supplies the grid spacing (for step
    /// sizing and the wire hit radius, which matches the Dirichlet mask).
    pub fn new(
        device: &Device,
        fields: &'a FieldMap<'a>,
        solution: &Solution,
        opts: TraceOptions,
    ) -> Self {
        let h = solution.dr.min(solution.dz);
        let mask_floor = 0.75 * solution.dr.max(solution.dz);
        let wires: Vec<WireHit> = device
            .rings
            .iter()
            .map(|ring| {
                let a = (ring.wire_radius_mm * 1e-3).max(mask_floor);
                WireHit {
                    r0: ring.ring_radius_mm * 1e-3,
                    z0: ring.z_mm * 1e-3,
                    a,
                    a2: a * a,
                }
            })
            .collect();
        let grid = (
            solution.nr,
            solution.nz,
            solution.dr,
            solution.dz,
            solution.z_half,
        );
        let r_wall = device.chamber_radius_mm * 1e-3 - 0.6 * solution.dr;
        let z_wall = device.chamber_half_height_mm * 1e-3 - 0.6 * solution.dz;
        let min_dim = (device.chamber_radius_mm.min(device.chamber_half_height_mm)) * 1e-3;
        let shell_radius = opts.launch_shell_fraction * min_dim;
        let core_radius = 0.35 * device.min_ring_spherical_radius_mm() * 1e-3;
        let drop_v = device.max_potential_drop_v().max(1.0);
        let axisym_tori = wires.iter().map(|w: &WireHit| (w.r0, w.z0, w.a)).collect();
        Self {
            fields,
            axisym_tori,
            geometry: Box::new(AxisymGeometry {
                wires,
                r_wall,
                z_wall,
            }),
            grid: Some(grid),
            core_radius,
            shell_radius,
            drop_v,
            h,
            opts,
        }
    }

    /// Prepare a tracer over any field and collision geometry (3D
    /// devices). There is no (r, z) grid, so
    /// [`Self::launch_ensemble_dwell`] returns an empty dwell vector.
    pub fn with_geometry(
        fields: &'a dyn FieldSampler,
        geometry: Box<dyn TraceGeometry>,
        scales: TraceScales,
        opts: TraceOptions,
    ) -> Self {
        Self {
            fields,
            geometry,
            grid: None,
            axisym_tori: Vec::new(),
            core_radius: scales.core_radius_m,
            shell_radius: scales.shell_radius_m,
            drop_v: scales.potential_drop_v.max(1.0),
            h: scales.step_length_m,
            opts,
        }
    }

    /// Trace one particle from `pos` (m) with velocity `vel` (m/s).
    pub fn trace_from(&self, species: Species, pos: [f64; 3], vel: [f64; 3]) -> TraceOutcome {
        self.trace_impl(species, pos, vel, None, None)
    }

    /// Trace one particle, calling `observer` after every integrator step.
    /// The observer returns `false` to stop the trace (reported as
    /// [`Fate::Survived`]; the observer knows why it stopped). This is the
    /// seam [`crate::chain`] hangs its collision physics on.
    pub fn trace_observed(
        &self,
        species: Species,
        pos: [f64; 3],
        vel: [f64; 3],
        observer: &mut dyn FnMut(&StepEvent) -> bool,
    ) -> TraceOutcome {
        self.trace_impl(species, pos, vel, None, Some(observer))
    }

    /// Grid geometry `(nr, nz, dr, dz, z_half)` of the axisymmetric
    /// solution this tracer was built on (for callers depositing on the
    /// same grid); `None` for [`Self::with_geometry`] tracers.
    pub fn grid(&self) -> Option<(usize, usize, f64, f64, f64)> {
        self.grid
    }

    /// Axisymmetric wire hit tori as `(ring_radius_m, z_m, hit_radius_m)`
    /// (empty for [`Self::with_geometry`] tracers).
    pub fn wire_tori(&self) -> Vec<(f64, f64, f64)> {
        self.axisym_tori.clone()
    }

    /// Straight-line distance from `p` along `v` to the chamber boundary,
    /// m (fast-neutral continuation).
    pub fn exit_distance_m(&self, p: [f64; 3], v: [f64; 3]) -> f64 {
        self.geometry.exit_distance(p, v)
    }

    /// [`Self::trace_from`], additionally recording the flight path as a
    /// polyline in millimetres (Cartesian xyz). Points are kept at least
    /// `spacing_mm` apart; when the path would exceed `max_points`, every
    /// other point is dropped and the spacing doubles, so a long
    /// recirculating trace still covers its whole flight. The first and
    /// terminal positions are always present. Recording does not change
    /// the outcome (same integrator, same steps).
    pub fn trace_path(
        &self,
        species: Species,
        pos: [f64; 3],
        vel: [f64; 3],
        spacing_mm: f64,
        max_points: usize,
    ) -> TracePath {
        let mut rec = PathRecorder::new(spacing_mm, max_points);
        rec.push(pos);
        let outcome = self.trace_observed(species, pos, vel, &mut |ev| {
            rec.push(ev.pos);
            true
        });
        rec.end(outcome.final_pos);
        TracePath {
            outcome,
            points_mm: rec.finish(),
        }
    }

    /// The deterministic launch point of particle `k` of `n` (at rest on
    /// the launch shell, uniform in cos θ, in the x–z meridional plane) —
    /// the same positions [`Self::launch_ensemble`] uses.
    pub fn launch_position(&self, k: usize, n: usize) -> [f64; 3] {
        let n = n.max(2);
        let c =
            -self.opts.launch_cos_max + 2.0 * self.opts.launch_cos_max * k as f64 / (n - 1) as f64;
        let s = (1.0 - c * c).max(0.0).sqrt();
        [self.shell_radius * s, 0.0, self.shell_radius * c]
    }

    /// [`Self::launch_ensemble`] with every particle's path recorded (see
    /// [`Self::trace_path`]). Outcomes are identical to `launch_ensemble`.
    pub fn launch_ensemble_paths(
        &self,
        species: Species,
        n: usize,
        spacing_mm: f64,
        max_points: usize,
    ) -> Vec<TracePath> {
        let n = n.max(2);
        (0..n)
            .map(|k| {
                self.trace_path(
                    species,
                    self.launch_position(k, n),
                    [0.0, 0.0, 0.0],
                    spacing_mm,
                    max_points,
                )
            })
            .collect()
    }

    fn trace_impl(
        &self,
        species: Species,
        pos: [f64; 3],
        vel: [f64; 3],
        mut dwell: Option<&mut [f64]>,
        mut observer: Option<&mut dyn FnMut(&StepEvent) -> bool>,
    ) -> TraceOutcome {
        let mut p = pos;
        let mut v = vel;
        let qm = species.charge_c / species.mass_kg;
        let v_ref = (2.0 * species.charge_c.abs() * self.drop_v / species.mass_kg).sqrt();
        let dv_scale = species.charge_c.abs() * self.drop_v;
        let t_max = self.opts.time_budget_factor
            * self.opts.max_passes as f64
            * (2.0 * self.shell_radius / v_ref);

        let h0 = 0.5 * species.mass_kg * dot(v, v) + species.charge_c * self.fields.potential(p);
        let far2 = (0.5 * self.shell_radius).powi(2);

        let mut fate = Fate::Survived;
        let mut passes = 0_u32;
        let mut inside_core = sq_len(p) < self.core_radius * self.core_radius;
        let mut time = 0.0_f64;
        let mut steps = 0_u64;
        let mut drift = 0.0_f64;
        let mut sigv = 0.0_f64;
        let mut sigv_p = 0.0_f64;
        let mut survival = 1.0_f64;
        let mut n_ion = 0.0_f64;
        let mut n_cx = 0.0_f64;
        let launch_cos_theta = p[2] / sq_len(p).sqrt().max(1e-300);
        let deuteron_like =
            species.charge_c > 0.0 && (species.mass_kg / DEUTERON.mass_kg - 1.0).abs() < 0.01;
        let cx = if deuteron_like { self.opts.cx } else { None };

        let synced = self.opts.integrator == Integrator::Synchronized;
        let field_at = |p: [f64; 3], t: f64| -> [f64; 3] {
            let e = self.fields.e_cart(p);
            match &self.opts.drive {
                Some(d) => scale(e, d.scale(t)),
                None => e,
            }
        };
        // Synchronized scheme caches E at the current position.
        let mut e_now = field_at(p, 0.0);

        'outer: while time < t_max && steps < MAX_STEPS {
            let speed = dot(v, v).sqrt();
            let mut dt = self.opts.step_fraction * self.h / speed.max(0.05 * v_ref);
            if synced && self.opts.accel_fraction > 0.0 && speed > 0.05 * v_ref {
                let a = qm.abs() * dot(e_now, e_now).sqrt();
                if a > 0.0 {
                    dt = dt.min(self.opts.accel_fraction * speed / a);
                }
            }
            // Refine the step near wires, where field gradients are the
            // steepest thing in the problem (bounds the energy drift).
            dt *= self
                .geometry
                .near_wire_ratio(p)
                .clamp(self.opts.near_wire_min_factor.clamp(1e-4, 1.0), 1.0);
            let b = self.fields.b_cart(p);
            let bmag = dot(b, b).sqrt();
            let n_sub = ((qm.abs() * bmag * dt / 0.3).ceil() as u64).clamp(1, 64);
            let dth = dt / n_sub as f64;
            for _ in 0..n_sub {
                let p_prev = p;
                if synced {
                    // kick(½) – rotate(½) – drift – rotate(½) – kick(½)
                    let half = 0.5 * qm * dth;
                    let v1 = add(v, scale(e_now, half));
                    let v2 = boris_rotate(v1, self.fields.b_cart(p), 0.5 * half);
                    p = add(p, scale(v2, dth));
                    time += dth;
                    let v3 = boris_rotate(v2, self.fields.b_cart(p), 0.5 * half);
                    e_now = field_at(p, time);
                    v = add(v3, scale(e_now, half));
                } else {
                    let e = field_at(p, time);
                    let b = self.fields.b_cart(p);
                    let half = 0.5 * qm * dth;
                    let vm = add(v, scale(e, half));
                    let vp = boris_rotate(vm, b, half);
                    v = add(vp, scale(e, half));
                    p = add(p, scale(v, dth));
                    time += dth;
                }
                steps += 1;
                let ds = sq_len([p[0] - p_prev[0], p[1] - p_prev[1], p[2] - p_prev[2]]).sqrt();

                if let (Some(d), Some((nr, nz, dr, dz, z_half))) = (dwell.as_deref_mut(), self.grid)
                {
                    let i = ((p[0] * p[0] + p[1] * p[1]).sqrt() / dr).round() as usize;
                    let j = (((p[2] + z_half) / dz).round().max(0.0)) as usize;
                    if i < nr && j < nz {
                        // Expected population: weight by CX survival
                        // (1 when no CX model is set).
                        d[i * nz + j] += survival * dth;
                    }
                }

                if deuteron_like {
                    let v2 = dot(v, v);
                    let e_lab_kev =
                        0.5 * species.mass_kg * v2 / (crate::constants::ELEMENTARY_CHARGE * 1.0e3);
                    let sig = crate::xsection::dd_n_sigma_m2(0.5 * e_lab_kev);
                    let sig_p = crate::xsection::dd_p_sigma_m2(0.5 * e_lab_kev);
                    if sig > 0.0 {
                        let speed_now = v2.sqrt();
                        sigv += sig * speed_now * dth;
                        sigv_p += sig_p * speed_now * dth;
                        if let Some(cxm) = &cx {
                            let n_bg = cxm.background_deuteron_density_m3;
                            n_ion += survival * n_bg * sig * speed_now * dth;
                            let rate_cx = n_bg * cxm.sigma_cx_m2 * speed_now;
                            let p_birth = survival * rate_cx * dth;
                            if p_birth > 0.0 {
                                let l_exit = self.geometry.exit_distance(p, v);
                                n_cx += p_birth * n_bg * sig * l_exit;
                            }
                            survival *= (-rate_cx * dth).exp();
                        }
                    } else if let Some(cxm) = &cx {
                        // Below the fusion floor, CX attrition still runs.
                        let speed_now = v2.sqrt();
                        let rate_cx =
                            cxm.background_deuteron_density_m3 * cxm.sigma_cx_m2 * speed_now;
                        survival *= (-rate_cx * dth).exp();
                    }
                }

                if self.geometry.at_wall(p) {
                    fate = Fate::Wall;
                    break 'outer;
                }
                if let Some(obs) = observer.as_deref_mut() {
                    let ev = StepEvent {
                        pos: p,
                        vel: v,
                        dt_s: dth,
                        ds_m: ds,
                        kinetic_ev: 0.5 * species.mass_kg * dot(v, v)
                            / crate::constants::ELEMENTARY_CHARGE,
                        time_s: time,
                    };
                    if !obs(&ev) {
                        break 'outer;
                    }
                }
                if let Some(k) = self.geometry.wire_hit(p) {
                    fate = Fate::Wire(k);
                    break 'outer;
                }
                let s2 = sq_len(p);
                let now_inside = s2 < self.core_radius * self.core_radius;
                if now_inside && !inside_core {
                    passes += 1;
                    if passes >= self.opts.max_passes {
                        break 'outer;
                    }
                }
                inside_core = now_inside;
                if s2 > far2 && self.opts.drive.is_none() {
                    let hnow = 0.5 * species.mass_kg * dot(v, v)
                        + species.charge_c * self.fields.potential(p);
                    let d = (hnow - h0).abs() / dv_scale;
                    if d > drift {
                        drift = d;
                    }
                }
            }
        }

        TraceOutcome {
            fate,
            core_passes: passes,
            time_s: time,
            steps,
            energy_drift_rel: drift,
            launch_cos_theta,
            ddn_sigma_v_m3: sigv,
            ddp_sigma_v_m3: sigv_p,
            neutrons_ion_channel: n_ion,
            neutrons_cx_channel: n_cx,
            final_pos: p,
            final_vel: v,
        }
    }

    /// [`Self::launch_ensemble`], additionally accumulating per-node dwell
    /// time (seconds, nearest-node deposit, row-major `i·nz + j`) — the
    /// input to the space-charge estimator. Empty for tracers built with
    /// [`Self::with_geometry`] (no (r, z) grid).
    pub fn launch_ensemble_dwell(
        &self,
        species: Species,
        n: usize,
    ) -> (Vec<TraceOutcome>, Vec<f64>) {
        let mut dwell = match self.grid {
            Some((nr, nz, _, _, _)) => vec![0.0_f64; nr * nz],
            None => Vec::new(),
        };
        let n = n.max(2);
        let outcomes = (0..n)
            .map(|k| {
                let c = -self.opts.launch_cos_max
                    + 2.0 * self.opts.launch_cos_max * k as f64 / (n - 1) as f64;
                let s = (1.0 - c * c).max(0.0).sqrt();
                let pos = [self.shell_radius * s, 0.0, self.shell_radius * c];
                self.trace_impl(species, pos, [0.0, 0.0, 0.0], Some(&mut dwell), None)
            })
            .collect();
        (outcomes, dwell)
    }

    /// Launch `n` particles at rest on the launch shell, on a deterministic
    /// grid of polar angles (uniform in cos θ), and trace each.
    pub fn launch_ensemble(&self, species: Species, n: usize) -> Vec<TraceOutcome> {
        let n = n.max(2);
        (0..n)
            .map(|k| {
                let c = -self.opts.launch_cos_max
                    + 2.0 * self.opts.launch_cos_max * k as f64 / (n - 1) as f64;
                let s = (1.0 - c * c).max(0.0).sqrt();
                let pos = [self.shell_radius * s, 0.0, self.shell_radius * c];
                self.trace_from(species, pos, [0.0, 0.0, 0.0])
            })
            .collect()
    }

    /// Core radius used for pass counting, m.
    pub fn core_radius_m(&self) -> f64 {
        self.core_radius
    }

    /// Launch shell radius, m.
    pub fn shell_radius_m(&self) -> f64 {
        self.shell_radius
    }
}

/// Boris rotation of `v` about `B` with `t = (q/m)·B·τ` already scaled
/// by the caller's `τ` (`half` = ½·(q/m)·dt rotates by ≈ ω·dt).
#[inline]
fn boris_rotate(v: [f64; 3], b: [f64; 3], half_qm_dt: f64) -> [f64; 3] {
    if b == [0.0, 0.0, 0.0] {
        return v;
    }
    let t = scale(b, half_qm_dt);
    let t2 = dot(t, t);
    let vprime = add(v, cross(v, t));
    let s = 2.0 / (1.0 + t2);
    add(v, scale(cross(vprime, t), s))
}

#[inline]
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn sq_len(a: [f64; 3]) -> f64 {
    dot(a, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Device;
    use crate::field::FieldMap;
    use crate::poisson::{solve, SolveOptions};

    fn trace_fusor(n_particles: usize, max_passes: u32) -> Vec<TraceOutcome> {
        let device = Device::classic_fusor(120.0, 40.0, 5, 1.0, -5_000.0);
        let sol = solve(&device, 81, 161, &SolveOptions::default()).unwrap();
        let fields = FieldMap::new(&device, &sol);
        let opts = TraceOptions {
            max_passes,
            ..TraceOptions::default()
        };
        let tracer = Tracer::new(&device, &fields, &sol, opts);
        tracer.launch_ensemble(DEUTERON, n_particles)
    }

    #[test]
    fn ions_fall_inward_and_recirculate() {
        let outcomes = trace_fusor(12, 8);
        let total_passes: u32 = outcomes.iter().map(|o| o.core_passes).sum();
        assert!(
            total_passes >= 12,
            "ions did not recirculate: {total_passes} total passes"
        );
        // Nobody should reach the wall in a well-formed fusor well.
        let walls = outcomes.iter().filter(|o| o.fate == Fate::Wall).count();
        assert!(walls <= 2, "too many wall deaths: {walls}/12");
    }

    #[test]
    fn energy_is_approximately_conserved_far_from_wires() {
        let outcomes = trace_fusor(6, 6);
        for o in &outcomes {
            assert!(
                o.energy_drift_rel < 0.08,
                "energy drift {:.3} too large (fate {:?}, passes {})",
                o.energy_drift_rel,
                o.fate,
                o.core_passes
            );
        }
    }

    #[test]
    fn synchronized_integrator_beats_leapfrog_at_equal_step() {
        // Same geometry, same step: the synchronized scheme must cut the
        // worst-case energy drift by a large factor (19% → 0.17% on the
        // production open-fusor grid; see examples/integrator_drift.rs).
        let device = Device::classic_fusor(76.0, 19.0, 4, 1.0, -30_000.0);
        let sol = solve(&device, 77, 153, &SolveOptions::default()).unwrap();
        let fields = FieldMap::new(&device, &sol);
        let worst = |integrator: Integrator| {
            let opts = TraceOptions {
                max_passes: 10,
                integrator,
                ..TraceOptions::default()
            };
            Tracer::new(&device, &fields, &sol, opts)
                .launch_ensemble(DEUTERON, 12)
                .iter()
                .map(|o| o.energy_drift_rel)
                .fold(0.0, f64::max)
        };
        let (lf, sy) = (worst(Integrator::Leapfrog), worst(Integrator::Synchronized));
        assert!(sy < 0.2 * lf, "synchronized {sy:.4} vs leapfrog {lf:.4}");
        assert!(sy < 0.02, "synchronized drift {sy:.4}");
    }

    #[test]
    fn charge_exchange_splits_the_yield() {
        // At 2 mTorr D₂ the CX mean free path (~4 cm at 1e-19 m²) is
        // shorter than one pass: the surviving-ion channel must be heavily
        // suppressed relative to the raw no-CX integral, and the
        // fast-neutral channel must be alive.
        let device = Device::classic_fusor(120.0, 40.0, 5, 1.0, -30_000.0);
        let sol = solve(&device, 81, 161, &SolveOptions::default()).unwrap();
        let fields = FieldMap::new(&device, &sol);
        let n_bg = crate::xsection::d2_deuteron_density_m3(2.0, 300.0);
        let opts = TraceOptions {
            max_passes: 8,
            cx: Some(CxModel {
                sigma_cx_m2: 1.0e-19,
                background_deuteron_density_m3: n_bg,
            }),
            ..TraceOptions::default()
        };
        let tracer = Tracer::new(&device, &fields, &sol, opts);
        let outs = tracer.launch_ensemble(DEUTERON, 8);
        let mean =
            |f: &dyn Fn(&TraceOutcome) -> f64| outs.iter().map(f).sum::<f64>() / outs.len() as f64;
        let ion = mean(&|o| o.neutrons_ion_channel);
        let cxc = mean(&|o| o.neutrons_cx_channel);
        let raw = mean(&|o| o.ddn_sigma_v_m3) * n_bg;
        assert!(ion > 0.0, "ion channel dead");
        assert!(cxc > 0.0, "cx channel dead");
        assert!(
            ion < 0.8 * raw,
            "survival weighting must suppress the ion channel: ion {ion:.3e} vs raw {raw:.3e}"
        );
    }

    #[test]
    fn electron_physics_has_the_right_sign() {
        // In a negative-cathode well the electron is repelled outward:
        // it must reach the wall without ever entering the core.
        let device = Device::classic_fusor(120.0, 40.0, 5, 1.0, -5_000.0);
        let sol = solve(&device, 61, 121, &SolveOptions::default()).unwrap();
        let fields = FieldMap::new(&device, &sol);
        let tracer = Tracer::new(&device, &fields, &sol, TraceOptions::default());
        let out = tracer.trace_from(ELECTRON, [0.08, 0.0, 0.03], [0.0, 0.0, 0.0]);
        assert_eq!(out.fate, Fate::Wall, "electron must be expelled: {out:?}");
        assert_eq!(out.core_passes, 0);
        assert_eq!(out.ddn_sigma_v_m3, 0.0, "no D-D yield for electrons");
    }

    #[test]
    fn zero_depth_drive_is_exactly_the_static_field() {
        let device = Device::classic_fusor(120.0, 40.0, 5, 1.0, -5_000.0);
        let sol = solve(&device, 61, 121, &SolveOptions::default()).unwrap();
        let fields = FieldMap::new(&device, &sol);
        let base = TraceOptions {
            max_passes: 6,
            ..TraceOptions::default()
        };
        let driven = TraceOptions {
            drive: Some(Drive {
                modulation: 0.0,
                frequency_hz: 1.0e6,
                phase_rad: 0.3,
            }),
            ..base
        };
        let t0 = Tracer::new(&device, &fields, &sol, base);
        let t1 = Tracer::new(&device, &fields, &sol, driven);
        let a = t0.trace_from(DEUTERON, [0.08, 0.0, 0.03], [0.0, 0.0, 0.0]);
        let b = t1.trace_from(DEUTERON, [0.08, 0.0, 0.03], [0.0, 0.0, 0.0]);
        assert_eq!(a.fate, b.fate);
        assert_eq!(a.core_passes, b.core_passes);
        assert!((a.time_s - b.time_s).abs() < 1e-15);
    }

    #[test]
    fn resonant_drive_moves_the_physics() {
        // Two-ring device: the axial channel is wire-free, so a near-axis
        // ion is a clean radial oscillator to measure the bounce
        // frequency on (single fusor trajectories defocus into wires).
        let device = Device::shielded_two_ring(120.0, 40.0, 22.0, 4.0, -5_000.0, 0.0);
        let sol = solve(&device, 61, 121, &SolveOptions::default()).unwrap();
        let fields = FieldMap::new(&device, &sol);
        let base = TraceOptions {
            max_passes: 12,
            ..TraceOptions::default()
        };
        let t0 = Tracer::new(&device, &fields, &sol, base);
        let probe = t0.trace_from(DEUTERON, [0.008, 0.0, 0.09], [0.0, 0.0, 0.0]);
        assert!(probe.core_passes >= 2, "probe must oscillate: {probe:?}");
        // Bounce frequency from the probe: two core entries per period.
        let f_bounce = probe.core_passes as f64 / (2.0 * probe.time_s);
        let driven = TraceOptions {
            drive: Some(Drive {
                modulation: 0.25,
                frequency_hz: f_bounce,
                phase_rad: 0.0,
            }),
            ..base
        };
        let t1 = Tracer::new(&device, &fields, &sol, driven);
        let a = t0.launch_ensemble(DEUTERON, 8);
        let b = t1.launch_ensemble(DEUTERON, 8);
        let sig = |o: &[TraceOutcome]| o.iter().map(|x| x.ddn_sigma_v_m3).sum::<f64>();
        let (sa, sb) = (sig(&a), sig(&b));
        assert!(
            (sa - sb).abs() / sa.max(1e-300) > 0.01,
            "a 25% drive at the bounce frequency must move the yield: {sa:.3e} vs {sb:.3e}"
        );
    }

    #[test]
    fn fusion_yield_rewards_voltage() {
        // Same geometry, 6x the bias: the σ(E) integral must explode —
        // this is why fusors chase voltage.
        let yield_at = |volts: f64| {
            let device = Device::classic_fusor(120.0, 40.0, 5, 1.0, volts);
            let sol = solve(&device, 81, 161, &SolveOptions::default()).unwrap();
            let fields = FieldMap::new(&device, &sol);
            let opts = TraceOptions {
                max_passes: 8,
                ..TraceOptions::default()
            };
            let tracer = Tracer::new(&device, &fields, &sol, opts);
            let outcomes = tracer.launch_ensemble(DEUTERON, 8);
            outcomes.iter().map(|o| o.ddn_sigma_v_m3).sum::<f64>() / outcomes.len() as f64
        };
        let lo = yield_at(-5_000.0);
        let hi = yield_at(-30_000.0);
        assert!(hi > 0.0, "no yield at -30 kV");
        assert!(
            hi > 1.0e4 * lo.max(1e-60),
            "yield must rise steeply with voltage: lo {lo:.3e}, hi {hi:.3e}"
        );
    }

    #[test]
    fn wires_intercept_most_ions_in_a_classic_fusor() {
        let outcomes = trace_fusor(16, 30);
        let wires = outcomes
            .iter()
            .filter(|o| matches!(o.fate, Fate::Wire(_)))
            .count();
        assert!(
            wires * 2 >= outcomes.len(),
            "expected interception to dominate: {wires}/{}",
            outcomes.len()
        );
    }

    #[test]
    fn recorded_paths_match_unrecorded_outcomes() {
        let device = Device::classic_fusor(120.0, 40.0, 5, 1.0, -5_000.0);
        let sol = solve(&device, 81, 161, &SolveOptions::default()).unwrap();
        let fields = FieldMap::new(&device, &sol);
        let opts = TraceOptions {
            max_passes: 6,
            ..TraceOptions::default()
        };
        let tracer = Tracer::new(&device, &fields, &sol, opts);
        let plain = tracer.launch_ensemble(DEUTERON, 6);
        let paths = tracer.launch_ensemble_paths(DEUTERON, 6, 0.5, 64);
        assert_eq!(plain.len(), paths.len());
        for (k, (o, p)) in plain.iter().zip(&paths).enumerate() {
            assert_eq!(*o, p.outcome, "recording changed trace {k}");
            assert!(p.points_mm.len() >= 2 && p.points_mm.len() <= 64);
            let start = tracer.launch_position(k, 6);
            let first = p.points_mm[0];
            assert!((first[0] - start[0] * 1e3).abs() < 1e-9);
            assert!((first[2] - start[2] * 1e3).abs() < 1e-9);
            for q in &p.points_mm {
                // Launched at rest in the x–z plane with no B: stays there.
                assert!(q[1].abs() < 1e-9);
                assert!(q[0].hypot(q[1]) <= 120.0 + 1.0 && q[2].abs() <= 120.0 + 1.0);
            }
            // A recirculating ion crosses the axis: the path must too.
            if o.core_passes >= 2 {
                assert!(
                    p.points_mm.iter().any(|q| q[0] < 0.0),
                    "trace {k} never crossed x = 0"
                );
            }
        }
    }
}
