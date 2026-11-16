use vcad_kernel_particle::bem::{launch_fibonacci, solve, BemOptions};
use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::fom;
use vcad_kernel_particle::poisson::{solve_with_chamber, ChamberShape, SolveOptions};
use vcad_kernel_particle::trace::{TraceOptions, DEUTERON};
use vcad_kernel_particle::wire3d::{Chamber3d, Device3d, GreatCircleLayout};
fn main() {
    let v = -10_000.0;
    let rings = Device::classic_fusor(60.0, 20.0, 4, 0.75, v);
    let d3 = Device3d::from_rings(&rings, Chamber3d::Sphere { radius_mm: 60.0 });
    let f3 = solve(&d3, &BemOptions::default()).unwrap();
    println!("rep {:?}", f3.residual_report());
    let pts = [(0.0, 0.0),(5.0, 3.0),(10.0, -8.0),(0.0, 14.0),(15.0, 0.0),(26.0, 0.0),(30.0, 25.0),(0.0, -40.0),(45.0, 10.0)];
    for (nr, nz) in [(121, 241), (241, 481), (481, 961)] {
        let s2 = solve_with_chamber(&rings, ChamberShape::Sphere, nr, nz, &SolveOptions::default()).unwrap();
        let errs: Vec<String> = pts.iter().map(|&(r,z)| format!("{:+.4}", (f3.potential([r*1e-3,0.0,z*1e-3]) - s2.potential_at(r*1e-3,z*1e-3))/v.abs())).collect();
        println!("h={:.3}mm: {}", 60.0/(nr as f64-1.0), errs.join(" "));
    }
    let f = solve(&Device3d::great_circle_globe(76.0, 19.0, &GreatCircleLayout::Icosidodecahedral6.normals(), 1.0, -30e3), &BemOptions::default()).unwrap();
    println!("globe rep {:?} N={}", f.residual_report(), f.unknowns());
    for step in [f.default_step_length_m(), 0.25e-3] {
        let tracer = f.tracer(step, TraceOptions { max_passes: 6, ..TraceOptions::default() });
        let t0 = std::time::Instant::now();
        let out = launch_fibonacci(&tracer, DEUTERON, 24);
        let st = fom::stats(&out);
        println!("step {step:e}: {:?} t={:?}", st, t0.elapsed());
    }
}
