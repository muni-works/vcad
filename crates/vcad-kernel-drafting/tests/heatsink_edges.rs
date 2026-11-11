//! Hidden-line extraction on a boolean-built heatsink must report real BRep
//! edges, not tessellation diagonals.
//!
//! Regression for the Mac app drawing bench (2026-11-10): a 50 × 40 × 5 base
//! with eight 1.5 × 36 × 25 fins drew 1,292 hidden edges in the front view —
//! every triangle diagonal across the flat faces behind the front face.

use vcad_kernel::Solid;
use vcad_kernel_drafting::{project_mesh, ViewDirection, Visibility};

fn heatsink() -> Solid {
    let mut s = Solid::cube(50.0, 40.0, 5.0);
    let pitch = (50.0 - 1.5) / 7.0;
    for i in 0..8 {
        let fin = Solid::cube(1.5, 36.0, 25.0).translate(i as f64 * pitch, 2.0, 5.0);
        s = s.union(&fin);
    }
    s
}

fn counts(view: ViewDirection) -> (usize, usize) {
    let v = project_mesh(&heatsink().to_mesh(32), view);
    let vis = v
        .edges
        .iter()
        .filter(|e| e.visibility == Visibility::Visible)
        .count();
    (vis, v.edges.len() - vis)
}

#[test]
fn heatsink_views_have_only_feature_edges() {
    for (name, dir) in [
        ("front", ViewDirection::Front),
        ("top", ViewDirection::Top),
        ("right", ViewDirection::Right),
    ] {
        let (vis, hid) = counts(dir);
        eprintln!("{name}: visible {vis} hidden {hid}");
        // 12 base edges + 8 fins × 12 edges, some split where faces meet:
        // a couple of hundred at most, never one per triangle.
        assert!(hid < 100, "{name} hidden edges {hid}");
        assert!(vis < 100, "{name} visible edges {vis}");
    }
}
