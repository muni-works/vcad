//! Racing plasmoid vs. a hand-held magnet — the demo trajectory.
//!
//! Runs the reduced model in [`vcad_kernel_particle::plasmoid`] through a
//! 20 s choreography (race → magnet lowers → lock → drag → flick → re-lock
//! → lift away) and writes:
//!
//! - `<out>/trajectory.csv` — `t,x,y,mx,my,mz,beta,locked` at 120 Hz (m)
//! - `<out>/beta_map.csv`   — `x,y,beta_1torr,beta_1atm` over the dish with
//!   the magnet at its hold pose (for the doc's magnetization figure)
//!
//! ```bash
//! cargo run --release -p vcad-kernel-particle --example plasmoid_magnet -- out/
//! ```
//!
//! Render with `docs/plasmoid-sim/render.py`.

use std::fmt::Write as _;
use vcad_kernel_particle::plasmoid::{
    hall_parameter, simulate, DiscMagnet, MagnetPose, PlasmaParams,
};

/// Air gap between the magnet's lower face and the dish while held, m.
const GAP: f64 = 10e-3;
/// Mobility margin over the analytic depinning threshold.
const MARGIN: f64 = 1.8;

/// Choreography keyframes: (t, r, angle°, gap). Gap 0.25 m = out of frame.
const KEYS: &[(f64, f64, f64, f64)] = &[
    (0.0, 0.055, 120.0, 0.25),
    (4.0, 0.055, 120.0, 0.25),
    (6.0, 0.055, 120.0, GAP), // lowered: lock as the racer comes round
    (8.0, 0.055, 120.0, GAP),
    (11.0, 0.055, 20.0, GAP),  // drag clockwise, against the race
    (13.0, 0.030, -40.0, GAP), // and inward
    (14.0, 0.050, -70.0, GAP),
    (14.12, 0.050, -150.0, GAP), // flick: ~1.1 m/s, beats the pin
    (16.0, 0.050, -150.0, GAP),
    (16.5, 0.050, -150.0, GAP),
    (18.0, 0.050, -150.0, GAP),  // re-captured on the next pass
    (19.0, 0.050, -150.0, 0.25), // lifted away: releases, resumes racing
    (21.0, 0.050, -150.0, 0.25),
];

fn smooth(u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

fn pose_at(m: &DiscMagnet, t: f64) -> MagnetPose {
    let i = KEYS
        .windows(2)
        .position(|w| t >= w[0].0 && t < w[1].0)
        .unwrap_or(KEYS.len() - 2);
    let (a, b) = (KEYS[i], KEYS[i + 1]);
    let u = smooth((t - a.0) / (b.0 - a.0));
    let lerp = |p: f64, q: f64| p + (q - p) * u;
    let (r, ang, gap) = (lerp(a.1, b.1), lerp(a.2, b.2).to_radians(), lerp(a.3, b.3));
    MagnetPose {
        center_m: [r * ang.cos(), r * ang.sin(), gap + 0.5 * m.thickness_m],
        axis: [0.0, 0.0, -1.0], // north pole down
    }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "plasmoid-out".into());
    std::fs::create_dir_all(&out).expect("create output dir");

    let m = DiscMagnet::n52_10x5();
    let mut p = PlasmaParams::argon_dish(1.0);
    let hold = pose_at(&m, 7.0);
    let mu_star = p.depinning_mobility(&m, &hold);
    p.mobility_m2_s = MARGIN * mu_star;

    let b_foot = norm(hold.b_world(&m, [hold.center_m[0], hold.center_m[1], p.glow_height_m]));
    let atm = PlasmaParams::argon_dish(760.0);
    println!(
        "magnet: N52 {:.0}x{:.0} mm, m = {:.3} A·m², gap {:.0} mm",
        2e3 * m.radius_m,
        1e3 * m.thickness_m,
        m.moment(),
        1e3 * GAP
    );
    println!("|B| at glow under magnet: {:.1} mT", 1e3 * b_foot);
    println!(
        "ν_m (Ar, 1 Torr): {:.2e} /s → β = {:.2}",
        p.nu_per_s,
        hall_parameter(b_foot, p.nu_per_s)
    );
    println!(
        "ν_m (Ar, 760 Torr): {:.2e} /s → β = {:.4}",
        atm.nu_per_s,
        hall_parameter(b_foot, atm.nu_per_s)
    );
    println!(
        "depinning mobility μ* = {mu_star:.3e} m²/s; using {MARGIN}× = {:.3e}",
        p.mobility_m2_s
    );
    println!(
        "max pin speed μ·max|∇Φ| = {:.2} m/s vs race {:.2} m/s",
        p.mobility_m2_s * p.max_grad(&m, &hold, 0.03),
        p.race_speed_m_s
    );

    let dt = 1.0 / 2400.0;
    let tr = simulate(
        &p,
        &m,
        |t| Some(pose_at(&m, t)),
        [p.orbit_radius_m, 0.0],
        20.0,
        dt,
        20,
    );

    let mut csv = String::from("t,x,y,mx,my,mz,beta,locked\n");
    for s in &tr {
        let c = s.magnet.unwrap().center_m;
        let d = ((s.pos_m[0] - c[0]).powi(2) + (s.pos_m[1] - c[1]).powi(2)).sqrt();
        let locked = d < 8e-3 && c[2] < 0.05;
        writeln!(
            csv,
            "{:.5},{:.6},{:.6},{:.6},{:.6},{:.6},{:.4},{}",
            s.t_s, s.pos_m[0], s.pos_m[1], c[0], c[1], c[2], s.beta, locked as u8
        )
        .unwrap();
    }
    std::fs::write(format!("{out}/trajectory.csv"), csv).expect("write trajectory");

    // Phase report: when the foot is locked.
    let mut prev = false;
    for s in &tr {
        let c = s.magnet.unwrap().center_m;
        let d = ((s.pos_m[0] - c[0]).powi(2) + (s.pos_m[1] - c[1]).powi(2)).sqrt();
        let locked = d < 8e-3 && c[2] < 0.05;
        if locked != prev {
            println!(
                "t = {:6.2} s: {}",
                s.t_s,
                if locked { "LOCK" } else { "release" }
            );
            prev = locked;
        }
    }

    let mut map = String::from("x,y,beta_1torr,beta_1atm\n");
    let n = 80;
    for i in 0..=n {
        for j in 0..=n {
            let x = -p.dish_radius_m + 2.0 * p.dish_radius_m * i as f64 / n as f64;
            let y = -p.dish_radius_m + 2.0 * p.dish_radius_m * j as f64 / n as f64;
            let b = norm(hold.b_world(&m, [x, y, p.glow_height_m]));
            writeln!(
                map,
                "{x:.5},{y:.5},{:.5},{:.6}",
                hall_parameter(b, p.nu_per_s),
                hall_parameter(b, atm.nu_per_s)
            )
            .unwrap();
        }
    }
    std::fs::write(format!("{out}/beta_map.csv"), map).expect("write beta map");
    println!(
        "wrote {out}/trajectory.csv ({} samples), {out}/beta_map.csv",
        tr.len()
    );
}

fn norm(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
