//! Derive the plasmoid's magnet lock from the magnetized glow fluid and
//! compare it with the reduced model's fitted pull (gap #4 of
//! `docs/plasmoid-sim.md`).
//!
//! Prints the derived-vs-fitted tables and writes CSVs:
//!
//! - `<out>/lock_profile.csv` — per offset ρ at 1 Torr, 10 mm gap: β, Φ,
//!   fitted and derived pulls (both / energy-only / particle-only channels),
//!   the floating-conductor pull, derived and fitted swirl
//! - `<out>/pressure_sweep.csv`, `<out>/gap_sweep.csv`,
//!   `<out>/sigma_sweep.csv` — peak pulls vs pressure, magnet gap, foot width
//! - `<out>/lock_map.csv` — the tabulated map fed back into the reduced model
//!
//! ```bash
//! cargo run --release -p vcad-kernel-particle --example plasmoid_fluid -- out/
//! ```

use std::fmt::Write as _;
use vcad_kernel_particle::plasmoid::{
    simulate_scene_with, DiscMagnet, LockModel, MagnetPose, PlasmaParams,
};
use vcad_kernel_particle::plasmoid_fluid::{
    equivalent_sphere_radius, es_field_ratio, ConductorState, FluidParams, FootModel, LockMap,
    LockReport,
};

const GAP: f64 = 10e-3;
const MARGIN: f64 = 1.8;

fn pose_above(x: f64, y: f64, gap: f64, m: &DiscMagnet) -> MagnetPose {
    MagnetPose {
        center_m: [x, y, gap + 0.5 * m.thickness_m],
        axis: [0.0, 0.0, -1.0],
    }
}

/// Index and signed value of the largest-magnitude entry.
fn peak(v: &[f64]) -> (usize, f64) {
    v.iter().enumerate().fold(
        (0, 0.0),
        |b, (i, &x)| if x.abs() > b.1.abs() { (i, x) } else { b },
    )
}

fn write(out: &str, name: &str, body: &str) {
    std::fs::write(format!("{out}/{name}"), body).expect("write csv");
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "plasmoid-fluid-out".into());
    std::fs::create_dir_all(&out).expect("create output dir");
    let m = DiscMagnet::n52_10x5();

    // The reduced model exactly as the demo calibrates it (1 Torr, 10 mm gap).
    let mut reduced = PlasmaParams::argon_dish(1.0);
    let r0 = reduced.orbit_radius_m;
    reduced.mobility_m2_s = MARGIN * reduced.depinning_mobility(&m, &pose_above(0.0, r0, GAP, &m));
    println!(
        "reduced model: mu_fit = {:.3e} m^2/s, v_race = {} m/s, v_spin = {} m/s",
        reduced.mobility_m2_s, reduced.race_speed_m_s, reduced.spin_speed_m_s
    );

    let fp = FluidParams {
        cells: 96,
        ..FluidParams::argon(1.0)
    };
    println!(
        "fluid @1 Torr: nu = {:.3e} /s, mu_e = {:.2} m^2/Vs, mu_i = {:.3} m^2/Vs, beta_c = sqrt(mu_e/mu_i) = {:.1}",
        fp.nu(),
        fp.mu_e(),
        fp.mu_i(),
        fp.critical_beta()
    );

    // --- 1. Profile at the demo operating point --------------------------
    let rhos: Vec<f64> = (0..=24).map(|i| i as f64 * 1e-3).collect();
    let rep = LockReport::build(fp, &m, GAP, &rhos[1..], &reduced).expect("lock report");
    println!(
        "calibrated foot: H0 = {:.3e} V/s per electron, Te_peak = {:.2} V; max electron ExB fluid speed {:.0} m/s",
        rep.heating_per_electron, rep.te_peak_v, rep.max_exb_m_s
    );
    println!(
        "\n  rho  beta   Phi    fit_in  derived  E-only  P-only  ES-float  F-drive  C-drive | swirl_der swirl_fit | growth/s"
    );
    let mut csv = String::from(
        "rho_mm,beta,phi_fit,fitted_inward,derived_inward,energy_only,particle_only,es_floating,field_driven,current_driven,derived_swirl,fitted_swirl,growth\n",
    );
    for i in 0..rep.rhos_m.len() {
        let row = (
            rep.rhos_m[i] * 1e3,
            rep.beta[i],
            rep.phi_fit[i],
            rep.fitted_inward_m_s[i],
            rep.derived_inward_m_s[i],
            rep.energy_only_inward_m_s[i],
            rep.particle_only_inward_m_s[i],
            rep.es_floating_inward_m_s[i],
            rep.derived_swirl_m_s[i],
            rep.fitted_swirl_m_s[i],
        );
        let (fd, cd, gr) = (
            rep.field_driven_inward_m_s[i],
            rep.current_driven_inward_m_s[i],
            rep.growth_per_s[i],
        );
        println!(
            "{:5.1} {:5.2} {:6.3} {:7.3} {:8.3} {:7.3} {:7.4} {:8.3} {:8.3} {:8.3} | {:8.4} {:8.4} | {:7.0}",
            row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, fd, cd, row.8, row.9, gr
        );
        writeln!(
            csv,
            "{},{},{},{},{},{},{},{},{fd},{cd},{},{},{gr}",
            row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9
        )
        .unwrap();
    }
    write(&out, "lock_profile.csv", &csv);
    let (rd, vd) = rep.derived_peak();
    let (rf, vf) = rep.fitted_peak();
    println!(
        "\npeak pull: derived {vd:.3} m/s at {:.0} mm | fitted {vf:.3} m/s at {:.0} mm | race {} m/s",
        rd * 1e3,
        rf * 1e3,
        reduced.race_speed_m_s
    );
    println!(
        "shape correlation derived vs fitted = {:+.3}; derived/fitted amplitude {:.1} (amplitude NOT a derived foot speed)",
        rep.shape_correlation(),
        vd / vf
    );
    println!(
        "magnetic / floating-conductor pull = {:+.3}",
        rep.magnetic_to_floating_ratio()
    );
    let (_, vfd) = peak(&rep.field_driven_inward_m_s);
    let (_, vcd) = peak(&rep.current_driven_inward_m_s);
    println!("heating drive: uniform {vd:+.3}, field-driven {vfd:+.3}, current-driven {vcd:+.3} m/s (peak inward)");
    let (_, ve) = peak(&rep.energy_only_inward_m_s);
    let (_, vp) = peak(&rep.particle_only_inward_m_s);
    let (_, ves) = peak(&rep.es_floating_inward_m_s);
    {
        let (ip, _) = peak(&rep.derived_inward_m_s);
        let t = rep.te_profiles_v[ip];
        println!(
            "at the |pull| peak (rho = {:.0} mm): Te at -2s,-s,0,+s,+2s (magnet side first) = {:.4} {:.4} {:.4} {:.4} {:.4} V; growth there {:+.0} /s, under the magnet {:+.0} /s",
            rep.rhos_m[ip] * 1e3,
            t[0], t[1], t[2], t[3], t[4],
            rep.growth_per_s[ip],
            rep.growth_under_magnet_per_s
        );
    }
    println!("channels: energy-only peak {ve:.3}, particle-only peak {vp:.4}; floating conductor peak {ves:.3} m/s");
    println!(
        "RF heating multiplier under the magnet: floating {:.2}x, hand-grounded {:.1}x",
        rep.es_floating_heating_peak, rep.es_grounded_heating_peak
    );
    for c in rep.claims() {
        println!("claim {} = {:?}", c.id, c.measured);
    }

    // --- 2. Pressure sweep (gap 10 mm) -----------------------------------
    println!("\npressure  beta(0)  derived  F-drive  C-drive  fitted(mu@1Torr)  ES_float  mag/ES   (peak inward, m/s)");
    let mut csv = String::from("torr,beta0,derived_peak,field_driven_peak,current_driven_peak,fitted_peak,es_floating_peak,mag_to_es\n");
    let sweep_rhos: Vec<f64> = (1..=16).map(|i| i as f64 * 1e-3).collect();
    for torr in [0.1, 0.3, 1.0, 3.0, 10.0, 30.0] {
        let mut red = reduced;
        red.nu_per_s = PlasmaParams::argon_dish(torr).nu_per_s;
        let fpp = FluidParams {
            cells: 72,
            ..FluidParams::argon(torr)
        };
        match LockReport::build(fpp, &m, GAP, &sweep_rhos, &red) {
            Ok(r) => {
                let b0 = r.beta[0];
                let (_, d) = r.derived_peak();
                let (_, f) = r.fitted_peak();
                let e = peak(&r.es_floating_inward_m_s).1;
                let fd = peak(&r.field_driven_inward_m_s).1;
                let cd = peak(&r.current_driven_inward_m_s).1;
                let q = r.magnetic_to_floating_ratio();
                println!(
                    "{torr:8.2} {b0:7.2} {d:9.4} {fd:8.4} {cd:8.4} {f:12.4} {e:10.4} {q:+7.3}"
                );
                writeln!(csv, "{torr},{b0},{d},{fd},{cd},{f},{e},{q}").unwrap();
            }
            Err(e) => println!("{torr:8.2}  refused: {e}"),
        }
    }
    write(&out, "pressure_sweep.csv", &csv);

    // --- 3. Gap sweep (1 Torr) -------------------------------------------
    println!("\ngap_mm  derived_peak  fitted_peak  ES_float_peak  grounded_heating");
    let mut csv =
        String::from("gap_mm,derived_peak,fitted_peak,es_floating_peak,grounded_heating\n");
    let fpg = FluidParams {
        cells: 72,
        ..FluidParams::argon(1.0)
    };
    for gap in [5e-3, 7.5e-3, 10e-3, 15e-3, 20e-3, 30e-3] {
        let r = LockReport::build(fpg, &m, gap, &sweep_rhos, &reduced).expect("gap sweep");
        let (_, d) = r.derived_peak();
        let (_, f) = r.fitted_peak();
        let e = peak(&r.es_floating_inward_m_s).1;
        println!(
            "{:6.1} {d:13.4} {f:12.4} {e:14.4} {:16.1}",
            gap * 1e3,
            r.es_grounded_heating_peak
        );
        writeln!(
            csv,
            "{},{d},{f},{e},{}",
            gap * 1e3,
            r.es_grounded_heating_peak
        )
        .unwrap();
    }
    write(&out, "gap_sweep.csv", &csv);

    // --- 4. Foot width ---------------------------------------------------
    println!("\nsigma_mm  derived_peak  ratio_to_3mm");
    let mut csv = String::from("sigma_mm,derived_peak\n");
    let mut base = None;
    for sigma in [3e-3, 1.5e-3, 4.5e-3, 6e-3] {
        let fps = FluidParams {
            cells: 72,
            foot_sigma_m: sigma,
            heat_sigma_m: sigma,
            ..FluidParams::argon(1.0)
        };
        let r = LockReport::build(fps, &m, GAP, &sweep_rhos, &reduced).expect("sigma sweep");
        let (_, d) = r.derived_peak();
        if sigma == 3e-3 {
            base = Some(d);
        }
        println!(
            "{:8.1} {d:13.4} {:12.2}",
            sigma * 1e3,
            base.map(|b| d / b).unwrap_or(f64::NAN)
        );
        writeln!(csv, "{},{d}", sigma * 1e3).unwrap();
    }
    write(&out, "sigma_sweep.csv", &csv);

    // --- 4b. Heating-structure assumptions: does the sign survive? -------
    println!("\nheated core sigma_h/sigma, background f_bg -> pull at rho = 3, 6, 9 mm (m/s)");
    let mut csv = String::from("heat_ratio,f_bg,pull_3mm,pull_6mm,pull_9mm\n");
    for ratio in [0.5, 1.0, 1.5] {
        for fbg in [0.0, 0.25, 0.6] {
            let fph = FluidParams {
                cells: 72,
                heat_sigma_m: ratio * 3e-3,
                background_heating: fbg,
                ..FluidParams::argon(1.0)
            };
            match LockReport::build(fph, &m, GAP, &[3e-3, 6e-3, 9e-3], &reduced) {
                Ok(r) => {
                    let d = &r.derived_inward_m_s;
                    println!(
                        "  {ratio:4.1} {fbg:5.2}  {:8.3} {:8.3} {:8.3}",
                        d[0], d[1], d[2]
                    );
                    writeln!(csv, "{ratio},{fbg},{},{},{}", d[0], d[1], d[2]).unwrap();
                }
                Err(e) => println!("  {ratio:4.1} {fbg:5.2}  refused: {e}"),
            }
        }
    }
    write(&out, "heating_sweep.csv", &csv);

    // --- 5. Feed back: the reduced model on the derived map ---------------
    let model = FootModel::calibrate(FluidParams {
        cells: 72,
        ..FluidParams::argon(1.0)
    })
    .expect("calibrate");
    let gaps = [5e-3, 7.5e-3, 10e-3, 15e-3, 20e-3, 30e-3, 50e-3];
    let raw = LockMap::compute(&model, m, &gaps, &rhos).expect("lock map");
    // The fluid fixes shape, sign and swirl ratio; the amplitude is pinned to
    // the reduced model's own peak pull at the hold gap (μ_fit max|∇Φ|).
    let map = raw.with_peak_inward(vf.abs());
    println!(
        "\nlock map: raw peak |inward| {:.3} m/s rescaled to the fitted peak {:.3} m/s",
        raw.peak_inward_magnitude(),
        vf.abs()
    );
    let mut csv = String::from("gap_mm,rho_mm,inward_raw,swirl_raw,growth\n");
    for (gi, g) in gaps.iter().enumerate() {
        for (ri, r) in rhos.iter().enumerate() {
            let k = gi * rhos.len() + ri;
            writeln!(
                csv,
                "{},{},{},{},{}",
                g * 1e3,
                r * 1e3,
                raw.inward_m_s[k],
                raw.swirl_m_s[k],
                raw.growth_per_s[k]
            )
            .unwrap();
        }
    }
    write(&out, "lock_map.csv", &csv);
    // Hold the magnet over the orbit and let the racer come round.
    for (label, lock) in [
        ("fitted ", LockModel::Fitted),
        ("derived", LockModel::Derived(&map)),
    ] {
        let mut p = reduced;
        p.radial_rate_per_s = 3.0;
        let hold = pose_above(0.0, r0, GAP, &m);
        let tr = simulate_scene_with(
            &p,
            lock,
            |_| Some(hold.scene(&m)),
            [r0, 0.0],
            4.0,
            1e-4,
            500,
        );
        let e = tr.last().unwrap().pos_m;
        let d = (e[0] - hold.center_m[0]).hypot(e[1] - hold.center_m[1]);
        println!(
            "{label} lock model, magnet held at 10 mm over the orbit: foot {:.1} mm from the footprint after 4 s -> {}",
            d * 1e3,
            if d < 8e-3 { "LOCKED" } else { "racing" }
        );
    }
    let a = equivalent_sphere_radius(&m);
    println!(
        "\nES check: floating E/E0 directly under at the glow = {:.3}; grounded (D = 150 mm) = {:.2}",
        es_field_ratio(ConductorState::Floating, a, GAP + 2.5e-3, 2e-3, 0.0),
        es_field_ratio(
            ConductorState::Grounded {
                ground_distance_m: 0.15
            },
            a,
            GAP + 2.5e-3,
            2e-3,
            0.0
        )
    );
}
