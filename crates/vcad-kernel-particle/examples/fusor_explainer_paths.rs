//! Data for the "how a fusor works" explainer (`docs/videos/fusor-explainer`).
//!
//! The six-great-circle globe of `docs/videos/open-fusor/fusor.loon`
//! (19 mm grid, 2 mm wire, −30 kV) in a 76 mm grounded sphere, solved by the
//! 3D thin-wire BEM and traced with the Boris tracer — the same device and
//! launch as `globe_vs_rings`. Writes three CSVs into the output directory:
//!
//! - `paths.csv`: every deuteron's flight, sampled at a fixed time step
//!   (`ion,t_ns,x_mm,y_mm,z_mm,ke_ev`), so a renderer shows true speed.
//! - `fates.csv`: one row per ion (`ion,fate,core_passes,time_ns`).
//! - `potential.csv`: φ on the y = 0 plane (`x_mm,z_mm,phi_v`).
//!
//! Paths are vacuum orbits: charge exchange with the gas is not applied to
//! the trajectory (the tracer weights yield by survival instead), so an ion
//! here flies until it hits a wire or reaches the pass cap.
//!
//! `cargo run --release -p vcad-kernel-particle --example fusor_explainer_paths -- <out dir> [ions]`

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use vcad_kernel_particle::bem::{solve, BemOptions};
use vcad_kernel_particle::trace::{Fate, TraceOptions, DEUTERON};
use vcad_kernel_particle::wire3d::{fibonacci_sphere, Device3d, GreatCircleLayout};

const CHAMBER_R_MM: f64 = 76.0;
const GRID_R_MM: f64 = 19.0;
const WIRE_A_MM: f64 = 1.0;
const VOLTS: f64 = -30_000.0;
const SAMPLE_NS: f64 = 2.0;
const PLANE_N: usize = 201;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let ions: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(96);
    fs::create_dir_all(&out).expect("create output dir");

    let device = Device3d::great_circle_globe(
        CHAMBER_R_MM,
        GRID_R_MM,
        &GreatCircleLayout::Icosidodecahedral6.normals(),
        WIRE_A_MM,
        VOLTS,
    );
    let field = solve(&device, &BemOptions::default()).expect("BEM solve");
    let tracer = field.tracer(field.default_step_length_m(), TraceOptions::default());
    let shell = tracer.shell_radius_m();

    let mut paths = String::from("ion,t_ns,x_mm,y_mm,z_mm,ke_ev\n");
    let mut fates = String::from("ion,fate,core_passes,time_ns\n");
    let (mut wire, mut passes) = (0usize, 0u64);
    for (k, d) in fibonacci_sphere(ions).into_iter().enumerate() {
        let start = [d[0] * shell, d[1] * shell, d[2] * shell];
        let mut next_ns = 0.0;
        let mut row = |t_ns: f64, p: [f64; 3], ke: f64| {
            let _ = writeln!(
                paths,
                "{k},{t_ns:.2},{:.3},{:.3},{:.3},{ke:.0}",
                p[0] * 1e3,
                p[1] * 1e3,
                p[2] * 1e3
            );
        };
        row(0.0, start, 0.0);
        let o = tracer.trace_observed(DEUTERON, start, [0.0; 3], &mut |ev| {
            let t_ns = ev.time_s * 1e9;
            if t_ns >= next_ns {
                row(t_ns, ev.pos, ev.kinetic_ev);
                next_ns = t_ns + SAMPLE_NS;
            }
            true
        });
        let fate = match o.fate {
            Fate::Wire(_) => "wire",
            Fate::Wall => "wall",
            Fate::Survived => "survived",
        };
        wire += usize::from(matches!(o.fate, Fate::Wire(_)));
        passes += u64::from(o.core_passes);
        let _ = writeln!(fates, "{k},{fate},{},{:.2}", o.core_passes, o.time_s * 1e9);
    }

    let mut plane = String::from("x_mm,z_mm,phi_v\n");
    for i in 0..PLANE_N {
        for j in 0..PLANE_N {
            let f = |n: usize| -CHAMBER_R_MM + 2.0 * CHAMBER_R_MM * n as f64 / (PLANE_N - 1) as f64;
            let (x, z) = (f(i), f(j));
            if x.hypot(z) > CHAMBER_R_MM {
                continue;
            }
            let phi = field.potential([x * 1e-3, 0.0, z * 1e-3]);
            let _ = writeln!(plane, "{x:.2},{z:.2},{phi:.1}");
        }
    }

    fs::write(out.join("paths.csv"), paths).expect("write paths.csv");
    fs::write(out.join("fates.csv"), fates).expect("write fates.csv");
    fs::write(out.join("potential.csv"), plane).expect("write potential.csv");
    println!(
        "{ions} ions: {wire} hit a wire, mean core passes {:.2}, phi(center) = {:.2} kV",
        passes as f64 / ions as f64,
        field.potential([0.0; 3]) / 1e3
    );
}
