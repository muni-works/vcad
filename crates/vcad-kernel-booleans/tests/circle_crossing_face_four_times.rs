//! Regression: a plain box ∪ cylinder came out OPEN when the cylinder
//! crossed two opposite edges of the box's top and bottom faces.
//!
//! The r8 cylinder through a 12 mm-wide box crosses each of those faces'
//! long edges twice — four crossings — so the faces are split by
//! `split_planar_face_by_multi_arc`. That splitter was written for fillet
//! corners (#754), where the circle comes from a SPHERE and rides the raw
//! `θ = 2πk/segments` grid, and it sampled every arc that way. The cylinder
//! wall bordering the same circle rides the sag-adaptive canonical grid
//! (`canonical_arc_points`), so not one arc vertex matched: 144 unpaired
//! edges per rim, an "Analytic" result that was not a closed solid.
//!
//! Found as the root cause of a wheel-adapter channel that a later
//! `body − cutters` silently failed to cut (see
//! `vcad-eval/tests/channel_cut_across_counterbored_block.rs`): the open
//! cutter union was the tool.
//!
//! As everywhere in this crate the volume is checked against a closed form —
//! a closed mesh of the wrong solid is no better than an open one.

use vcad_kernel_booleans::{boolean_op_reported, mesh_report, BooleanOp, BooleanResult};
use vcad_kernel_math::Transform;
use vcad_kernel_primitives::{make_cube, make_cylinder, BRepSolid};
use vcad_kernel_tessellate::tessellate_brep;

const SEGMENTS: u32 = 32;
const R: f64 = 8.0;
const CYL_H: f64 = 57.0;
const BOX: (f64, f64, f64) = (46.0, 12.0, 11.0);

fn translated(mut b: BRepSolid, x: f64, y: f64, z: f64) -> BRepSolid {
    let t = Transform::translation(x, y, z);
    for (_, v) in &mut b.topology.vertices {
        v.point = t.apply_point(&v.point);
    }
    b.geometry.surfaces = b
        .geometry
        .surfaces
        .drain(..)
        .map(|s| s.transform(&t))
        .collect();
    b
}

/// Box centred on the cylinder axis in x and y, z ∈ [-1, 10]; the cylinder
/// starts at `cyl_z0` and runs past the box top.
fn operands(cyl_z0: f64) -> (BRepSolid, BRepSolid) {
    let bx = translated(
        make_cube(BOX.0, BOX.1, BOX.2),
        -BOX.0 / 2.0,
        -BOX.1 / 2.0,
        -1.0,
    );
    let cy = translated(make_cylinder(R, CYL_H, SEGMENTS), 0.0, 0.0, cyl_z0);
    (bx, cy)
}

/// Area of the disk of radius `R` inside the strip |y| ≤ BOX.1 / 2.
fn disk_in_strip() -> f64 {
    let d = BOX.1 / 2.0;
    let segment = R * R * (d / R).acos() - d * (R * R - d * d).sqrt();
    std::f64::consts::PI * R * R - 2.0 * segment
}

fn check(name: &str, a: &BRepSolid, b: &BRepSolid, op: BooleanOp, expected: f64) {
    let (res, report) = boolean_op_reported(a, b, op, SEGMENTS).expect("boolean");
    let BooleanResult::BRep(solid) = res;
    let m = mesh_report(&tessellate_brep(&solid, SEGMENTS));
    let err = (m.signed_volume - expected).abs() / expected;
    assert_eq!(
        report.reason, None,
        "{name}: degraded to the mesh fallback ({:?}) — the analytic result \
         is what this test pins",
        report.reason
    );
    assert_eq!(
        m.open_edges, 0,
        "{name}: {} unpaired directed edges (volume {:.1})",
        m.open_edges, m.signed_volume
    );
    // The inscribed 32-gon caps sit ~0.6% under the true disk; the
    // sag-adaptive walls and rims are far tighter. 0.2% clears tessellation
    // and still catches a missing or doubled face.
    assert!(
        err < 0.002,
        "{name}: volume {:.1} vs closed form {expected:.1} ({:.3}% off)",
        m.signed_volume,
        err * 100.0
    );
}

#[test]
fn union_with_cylinder_crossing_both_long_edges_is_closed() {
    let box_vol = BOX.0 * BOX.1 * BOX.2;
    let cyl_vol = std::f64::consts::PI * R * R * CYL_H;
    // Cylinder from below the box (both box caps cut through), and flush
    // with the box bottom (the original channel ∪ bore: coplanar z = -1).
    for z0 in [-2.0, -1.0] {
        let (bx, cy) = operands(z0);
        let overlap = disk_in_strip() * BOX.2;
        let union = box_vol + cyl_vol - overlap;
        check(
            &format!("box∪cyl z0={z0}"),
            &bx,
            &cy,
            BooleanOp::Union,
            union,
        );
        check(
            &format!("cyl∪box z0={z0}"),
            &cy,
            &bx,
            BooleanOp::Union,
            union,
        );
        check(
            &format!("box−cyl z0={z0}"),
            &bx,
            &cy,
            BooleanOp::Difference,
            box_vol - overlap,
        );
        check(
            &format!("cyl−box z0={z0}"),
            &cy,
            &bx,
            BooleanOp::Difference,
            cyl_vol - overlap,
        );
    }
}
