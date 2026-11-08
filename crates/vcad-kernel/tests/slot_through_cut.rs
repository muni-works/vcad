//! A tool whose wall mixes straight and curved faces must cut its real
//! outline out of the planar faces it passes through. Each half-cylinder of a
//! slot used to imprint its full circle, so the cap got two round holes and
//! the straight middle of the slot stayed covered, while the volume still
//! looked plausible because the leftover skins cancelled.

use std::f64::consts::PI;
use vcad_kernel::vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel::vcad_kernel_sketch::{SketchProfile, SketchSegment};
use vcad_kernel::{Solid, SolidFidelity};

const HALF: f64 = 6.0;
const R: f64 = 3.0;
const THICK: f64 = 4.0;

fn slot_tool(depth: f64) -> Solid {
    let p = |x: f64, y: f64| Point2::new(x, y);
    let segments = vec![
        SketchSegment::Line {
            start: p(HALF, R),
            end: p(-HALF, R),
        },
        SketchSegment::Arc {
            start: p(-HALF, R),
            end: p(-HALF, -R),
            center: p(-HALF, 0.0),
            ccw: true,
        },
        SketchSegment::Line {
            start: p(-HALF, -R),
            end: p(HALF, -R),
        },
        SketchSegment::Arc {
            start: p(HALF, -R),
            end: p(HALF, R),
            center: p(HALF, 0.0),
            ccw: true,
        },
    ];
    let profile = SketchProfile::new(
        Point3::new(30.0, 28.0, THICK),
        Vec3::x(),
        Vec3::y(),
        segments,
    )
    .unwrap();
    Solid::extrude(profile, Vec3::new(0.0, 0.0, -depth)).unwrap()
}

/// Z of every triangle a vertical line through (x, y) passes through.
fn hits(mesh: &vcad_kernel::vcad_kernel_tessellate::TriangleMesh, x: f32, y: f32) -> Vec<f32> {
    let v = |i: u32| {
        let i = i as usize * 3;
        (mesh.vertices[i], mesh.vertices[i + 1], mesh.vertices[i + 2])
    };
    let side =
        |a: (f32, f32, f32), b: (f32, f32, f32)| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
    mesh.indices
        .chunks(3)
        .filter_map(|t| {
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            let (d0, d1, d2) = (side(a, b), side(b, c), side(c, a));
            let inside = (d0 > 0.0 && d1 > 0.0 && d2 > 0.0) || (d0 < 0.0 && d1 < 0.0 && d2 < 0.0);
            inside.then_some((a.2 + b.2 + c.2) / 3.0)
        })
        .collect()
}

fn check_cut(depth: f64) {
    let plate = Solid::cube(60.0, 56.0, THICK);
    let cut = plate.difference(&slot_tool(depth));
    assert_eq!(
        cut.fidelity(),
        SolidFidelity::Analytic,
        "depth {depth}: fell back to a mesh"
    );
    let mesh = cut.to_mesh(32);
    let area = 2.0 * HALF * 2.0 * R + PI * R * R;
    let expected = 60.0 * 56.0 * THICK - area * THICK;
    let vol = cut.volume();
    assert!(
        (vol - expected).abs() < expected * 1e-3,
        "depth {depth}: volume {vol}, expected {expected}"
    );
    assert!(
        mesh.boundary_edges().is_empty(),
        "depth {depth}: mesh is open"
    );
    for (x, y) in [
        (30.0, 28.0),
        (26.0, 28.0),
        (34.0, 28.0),
        (22.0, 28.0),
        (38.0, 28.0),
    ] {
        assert!(
            hits(&mesh, x, y).is_empty(),
            "depth {depth}: skin across slot at ({x}, {y})"
        );
    }
    assert_eq!(
        hits(&mesh, 10.0, 10.0).len(),
        2,
        "depth {depth}: plate faces must survive"
    );
}

#[test]
fn slot_cut_past_the_far_face() {
    check_cut(6.0);
}

#[test]
fn slot_cut_flush_with_the_far_face() {
    check_cut(THICK);
}

/// The L section of a motor bracket, base in the XY plane.
fn angle_bracket() -> Solid {
    let p = |x: f64, y: f64| Point2::new(x, y);
    let pts = [
        p(0.0, 0.0),
        p(60.0, 0.0),
        p(60.0, THICK),
        p(THICK, THICK),
        p(THICK, 50.0),
        p(0.0, 50.0),
    ];
    let segments = (0..pts.len())
        .map(|i| SketchSegment::Line {
            start: pts[i],
            end: pts[(i + 1) % pts.len()],
        })
        .collect();
    let profile = SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::z(), segments).unwrap();
    Solid::extrude(profile, Vec3::new(0.0, -56.0, 0.0)).unwrap()
}

fn slot_at(cx: f64, cy: f64, depth: f64) -> Solid {
    let p = |x: f64, y: f64| Point2::new(x, y);
    let segments = vec![
        SketchSegment::Line {
            start: p(HALF, R),
            end: p(-HALF, R),
        },
        SketchSegment::Arc {
            start: p(-HALF, R),
            end: p(-HALF, -R),
            center: p(-HALF, 0.0),
            ccw: true,
        },
        SketchSegment::Line {
            start: p(-HALF, -R),
            end: p(HALF, -R),
        },
        SketchSegment::Arc {
            start: p(HALF, -R),
            end: p(HALF, R),
            center: p(HALF, 0.0),
            ccw: true,
        },
    ];
    let profile =
        SketchProfile::new(Point3::new(cx, cy, THICK), Vec3::x(), Vec3::y(), segments).unwrap();
    Solid::extrude(profile, Vec3::new(0.0, 0.0, -depth)).unwrap()
}

#[test]
fn two_slots_in_a_bracket_base() {
    for depth in [6.0, THICK] {
        let cut = angle_bracket()
            .difference(&slot_at(32.0, -14.0, depth))
            .difference(&slot_at(32.0, -42.0, depth));
        let mesh = cut.to_mesh(32);
        let area = 2.0 * HALF * 2.0 * R + PI * R * R;
        let expected = (60.0 * THICK + 46.0 * THICK) * 56.0 - 2.0 * area * THICK;
        let vol = cut.volume();
        assert!(
            (vol - expected).abs() < expected * 1e-3,
            "depth {depth}: volume {vol}, expected {expected}"
        );
        assert!(
            mesh.boundary_edges().is_empty(),
            "depth {depth}: mesh is open"
        );
        for y in [-14.0, -42.0] {
            for x in [32.0, 28.0, 36.0] {
                assert!(
                    hits(&mesh, x, y).is_empty(),
                    "depth {depth}: skin across slot at ({x}, {y})"
                );
            }
        }
    }
}

fn hole(y: f64, z: f64, r: f64) -> Solid {
    let p = |a: f64, b: f64| Point2::new(a, b);
    let segments = vec![
        SketchSegment::Arc {
            start: p(r, 0.0),
            end: p(-r, 0.0),
            center: p(0.0, 0.0),
            ccw: true,
        },
        SketchSegment::Arc {
            start: p(-r, 0.0),
            end: p(r, 0.0),
            center: p(0.0, 0.0),
            ccw: true,
        },
    ];
    let profile =
        SketchProfile::new(Point3::new(THICK, y, z), Vec3::y(), Vec3::z(), segments).unwrap();
    Solid::extrude(profile, Vec3::new(-THICK, 0.0, 0.0)).unwrap()
}

/// The whole part, in the feature order the app builds it.
#[test]
fn slots_after_the_motor_holes() {
    let mut part = angle_bracket().difference(&hole(-28.0, 25.0, 11.0));
    for (y, z) in [(-12.5, 40.5), (-43.5, 40.5), (-12.5, 9.5), (-43.5, 9.5)] {
        part = part.difference(&hole(y, z, 1.7));
    }
    let cut = part
        .difference(&slot_at(32.0, -14.0, 6.0))
        .difference(&slot_at(32.0, -42.0, 6.0));
    for segs in [32, 64] {
        let mesh = cut.to_mesh(segs);
        for y in [-14.0, -42.0] {
            for x in [32.0, 28.0, 36.0] {
                assert!(
                    hits(&mesh, x, y).is_empty(),
                    "{segs} segs: skin across slot at ({x}, {y})"
                );
            }
        }
    }
    let base = (60.0 * THICK + 46.0 * THICK) * 56.0;
    let holes = PI * (121.0 + 4.0 * 1.7 * 1.7) * THICK;
    let slots = 2.0 * (2.0 * HALF * 2.0 * R + PI * R * R) * THICK;
    let (vol, expected) = (cut.volume(), base - holes - slots);
    assert!(
        (vol - expected).abs() < expected * 1e-3,
        "volume {vol}, expected {expected}"
    );
}

fn offset_slot(cy: f64) -> Solid {
    let p = |x: f64, y: f64| Point2::new(x, y);
    let segments = vec![
        SketchSegment::Line {
            start: p(HALF, cy + R),
            end: p(-HALF, cy + R),
        },
        SketchSegment::Arc {
            start: p(-HALF, cy + R),
            end: p(-HALF, cy - R),
            center: p(-HALF, cy),
            ccw: true,
        },
        SketchSegment::Line {
            start: p(-HALF, cy - R),
            end: p(HALF, cy - R),
        },
        SketchSegment::Arc {
            start: p(HALF, cy - R),
            end: p(HALF, cy + R),
            center: p(HALF, cy),
            ccw: true,
        },
    ];
    let profile = SketchProfile::new(
        Point3::new(32.0, -28.0, THICK),
        Vec3::x(),
        Vec3::y(),
        segments,
    )
    .unwrap();
    Solid::extrude(profile, Vec3::new(0.0, 0.0, -6.0)).unwrap()
}

/// Tools sketched away from their own centre, as the app sketches them.
#[test]
fn slots_after_a_hole_sketched_off_origin() {
    let p = |a: f64, b: f64| Point2::new(a, b);
    let (c, r) = (p(15.5, -17.5), 1.7);
    let circle = vec![
        SketchSegment::Arc {
            start: p(c.x + r, c.y),
            end: p(c.x - r, c.y),
            center: c,
            ccw: true,
        },
        SketchSegment::Arc {
            start: p(c.x - r, c.y),
            end: p(c.x + r, c.y),
            center: c,
            ccw: true,
        },
    ];
    let profile = SketchProfile::new(
        Point3::new(THICK, -28.0, 27.0),
        Vec3::y(),
        Vec3::z(),
        circle,
    )
    .unwrap();
    let hole = Solid::extrude(profile, Vec3::new(-THICK, 0.0, 0.0)).unwrap();
    let cut = angle_bracket()
        .difference(&hole)
        .difference(&offset_slot(14.0))
        .difference(&offset_slot(-14.0));
    assert_eq!(
        cut.fidelity(),
        SolidFidelity::Analytic,
        "fell back to a mesh"
    );
    let mesh = cut.to_mesh(32);
    for y in [-14.0, -42.0] {
        assert!(
            hits(&mesh, 32.0, y).is_empty(),
            "skin across slot at y={y}: {:?}",
            hits(&mesh, 32.0, y)
        );
    }
}

/// The real part: inner corner radiused in the profile, cuts in app order.
#[test]
fn radiused_bracket_stays_analytic() {
    let p = |x: f64, y: f64| Point2::new(x, y);
    let segments = vec![
        SketchSegment::Line {
            start: p(0.0, 0.0),
            end: p(60.0, 0.0),
        },
        SketchSegment::Line {
            start: p(60.0, 0.0),
            end: p(60.0, 4.0),
        },
        SketchSegment::Line {
            start: p(60.0, 4.0),
            end: p(7.0, 4.0),
        },
        SketchSegment::Arc {
            start: p(7.0, 4.0),
            end: p(4.0, 7.0),
            center: p(7.0, 7.0),
            ccw: false,
        },
        SketchSegment::Line {
            start: p(4.0, 7.0),
            end: p(4.0, 50.0),
        },
        SketchSegment::Line {
            start: p(4.0, 50.0),
            end: p(0.0, 50.0),
        },
        SketchSegment::Line {
            start: p(0.0, 50.0),
            end: p(0.0, 0.0),
        },
    ];
    let profile = SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::z(), segments).unwrap();
    let mut part = Solid::extrude(profile, Vec3::new(0.0, -56.0, 0.0)).unwrap();
    for (i, (y, z, r)) in [
        (-28.0, 25.0, 11.0),
        (-12.5, 40.5, 1.7),
        (-43.5, 40.5, 1.7),
        (-12.5, 9.5, 1.7),
        (-43.5, 9.5, 1.7),
    ]
    .into_iter()
    .enumerate()
    {
        part = part.difference(&hole(y, z, r));
        assert_eq!(
            part.fidelity(),
            SolidFidelity::Analytic,
            "hole {i} fell back to a mesh"
        );
    }
    part = part.difference(&slot_at(32.0, -42.0, 6.0));
    assert_eq!(
        part.fidelity(),
        SolidFidelity::Analytic,
        "first slot fell back to a mesh"
    );
    part = part.difference(&slot_at(32.0, -14.0, 6.0));
    assert_eq!(
        part.fidelity(),
        SolidFidelity::Analytic,
        "second slot fell back to a mesh"
    );
    let mesh = part.to_mesh(32);
    for y in [-14.0, -42.0] {
        assert!(hits(&mesh, 32.0, y).is_empty(), "skin across slot at y={y}");
    }
}
