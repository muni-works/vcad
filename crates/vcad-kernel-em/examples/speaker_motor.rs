//! A 3" loudspeaker motor (ferrite ring, T-yoke, top plate, 0.5 mm gap):
//! gap flux density and Bl, hand-built — the reference the document
//! extractor (`vcad_eval::sim_extract::em`) is checked against.
//!
//! ```text
//! cargo run --release -p vcad-kernel-em --example speaker_motor [dx_mm] [linear]
//! ```
//!
//! Geometry (mm, Z-up, axis = Z): back plate r 0–30 z 0–4; pole r 0–9.75
//! z 4–16; Y30 ferrite ring r 12–30 z 4–12 (B_r 0.39 T, μ_r 1.05, north
//! up); top plate r 10.25–30 z 12–16; voice coil r 9.85–10.15 z 10–18,
//! 60 turns. Steel is 1010 (μ_ri 1000, J_s 2.0 T) unless `linear`.

use std::time::Instant;

use vcad_kernel_em::axisym::{
    Annulus, AxisymMagnet, AxisymMagnetostatics, Coil, Material, PicardOptions,
};
use vcad_kernel_em::grid::SolveOptions;

fn ann(r0: f64, r1: f64, z0: f64, z1: f64) -> Annulus {
    Annulus {
        r_inner_mm: r0,
        r_outer_mm: r1,
        z_min_mm: z0,
        z_max_mm: z1,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dx: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.125);
    let linear = args.iter().any(|a| a == "linear");
    let (r_max, z_lo, z_hi) = (60.0, -30.0, 46.0);
    let mut dev = AxisymMagnetostatics::new(r_max, z_lo, z_hi);
    let steel = |a: Annulus| {
        if linear {
            Material::linear(a, 1000.0)
        } else {
            Material::saturable(a, 1000.0, 2.0)
        }
    };
    dev.materials.push(steel(ann(0.0, 30.0, 0.0, 4.0)));
    dev.materials.push(steel(ann(0.0, 9.75, 4.0, 16.0)));
    dev.materials.push(steel(ann(10.25, 30.0, 12.0, 16.0)));
    dev.magnets.push(AxisymMagnet {
        region: ann(12.0, 30.0, 4.0, 12.0),
        br_z_t: 0.39,
        mu_r: 1.05,
    });
    dev.coils.push(Coil {
        region: ann(9.85, 10.15, 10.0, 18.0),
        turns: 60.0,
        current_a: 0.0,
    });
    let nr = (r_max / dx).round() as usize + 1;
    let nz = ((z_hi - z_lo) / dx).round() as usize + 1;
    let t = Instant::now();
    let sopts = SolveOptions::default();
    let sol = if linear {
        dev.solve(nr, nz, &sopts).expect("solve")
    } else {
        let (sol, rep) = dev
            .solve_nonlinear(nr, nz, &sopts, &PicardOptions::default())
            .expect("nonlinear solve");
        println!("picard iterations {}", rep.iterations);
        sol
    };
    println!(
        "grid {nr}x{nz} (dx {dx} mm) sweeps {} in {:.1}s",
        sol.sweeps,
        t.elapsed().as_secs_f64()
    );
    let gap = ann(9.75, 10.25, 12.0, 16.0);
    let (br_mean, b_mean) = sol.mean_field_in(&gap, 16);
    let (br_mid, bz_mid) = sol.b_at(10.0e-3, 14.0e-3);
    println!("gap mid-plate B = ({br_mid:.4}, {bz_mid:.4}) T; mean |B_r| over gap {br_mean:.4} T, |B| {b_mean:.4} T");
    let bl = sol.force_factor(0);
    let bl2 = sol.force_factor_sampled(0, 32);
    println!(
        "Bl = {bl:.4} N/A (field route {bl2:.4}, residual {:.2e})",
        (bl - bl2).abs() / bl.abs()
    );
    // Magnetic-circuit estimate, no leakage, ideal steel.
    let mu0 = 4e-7 * std::f64::consts::PI;
    let mmf = 0.39 / (mu0 * 1.05) * 8e-3;
    let a_g = 2.0 * std::f64::consts::PI * 10e-3 * 4e-3;
    let r_g = 0.5e-3 / (mu0 * a_g);
    let a_m = std::f64::consts::PI * (30e-3f64.powi(2) - 12e-3f64.powi(2));
    let r_m = 8e-3 / (mu0 * 1.05 * a_m);
    let b_circ = mmf / (r_g + r_m) / a_g;
    println!(
        "circuit (no leakage) B_gap {b_circ:.3} T → implied leakage factor {:.2}",
        b_circ / br_mean
    );
}
