//! Beam-solver reference for the plain 131.32×25×6 6061 bar (no holes/cut).
use vcad_kernel_acoustics::instrument::{modal, BarGeometry, BarMaterial, ModelOptions};
fn main() {
    let g = BarGeometry {
        length_mm: 131.32,
        width_mm: 25.0,
        thickness_mm: 6.0,
        undercut_depth_mm: 0.0,
        undercut_span: 0.2,
        hole_dia_mm: 0.0,
        holes_mm: vec![],
    };
    let r = modal(&g, BarMaterial::Aluminium6061, 3, &ModelOptions::default());
    let eb = modal(
        &g,
        BarMaterial::Aluminium6061,
        3,
        &ModelOptions::euler_bernoulli(),
    );
    println!(
        "beam (timoshenko-1st) {:?}\nbeam (EB)             {:?}",
        r.hz, eb.hz
    );
}
