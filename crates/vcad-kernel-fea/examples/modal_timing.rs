//! Timing + accuracy probe for the solid modal solver.
use vcad_kernel_fea::mesh::box_mesh;
use vcad_kernel_fea::modal::{modal_analysis, ModalSpec};

fn run(name: &str, min: [f64; 3], max: [f64; 3], res: usize, pitch: Option<[f64; 3]>) {
    let mesh = box_mesh(min, max);
    let spec = ModalSpec {
        resolution: res,
        max_pitch_mm: pitch,
        youngs_modulus_mpa: 68_900.0,
        poisson: 0.33,
        density_kg_m3: 2700.0,
        supports: vec![],
        modes: 6,
    };
    let t = std::time::Instant::now();
    match modal_analysis(&mesh, &spec) {
        Ok(r) => println!(
            "{name:<28} {:>6} dofs {:>5} cells {:>4} it {:>7.0} ms  hz {:?}",
            r.dofs,
            r.cells,
            r.iterations,
            t.elapsed().as_secs_f64() * 1e3,
            r.modes
                .iter()
                .map(|m| (m.hz * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        ),
        Err(e) => println!("{name}: {e}"),
    }
}

fn main() {
    for p in [3.0f64, 2.0, 1.5, 1.0] {
        run(
            &format!("C6 bar 131.3x25x6 @{p}mm"),
            [0.0; 3],
            [131.32, 25.0, 6.0],
            (131.32 / p).round() as usize,
            Some([p, p, p.min(3.0)]),
        );
    }
    run("cube 60 @ res 12", [0.0; 3], [60.0; 3], 12, None);
    run("cube 60 @ res 20", [0.0; 3], [60.0; 3], 20, None);
    run(
        "plate 100x100x3 @2.5/1.5",
        [0.0; 3],
        [100.0, 100.0, 3.0],
        40,
        Some([2.5, 2.5, 1.5]),
    );
}
