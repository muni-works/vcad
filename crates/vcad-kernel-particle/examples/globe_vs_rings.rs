//! Hobby fusor: a real 6-great-circle globe cathode vs the latitude
//! ring-stack approximation the axisymmetric solver forced on us.
//!
//! 6" chamber (76 mm inner radius sphere), 1.5" grid (19 mm radius),
//! 2 mm wire (a = 1 mm), −30 kV, 10 mA ion current, 5 mTorr D₂, CX
//! σ = 1×10⁻¹⁹ m² — the operating point of `docs/videos/open-fusor.md`.
//!
//! Every configuration is solved by the same 3D thin-wire BEM
//! ([`vcad_kernel_particle::bem`]) in the same spherical chamber and traced
//! by the same Boris tracer from the same full-sphere Fibonacci launch, so
//! the differences are the grid geometry alone. The old 2D path (rings in
//! a cylinder, meridian-plane launch) is printed as a cross-check.
//!
//! `cargo run --release -p vcad-kernel-particle --example globe_vs_rings [particles]`

use std::time::Instant;

use vcad_kernel_particle::bem::{launch_fibonacci, solve, BemOptions, WireField};
use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::field::FieldMap;
use vcad_kernel_particle::fom::{self, EnsembleStats};
use vcad_kernel_particle::poisson::{self, ChamberShape, SolveOptions};
use vcad_kernel_particle::trace::{CxModel, TraceOptions, TraceOutcome, Tracer, DEUTERON};
use vcad_kernel_particle::wire3d::{Chamber3d, Device3d, GreatCircleLayout};
use vcad_kernel_particle::xsection::d2_deuteron_density_m3;

const CHAMBER_R_MM: f64 = 76.0;
const GRID_R_MM: f64 = 19.0;
const WIRE_A_MM: f64 = 1.0;
const VOLTS: f64 = -30_000.0;
const ION_CURRENT_A: f64 = 0.010;
const PRESSURE_MTORR: f64 = 5.0;
const SIGMA_CX_M2: f64 = 1.0e-19;
const MAX_PASSES: u32 = 40;

fn trace_opts() -> TraceOptions {
    TraceOptions {
        max_passes: MAX_PASSES,
        cx: Some(CxModel {
            sigma_cx_m2: SIGMA_CX_M2,
            background_deuteron_density_m3: d2_deuteron_density_m3(PRESSURE_MTORR, 300.0),
        }),
        ..TraceOptions::default()
    }
}

struct Row {
    name: String,
    wires: usize,
    unknowns: usize,
    solve_s: f64,
    residual: String,
    phi_center_kv: f64,
    t_geo: f64,
    t_first: f64,
    stats: EnsembleStats,
    trace_s: f64,
}

fn yields(st: &EnsembleStats) -> (f64, f64) {
    let n_d = d2_deuteron_density_m3(PRESSURE_MTORR, 300.0);
    let ceiling = fom::neutron_rate_per_s(st.mean_ddn_sigma_v_m3, ION_CURRENT_A, n_d);
    let floor = fom::neutron_rate_from_counts(
        st.mean_neutrons_ion_channel + st.mean_neutrons_cx_channel,
        ION_CURRENT_A,
    );
    (floor, ceiling)
}

fn first_pass(out: &[TraceOutcome]) -> f64 {
    out.iter().filter(|o| o.core_passes >= 1).count() as f64 / out.len().max(1) as f64
}

fn run3d(name: &str, device: &Device3d, particles: usize) -> (Row, WireField) {
    let t0 = Instant::now();
    let field = solve(device, &BemOptions::default()).expect("BEM solve");
    let solve_s = t0.elapsed().as_secs_f64();
    let rep = field.residual_report();
    let t1 = Instant::now();
    let tracer = field.tracer(field.default_step_length_m(), trace_opts());
    let out = launch_fibonacci(&tracer, DEUTERON, particles);
    let trace_s = t1.elapsed().as_secs_f64();
    let row = Row {
        name: name.to_string(),
        wires: field.wire_count(),
        unknowns: field.unknowns(),
        solve_s,
        residual: format!(
            "{:.1}/{:.1}/{:.1}",
            rep.mean * 100.0,
            rep.junction_mean * 100.0,
            rep.spread * 100.0
        ),
        phi_center_kv: field.potential([0.0; 3]) / 1e3,
        t_geo: device.geometric_transparency(20_000),
        t_first: first_pass(&out),
        stats: fom::stats(&out),
        trace_s,
    };
    (row, field)
}

fn main() {
    let particles: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(192);
    let sphere = Chamber3d::Sphere {
        radius_mm: CHAMBER_R_MM,
    };
    let rings = Device::classic_fusor(CHAMBER_R_MM, GRID_R_MM, 4, WIRE_A_MM, VOLTS);
    let configs: Vec<(&str, Device3d)> = vec![
        (
            "ring stack (4 latitude rings)",
            Device3d::from_rings(&rings, sphere),
        ),
        (
            "6 great circles (icosidodecahedral)",
            Device3d::great_circle_globe(
                CHAMBER_R_MM,
                GRID_R_MM,
                &GreatCircleLayout::Icosidodecahedral6.normals(),
                WIRE_A_MM,
                VOLTS,
            ),
        ),
        (
            "lat/long globe (3 lat + 4 meridians)",
            Device3d::lat_long_globe(CHAMBER_R_MM, GRID_R_MM, 3, 4, WIRE_A_MM, VOLTS),
        ),
    ];

    println!(
        "# hobby fusor: {CHAMBER_R_MM} mm sphere, {GRID_R_MM} mm grid, a = {WIRE_A_MM} mm, \
         {} kV, {} mA, {PRESSURE_MTORR} mTorr, σ_cx = {SIGMA_CX_M2:e} m², {particles} D⁺ \
         (Fibonacci, at rest on 0.85 R), {MAX_PASSES}-pass cap",
        VOLTS / 1e3,
        ION_CURRENT_A * 1e3
    );
    println!(
        "config,wires,unknowns,solve_s,residual_pct(mean/junction/spread),phi_center_kv,\
         t_geometric,t_first_pass,interception,survivors,mean_passes,mean_drift,\
         floor_n_per_s,ceiling_n_per_s,trace_s"
    );
    let mut ring_field = None;
    for (k, (name, dev)) in configs.iter().enumerate() {
        let (row, field) = run3d(name, dev, particles);
        let (floor, ceiling) = yields(&row.stats);
        println!(
            "{},{},{},{:.2},{},{:.2},{:.3},{:.3},{:.3},{:.3},{:.2},{:.4},{:.3e},{:.3e},{:.1}",
            row.name,
            row.wires,
            row.unknowns,
            row.solve_s,
            row.residual,
            row.phi_center_kv,
            row.t_geo,
            row.t_first,
            row.stats.interception_fraction,
            row.stats.survivor_fraction,
            row.stats.mean_passes,
            row.stats.mean_energy_drift_rel,
            floor,
            ceiling,
            row.trace_s
        );
        if k == 0 {
            ring_field = Some(field);
        }
    }

    // --- 2D cross-checks for the ring stack -----------------------------
    let f3 = ring_field.expect("ring stack solved");
    let sopts = SolveOptions::default();
    let s2 = poisson::solve_with_chamber(&rings, ChamberShape::Sphere, 305, 609, &sopts)
        .expect("2D sphere solve");
    println!("\n# 2D (sphere chamber, h = 0.25 mm) vs 3D potential, ring stack, kV");
    for (r, z) in [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0), (30.0, 20.0), (50.0, -10.0)] {
        let p3 = f3.potential([r * 1e-3, 0.0, z * 1e-3]);
        let p2 = s2.potential_at(r * 1e-3, z * 1e-3);
        println!(
            "  ({r:>4},{z:>4}) mm: 3D {:>8.3}  2D {:>8.3}  Δ {:+.2}% of |V|",
            p3 / 1e3,
            p2 / 1e3,
            (p2 - p3) / VOLTS.abs() * 100.0
        );
    }
    // The old pipeline: rings in a cylinder, meridian-plane launch.
    let t0 = Instant::now();
    let s_cyl = poisson::solve(&rings, 305, 609, &sopts).expect("2D cylinder solve");
    let fields = FieldMap::new(&rings, &s_cyl);
    let tracer = Tracer::new(&rings, &fields, &s_cyl, trace_opts());
    let out = tracer.launch_ensemble(DEUTERON, particles);
    let st = fom::stats(&out);
    let (floor, ceiling) = yields(&st);
    println!(
        "\n# old 2D path (ring stack, cylinder, meridian-plane launch): interception {:.3}, \
         survivors {:.3}, mean passes {:.2}, first pass {:.3}, floor {:.3e}, ceiling {:.3e} n/s \
         ({:.1} s)",
        st.interception_fraction,
        st.survivor_fraction,
        st.mean_passes,
        first_pass(&out),
        floor,
        ceiling,
        t0.elapsed().as_secs_f64()
    );
}
