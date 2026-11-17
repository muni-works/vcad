//! FD vs hybrid-adjoint `optimize_electrodes` on the shielded two-ring
//! (the MCP tool's defaults: 81×161 grid, 48 particles, 20 passes,
//! 8 iterations, three starts). Same starts, bounds and grid; runs
//! sequentially; every reported yield is the tracer objective.
//!
//! Run: `cargo run --release -p vcad-kernel-particle --example adjoint_vs_fd`
//!
//! Env: `CASE=A|B` runs one case; `MODES=hybrid` skips FD;
//! `ADJ_CROSSINGS=<f64>` overrides the adjoint horizon.

use std::collections::BTreeMap;
use std::time::Instant;

use vcad_kernel_particle::electrode_opt::{
    optimize_electrodes, ElectrodeOptProblem, GradientMode, OptVariable,
};
use vcad_kernel_particle::optimize::GradSource;
use vcad_kernel_particle::spec::DeviceSpec;

const SPEC: &str = r#"{
  "chamber_radius_mm": 150, "chamber_half_height_mm": 150,
  "rings": [
    {"ring_radius_mm": 45, "z_mm": "ring_z", "wire_radius_mm": 3,
     "potential_v": "cathode_v", "ampere_turns": "shield_at"},
    {"ring_radius_mm": 45, "z_mm": "ring_z_neg", "wire_radius_mm": 3,
     "potential_v": "cathode_v", "ampere_turns": "shield_at_neg"}
  ]
}"#;

fn var(name: &str, lo: f64, hi: f64) -> OptVariable {
    OptVariable {
        name: name.into(),
        lo,
        hi,
        start: None,
    }
}

fn main() {
    let spec: DeviceSpec = serde_json::from_str(SPEC).unwrap();
    let base: BTreeMap<String, f64> = [
        ("cathode_v", -30_000.0),
        ("shield_at", 40_000.0),
        ("shield_at_neg", -40_000.0),
        ("ring_z", 25.0),
        ("ring_z_neg", -25.0),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), *v))
    .collect();

    let cases: Vec<(&str, Vec<OptVariable>)> = vec![
        (
            "A: bias + both shield currents (all adjoint-capable)",
            vec![
                var("cathode_v", -40_000.0, -10_000.0),
                var("shield_at", 0.0, 200_000.0),
                var("shield_at_neg", -200_000.0, 0.0),
            ],
        ),
        (
            "B: both shield currents + ring spacing (hybrid: 2 adjoint + 1 FD)",
            vec![
                var("shield_at", 0.0, 200_000.0),
                var("shield_at_neg", -200_000.0, 0.0),
                var("ring_z", 15.0, 40.0),
            ],
        ),
    ];

    let case = std::env::var("CASE").ok();
    let modes: Vec<GradientMode> = match std::env::var("MODES").ok().as_deref() {
        Some("hybrid") => vec![GradientMode::Hybrid],
        Some("fd") => vec![GradientMode::Fd],
        _ => vec![GradientMode::Fd, GradientMode::Hybrid],
    };
    let crossings: Option<f64> = std::env::var("ADJ_CROSSINGS")
        .ok()
        .and_then(|s| s.parse().ok());
    for (label, vars) in cases {
        if let Some(c) = &case {
            if !label.starts_with(c.as_str()) {
                continue;
            }
        }
        println!("\n## {label}");
        for &mode in &modes {
            let mut problem = ElectrodeOptProblem {
                gradient: mode,
                ..ElectrodeOptProblem::new(vars.clone())
            };
            if let Some(c) = crossings {
                problem.adjoint_crossings = c;
            }
            let t = Instant::now();
            let r = optimize_electrodes(&spec, &base, &problem).unwrap();
            let wall = t.elapsed().as_secs_f64();
            let (mut adj, mut fb, mut fdc) = (0, 0, 0);
            for s in &r.starts {
                for it in &s.iterations {
                    for src in &it.sources {
                        match src {
                            GradSource::Adjoint => adj += 1,
                            GradSource::FdFallback => fb += 1,
                            GradSource::FiniteDifference => fdc += 1,
                        }
                    }
                }
            }
            println!(
                "{mode:?}: best {:.4e} m^3 at {:?}\n  wall {wall:.1}s, tracer evals {}, adjoint runs {}, \
                 components adjoint/fallback/fd = {adj}/{fb}/{fdc}",
                r.best_sigma_v_m3, r.best_params, r.evals, r.adjoint_evals
            );
            for (k, s) in r.starts.iter().enumerate() {
                let iters: Vec<String> = s
                    .iterations
                    .iter()
                    .map(|it| {
                        let tag: String = it
                            .sources
                            .iter()
                            .map(|s| match s {
                                GradSource::Adjoint => 'A',
                                GradSource::FdFallback => 'f',
                                GradSource::FiniteDifference => 'F',
                            })
                            .collect();
                        format!(
                            "{}{}{:.3e}",
                            tag,
                            if it.accepted { "+" } else { "=" },
                            it.value_after
                        )
                    })
                    .collect();
                println!(
                    "  start {k}: {:.4e} (evals {}, adj {}) [{}]",
                    s.value,
                    s.evals,
                    s.adjoint_evals,
                    iters.join(" ")
                );
            }
        }
    }
}
