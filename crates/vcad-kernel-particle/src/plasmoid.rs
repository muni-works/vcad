//! Magnet-steered plasmoid on an RF-driven dish electrode (reduced model).
//!
//! Reproduces the "racing plasmoid locks to a hand-held neodymium magnet"
//! demo: a luminous filament circulates around a dish-shaped HV electrode;
//! bring a small magnet close and the filament's foot pins under it and
//! follows it around; move the magnet too fast and it breaks away and
//! resumes racing. See `docs/plasmoid-sim.md` for the full triage.
//!
//! Two layers, deliberately separated by how much of each is physics:
//!
//! **Real physics.**
//! - [`DiscMagnet`]: an axially magnetized disc is *exactly* a solenoidal
//!   bound-current sheet `K = M × n̂ = (B_r/μ0) φ̂`, so its field is the sum
//!   of coaxial current loops — evaluated with the crate's exact
//!   elliptic-integral loop field ([`crate::field::b_ring`]). It lives in
//!   [`crate::field3d`] (the general posed-source module, re-exported here)
//!   and is posed in 3D via [`MagnetPose`], a thin wrapper over
//!   [`crate::field3d::Pose`].
//! - [`hall_parameter`]: electron magnetization `β = ω_ce / ν_m` from the
//!   local |B| and a gas collision frequency ([`collision_frequency`]).
//!   Classical cross-field transport scales as `1/(1+β²)`, so a region with
//!   β ≳ 1 loses electrons more slowly and ionizes more for the same RF
//!   power — the discharge prefers it. At 1 Torr argon a 10 × 5 mm N52
//!   disc 10 mm above the dish (8 mm above the glow) gives β ≈ 2.9; at
//!   atmosphere β ≈ 0.004 — the
//!   model predicts the effect needs a low-pressure glow.
//!
//! **Phenomenological.** The filament foot is an overdamped quasi-particle
//! on the dish plane:
//!
//! ```text
//! dx/dt = v_race φ̂ + k (r0 − r) r̂            (racing ionization wave)
//!       + μ ∇Φ,   Φ = ln(1 + β²)              (pull toward lower loss)
//!       + v_spin · β²/(1+β²) · unit(E_dc × B)  (sheath-field drift sense)
//! ```
//!
//! `v_race`, `k`, `μ` and `v_spin` are fit parameters, not derived. What the
//! model gets without being told: whether it locks (a depinning threshold,
//! `μ·max|∇Φ| > v_race`, [`PlasmaParams::depinning_mobility`]), where the
//! foot sits relative to the magnet, and the magnet speed at which it
//! breaks free. Note the drive is RF: `J×B` on the filament current
//! averages to zero over a cycle, which is why the lock is modelled through
//! magnetized transport rather than as a Lorentz drag on the channel. The
//! E×B term uses the *time-averaged* (rectified) sheath field, which does
//! not average out.

use crate::constants::{ELECTRON_MASS, ELEMENTARY_CHARGE};
pub use crate::field3d::{b_dipole, DiscMagnet};
use crate::field3d::{norm, Pose};

/// Boltzmann constant, J/K.
const K_B: f64 = 1.380_649e-23;

/// Where a magnet is and which way its north pole points, SI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MagnetPose {
    /// Magnet center, m (world frame, dish surface at z = 0).
    pub center_m: [f64; 3],
    /// Magnetization direction (need not be normalized).
    pub axis: [f64; 3],
}

impl MagnetPose {
    /// The equivalent general [`Pose`] (same local frame construction).
    pub fn pose(&self) -> Pose {
        Pose::from_axis(self.center_m, self.axis)
    }

    /// World-frame field of `magnet` in this pose at world point `p`.
    pub fn b_world(&self, magnet: &DiscMagnet, p: [f64; 3]) -> [f64; 3] {
        let pose = self.pose();
        pose.vec_to_world(magnet.b_local(pose.to_local(p)))
    }
}

/// Electron–neutral momentum-transfer collision frequency, s⁻¹:
/// `ν = n σ v̄`, with `n = p/(k T_gas)` and mean electron speed
/// `v̄ = sqrt(8 e T_e / (π m_e))`.
pub fn collision_frequency(pressure_pa: f64, gas_temp_k: f64, sigma_m2: f64, te_ev: f64) -> f64 {
    let n = pressure_pa / (K_B * gas_temp_k);
    let v = (8.0 * ELEMENTARY_CHARGE * te_ev / (std::f64::consts::PI * ELECTRON_MASS)).sqrt();
    n * sigma_m2 * v
}

/// Electron Hall parameter `β = ω_ce / ν_m = e|B| / (m_e ν_m)`.
pub fn hall_parameter(b_t: f64, nu_per_s: f64) -> f64 {
    ELEMENTARY_CHARGE * b_t.abs() / (ELECTRON_MASS * nu_per_s)
}

/// Parameters of the reduced plasmoid model (SI).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlasmaParams {
    /// Dish radius, m — the foot is confined to `r ≤ dish_radius_m`.
    pub dish_radius_m: f64,
    /// Preferred orbit radius of the racing filament, m.
    pub orbit_radius_m: f64,
    /// Racing (azimuthal ionization-wave) speed, m/s. Phenomenological.
    pub race_speed_m_s: f64,
    /// Radial relaxation rate toward the orbit radius, 1/s.
    pub radial_rate_per_s: f64,
    /// Mobility up the magnetized-transport potential Φ, m²/s.
    pub mobility_m2_s: f64,
    /// Peak sheath-field drift speed at full magnetization, m/s.
    pub spin_speed_m_s: f64,
    /// Height above the dish where the glow (and the sampled B) sits, m.
    pub glow_height_m: f64,
    /// Electron collision frequency, s⁻¹.
    pub nu_per_s: f64,
}

impl PlasmaParams {
    /// Argon glow at `pressure_torr` on an 80 mm-radius dish, with the
    /// racing/mobility parameters at their demo defaults.
    pub fn argon_dish(pressure_torr: f64) -> Self {
        Self {
            dish_radius_m: 0.08,
            orbit_radius_m: 0.055,
            race_speed_m_s: 0.6,
            radial_rate_per_s: 3.0,
            mobility_m2_s: 0.0,
            spin_speed_m_s: 0.05,
            glow_height_m: 2e-3,
            // σ_m(Ar, ~3 eV) ≈ 1e-19 m², T_gas 300 K.
            nu_per_s: collision_frequency(pressure_torr * 133.322, 300.0, 1e-19, 3.0),
        }
    }

    /// Transport potential Φ = ln(1 + β²) at dish point `(x, y)`.
    pub fn potential(&self, magnet: &DiscMagnet, pose: &MagnetPose, x: f64, y: f64) -> f64 {
        let b = pose.b_world(magnet, [x, y, self.glow_height_m]);
        let beta = hall_parameter(norm(b), self.nu_per_s);
        (1.0 + beta * beta).ln()
    }

    /// ∇Φ in the dish plane by central differences (1/m).
    pub fn grad_potential(
        &self,
        magnet: &DiscMagnet,
        pose: &MagnetPose,
        x: f64,
        y: f64,
    ) -> [f64; 2] {
        let h = 2e-4;
        let px = self.potential(magnet, pose, x + h, y) - self.potential(magnet, pose, x - h, y);
        let py = self.potential(magnet, pose, x, y + h) - self.potential(magnet, pose, x, y - h);
        [px / (2.0 * h), py / (2.0 * h)]
    }

    /// Largest |∇Φ| on the dish for this pose (grid search, 1 mm cells
    /// within `search_m` of the magnet's footprint).
    pub fn max_grad(&self, magnet: &DiscMagnet, pose: &MagnetPose, search_m: f64) -> f64 {
        let (cx, cy) = (pose.center_m[0], pose.center_m[1]);
        let n = (search_m / 1e-3).ceil() as i64;
        let mut best = 0.0f64;
        for i in -n..=n {
            for j in -n..=n {
                let (x, y) = (cx + i as f64 * 1e-3, cy + j as f64 * 1e-3);
                let g = self.grad_potential(magnet, pose, x, y);
                best = best.max((g[0] * g[0] + g[1] * g[1]).sqrt());
            }
        }
        best
    }

    /// Mobility at which the pin exactly balances the racing drive,
    /// `μ* = v_race / max|∇Φ|` — the analytic depinning threshold (with
    /// the radial and spin terms off). Above it a fixed point exists and
    /// the foot locks; below it the foot slips past.
    pub fn depinning_mobility(&self, magnet: &DiscMagnet, pose: &MagnetPose) -> f64 {
        self.race_speed_m_s / self.max_grad(magnet, pose, 0.03)
    }

    /// Foot velocity at `pos` (m/s); `pose = None` means no magnet.
    pub fn velocity(
        &self,
        magnet: &DiscMagnet,
        pose: Option<&MagnetPose>,
        pos: [f64; 2],
    ) -> [f64; 2] {
        let (x, y) = (pos[0], pos[1]);
        let r = (x * x + y * y).sqrt().max(1e-9);
        let (er, ephi) = ([x / r, y / r], [-y / r, x / r]);
        let vr = self.radial_rate_per_s * (self.orbit_radius_m - r);
        let mut v = [
            self.race_speed_m_s * ephi[0] + vr * er[0],
            self.race_speed_m_s * ephi[1] + vr * er[1],
        ];
        if let Some(pose) = pose {
            let g = self.grad_potential(magnet, pose, x, y);
            v[0] += self.mobility_m2_s * g[0];
            v[1] += self.mobility_m2_s * g[1];
            if self.spin_speed_m_s != 0.0 {
                let b = pose.b_world(magnet, [x, y, self.glow_height_m]);
                let beta = hall_parameter(norm(b), self.nu_per_s);
                // E_dc points from the glow into the dish (−z); E×B ∝ −ẑ×B.
                let d = [b[1], -b[0]];
                let dn = (d[0] * d[0] + d[1] * d[1]).sqrt();
                if dn > 1e-12 {
                    let s = self.spin_speed_m_s * beta * beta / (1.0 + beta * beta) / dn;
                    v[0] += s * d[0];
                    v[1] += s * d[1];
                }
            }
        }
        v
    }
}

/// One sample of a plasmoid trajectory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlasmoidSample {
    /// Time, s.
    pub t_s: f64,
    /// Foot position on the dish, m.
    pub pos_m: [f64; 2],
    /// Magnet pose at this time (if any).
    pub magnet: Option<MagnetPose>,
    /// Hall parameter at the foot.
    pub beta: f64,
}

/// Integrate the foot from `start` over `[0, t_end]` with RK4 at step
/// `dt`, the magnet pose given as a function of time. Returns a sample
/// every `stride` steps (plus the first).
pub fn simulate<F>(
    params: &PlasmaParams,
    magnet: &DiscMagnet,
    pose_at: F,
    start: [f64; 2],
    t_end: f64,
    dt: f64,
    stride: usize,
) -> Vec<PlasmoidSample>
where
    F: Fn(f64) -> Option<MagnetPose>,
{
    let steps = (t_end / dt).round() as usize;
    let stride = stride.max(1);
    let mut p = start;
    let mut out = Vec::with_capacity(steps / stride + 1);
    let sample = |t: f64, p: [f64; 2]| {
        let pose = pose_at(t);
        let beta = pose
            .map(|q| {
                let b = q.b_world(magnet, [p[0], p[1], params.glow_height_m]);
                hall_parameter(norm(b), params.nu_per_s)
            })
            .unwrap_or(0.0);
        PlasmoidSample {
            t_s: t,
            pos_m: p,
            magnet: pose,
            beta,
        }
    };
    out.push(sample(0.0, p));
    for s in 0..steps {
        let t = s as f64 * dt;
        p = rk4_step(params, magnet, &pose_at, t, p, dt);
        if (s + 1) % stride == 0 {
            out.push(sample(t + dt, p));
        }
    }
    out
}

/// One RK4 step of the foot from `p` at time `t` (pose sampled at `t`,
/// `t + dt/2` and `t + dt`), then the dish clamp. Shared by [`simulate`]
/// and [`PlasmoidStepper`] so the two cannot drift.
fn rk4_step<F>(
    params: &PlasmaParams,
    magnet: &DiscMagnet,
    pose_at: &F,
    t: f64,
    mut p: [f64; 2],
    dt: f64,
) -> [f64; 2]
where
    F: Fn(f64) -> Option<MagnetPose>,
{
    let f = |tt: f64, q: [f64; 2]| params.velocity(magnet, pose_at(tt).as_ref(), q);
    let k1 = f(t, p);
    let k2 = f(
        t + 0.5 * dt,
        [p[0] + 0.5 * dt * k1[0], p[1] + 0.5 * dt * k1[1]],
    );
    let k3 = f(
        t + 0.5 * dt,
        [p[0] + 0.5 * dt * k2[0], p[1] + 0.5 * dt * k2[1]],
    );
    let k4 = f(t + dt, [p[0] + dt * k3[0], p[1] + dt * k3[1]]);
    p[0] += dt / 6.0 * (k1[0] + 2.0 * k2[0] + 2.0 * k3[0] + k4[0]);
    p[1] += dt / 6.0 * (k1[1] + 2.0 * k2[1] + 2.0 * k3[1] + k4[1]);
    // The foot cannot leave the dish.
    let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
    if r > params.dish_radius_m {
        let s = params.dish_radius_m / r;
        p = [p[0] * s, p[1] * s];
    }
    p
}

/// Horizontal foot–footprint distance under which the foot counts as
/// locked to the magnet, m (the demo example's criterion).
pub const LOCK_RADIUS_M: f64 = 8e-3;
/// Magnet height above the dish surface beyond which it is "lifted away"
/// and nothing counts as locked, m.
pub const LOCK_MAX_HEIGHT_M: f64 = 0.05;

/// Hall parameter below which the glow under the foot is not magnetized
/// and nothing counts as locked (a racer merely passing under a magnet at
/// 1 atm is not "locked").
pub const LOCK_MIN_BETA: f64 = 1.0;

/// Whether a foot at `pos` (Hall parameter `beta` there) is locked to a
/// magnet at `pose`: within [`LOCK_RADIUS_M`] of its footprint, the magnet
/// held low, and the glow magnetized (β ≥ [`LOCK_MIN_BETA`]).
pub fn is_locked(pos: [f64; 2], beta: f64, pose: Option<&MagnetPose>) -> bool {
    let Some(q) = pose else { return false };
    let c = q.center_m;
    let d = ((pos[0] - c[0]).powi(2) + (pos[1] - c[1]).powi(2)).sqrt();
    d < LOCK_RADIUS_M && c[2] < LOCK_MAX_HEIGHT_M && beta >= LOCK_MIN_BETA
}

/// Snapshot of a live [`PlasmoidStepper`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlasmoidState {
    /// Simulated time since creation / reset, s.
    pub t_s: f64,
    /// Foot position on the dish, m.
    pub pos_m: [f64; 2],
    /// Hall parameter at the foot (0 with no magnet).
    pub beta: f64,
    /// Foot locked to the magnet ([`is_locked`]).
    pub locked: bool,
}

/// Incremental (live) integration of the same model as [`simulate`], for a
/// magnet whose pose is only known as it happens — e.g. dragged by hand in
/// the app. Each [`advance`](Self::advance) call takes the magnet pose at
/// the start and end of a frame and integrates fixed RK4 substeps with the
/// pose linearly interpolated between them, so a fast flick still reaches
/// the integrator as a smooth path rather than one teleport per frame.
#[derive(Debug, Clone)]
pub struct PlasmoidStepper {
    /// Model parameters (pressure can be changed with
    /// [`set_pressure_torr`](Self::set_pressure_torr)).
    pub params: PlasmaParams,
    /// The magnet.
    pub magnet: DiscMagnet,
    start: [f64; 2],
    pos: [f64; 2],
    t: f64,
    last_pose: Option<MagnetPose>,
}

impl PlasmoidStepper {
    /// A stepper with the foot at `start` (m) and no magnet seen yet.
    pub fn new(params: PlasmaParams, magnet: DiscMagnet, start: [f64; 2]) -> Self {
        Self {
            params,
            magnet,
            start,
            pos: start,
            t: 0.0,
            last_pose: None,
        }
    }

    /// The demo configuration: argon at `pressure_torr` on a dish of
    /// `dish_radius_m`, the racer on an orbit at `orbit_frac` of the dish
    /// radius, and the mobility calibrated *once* at 1 Torr to `margin × μ*`
    /// for the magnet held `gap_m` above the orbit. Pressure changes later
    /// move only ν (hence β), never μ — so raising the pressure makes the
    /// lock vanish instead of being renormalized away.
    pub fn demo(
        magnet: DiscMagnet,
        dish_radius_m: f64,
        orbit_frac: f64,
        gap_m: f64,
        margin: f64,
        pressure_torr: f64,
    ) -> Self {
        let mut p = PlasmaParams::argon_dish(1.0);
        p.dish_radius_m = dish_radius_m;
        p.orbit_radius_m = orbit_frac * dish_radius_m;
        let hold = MagnetPose {
            center_m: [0.0, p.orbit_radius_m, gap_m + 0.5 * magnet.thickness_m],
            axis: [0.0, 0.0, -1.0],
        };
        p.mobility_m2_s = margin * p.depinning_mobility(&magnet, &hold);
        let mut s = Self::new(p, magnet, [p.orbit_radius_m, 0.0]);
        s.set_pressure_torr(pressure_torr);
        s
    }

    /// Change the gas pressure (argon, 300 K): updates ν only.
    pub fn set_pressure_torr(&mut self, torr: f64) {
        self.params.nu_per_s = collision_frequency(torr.max(1e-6) * 133.322, 300.0, 1e-19, 3.0);
    }

    /// Back to the start position at t = 0.
    pub fn reset(&mut self) {
        self.pos = self.start;
        self.t = 0.0;
        self.last_pose = None;
    }

    /// Advance by `substeps` RK4 steps of `dt` each, the magnet pose moving
    /// linearly from `from` to `to` over the interval. `to = None` means no
    /// magnet for the whole interval; `from = None` reuses the pose the
    /// previous call ended on (or `to`, on the first call), which is what
    /// a frame loop wants.
    pub fn advance(
        &mut self,
        from: Option<MagnetPose>,
        to: Option<MagnetPose>,
        dt: f64,
        substeps: usize,
    ) -> PlasmoidState {
        let from = from.or(self.last_pose).or(to);
        let t0 = self.t;
        let span = dt * substeps as f64;
        let pose_at = |t: f64| -> Option<MagnetPose> {
            let (a, b) = (from?, to?);
            let u = if span > 0.0 {
                ((t - t0) / span).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let l = |x: f64, y: f64| x + (y - x) * u;
            Some(MagnetPose {
                center_m: [
                    l(a.center_m[0], b.center_m[0]),
                    l(a.center_m[1], b.center_m[1]),
                    l(a.center_m[2], b.center_m[2]),
                ],
                axis: [
                    l(a.axis[0], b.axis[0]),
                    l(a.axis[1], b.axis[1]),
                    l(a.axis[2], b.axis[2]),
                ],
            })
        };
        for k in 0..substeps {
            let t = t0 + k as f64 * dt;
            self.pos = rk4_step(&self.params, &self.magnet, &pose_at, t, self.pos, dt);
        }
        self.t = t0 + span;
        self.last_pose = to;
        self.state(to.as_ref())
    }

    /// The current state with the magnet at `pose`.
    pub fn state(&self, pose: Option<&MagnetPose>) -> PlasmoidState {
        let beta = pose
            .map(|q| {
                let b = q.b_world(
                    &self.magnet,
                    [self.pos[0], self.pos[1], self.params.glow_height_m],
                );
                hall_parameter(norm(b), self.params.nu_per_s)
            })
            .unwrap_or(0.0);
        PlasmoidState {
            t_s: self.t,
            pos_m: self.pos,
            beta,
            locked: is_locked(self.pos, beta, pose),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field3d::sub;

    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs().max(1e-30)
    }

    /// Magnet held `gap` above the dish, north pole down.
    fn pose_above(x: f64, y: f64, gap: f64, m: &DiscMagnet) -> MagnetPose {
        MagnetPose {
            center_m: [x, y, gap + 0.5 * m.thickness_m],
            axis: [0.0, 0.0, -1.0],
        }
    }

    #[test]
    fn on_axis_matches_closed_form() {
        let m = DiscMagnet::n52_10x5();
        for z in [3.0e-3, 5e-3, 10e-3, 20e-3, 50e-3] {
            let b = m.b_local([0.0, 0.0, z])[2];
            let exact = m.b_axis_exact(z);
            assert!(rel(b, exact) < 2e-3, "z={z}: {b} vs {exact}");
        }
    }

    #[test]
    fn far_field_matches_point_dipole() {
        let m = DiscMagnet::n52_10x5();
        let mm = m.moment();
        for d in [[0.1, 0.0, 0.0], [0.06, 0.03, 0.08], [0.0, -0.05, -0.12]] {
            let b = m.b_local(d);
            let bd = b_dipole(mm, [0.0, 0.0, 1.0], d);
            let err = norm(sub(b, bd)) / norm(bd);
            assert!(err < 1e-2, "d={d:?}: rel err {err}");
        }
    }

    #[test]
    fn pose_rotates_the_field() {
        let m = DiscMagnet::n52_10x5();
        // Axis along +x: on-axis field at +x is along +x with the local
        // on-axis magnitude.
        let pose = MagnetPose {
            center_m: [0.01, 0.02, 0.03],
            axis: [1.0, 0.0, 0.0],
        };
        let b = pose.b_world(&m, [0.01 + 0.02, 0.02, 0.03]);
        assert!(rel(b[0], m.b_axis_exact(0.02)) < 2e-3);
        assert!(b[1].abs() < 1e-9 && b[2].abs() < 1e-9);
    }

    #[test]
    fn magnetized_only_at_low_pressure() {
        let m = DiscMagnet::n52_10x5();
        let b = m.b_axis_exact(10e-3 + 2.5e-3);
        let low = PlasmaParams::argon_dish(1.0);
        let atm = PlasmaParams::argon_dish(760.0);
        let (bl, ba) = (
            hall_parameter(b, low.nu_per_s),
            hall_parameter(b, atm.nu_per_s),
        );
        assert!(bl > 1.0, "1 Torr β = {bl}");
        assert!(ba < 0.05, "1 atm β = {ba}");
    }

    #[test]
    fn circulates_at_constant_rate_without_magnet() {
        let p = PlasmaParams::argon_dish(1.0);
        let m = DiscMagnet::n52_10x5();
        let tr = simulate(&p, &m, |_| None, [p.orbit_radius_m, 0.0], 3.0, 1e-3, 100);
        let omega = p.race_speed_m_s / p.orbit_radius_m;
        let mut prev = 0.0f64;
        let mut unwrapped = 0.0;
        for s in &tr {
            let a = s.pos_m[1].atan2(s.pos_m[0]);
            let mut d = a - prev;
            d -= (d / std::f64::consts::TAU).round() * std::f64::consts::TAU;
            unwrapped += d;
            prev = a;
            let r = (s.pos_m[0].powi(2) + s.pos_m[1].powi(2)).sqrt();
            assert!(rel(r, p.orbit_radius_m) < 1e-6);
        }
        assert!(rel(unwrapped, omega * 3.0) < 1e-3, "{unwrapped}");
    }

    /// Run a fixed magnet over the orbit with mobility `scale × μ*`
    /// (radial + spin terms off so the threshold is the clean analytic
    /// one); return the foot's distance to the footprint after 4 s.
    fn final_distance(scale: f64) -> f64 {
        let m = DiscMagnet::n52_10x5();
        let mut p = PlasmaParams::argon_dish(1.0);
        p.radial_rate_per_s = 0.0;
        p.spin_speed_m_s = 0.0;
        let pose = pose_above(0.0, p.orbit_radius_m, 10e-3, &m);
        p.mobility_m2_s = scale * p.depinning_mobility(&m, &pose);
        let tr = simulate(
            &p,
            &m,
            |_| Some(pose),
            [p.orbit_radius_m, 0.0],
            4.0,
            5e-4,
            100,
        );
        let e = tr.last().unwrap().pos_m;
        ((e[0] - pose.center_m[0]).powi(2) + (e[1] - pose.center_m[1]).powi(2)).sqrt()
    }

    #[test]
    fn locks_only_above_depinning_threshold() {
        // Measured transition sits between 0.95 μ* and 1.0 μ*.
        // Above threshold: captured within a few mm of the footprint.
        assert!(final_distance(1.3) < 8e-3, "{}", final_distance(1.3));
        // Below: the drive beats the pin and the foot slips through.
        assert!(final_distance(0.7) > 20e-3, "{}", final_distance(0.7));
    }

    /// Magnet sweeps along the orbit circle at angular speed `w`; returns
    /// the fraction of t ∈ [2, 4] s the foot spends under it.
    fn follows_sweep(w: f64) -> f64 {
        let m = DiscMagnet::n52_10x5();
        let mut p = PlasmaParams::argon_dish(1.0);
        p.spin_speed_m_s = 0.0;
        let r0 = p.orbit_radius_m;
        let a0 = std::f64::consts::FRAC_PI_2;
        let pose0 = pose_above(0.0, r0, 10e-3, &m);
        p.mobility_m2_s = 2.0 * p.depinning_mobility(&m, &pose0);
        // Magnet sweeps clockwise — against the race.
        let pose_at = |t: f64| {
            let a = a0 - w * (t - 1.0).max(0.0);
            Some(pose_above(r0 * a.cos(), r0 * a.sin(), 10e-3, &m))
        };
        let tr = simulate(&p, &m, pose_at, [r0, 0.0], 4.0, 5e-4, 100);
        let late: Vec<_> = tr.iter().filter(|s| s.t_s >= 2.0).collect();
        let under = late
            .iter()
            .filter(|s| {
                let c = s.magnet.unwrap().center_m;
                ((s.pos_m[0] - c[0]).powi(2) + (s.pos_m[1] - c[1]).powi(2)).sqrt() < 8e-3
            })
            .count();
        under as f64 / late.len() as f64
    }

    #[test]
    fn follows_slow_magnet_and_breaks_away_from_fast() {
        // With μ = 2μ*, the pin's spare speed against the race is ≈ v_race
        // = 0.6 m/s → release near ω = 0.6 / 0.055 ≈ 11 rad/s (measured
        // transition: between 10 and 15 rad/s).
        let slow = follows_sweep(5.0);
        let fast = follows_sweep(20.0);
        assert!(slow > 0.99, "slow sweep: under the magnet {slow}");
        assert!(fast < 0.2, "fast flick: under the magnet {fast}");
    }

    /// The live stepper, fed frame-by-frame poses from a piecewise-linear
    /// path, reproduces `simulate` on the same path.
    #[test]
    fn stepper_matches_simulate_on_scripted_path() {
        let m = DiscMagnet::n52_10x5();
        let mut p = PlasmaParams::argon_dish(1.0);
        let r0 = p.orbit_radius_m;
        p.mobility_m2_s = 1.8 * p.depinning_mobility(&m, &pose_above(0.0, r0, 10e-3, &m));
        // Knots every frame (1/60 s): lower, hold, drag against the race, flick.
        let frame = 1.0 / 60.0;
        let n_frames = 240usize; // 4 s
        let knot = |i: usize| {
            let t = i as f64 * frame;
            let a = std::f64::consts::FRAC_PI_2 - 3.0 * (t - 1.0).max(0.0)
                - 15.0 * (t - 3.2).max(0.0);
            let gap = if t < 0.5 { 0.2 } else { 10e-3 };
            pose_above(r0 * a.cos(), r0 * a.sin(), gap, &m)
        };
        let lerp_pose = |t: f64| {
            let i = ((t / frame).floor() as usize).min(n_frames - 1);
            let u = (t - i as f64 * frame) / frame;
            let (a, b) = (knot(i), knot(i + 1));
            let l = |x: f64, y: f64| x + (y - x) * u;
            Some(MagnetPose {
                center_m: [
                    l(a.center_m[0], b.center_m[0]),
                    l(a.center_m[1], b.center_m[1]),
                    l(a.center_m[2], b.center_m[2]),
                ],
                axis: a.axis,
            })
        };
        let sub = 40usize;
        let dt = frame / sub as f64;
        let reference = simulate(&p, &m, lerp_pose, [r0, 0.0], n_frames as f64 * frame, dt, sub);
        let mut st = PlasmoidStepper::new(p, m, [r0, 0.0]);
        let mut locked_frames = 0;
        for i in 0..n_frames {
            let s = st.advance(Some(knot(i)), Some(knot(i + 1)), dt, sub);
            let r = reference[i + 1];
            let err = ((s.pos_m[0] - r.pos_m[0]).powi(2) + (s.pos_m[1] - r.pos_m[1]).powi(2)).sqrt();
            assert!(err < 1e-7, "frame {i}: {err} m off simulate");
            assert!(rel(s.beta + 1e-12, r.beta + 1e-12) < 1e-4, "frame {i}: β");
            locked_frames += s.locked as usize;
        }
        // It did lock somewhere along the way (the path is a real demo).
        assert!(locked_frames > 30, "locked {locked_frames} frames");
    }

    /// Drag slowly → stays locked; flick → breaks away; hold still → the
    /// racer comes round and is recaptured. And the same drag at 1 atm
    /// never locks (β ≈ 0.004, the mobility calibrated at 1 Torr).
    #[test]
    fn stepper_lock_drag_flick_recapture_and_pressure() {
        let run = |torr: f64| {
            let m = DiscMagnet::n52_10x5();
            let mut st = PlasmoidStepper::demo(m, 0.08, 0.6875, 10e-3, 1.8, torr);
            let r0 = st.params.orbit_radius_m;
            let frame = 1.0 / 60.0;
            let mut ang = std::f64::consts::FRAC_PI_2;
            let mut log = Vec::new();
            for i in 0..(60 * 10) {
                let t = i as f64 * frame;
                // 0-3 s hold; 3-6 s slow drag (2 rad/s, against the race);
                // 6-6.15 s flick (25 rad/s); then hold.
                let w = if (3.0..6.0).contains(&t) {
                    2.0
                } else if (6.0..6.15).contains(&t) {
                    25.0
                } else {
                    0.0
                };
                ang -= w * frame;
                let pose = pose_above(r0 * ang.cos(), r0 * ang.sin(), 10e-3, &m);
                let s = st.advance(None, Some(pose), frame / 40.0, 40);
                log.push((t, s.locked, s.beta));
            }
            log
        };
        let low = run(1.0);
        let frac = |a: f64, b: f64, l: &[(f64, bool, f64)]| {
            let w: Vec<_> = l.iter().filter(|s| s.0 >= a && s.0 < b).collect();
            w.iter().filter(|s| s.1).count() as f64 / w.len() as f64
        };
        assert!(frac(2.0, 3.0, &low) > 0.95, "held: {}", frac(2.0, 3.0, &low));
        assert!(frac(3.5, 6.0, &low) > 0.95, "slow drag: {}", frac(3.5, 6.0, &low));
        // The flick leaves the foot behind at once…
        assert!(frac(6.02, 6.17, &low) == 0.0, "after flick: {}", frac(6.02, 6.17, &low));
        // …and the racer, meeting the (now still) magnet head-on, is
        // recaptured on its next pass (≈ 0.2 s later) and stays.
        assert!(frac(6.5, 10.0, &low) > 0.99, "recaptured: {}", frac(6.5, 10.0, &low));
        let atm = run(760.0);
        assert!(atm.iter().all(|s| !s.1), "1 atm locked");
        assert!(atm.iter().all(|s| s.2 < 0.01));
    }
}
