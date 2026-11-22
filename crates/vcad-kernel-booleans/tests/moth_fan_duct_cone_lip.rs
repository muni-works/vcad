//! Moth EDF drone regression (2026-11): the fan duct with a cone-minus-
//! cylinder "flared lip" unioned in hung `vcad info` for 300+ s at 100% CPU
//! on an older build (the user's release binary). On main it evaluates in
//! about a second. This pins it with a wall-clock budget so a reintroduced
//! hang fails instead of stalling CI.
//!
//! Loon source (`[difference A B]` is B − A):
//!   tube ro ri z0 h = cyl(ro, h)@z0 − cyl(ri, h+2)@(z0−1)
//!   lip  = cone(56,56,6) − cone(53,51,8)@z−1   (equal radii = cylinder)
//!   fan_duct = tube(56,53.5,0,104) ∪ (lip ∪ tube(49,46.5,104,10.2))

use std::sync::mpsc;
use std::time::Duration;

use vcad_kernel_booleans::{boolean_op, BooleanOp, BooleanResult};
use vcad_kernel_math::Transform;
use vcad_kernel_primitives::{make_cone, make_cylinder, BRepSolid};

const SEGMENTS: u32 = 32;

fn at_z(mut brep: BRepSolid, dz: f64) -> BRepSolid {
    let t = Transform::translation(0.0, 0.0, dz);
    for (_, v) in &mut brep.topology.vertices {
        v.point = t.apply_point(&v.point);
    }
    brep.geometry.surfaces = brep
        .geometry
        .surfaces
        .drain(..)
        .map(|s| s.transform(&t))
        .collect();
    brep
}

fn solid(r: BooleanResult) -> BRepSolid {
    r.into_brep().expect("boolean returned no B-rep")
}

fn op(a: &BRepSolid, b: &BRepSolid, o: BooleanOp) -> BRepSolid {
    solid(boolean_op(a, b, o, SEGMENTS).expect("boolean failed"))
}

fn tube(ro: f64, ri: f64, z0: f64, h: f64) -> BRepSolid {
    let outer = at_z(make_cylinder(ro, h, SEGMENTS), z0);
    let inner = at_z(make_cylinder(ri, h + 2.0, SEGMENTS), z0 - 1.0);
    op(&outer, &inner, BooleanOp::Difference)
}

#[test]
fn fan_duct_with_cone_lip_terminates() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let duct = tube(56.0, 53.5, 0.0, 104.0);
        let lip = op(
            &make_cone(56.0, 56.0, 6.0, SEGMENTS),
            &at_z(make_cone(53.0, 51.0, 8.0, SEGMENTS), -1.0),
            BooleanOp::Difference,
        );
        let spig = tube(49.0, 46.5, 104.0, 10.2);
        let rest = op(&lip, &spig, BooleanOp::Union);
        let fan_duct = op(&duct, &rest, BooleanOp::Union);
        let _ = tx.send(fan_duct.topology.faces.len());
    });
    let faces = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("fan duct + cone lip union did not finish within 120 s (hang regression)");
    assert!(faces > 0, "fan duct union produced an empty solid");
}
