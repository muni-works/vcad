//! Probe: does the adjoint's surrogate J agree in sign with the tracer
//! objective (what `optimize_electrodes` scores) on the shielded two-ring?
//!
//! Run: `cargo run --release -p vcad-kernel-particle --example adjoint_probe`

use vcad_kernel_particle::adjoint::{yield_gradient, AdjointConfig};
use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::field::FieldMap;
use vcad_kernel_particle::fom::stats;
use vcad_kernel_particle::poisson::{solve, SolveOptions};
use vcad_kernel_particle::trace::{TraceOptions, Tracer, DEUTERON};

fn tracer(v: f64, at: f64) -> f64 {
    let d = Device::shielded_two_ring(150.0, 45.0, 25.0, 3.0, v, at);
    let sol = solve(&d, 81, 161, &SolveOptions::default()).unwrap();
    let f = FieldMap::new(&d, &sol);
    let t = Tracer::new(
        &d,
        &f,
        &sol,
        TraceOptions {
            max_passes: 20,
            ..TraceOptions::default()
        },
    );
    stats(&t.launch_ensemble(DEUTERON, 48)).mean_ddn_sigma_v_m3
}

fn adj(v: f64, at: f64, drop_v: f64) -> f64 {
    let d = Device::shielded_two_ring(150.0, 45.0, 25.0, 3.0, v, at);
    yield_gradient(
        &d,
        81,
        161,
        &SolveOptions::default(),
        &AdjointConfig {
            reference_drop_v: Some(drop_v),
            ..AdjointConfig::default()
        },
    )
    .unwrap()
    .value
}

fn main() {
    for &(v, at) in &[
        (-30_000.0, 20_000.0),
        (-30_000.0, 80_000.0),
        (-30_000.0, 160_000.0),
        (-15_000.0, 40_000.0),
    ] {
        let d = Device::shielded_two_ring(150.0, 45.0, 25.0, 3.0, v, at);
        let t0 = std::time::Instant::now();
        let g = yield_gradient(
            &d,
            81,
            161,
            &SolveOptions::default(),
            &AdjointConfig {
                reference_drop_v: Some(v.abs()),
                ..AdjointConfig::default()
            },
        )
        .unwrap();
        let t_adj = t0.elapsed().as_secs_f64();
        let t1 = std::time::Instant::now();
        let f0 = tracer(v, at);
        let t_tr = t1.elapsed().as_secs_f64();
        let dat = 0.1 * at;
        let dv = 0.05 * v.abs();
        let fd_i = (tracer(v, at + dat) - tracer(v, at - dat)) / (2.0 * dat);
        let fd_v = (tracer(v + dv, at) - tracer(v - dv, at)) / (2.0 * dv);
        // The adjoint's own J, secant at the same steps (frozen drop):
        // separates "J differs from the tracer" from "J is rough".
        let js_i = (adj(v, at + dat, v.abs()) - adj(v, at - dat, v.abs())) / (2.0 * dat);
        let js_v = (adj(v + dv, at, v.abs()) - adj(v - dv, at, v.abs())) / (2.0 * dv);
        let adj_i = g.d_ampere_turns[0] - g.d_ampere_turns[1];
        let adj_v: f64 = g.d_ring_potentials.iter().sum();
        println!(
            "V={v:.0} AT={at:.0}: tracer f={f0:.3e} ({t_tr:.2}s)  adjoint J={:.3e} ({t_adj:.2}s)\n  \
             dlnf/dI fd={:.3e}  dlnJ/dI adj={:.3e}  dlnJ/dI J-secant={:.3e}\n  \
             dlnf/dV fd={:.3e}  dlnJ/dV adj={:.3e}  dlnJ/dV J-secant={:.3e}",
            g.value,
            fd_i / f0,
            adj_i / g.value,
            js_i / g.value,
            fd_v / f0,
            adj_v / g.value,
            js_v / g.value
        );
    }
}
