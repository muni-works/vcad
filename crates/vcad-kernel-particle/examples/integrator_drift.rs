//! Integrator energy drift, like for like (`docs/particle-optics-m0.md` M7).
//!
//! The open-fusor hobby geometry at h = 0.25 mm, CX off, 40-pass cap, 96
//! ions: the staggered Boris leapfrog vs the synchronized
//! kick–rotate–drift–rotate–kick scheme at identical step fractions.
//!
//! Run: `cargo run --release -p vcad-kernel-particle --example integrator_drift`
use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::field::FieldMap;
use vcad_kernel_particle::fom::stats;
use vcad_kernel_particle::poisson::{solve, SolveOptions};
use vcad_kernel_particle::trace::{Integrator, TraceOptions, Tracer, DEUTERON};
fn main() {
    let device = Device::classic_fusor(76.0, 19.0, 4, 1.0, -30_000.0);
    let sol = solve(&device, 305, 609, &SolveOptions::default()).unwrap();
    let fields = FieldMap::new(&device, &sol);
    println!("integrator,step_fraction,max_drift,mean_drift,survivor_frac,mean_passes,sigv");
    for integ in [Integrator::Leapfrog, Integrator::Synchronized] {
        for sf in [0.25, 0.125, 0.0625] {
            let opts = TraceOptions {
                max_passes: 40,
                step_fraction: sf,
                integrator: integ,
                ..TraceOptions::default()
            };
            let s = stats(&Tracer::new(&device, &fields, &sol, opts).launch_ensemble(DEUTERON, 96));
            println!(
                "{integ:?},{sf},{:.4},{:.5},{:.3},{:.2},{:.4e}",
                s.max_energy_drift_rel,
                s.mean_energy_drift_rel,
                s.survivor_fraction,
                s.mean_passes,
                s.mean_ddn_sigma_v_m3
            );
        }
    }
}
