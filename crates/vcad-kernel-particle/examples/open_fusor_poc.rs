//! Open-fusor video proof of concept (`docs/videos/open-fusor.md`).
//!
//! A plausible hobby fusor — 6" chamber (76 mm inner radius), 1.5" grid
//! (19 mm radius) built as a 4-ring latitude stack of 2 mm wire — priced
//! end to end with the code that exists today:
//!
//! 1. **particle** — voltage sweep −20…−40 kV at 10 mA / 5 mTorr D₂. Every
//!    point is reported as a bracket: `low` = CX on (single generation,
//!    σ_cx = 1e-19 m²: ion channel + fast-neutral channel), `high` = CX off
//!    (ions recirculate unmolested, beam-on-background). Both are floors
//!    on their own physics (no CX chain, no volume ionization, no
//!    beam–beam), so the band is *not* a confidence interval.
//! 2. **convergence** — the −30 kV point at two grid resolutions and two
//!    ensemble sizes; the spread is printed so nobody reads the third digit.
//! 3. **space charge** — the linearity gauge at the operating point.
//! 4. **neutronics** — dose vs distance, bare and behind 15 cm HDPE, at a
//!    deliberately conservative **upper-bound** source of 5×10⁶ n/s (the
//!    amateur record scale), never the predicted floor: a safety case
//!    must bound the machine over-performing, not under-performing.
//!
//! Output is CSV blocks on stdout (`# section` headers) for the plot
//! script `docs/videos/open-fusor/plot.py`.
//!
//! Run: `cargo run --release -p vcad-kernel-particle --example open_fusor_poc`

use std::collections::BTreeMap;
use std::time::Instant;

use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::field::FieldMap;
use vcad_kernel_particle::fom::{
    geometric_transparency, neutron_rate_from_counts, neutron_rate_per_s, stats, EnsembleStats,
};
use vcad_kernel_particle::poisson::{solve, Solution, SolveOptions};
use vcad_kernel_particle::space_charge;
use vcad_kernel_particle::trace::{CxModel, TraceOptions, Tracer, DEUTERON};
use vcad_kernel_particle::xsection::d2_deuteron_density_m3;

// ── The hobby fusor ────────────────────────────────────────────────────
const CHAMBER_R_MM: f64 = 76.0; // 6" chamber, inner radius
const CATHODE_R_MM: f64 = 19.0; // 1.5" grid
const N_RINGS: usize = 4;
const WIRE_A_MM: f64 = 1.0; // 2 mm wire
const ION_CURRENT_A: f64 = 0.010;
const PRESSURE_MTORR: f64 = 5.0;
const SIGMA_CX_M2: f64 = 1.0e-19;
const MAX_PASSES: u32 = 40;
// Grids: h = 0.25 / 0.5 / 1.0 mm (wire radius 1 mm). The sweep runs on the
// finest; the convergence block prints the spread across all three.
const FINE: (usize, usize) = (153, 305); // h = 0.5 mm
const COARSE: (usize, usize) = (77, 153);
const XFINE: (usize, usize) = (305, 609); // h = 0.25 mm
const UPPER_BOUND_N_PER_S: f64 = 5.0e6;

struct Point {
    stats_nocx: EnsembleStats,
    stats_cx: EnsembleStats,
    sol: Solution,
    dwell: Vec<f64>,
    n: usize,
}

fn run(volts: f64, grid: (usize, usize), n: usize) -> Point {
    let device = Device::classic_fusor(CHAMBER_R_MM, CATHODE_R_MM, N_RINGS, WIRE_A_MM, volts);
    let sol = solve(&device, grid.0, grid.1, &SolveOptions::default()).expect("poisson");
    let fields = FieldMap::new(&device, &sol);
    let base = TraceOptions {
        max_passes: MAX_PASSES,
        ..TraceOptions::default()
    };
    let (out, dwell) = Tracer::new(&device, &fields, &sol, base).launch_ensemble_dwell(DEUTERON, n);
    let stats_nocx = stats(&out);
    let cx = TraceOptions {
        cx: Some(CxModel {
            sigma_cx_m2: SIGMA_CX_M2,
            background_deuteron_density_m3: d2_deuteron_density_m3(PRESSURE_MTORR, 300.0),
        }),
        ..base
    };
    let stats_cx = stats(&Tracer::new(&device, &fields, &sol, cx).launch_ensemble(DEUTERON, n));
    Point {
        stats_nocx,
        stats_cx,
        sol,
        dwell,
        n,
    }
}

fn bracket(p: &Point) -> (f64, f64) {
    let n_d = d2_deuteron_density_m3(PRESSURE_MTORR, 300.0);
    let high = neutron_rate_per_s(p.stats_nocx.mean_ddn_sigma_v_m3, ION_CURRENT_A, n_d);
    let low = neutron_rate_from_counts(
        p.stats_cx.mean_neutrons_ion_channel + p.stats_cx.mean_neutrons_cx_channel,
        ION_CURRENT_A,
    );
    (low, high)
}

fn main() {
    let t0 = Instant::now();
    let dev = Device::classic_fusor(CHAMBER_R_MM, CATHODE_R_MM, N_RINGS, WIRE_A_MM, -30_000.0);
    println!(
        "# config,chamber_r_mm={CHAMBER_R_MM},cathode_r_mm={CATHODE_R_MM},rings={N_RINGS},\
         wire_a_mm={WIRE_A_MM},ion_current_a={ION_CURRENT_A},pressure_mtorr={PRESSURE_MTORR},\
         sigma_cx_m2={SIGMA_CX_M2},max_passes={MAX_PASSES},geometric_transparency={:.3}",
        geometric_transparency(&dev)
    );

    // ── 1. voltage sweep on the extra-fine grid (h = 0.25 mm) ──────────────────────────────
    println!("# sweep");
    println!(
        "kv,mean_passes,intercept_frac,eff_transparency,max_drift,rate_low_cx_on,rate_high_cx_off"
    );
    let mut op30: Option<Point> = None;
    for kv in [20.0_f64, 25.0, 30.0, 35.0, 40.0] {
        let p = run(-kv * 1e3, XFINE, 96);
        let (lo, hi) = bracket(&p);
        println!(
            "{kv},{:.2},{:.3},{:.3},{:.3},{lo:.3e},{hi:.3e}",
            p.stats_nocx.mean_passes,
            p.stats_nocx.interception_fraction,
            p.stats_nocx.effective_transparency,
            p.stats_nocx.max_energy_drift_rel
        );
        if kv == 30.0 {
            op30 = Some(p);
        }
        eprintln!("# {kv} kV done at {:.0} s", t0.elapsed().as_secs_f64());
    }
    let op30 = op30.expect("30 kV point");

    // ── 2. convergence spread at −30 kV ────────────────────────────────
    println!("# convergence");
    println!("grid,particles,rate_low_cx_on,rate_high_cx_off,intercept_frac,mean_passes,mean_drift,max_drift");
    let (lo, hi) = bracket(&op30);
    println!(
        "{}x{},{},{lo:.3e},{hi:.3e},{:.3},{:.2},{:.4},{:.3}",
        XFINE.0,
        XFINE.1,
        op30.n,
        op30.stats_nocx.interception_fraction,
        op30.stats_nocx.mean_passes,
        op30.stats_nocx.mean_energy_drift_rel,
        op30.stats_nocx.max_energy_drift_rel
    );
    for (grid, n) in [(XFINE, 48usize), (FINE, 96), (COARSE, 96)] {
        let p = run(-30_000.0, grid, n);
        let (lo, hi) = bracket(&p);
        println!(
            "{}x{},{n},{lo:.3e},{hi:.3e},{:.3},{:.2},{:.4},{:.3}",
            grid.0,
            grid.1,
            p.stats_nocx.interception_fraction,
            p.stats_nocx.mean_passes,
            p.stats_nocx.mean_energy_drift_rel,
            p.stats_nocx.max_energy_drift_rel
        );
    }

    // ── 3. space-charge gauge at the operating point ───────────────────
    println!("# space_charge");
    match space_charge::estimate(
        &op30.sol,
        &op30.dwell,
        op30.n,
        ION_CURRENT_A,
        &SolveOptions::default(),
    ) {
        Ok(sc) => println!(
            "ratio={:.3},phi_beam_peak_v={:.1},well_depth_v={:.0},current_at_10pct_a={:.2e}",
            sc.ratio,
            sc.phi_beam_peak_v,
            sc.well_depth_v,
            sc.current_at_ratio_a(0.10)
        ),
        Err(e) => println!("error={e:?}"),
    }

    // ── 4. dose vs distance at the upper-bound source ──────────────────
    use vcad_kernel_neutronics::spec::{
        DetectorSpec, LayerSpec, ParamValue, RunSpec, ShieldSpec, SourceSpec,
    };
    println!("# dose");
    println!("shield_hdpe_mm,source_n_per_s,detector_m,dose_usv_h,rse");
    let detectors_m = [0.5_f64, 1.0, 2.0, 3.0];
    for hdpe in [0.0_f64, 150.0] {
        // chamber stand-in: 300 mm air, then the HDPE castle, then room air.
        let mut layers = vec![LayerSpec {
            material: "air".into(),
            thickness_mm: ParamValue::Literal(300.0),
        }];
        if hdpe > 0.0 {
            layers.push(LayerSpec {
                material: "hdpe".into(),
                thickness_mm: ParamValue::Literal(hdpe),
            });
        }
        layers.push(LayerSpec {
            material: "air".into(),
            thickness_mm: ParamValue::Literal(3_300.0 - 300.0 - hdpe),
        });
        let spec = ShieldSpec {
            layers,
            source: SourceSpec {
                rate_n_per_s: ParamValue::Literal(UPPER_BOUND_N_PER_S),
                energy_ev: ParamValue::Literal(2.45e6),
            },
            detectors: detectors_m
                .iter()
                .map(|m| DetectorSpec {
                    label: format!("{m}m"),
                    radius_mm: ParamValue::Literal(m * 1_000.0),
                    half_width_mm: ParamValue::Literal(20.0),
                })
                .collect(),
            run: RunSpec::default(),
        };
        let set = vcad_kernel_neutronics::receipt::predicted_claims(&spec, &BTreeMap::new())
            .expect("neutronics");
        for m in detectors_m {
            let label = format!("{m}m");
            if let Some(c) = set
                .claims
                .iter()
                .find(|c| c.name.starts_with("dose_rate") && c.name.ends_with(&label))
            {
                println!(
                    "{hdpe},{UPPER_BOUND_N_PER_S:.1e},{m},{:.4},{:.4}",
                    c.value, c.rse
                );
            }
        }
    }
    // Uncollided fluence a bare detector would see per 1e5 n/s (analytic).
    println!("# uncollided_flux_per_1e5_n_s");
    println!("detector_m,flux_n_cm2_s");
    for m in detectors_m {
        let r_cm = m * 100.0;
        println!(
            "{m},{:.4}",
            1.0e5 / (4.0 * std::f64::consts::PI * r_cm * r_cm)
        );
    }
    eprintln!("# total {:.0} s", t0.elapsed().as_secs_f64());
}
