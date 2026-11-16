//! Open-fusor two-sided yield prediction (`docs/videos/open-fusor.md`,
//! blocker #2; `docs/particle-optics-m0.md` M7).
//!
//! The hobby fusor of the open-fusor plan — 6" chamber (76 mm inner
//! radius), 1.5" grid (19 mm) as a 4-ring latitude stack of 2 mm wire, D₂
//! at 5 mTorr — priced at what an operator reads: **supply current**
//! (10 mA), voltage, pressure. Every point is ONE predicted rate with a
//! two-sided band (numerical + input in log-quadrature, model form as a
//! linear envelope), so a neutron count can violate it.
//!
//! Output: CSV blocks on stdout (`# section` headers).
//!
//! ```text
//! cargo run --release -p vcad-kernel-particle --example open_fusor_band            # sweep
//! cargo run --release -p vcad-kernel-particle --example open_fusor_band -- diag    # −30 kV generation table
//! cargo run --release -p vcad-kernel-particle --example open_fusor_band -- quick   # coarse sweep
//! ```

use std::time::Instant;

use vcad_kernel_particle::chain::{
    self, BandKind, ChainOptions, InitialSource, ModelForm, OperatorReading, PredictNumerics,
};
use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::poisson::{solve, SolveOptions};
use vcad_kernel_particle::receipt;
use vcad_kernel_particle::xsection::GAMMA_SE_15KEV_CENTRAL;

const CHAMBER_R_MM: f64 = 76.0;
const CATHODE_R_MM: f64 = 19.0;
const N_RINGS: usize = 4;
const WIRE_A_MM: f64 = 1.0;
const SUPPLY_A: f64 = 0.010;
const PRESSURE_MTORR: f64 = 5.0;

fn device(volts: f64) -> Device {
    Device::classic_fusor(CHAMBER_R_MM, CATHODE_R_MM, N_RINGS, WIRE_A_MM, volts)
}

fn diag(grid: (usize, usize), particles: usize) {
    let dev = device(-30_000.0);
    let sol = solve(&dev, grid.0, grid.1, &SolveOptions::default()).expect("poisson");
    for shape in [InitialSource::UniformVolume, InitialSource::EdgeShell] {
        let t0 = Instant::now();
        let opts = ChainOptions {
            particles,
            initial_source: shape,
            ..ChainOptions::default()
        };
        let r = chain::run(&dev, &sol, &opts).expect("chain");
        println!(
            "# diag grid={}x{} particles={particles} start={shape:?}",
            grid.0, grid.1
        );
        println!(
            "gen,waves,truncated,cathode_ions,wall_ions,censored,cold,cx_relaunch,impact_births,\
             electron_births,breakups,sustainment_M,mean_cathode_keV,n_ion,n_fast,n_wall_per_loading,\
             neutrons_per_supply_charge,max_drift,mean_drift"
        );
        for (g, t) in r.generations.iter().enumerate() {
            println!(
                "{g},{:.1},{:.1e},{:.3},{:.3},{:.1e},{:.3},{:.3},{:.4},{:.4},{:.3},{:.4},{:.2},{:.3e},{:.3e},{:.3e},{:.3e},{:.3},{:.4}",
                t.waves,
                t.truncated,
                t.cathode_ions,
                t.wall_ions,
                t.censored,
                t.cold_fraction,
                t.births_cx,
                t.births_impact,
                t.births_electron,
                t.breakups,
                t.sustainment,
                t.mean_cathode_energy_ev * 1e-3,
                t.neutrons_ion,
                t.neutrons_fast_neutral,
                t.neutrons_wall_per_loading,
                t.neutrons_per_supply_charge(GAMMA_SE_15KEV_CENTRAL, 0.0),
                t.max_energy_drift_rel,
                t.mean_energy_drift_rel,
            );
        }
        println!(
            "# converged={} spread={:.3} rate(10 mA, gamma {GAMMA_SE_15KEV_CENTRAL}, no wall)={:.3e} n/s, cathode ion current {:.2} mA ({:.0} s)",
            r.converged,
            r.mode_spread_rel,
            r.rate_n_per_s(SUPPLY_A, GAMMA_SE_15KEV_CENTRAL, 0.0),
            1e3 * r.cathode_ion_current_a(SUPPLY_A, GAMMA_SE_15KEV_CENTRAL),
            t0.elapsed().as_secs_f64()
        );
    }
}

fn sweep(numerics: &PredictNumerics, kvs: &[f64]) {
    let t0 = Instant::now();
    println!(
        "# config,chamber_r_mm={CHAMBER_R_MM},cathode_r_mm={CATHODE_R_MM},rings={N_RINGS},wire_a_mm={WIRE_A_MM},\
         supply_a={SUPPLY_A},pressure_mtorr={PRESSURE_MTORR},grid={}x{},particles={}",
        numerics.grid.0, numerics.grid.1, numerics.chain.particles
    );
    let mut rows = Vec::new();
    let mut comps_30 = None;
    for &kv in kvs {
        let reading = OperatorReading::hobby(SUPPLY_A, -kv * 1e3, PRESSURE_MTORR);
        let p = chain::predict(
            &device(-kv * 1e3),
            &reading,
            &ModelForm::default(),
            numerics,
        )
        .expect("predict");
        eprintln!("# {kv} kV done at {:.0} s", t0.elapsed().as_secs_f64());
        if (kv - 30.0).abs() < 1e-9 {
            comps_30 = Some(p.clone());
        }
        rows.push((kv, p));
    }
    println!("# sweep");
    println!(
        "kv,lower_n_s,central_n_s,upper_n_s,vacuum_n_s,cathode_ion_ma,sustainment_M,space_charge_ratio,\
         frac_ion,frac_fast_neutral,frac_wall,generations,converged,max_drift,sampling_noise_log"
    );
    for (kv, p) in &rows {
        println!(
            "{kv},{:.3e},{:.3e},{:.3e},{:.3e},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{:.3},{:.3}",
            p.lower_n_per_s,
            p.central_n_per_s,
            p.upper_n_per_s,
            p.vacuum_n_per_s,
            1e3 * p.cathode_ion_current_a,
            p.mode.sustainment,
            p.space_charge_ratio,
            p.channel_fractions[0],
            p.channel_fractions[1],
            p.channel_fractions[2],
            p.generations,
            p.converged,
            p.mode.max_energy_drift_rel,
            p.sampling_noise_log,
        );
    }
    if let Some(p) = comps_30.or_else(|| rows.first().map(|r| r.1.clone())) {
        println!(
            "# components_at_{:.0}kv",
            -p.reading.cathode_voltage_v * 1e-3
        );
        println!("name,kind,factor_low_setting,factor_high_setting,rerun,note");
        for c in &p.components {
            let kind = match c.kind {
                BandKind::Numerical => "numerical",
                BandKind::Input => "input",
                BandKind::ModelForm => "model_form",
            };
            println!(
                "{},{kind},{:.3},{:.3},{},\"{}\"",
                c.name, c.factor_low_setting, c.factor_high_setting, c.rerun, c.note
            );
        }
        let set = receipt::yield_band_claims(&p);
        println!("# claim_json");
        println!("{}", serde_json::to_string(&set.claims[0]).expect("json"));
    }
    eprintln!("# total {:.0} s", t0.elapsed().as_secs_f64());
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "diag" => diag((305, 609), 96),
        "diag-coarse" => diag((153, 305), 96),
        "quick" => sweep(
            &PredictNumerics {
                grid: (153, 305),
                coarse_grid: Some((77, 153)),
                chain: ChainOptions {
                    particles: 64,
                    ..ChainOptions::default()
                },
                step_check: false,
                ..PredictNumerics::default()
            },
            &[20.0, 30.0, 40.0],
        ),
        _ => sweep(&PredictNumerics::default(), &[20.0, 25.0, 30.0, 35.0, 40.0]),
    }
}
