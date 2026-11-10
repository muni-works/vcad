//! Extrusion must not care which way round a closed profile was drawn.
//!
//! Field report 2026-11-10 (Mac app, by mouse): a sketch rectangle clicked
//! top-left then bottom-right is a clockwise loop. Extruded along +Z it gave
//! an inside-out solid — the viewport culled the front and top faces, the top
//! face picked with normal (0,0,-1), the reported volume was a positive 60000
//! only because volume was abs'd, and a shell with the top face open
//! evaluated to nothing.

use vcad_kernel::{FaceRef, Solid};
use vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel_sketch::{SketchProfile, SketchSegment};

fn loop_of(pts: &[(f64, f64)]) -> Vec<SketchSegment> {
    (0..pts.len())
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            SketchSegment::Line {
                start: Point2::new(a.0, a.1),
                end: Point2::new(b.0, b.1),
            }
        })
        .collect()
}

/// The field rectangle: (-33.5,16) → (26.5,16) → (26.5,-24) → (-33.5,-24).
fn field_rect(cw: bool) -> SketchProfile {
    let mut pts = vec![(-33.5, 16.0), (26.5, 16.0), (26.5, -24.0), (-33.5, -24.0)];
    if !cw {
        pts.reverse();
    }
    SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::y(), loop_of(&pts)).unwrap()
}

fn box_of(cw: bool) -> Solid {
    Solid::extrude(field_rect(cw), Vec3::new(0.0, 0.0, 25.0)).unwrap()
}

/// Area-weighted normal of the mesh triangles lying in the plane z = 25.
fn top_normal(s: &Solid) -> Vec3 {
    let m = s.to_mesh(32);
    let v = |i: u32| {
        let i = i as usize * 3;
        Vec3::new(
            m.vertices[i] as f64,
            m.vertices[i + 1] as f64,
            m.vertices[i + 2] as f64,
        )
    };
    let mut acc = Vec3::zeros();
    for t in m.indices.chunks_exact(3) {
        let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
        if [a.z, b.z, c.z].iter().all(|z| (z - 25.0).abs() < 1e-4) {
            acc += (b - a).cross(&(c - a));
        }
    }
    acc.normalize()
}

#[test]
fn cw_and_ccw_rectangles_extrude_to_the_same_outward_solid() {
    for cw in [false, true] {
        let s = box_of(cw);
        let v = s.signed_volume();
        assert!((v - 60_000.0).abs() < 1e-3, "cw={cw}: signed volume {v}");
        assert!(!s.is_inside_out());
        let n = top_normal(&s);
        assert!((n.z - 1.0).abs() < 1e-9, "cw={cw}: top normal {n:?}");
    }
}

#[test]
fn shell_with_top_open_works_for_either_winding() {
    let expected = 60.0 * 40.0 * 25.0 - 56.0 * 36.0 * 23.0; // 13632
    for cw in [false, true] {
        let top = FaceRef {
            name: None,
            point: Some(Point3::new(-3.5, -4.0, 25.0)),
            normal: Some(Vec3::new(0.0, 0.0, 1.0)),
        };
        let shelled = box_of(cw).shell_open_refs(2.0, &[top]).unwrap();
        let v = shelled.signed_volume();
        assert!(
            (v - expected).abs() < 1e-2,
            "cw={cw}: shell volume {v}, expected {expected}"
        );
    }
}

#[test]
fn holed_profile_is_winding_independent() {
    let outer = [(0.0, 0.0), (30.0, 0.0), (30.0, 20.0), (0.0, 20.0)];
    let hole = [(10.0, 5.0), (20.0, 5.0), (20.0, 15.0), (10.0, 15.0)];
    let expected = (600.0 - 100.0) * 5.0;
    for outer_cw in [false, true] {
        for hole_cw in [false, true] {
            let mut o = outer.to_vec();
            if outer_cw {
                o.reverse();
            }
            let mut h = hole.to_vec();
            if hole_cw {
                h.reverse();
            }
            let prof =
                SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::y(), loop_of(&o)).unwrap();
            let s =
                Solid::extrude_with_holes(prof, &[loop_of(&h)], Vec3::new(0.0, 0.0, 5.0)).unwrap();
            let v = s.signed_volume();
            assert!(
                (v - expected).abs() < 1e-3,
                "outer_cw={outer_cw} hole_cw={hole_cw}: {v}"
            );
        }
    }
}

#[test]
fn twisted_extrude_of_cw_profile_is_outward() {
    let s =
        Solid::extrude_with_options(field_rect(true), Vec3::new(0.0, 0.0, 25.0), 0.3, 1.0).unwrap();
    assert!(s.signed_volume() > 0.0, "{}", s.signed_volume());
}

#[test]
fn revolve_of_cw_profile_is_outward() {
    // 10×20 rectangle at radius 5..15 in the XZ plane, revolved about Z.
    let pts = [(5.0, 0.0), (15.0, 0.0), (15.0, 20.0), (5.0, 20.0)];
    let expected = std::f64::consts::PI * (15.0f64.powi(2) - 5.0f64.powi(2)) * 20.0;
    for cw in [false, true] {
        let mut p = pts.to_vec();
        if cw {
            p.reverse();
        }
        let prof = SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::z(), loop_of(&p)).unwrap();
        let s = Solid::revolve(prof, Point3::origin(), Vec3::z(), 360.0).unwrap();
        let v = s.signed_volume();
        assert!(
            v > 0.0 && (v - expected).abs() < 0.02 * expected,
            "cw={cw}: {v} vs {expected}"
        );
    }
}
