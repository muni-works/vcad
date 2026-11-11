//! Scan f₂/f₁ and f₁ drop vs undercut span and depth for a 6061 bar.
use vcad_kernel_acoustics::instrument::{modal, BarGeometry, BarMaterial, ModelOptions};
fn main() {
    let mat = BarMaterial::Aluminium6061;
    let base = BarGeometry { length_mm: 170.0, width_mm: 25.0, thickness_mm: 6.0, undercut_depth_mm: 0.0, undercut_span: 0.5, hole_dia_mm: 0.0, holes_mm: vec![] };
    let p = modal(&base, mat, 2, &ModelOptions::default());
    println!("plain f1 {:.1} ratio {:.3}", p.hz[0], p.hz[1] / p.hz[0]);
    for span in [0.1, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5] {
        let mut row = format!("span {span:.2}:");
        for d in [0.2, 0.4, 0.5, 0.6, 0.7, 0.75] {
            let g = BarGeometry { undercut_depth_mm: d * 6.0, undercut_span: span, ..base.clone() };
            let r = modal(&g, mat, 2, &ModelOptions::default());
            row += &format!("  d{d:.2}: r={:.2} f1x={:.2}", r.hz[1] / r.hz[0], r.hz[0] / p.hz[0]);
        }
        println!("{row}");
    }
}
