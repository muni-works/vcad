//! Racing plasmoid vs. a hand-held magnet — the model's numbers at the
//! hold pose, and the magnetization map.
//!
//! Prints the field, Hall parameter and depinning threshold with the magnet
//! held over the racing orbit, and writes
//!
//! - `<out>/beta_map.csv` — `x,y,beta_1torr,beta_1atm` over the dish with
//!   the magnet at its hold pose (for the doc's magnetization figure)
//!
//! ```bash
//! cargo run --release -p vcad-kernel-particle --example plasmoid_magnet -- out/
//! ```
//!
//! The 20 s clip's choreography is not here any more: it is the timeline of
//! `docs/plasmoid-sim/plasmoid-clip.loon`, and `vcad-render --sim plasmoid`
//! steps this model from it (writing `trajectory.csv` next to the frames).
//! See docs/plasmoid-sim.md § Reproduce.

use std::fmt::Write as _;
use vcad_kernel_particle::plasmoid::{hall_parameter, DiscMagnet, MagnetPose, PlasmaParams};

/// Air gap between the magnet's lower face and the dish while held, m.
const GAP: f64 = 10e-3;
/// Mobility margin over the analytic depinning threshold.
const MARGIN: f64 = 1.8;
/// Hold pose over the orbit: radius (m) and azimuth (°) — the magnet's
/// default pose in plasmoid-clip.loon.
const HOLD_R: f64 = 0.055;
const HOLD_DEG: f64 = 120.0;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "plasmoid-out".into());
    std::fs::create_dir_all(&out).expect("create output dir");

    let m = DiscMagnet::n52_10x5();
    let mut p = PlasmaParams::argon_dish(1.0);
    let ang = HOLD_DEG.to_radians();
    let hold = MagnetPose {
        center_m: [
            HOLD_R * ang.cos(),
            HOLD_R * ang.sin(),
            GAP + 0.5 * m.thickness_m,
        ],
        axis: [0.0, 0.0, -1.0], // north pole down
    };
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
    println!("wrote {out}/beta_map.csv");
}

fn norm(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
