//! Closed-form volumes for extrusions whose loops are drawn as arcs.
//!
//! A circle authored as two semicircles (the usual way to write one as
//! segments) used to come out a 16-gon in the B-rep whenever the extrusion
//! did not take the analytic full-circle path: with any `Sketch2D.holes`, the
//! *outer* disc of a bolt-hole flange lost 2.55% of its area — and each hole
//! removed 2.55% too little — at every tessellation resolution. It now
//! matches the 32-segment polygonization a `Cylinder` gets (−0.64%).

use std::f64::consts::{PI, TAU};

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::{CsgOp, Document, Node, SceneEntry, SketchSegment2D, Vec2, Vec3};

/// Closed circle as two CCW semicircles.
fn circle_loop(cx: f64, cy: f64, r: f64) -> Vec<SketchSegment2D> {
    let a = Vec2 { x: cx + r, y: cy };
    let b = Vec2 { x: cx - r, y: cy };
    let c = Vec2 { x: cx, y: cy };
    vec![
        SketchSegment2D::Arc {
            start: a,
            end: b,
            center: c,
            ccw: true,
        },
        SketchSegment2D::Arc {
            start: b,
            end: a,
            center: c,
            ccw: true,
        },
    ]
}

fn extrude_doc(
    segments: Vec<SketchSegment2D>,
    holes: Option<Vec<Vec<SketchSegment2D>>>,
    h: f64,
) -> Document {
    let mut doc = Document::new();
    doc.nodes.insert(
        1,
        Node {
            id: 1,
            name: None,
            op: CsgOp::Sketch2D {
                origin: Vec3::new(0.0, 0.0, 0.0),
                x_dir: Vec3::new(1.0, 0.0, 0.0),
                y_dir: Vec3::new(0.0, 1.0, 0.0),
                segments,
                holes,
            },
        },
    );
    doc.nodes.insert(
        2,
        Node {
            id: 2,
            name: None,
            op: CsgOp::Extrude {
                sketch: 1,
                direction: Vec3::new(0.0, 0.0, h),
                twist_angle: None,
                scale_end: None,
            },
        },
    );
    doc.roots.push(SceneEntry {
        root: 2,
        material: "default".into(),
        visible: None,
    });
    doc
}

/// (signed volume, unpaired directed edges) of the evaluated root mesh.
fn eval(doc: &Document) -> (f64, usize) {
    let scene = evaluate_document(
        doc,
        &EvalOptions {
            skip_clash_detection: true,
            ..Default::default()
        },
    )
    .expect("evaluate");
    assert!(scene.failures.is_empty(), "failures: {:?}", scene.failures);
    let m = &scene.parts[0].mesh;
    let (pos, idx) = (&m.positions, &m.indices);
    let p = |i: u32| {
        let k = 3 * i as usize;
        [pos[k] as f64, pos[k + 1] as f64, pos[k + 2] as f64]
    };
    let vol = idx
        .chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum();
    let key = |i: u32| {
        let k = 3 * i as usize;
        [pos[k].to_bits(), pos[k + 1].to_bits(), pos[k + 2].to_bits()]
    };
    let mut net: std::collections::HashMap<_, i64> = std::collections::HashMap::new();
    for t in idx.chunks(3) {
        for e in 0..3 {
            let (a, b) = (key(t[e]), key(t[(e + 1) % 3]));
            if a < b {
                *net.entry((a, b)).or_default() += 1;
            } else if b < a {
                *net.entry((b, a)).or_default() -= 1;
            }
        }
    }
    (vol, net.values().filter(|v| **v != 0).count())
}

/// The 32-segment inscribed polygon a full circle is drawn with keeps
/// sin(2π/32)/(2π/32) = 99.36% of its area. 1% admits that and nothing
/// like the 2.55% a 16-gon loses.
const TOL: f64 = 0.01;

/// A DN160-style blank flange: r = 101.2 disc, 22.2 thick, `n` holes of
/// r = 4.2 on a 181 mm bolt circle, every circle drawn as two semicircles.
fn flange(n: usize) -> (f64, usize) {
    let holes: Vec<_> = (0..n)
        .map(|i| {
            let a = i as f64 * TAU / n as f64;
            circle_loop(90.5 * a.cos(), 90.5 * a.sin(), 4.2)
        })
        .collect();
    eval(&extrude_doc(
        circle_loop(0.0, 0.0, 101.2),
        (n > 0).then_some(holes),
        22.2,
    ))
}

#[test]
fn holed_flange_matches_closed_form() {
    let (r, h, hr) = (101.2, 22.2, 4.2);
    let disc = PI * r * r * h;
    let hole = PI * hr * hr * h;
    let mut vols = Vec::new();
    for n in [0usize, 1, 20] {
        let (vol, open) = flange(n);
        let expect = disc - n as f64 * hole;
        let err = (vol - expect) / expect;
        assert!(
            err.abs() < TOL,
            "{n} holes: volume {vol:.0} vs closed form {expect:.0} ({:+.2}%)",
            100.0 * err
        );
        assert_eq!(open, 0, "{n} holes: mesh has {open} open edges");
        vols.push(vol);
    }
    // A constant error in the outer loop cancels here; this isolates what
    // each hole actually removes. Difference against ONE hole, not none:
    // the hole-free disc goes through a different builder, whose rim is
    // drawn at the tessellation count, while every holed flange's rim is a
    // sampled arc chain drawn on the canonical arc grid — so only the holed
    // flanges share an outer discretization.
    let per_hole = (vols[1] - vols[2]) / 19.0;
    let err = (per_hole - hole) / hole;
    assert!(
        err.abs() < TOL,
        "each hole removes {per_hole:.1} mm³ vs {hole:.1} ({:+.2}%)",
        100.0 * err
    );
}
