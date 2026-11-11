//! Shell with open faces: hollow a solid and leave the picked faces as the
//! opening — the open-top box every enclosure starts as.

use vcad_kernel::{FaceRef, Solid};
use vcad_kernel_fillet::{BlendKey, BlendSection, EdgeQuery};
use vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel_sketch::{SketchProfile, SketchSegment};

const SEGMENTS: u32 = 64;

fn hint(p: [f64; 3], n: [f64; 3]) -> FaceRef {
    FaceRef {
        name: None,
        point: Some(Point3::new(p[0], p[1], p[2])),
        normal: Some(Vec3::new(n[0], n[1], n[2])),
    }
}

fn top() -> FaceRef {
    hint([45.0, 35.0, 25.0], [0.0, 0.0, 1.0])
}

fn open_edges(s: &Solid) -> usize {
    s.to_mesh(SEGMENTS).boundary_edges().len()
}

/// True when some triangle covers the XY point `(x, y)` above `z_min` —
/// i.e. a ray straight down at `(x, y)` would be stopped before `z_min`.
fn covered_above(s: &Solid, x: f64, y: f64, z_min: f64) -> bool {
    let m = s.to_mesh(SEGMENTS);
    let v = |i: u32| {
        let i = i as usize * 3;
        [
            m.vertices[i] as f64,
            m.vertices[i + 1] as f64,
            m.vertices[i + 2] as f64,
        ]
    };
    m.indices.chunks_exact(3).any(|t| {
        let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
        if a[2].min(b[2]).min(c[2]) <= z_min {
            return false;
        }
        let s = |p: [f64; 3], q: [f64; 3]| (q[0] - p[0]) * (y - p[1]) - (q[1] - p[1]) * (x - p[0]);
        let (d1, d2, d3) = (s(a, b), s(b, c), s(c, a));
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos)
    })
}

/// A `w × h × z` box whose four vertical edges are rounded to `r`, as an
/// extruded rounded rectangle (planes + analytic cylinders), placed at `at`.
fn rounded_box(w: f64, h: f64, z: f64, r: f64, at: [f64; 3]) -> Solid {
    let p = |x: f64, y: f64| Point2::new(x, y);
    let arc = |s: Point2, e: Point2, c: Point2| SketchSegment::Arc {
        start: s,
        end: e,
        center: c,
        ccw: true,
    };
    let line = |s: Point2, e: Point2| SketchSegment::Line { start: s, end: e };
    let segs = vec![
        line(p(r, 0.0), p(w - r, 0.0)),
        arc(p(w - r, 0.0), p(w, r), p(w - r, r)),
        line(p(w, r), p(w, h - r)),
        arc(p(w, h - r), p(w - r, h), p(w - r, h - r)),
        line(p(w - r, h), p(r, h)),
        arc(p(r, h), p(0.0, h - r), p(r, h - r)),
        line(p(0.0, h - r), p(0.0, r)),
        arc(p(0.0, r), p(r, 0.0), p(r, r)),
    ];
    let origin = Point3::new(at[0], at[1], at[2]);
    let prof = SketchProfile::new(
        origin,
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        segs,
    )
    .expect("rounded rectangle");
    Solid::extrude(prof, Vec3::new(0.0, 0.0, z)).expect("extrude")
}

/// A box with its four vertical edges filleted to `r` — the app's path
/// (box, then a fillet on the picked vertical edges).
fn filleted_box(sx: f64, sy: f64, sz: f64, r: f64) -> Solid {
    let keys = [0.0, 1.0].map(|t| BlendKey {
        t,
        section: BlendSection {
            size: r,
            shape: 1.0,
        },
    });
    Solid::cube(sx, sy, sz)
        .edge_blend(
            &EdgeQuery::Direction {
                axis: Vec3::new(0.0, 0.0, 1.0),
                tol_deg: 1.0,
            },
            &keys,
        )
        .expect("vertical-edge fillet")
}

#[test]
fn open_top_box_has_exact_volume_and_an_opening() {
    let shelled = Solid::cube(90.0, 70.0, 25.0)
        .shell_open_refs(2.0, &[top()])
        .expect("open-top shell");
    let expected = 90.0 * 70.0 * 25.0 - 86.0 * 66.0 * 23.0;
    assert!(
        (shelled.volume() - expected).abs() < 1e-6 * expected,
        "volume {} vs {expected}",
        shelled.volume()
    );
    assert_eq!(open_edges(&shelled), 0, "open-top shell is not watertight");
    // The opening is there: nothing covers the middle above the 2 mm floor…
    assert!(
        !covered_above(&shelled, 45.0, 35.0, 2.5),
        "top face still covers the cavity"
    );
    // …but the rim on top of the walls is.
    assert!(covered_above(&shelled, 1.0, 35.0, 24.0), "rim missing");
}

#[test]
fn named_face_reference_resolves() {
    let cube = Solid::cube(90.0, 70.0, 25.0);
    let names = cube.names().expect("cube carries face names");
    let top_name = names
        .faces
        .values()
        .find(|n| n.tag == "top")
        .expect("a top face")
        .to_string();
    let by_name = cube
        .shell_open_refs(
            2.0,
            &[FaceRef {
                name: Some(top_name),
                ..Default::default()
            }],
        )
        .expect("named open face");
    let by_hint = cube.shell_open_refs(2.0, &[top()]).unwrap();
    assert!((by_name.volume() - by_hint.volume()).abs() < 1e-9);
    // A lost name with no hint fails closed.
    let lost = cube.shell_open_refs(
        2.0,
        &[FaceRef {
            name: Some("cube:nope".into()),
            ..Default::default()
        }],
    );
    assert!(lost.is_err());
}

#[test]
fn open_top_box_with_rounded_vertical_edges_extruded() {
    // Analytic: planes + cylinders.
    let outer = rounded_box(90.0, 70.0, 25.0, 3.0, [0.0; 3]);
    let shelled = outer
        .shell_open_refs(2.0, &[top()])
        .expect("rounded open-top shell");
    let cavity = rounded_box(86.0, 66.0, 23.0, 1.0, [2.0, 2.0, 2.0]);
    let expected = outer.volume() - cavity.volume();
    assert!(
        (shelled.volume() - expected).abs() < 1e-3 * expected,
        "volume {} vs {expected}",
        shelled.volume()
    );
    assert_eq!(
        open_edges(&shelled),
        0,
        "rounded open-top shell is not watertight"
    );
    assert!(!covered_above(&shelled, 45.0, 35.0, 2.5));
}

#[test]
fn open_top_box_with_filleted_vertical_edges() {
    let outer = filleted_box(90.0, 70.0, 25.0, 3.0);
    let shelled = outer
        .shell_open_refs(2.0, &[top()])
        .expect("filleted open-top shell");
    let expected = outer.volume() - filleted_box(86.0, 66.0, 23.0, 1.0).volume();
    assert!(
        (shelled.volume() - expected).abs() < 1e-3 * expected,
        "volume {} vs {expected}",
        shelled.volume()
    );
    assert_eq!(
        open_edges(&shelled),
        0,
        "filleted open-top shell is not watertight"
    );
    assert!(!covered_above(&shelled, 45.0, 35.0, 2.5));
}

#[test]
fn two_open_faces_make_a_tube() {
    let bottom = hint([45.0, 35.0, 0.0], [0.0, 0.0, -1.0]);
    let shelled = Solid::cube(90.0, 70.0, 25.0)
        .shell_open_refs(2.0, &[top(), bottom])
        .expect("open top and bottom");
    let expected = 25.0 * (90.0 * 70.0 - 86.0 * 66.0);
    assert!(
        (shelled.volume() - expected).abs() < 1e-6 * expected,
        "volume {} vs {expected}",
        shelled.volume()
    );
    assert_eq!(open_edges(&shelled), 0);
    assert!(
        !covered_above(&shelled, 45.0, 35.0, -1.0),
        "tube is not open through"
    );
}

#[test]
fn open_top_cylinder_cup() {
    let outer = Solid::cylinder(10.0, 20.0, SEGMENTS);
    let shelled = outer
        .shell_open_refs(2.0, &[hint([0.0, 0.0, 20.0], [0.0, 0.0, 1.0])])
        .expect("cup");
    let cavity = Solid::cylinder(8.0, 18.0, SEGMENTS);
    let expected = outer.volume() - cavity.volume();
    assert!(
        (shelled.volume() - expected).abs() < 5e-3 * expected,
        "volume {} vs {expected}",
        shelled.volume()
    );
    assert_eq!(open_edges(&shelled), 0, "cup is not watertight");
    assert!(!covered_above(&shelled, 0.0, 0.0, 2.5), "cup is closed");
}

#[test]
fn thickness_that_consumes_the_solid_is_refused() {
    let cube = Solid::cube(90.0, 70.0, 25.0);
    // Open top: the floor would rise to z = 30, past the 25 mm top.
    let err = cube.shell_open_refs(30.0, &[top()]).unwrap_err();
    assert!(err.to_string().contains("consumes"), "{err}");
    // Through the 70 mm width.
    assert!(cube.shell_open_refs(40.0, &[top()]).is_err());
    for bad in [0.0, -1.0, f64::NAN] {
        assert!(cube.shell_open_refs(bad, &[top()]).is_err(), "{bad}");
    }
}

#[test]
fn unsupported_requests_fail_closed() {
    let cube = Solid::cube(90.0, 70.0, 25.0);
    // Adjacent open faces.
    let side = hint([90.0, 35.0, 12.5], [1.0, 0.0, 0.0]);
    assert!(cube.shell_open_refs(2.0, &[top(), side]).is_err());
    // A curved face cannot be opened.
    let cyl = Solid::cylinder(10.0, 20.0, SEGMENTS);
    assert!(cyl
        .shell_open_refs(2.0, &[hint([10.0, 0.0, 10.0], [1.0, 0.0, 0.0])])
        .is_err());
    // A point nowhere near the solid.
    assert!(cube
        .shell_open_refs(2.0, &[hint([1000.0, 0.0, 0.0], [0.0, 0.0, 1.0])])
        .is_err());
}

#[test]
fn no_open_faces_is_the_closed_shell() {
    let cube = Solid::cube(40.0, 40.0, 40.0);
    let a = cube.shell(2.0).unwrap();
    let b = cube.shell_open_refs(2.0, &[]).unwrap();
    assert_eq!(a.volume(), b.volume());
}

#[test]
fn holes_can_be_cut_through_an_open_shell_wall() {
    let shelled = Solid::cube(90.0, 70.0, 25.0)
        .shell_open_refs(2.0, &[top()])
        .unwrap();
    // A 10×10 window through the 2 mm front wall (y ∈ [0, 2]).
    let window = Solid::cube(10.0, 6.0, 10.0).translate(40.0, -2.0, 8.0);
    let cut = shelled.difference(&window);
    let expected = shelled.volume() - 10.0 * 2.0 * 10.0;
    assert!(
        (cut.volume() - expected).abs() < 1e-3 * expected,
        "volume {} vs {expected}",
        cut.volume()
    );
}
