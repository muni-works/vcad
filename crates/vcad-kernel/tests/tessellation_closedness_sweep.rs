//! Closedness sweep: every modelling primitive, tessellated raw (no export
//! repair) at every segment count, must come out closed.
//!
//! Adjacent faces share each B-rep edge, so they must share its samples:
//! a face that resamples an edge its neighbour renders verbatim opens a
//! crack along it. The arc-extruded D was exactly that — its curved wall
//! resampled the extrude's 8-point arc chain at 64+ segments while the
//! planar caps kept the chain, 144 open edges at 128 segments — and K1
//! wrist_adapter's mesh fallback was handed that open operand.
//!
//! Each case prints its open-edge count and volume per segment count, so a
//! failure reads as a matrix rather than a single assertion.

use std::collections::HashMap;

use vcad_kernel::vcad_kernel_geom::{Circle3d, Line3d};
use vcad_kernel::vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel::vcad_kernel_sketch::{SketchProfile, SketchSegment};
use vcad_kernel::vcad_kernel_sweep::{LoftOptions, SweepOptions};
use vcad_kernel::vcad_kernel_tessellate::{tessellate_brep, TriangleMesh};
use vcad_kernel::Solid;

const SEGMENTS: [u32; 6] = [8, 16, 32, 64, 128, 256];

/// Directed edges (positions quantized to 1e-4 mm) with no reverse partner.
fn open_edges(m: &TriangleMesh) -> usize {
    let key = |i: u32| {
        let k = i as usize * 3;
        [0, 1, 2].map(|j| (m.vertices[k + j] as f64 * 1e4).round() as i64)
    };
    let mut e: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
    for t in m.indices.chunks(3) {
        for k in 0..3 {
            let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
            if a == b {
                continue;
            }
            *e.entry((a, b)).or_default() += 1;
        }
    }
    e.iter()
        .filter(|((a, b), n)| e.get(&(*b, *a)) != Some(*n))
        .count()
}

fn volume(m: &TriangleMesh) -> f64 {
    let p = |i: u32| {
        let k = i as usize * 3;
        [0, 1, 2].map(|j| m.vertices[k + j] as f64)
    };
    m.indices
        .chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

fn line(a: (f64, f64), b: (f64, f64)) -> SketchSegment {
    SketchSegment::Line {
        start: Point2::new(a.0, a.1),
        end: Point2::new(b.0, b.1),
    }
}

fn arc(a: (f64, f64), b: (f64, f64), c: (f64, f64), ccw: bool) -> SketchSegment {
    SketchSegment::Arc {
        start: Point2::new(a.0, a.1),
        end: Point2::new(b.0, b.1),
        center: Point2::new(c.0, c.1),
        ccw,
    }
}

fn xy_profile(segments: Vec<SketchSegment>) -> SketchProfile {
    SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::y(), segments).expect("profile")
}

/// D: a semicircle closed by its diameter (the K1 wrist half).
fn d_profile() -> SketchProfile {
    xy_profile(vec![
        line((0.0, -13.7), (0.0, 13.7)),
        arc((0.0, 13.7), (0.0, -13.7), (0.0, 0.0), true),
    ])
}

/// Rounded rectangle: four quarter arcs joined by lines.
fn slot_profile() -> SketchProfile {
    let (w, h, r) = (20.0, 10.0, 3.0);
    xy_profile(vec![
        line((r, 0.0), (w - r, 0.0)),
        arc((w - r, 0.0), (w, r), (w - r, r), true),
        line((w, r), (w, h - r)),
        arc((w, h - r), (w - r, h), (w - r, h - r), true),
        line((w - r, h), (r, h)),
        arc((r, h), (0.0, h - r), (r, h - r), true),
        line((0.0, h - r), (0.0, r)),
        arc((0.0, r), (r, 0.0), (r, r), true),
    ])
}

/// Profile in the XZ half-plane x > 0, for revolving about Z (revolve takes
/// lines only): annular caps, two cylinders and a cone frustum.
fn revolve_profile() -> SketchProfile {
    SketchProfile::new(
        Point3::origin(),
        Vec3::x(),
        Vec3::z(),
        vec![
            line((4.0, 0.0), (8.0, 0.0)),
            line((8.0, 0.0), (8.0, 3.0)),
            line((8.0, 3.0), (6.0, 6.0)),
            line((6.0, 6.0), (4.0, 6.0)),
            line((4.0, 6.0), (4.0, 0.0)),
        ],
    )
    .expect("revolve profile")
}

fn square_profile(z: f64, s: f64) -> SketchProfile {
    SketchProfile::new(
        Point3::new(0.0, 0.0, z),
        Vec3::x(),
        Vec3::y(),
        vec![
            line((-s, -s), (s, -s)),
            line((s, -s), (s, s)),
            line((s, s), (-s, s)),
            line((-s, s), (-s, -s)),
        ],
    )
    .expect("square")
}

fn cases() -> Vec<(&'static str, Solid)> {
    let mut v: Vec<(&'static str, Solid)> = vec![
        ("cube", Solid::cube(10.0, 7.0, 5.0)),
        ("cylinder", Solid::cylinder(5.0, 10.0, 32)),
        ("sphere", Solid::sphere(5.0, 32)),
        ("cone_frustum", Solid::cone(5.0, 2.0, 8.0, 32)),
        ("cone_apex", Solid::cone(5.0, 0.0, 8.0, 32)),
        ("torus", Solid::torus(10.0, 3.0, 32)),
        (
            "extrude_d",
            Solid::extrude(d_profile(), Vec3::new(0.0, 0.0, 31.7)).expect("extrude D"),
        ),
        (
            "extrude_slot",
            Solid::extrude(slot_profile(), Vec3::new(0.0, 0.0, 6.0)).expect("extrude slot"),
        ),
        (
            "revolve_360",
            Solid::revolve(revolve_profile(), Point3::origin(), Vec3::z(), 360.0)
                .expect("revolve 360"),
        ),
        (
            "revolve_120",
            Solid::revolve(revolve_profile(), Point3::origin(), Vec3::z(), 120.0)
                .expect("revolve 120"),
        ),
    ];
    if let Ok(s) = Solid::sweep(
        slot_profile(),
        &Line3d::from_points(Point3::origin(), Point3::new(0.0, 0.0, 15.0)),
        SweepOptions::default(),
    ) {
        v.push(("sweep_line_slot", s));
    }
    if let Ok(s) = Solid::sweep(
        square_profile(0.0, 1.0),
        &Circle3d::new(Point3::origin(), 10.0),
        SweepOptions::default(),
    ) {
        v.push(("sweep_circle_square", s));
    }
    if let Ok(s) = Solid::loft(
        &[square_profile(0.0, 4.0), square_profile(10.0, 2.0)],
        LoftOptions::default(),
    ) {
        v.push(("loft_squares", s));
    }
    v
}

#[test]
fn every_primitive_tessellates_closed_at_every_segment_count() {
    let mut failures = Vec::new();
    for (name, solid) in cases() {
        let Some(brep) = solid.as_brep() else {
            failures.push(format!("{name}: not a B-rep"));
            continue;
        };
        let mut row = format!("{name:>20}:");
        for segments in SEGMENTS {
            let mesh = tessellate_brep(brep, segments);
            let (open, vol) = (open_edges(&mesh), volume(&mesh));
            row += &format!(
                "  {segments}: open {open:>3} tris {:>5} vol {vol:>9.2}",
                mesh.indices.len() / 3
            );
            if open != 0 || vol <= 0.0 {
                failures.push(format!("{name} @ {segments}: open {open}, vol {vol:.2}"));
            }
        }
        eprintln!("{row}");
    }
    assert!(failures.is_empty(), "not closed:\n{}", failures.join("\n"));
}
