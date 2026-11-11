//! Sequential through-cutter `Difference`s on a polygonal prism, the way a
//! board slab or a loon stator is built: one cut at a time, each feeding the
//! next. Every cut must leave a closed mesh at the closed-form volume, whatever
//! the cutter's height.

use std::collections::HashMap;
use std::f64::consts::TAU;

use vcad_kernel_booleans::{boolean_op_reported, BooleanOp, BooleanResult};
use vcad_kernel_math::{Point2, Point3, Transform, Vec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_sketch::{extrude, SketchProfile, SketchSegment};
use vcad_kernel_tessellate::{tessellate_brep, TriangleMesh};

type Loop = Vec<(f64, f64)>;

fn ngon(cx: f64, cy: f64, r: f64, n: usize) -> Loop {
    (0..n)
        .map(|i| {
            let a = TAU * i as f64 / n as f64;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

fn area(l: &Loop) -> f64 {
    (0..l.len())
        .map(|i| {
            let (a, b) = (l[i], l[(i + 1) % l.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

/// `subject` clipped to a convex counter-clockwise `window`.
fn clip(subject: &Loop, window: &Loop) -> Loop {
    let mut out = subject.clone();
    for i in 0..window.len() {
        let (a, b) = (window[i], window[(i + 1) % window.len()]);
        let side = |p: (f64, f64)| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let input = std::mem::take(&mut out);
        for k in 0..input.len() {
            let (p, q) = (input[k], input[(k + 1) % input.len()]);
            let (sp, sq) = (side(p), side(q));
            if sp >= 0.0 {
                out.push(p);
            }
            if sp * sq < 0.0 {
                let u = sp / (sp - sq);
                out.push((p.0 + (q.0 - p.0) * u, p.1 + (q.1 - p.1) * u));
            }
        }
    }
    out
}

fn prism(l: &Loop, z0: f64, h: f64) -> BRepSolid {
    let segments = (0..l.len())
        .map(|i| {
            let (a, b) = (l[i], l[(i + 1) % l.len()]);
            SketchSegment::Line {
                start: Point2::new(a.0, a.1),
                end: Point2::new(b.0, b.1),
            }
        })
        .collect();
    let profile = SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::y(), segments).unwrap();
    let mut solid = extrude(&profile, Vec3::new(0.0, 0.0, h)).unwrap();
    let t = Transform::translation(0.0, 0.0, z0);
    for (_, v) in &mut solid.topology.vertices {
        v.point = t.apply_point(&v.point);
    }
    for s in &mut solid.geometry.surfaces {
        *s = s.transform(&t);
    }
    solid
}

/// Directed edges left unmatched, with vertices compared exactly: a T-junction
/// or a sliver gap counts as open.
fn open_edges(m: &TriangleMesh) -> usize {
    let key = |i: u32| {
        let v = &m.vertices[i as usize * 3..i as usize * 3 + 3];
        [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()]
    };
    let mut net = HashMap::<_, i32>::new();
    for t in m.indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
            if a == b {
                continue;
            }
            let (lo, hi, d) = if a < b { (a, b, 1) } else { (b, a, -1) };
            *net.entry((lo, hi)).or_default() += d;
        }
    }
    net.values().map(|n| n.unsigned_abs() as usize).sum()
}

fn mesh_volume(m: &TriangleMesh) -> f64 {
    m.indices
        .chunks_exact(3)
        .map(|t| {
            let p = |i: u32| {
                let k = i as usize * 3;
                Vec3::new(
                    f64::from(m.vertices[k]),
                    f64::from(m.vertices[k + 1]),
                    f64::from(m.vertices[k + 2]),
                )
            };
            p(t[0]).dot(&p(t[1]).cross(&p(t[2]))) / 6.0
        })
        .sum()
}

#[derive(Debug, PartialEq)]
struct Step {
    open_raw: usize,
    open_export: usize,
    volume_err: f64,
}

/// Cut `holes` one at a time; report each step.
fn run(outer: &Loop, holes: &[Loop], t: f64, below: f64, above: f64) -> Vec<Step> {
    let mut solid = prism(outer, 0.0, t);
    let mut expected = area(outer) * t;
    let mut steps = Vec::new();
    for hole in holes {
        let cutter = prism(hole, -below, t + below + above);
        let (result, _) =
            boolean_op_reported(&solid, &cutter, BooleanOp::Difference, 64).expect("boolean");
        expected -= area(&clip(hole, outer)) * t;
        let raw = tessellate_brep(result.as_brep().unwrap(), 64);
        let export = result.to_mesh(64);
        steps.push(Step {
            open_raw: open_edges(&raw),
            open_export: open_edges(&export),
            volume_err: (mesh_volume(&export) - expected).abs() / expected,
        });
        let BooleanResult::BRep(b) = result;
        solid = *b;
    }
    steps
}

/// The stator-v3 outline: a 64-gon with a 32-gon bore and three 16-gon pockets.
fn stator() -> (Loop, Vec<Loop>) {
    (
        ngon(35.0, 35.0, 35.0, 64),
        vec![
            ngon(35.0, 35.0, 5.0, 32),
            ngon(39.0, 41.9282, 1.7, 16),
            ngon(27.0, 35.0, 1.7, 16),
            ngon(39.0, 28.0718, 1.7, 16),
        ],
    )
}

fn square_plate() -> (Loop, Vec<Loop>) {
    (
        vec![(0.0, 0.0), (100.0, 0.0), (100.0, 60.0), (0.0, 60.0)],
        vec![
            ngon(20.0, 30.0, 8.0, 12),
            ngon(50.0, 30.0, 8.0, 12),
            ngon(80.0, 30.0, 8.0, 12),
            vec![(40.0, 45.0), (60.0, 45.0), (60.0, 55.0), (40.0, 55.0)],
        ],
    )
}

/// Cutters that break through the outer boundary as well as ones inside it.
fn notched_plate() -> (Loop, Vec<Loop>) {
    (
        vec![(0.0, 0.0), (100.0, 0.0), (100.0, 60.0), (0.0, 60.0)],
        vec![
            vec![(10.0, -5.0), (20.0, -5.0), (20.0, 10.0), (10.0, 10.0)],
            ngon(50.0, 0.0, 8.0, 12),
            vec![(70.0, -5.0), (75.0, -5.0), (85.0, 15.0), (70.0, 12.0)],
            ngon(30.0, 30.0, 6.0, 12),
        ],
    )
}

const THICKNESS: f64 = 1.6;
/// (below, above) overshoot of the cutter past the slab's faces, mm: the
/// original board-slab cutter (5% each way), then a spread of arbitrary ones.
fn overshoots() -> Vec<(f64, f64)> {
    let mut out = vec![
        (0.08, 0.08),
        (1.0, 1.0),
        (0.5, 3.0),
        (0.013, 7.1),
        (5.0, 5.0),
    ];
    out.extend((1..=12).map(|i| {
        let i = f64::from(i);
        (
            0.01 + (i * 0.618_034).fract() * 3.0,
            0.01 + (i * 0.414_214).fract() * 3.0,
        )
    }));
    out
}

fn check(name: &str, (outer, holes): (Loop, Vec<Loop>)) {
    let mut failures = Vec::new();
    for (below, above) in overshoots() {
        let steps = run(&outer, &holes, THICKNESS, below, above);
        if steps
            .iter()
            .any(|s| s.open_raw != 0 || s.open_export != 0 || s.volume_err > 1e-6)
        {
            failures.push((below, above));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}: failing overshoots {failures:?}"
    );
}

#[test]
fn stator_outline_sequential_cutouts_stay_closed() {
    check("stator", stator());
}

#[test]
fn square_plate_sequential_cutouts_stay_closed() {
    check("plate", square_plate());
}

#[test]
fn notched_plate_sequential_cutouts_stay_closed() {
    check("notched", notched_plate());
}
