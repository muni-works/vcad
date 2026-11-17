//! Explainer data: the same magnet move under the two lock models.
//! The magnet comes down over the orbit (250 mm → 10 mm, t = 1–3 s) and is
//! held. `fitted` is the reduced model's assumed pull; `derived` is the
//! glow-fluid lock map (rescaled to the fitted peak, as in plasmoid_fluid).
//! Writes `<out>/lock_compare.csv`: model,t,x,y,gap.
use std::fmt::Write as _;
use vcad_kernel_particle::plasmoid::{
    simulate_scene_with, DiscMagnet, LockModel, MagnetPose, PlasmaParams,
};
use vcad_kernel_particle::plasmoid_fluid::{FluidParams, FootModel, LockMap, LockReport};

const GAP: f64 = 10e-3;
const MARGIN: f64 = 1.8;
const ANGLE_DEG: f64 = 135.0;

fn pose_above(x: f64, y: f64, gap: f64, m: &DiscMagnet) -> MagnetPose {
    MagnetPose {
        center_m: [x, y, gap + 0.5 * m.thickness_m],
        axis: [0.0, 0.0, -1.0],
    }
}

fn smooth(u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

fn gap_at(t: f64) -> f64 {
    0.25 + (GAP - 0.25) * smooth((t - 1.0) / 2.0)
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "out".into());
    std::fs::create_dir_all(&out).expect("create output dir");
    let m = DiscMagnet::n52_10x5();
    let mut reduced = PlasmaParams::argon_dish(1.0);
    let r0 = reduced.orbit_radius_m;
    reduced.mobility_m2_s = MARGIN * reduced.depinning_mobility(&m, &pose_above(0.0, r0, GAP, &m));
    reduced.radial_rate_per_s = 3.0;

    let rhos: Vec<f64> = (0..=24).map(|i| i as f64 * 1e-3).collect();
    let fp = FluidParams {
        cells: 72,
        ..FluidParams::argon(1.0)
    };
    let rep = LockReport::build(fp, &m, GAP, &rhos[1..], &reduced).expect("lock report");
    let (_, vf) = rep.fitted_peak();
    let model = FootModel::calibrate(fp).expect("calibrate");
    let gaps = [5e-3, 7.5e-3, 10e-3, 15e-3, 20e-3, 30e-3, 50e-3];
    let raw = LockMap::compute(&model, m, &gaps, &rhos).expect("lock map");
    let map = raw.with_peak_inward(vf.abs());
    println!(
        "lock map: raw peak |inward| {:.3} m/s rescaled to the fitted peak {:.3} m/s",
        raw.peak_inward_magnitude(),
        vf.abs()
    );

    let (ca, sa) = (ANGLE_DEG.to_radians().cos(), ANGLE_DEG.to_radians().sin());
    let mut csv = String::from("model,t,x,y,gap\n");
    for (label, lock) in [
        ("fitted", LockModel::Fitted),
        ("derived", LockModel::Derived(&map)),
    ] {
        let tr = simulate_scene_with(
            &reduced,
            lock,
            |t| Some(pose_above(r0 * ca, r0 * sa, gap_at(t), &m).scene(&m)),
            [r0, 0.0],
            10.0,
            1.0 / 2400.0,
            20,
        );
        for s in &tr {
            writeln!(csv, "{label},{:.5},{:.6},{:.6},{:.5}", s.t_s, s.pos_m[0], s.pos_m[1], gap_at(s.t_s)).unwrap();
        }
        let e = tr.last().unwrap().pos_m;
        let d = (e[0] - r0 * ca).hypot(e[1] - r0 * sa);
        println!("{label}: foot {:.1} mm from the footprint after 10 s", d * 1e3);
    }
    println!("magnet at ({:.4}, {:.4}) m", r0 * ca, r0 * sa);
    std::fs::write(format!("{out}/lock_compare.csv"), csv).expect("write csv");
}
