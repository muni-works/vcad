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
        if (s + 1) % stride == 0 {
            out.push(sample(t + dt, p));
        }
    }
    out
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
}
