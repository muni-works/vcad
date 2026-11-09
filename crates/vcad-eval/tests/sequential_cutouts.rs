//! Loon-authored stator: a 64-gon slab with a bore and three pockets cut one
//! `[difference]` at a time. Every intermediate must evaluate to a closed mesh
//! at the closed-form volume, for each cutter type and cutter height.

use std::collections::HashMap;
use std::f64::consts::TAU;

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_loon::eval_vcad;

/// (circumradius, x, y) of the bore and the three pockets on stator-v3.
const CUTS: [(f64, f64, f64, usize); 4] = [
    (5.0, 35.0, 35.0, 32),
    (1.7, 39.0, 41.9282, 16),
    (1.7, 27.0, 35.0, 16),
    (1.7, 39.0, 28.0718, 16),
];
const R_OUTER: f64 = 35.0;
const THICKNESS: f64 = 1.6;
/// Cutter (start z, height), including the old board-slab cutter.
const HEIGHTS: [(f64, f64); 5] = [
    (-0.08, 1.76),
    (-1.0, 3.6),
    (-0.5, 5.1),
    (-5.0, 11.6),
    (-0.37, 2.181),
];

fn ngon_area(r: f64, n: usize) -> f64 {
    0.5 * n as f64 * r * r * (TAU / n as f64).sin()
}

/// One loon cutter, centred at (x, y), spanning z0..z0+h.
fn cutter(kind: &str, r: f64, x: f64, y: f64, n: usize, z0: f64, h: f64) -> String {
    match kind {
        "prism" => format!("[translate {x:?} {y:?} {z0:?} [prism {n} {r:?} {h:?}]]"),
        // Counts of 8 and up are still true cylinders, not facets.
        "cylinder-n" => format!("[translate {x:?} {y:?} {z0:?} [cylinder-n {r:?} {h:?} {n}]]"),
        _ => format!("[translate {x:?} {y:?} {z0:?} [cylinder {r:?} {h:?}]]"),
    }
}

fn open_edges(positions: &[f32], indices: &[u32]) -> usize {
    let key = |i: u32| {
        let v = &positions[i as usize * 3..i as usize * 3 + 3];
        [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()]
    };
    let mut net = HashMap::<_, i32>::new();
    for t in indices.chunks_exact(3) {
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

/// `(open edges, volume, relative volume error)` of each cut in turn.
fn steps(kind: &str, z0: f64, h: f64) -> Vec<(usize, f64, f64)> {
    let mut body =
        format!("[translate {R_OUTER:?} {R_OUTER:?} 0.0 [prism 64 {R_OUTER:?} {THICKNESS:?}]]");
    let mut expected = ngon_area(R_OUTER, 64) * THICKNESS;
    let mut out = Vec::new();
    for (r, x, y, n) in CUTS {
        body = format!("[difference {} {body}]", cutter(kind, r, x, y, n, z0, h));
        expected -= if kind != "prism" {
            std::f64::consts::PI * r * r * THICKNESS
        } else {
            ngon_area(r, n) * THICKNESS
        };
        let doc = eval_vcad(&format!("[root {body} \"aluminum\"]"), None).expect("loon eval");
        let scene = evaluate_document(&doc, &EvalOptions::default()).expect("evaluates");
        let part = &scene.parts[0];
        let volume = part.solid.as_ref().expect("solid").volume().abs();
        out.push((
            open_edges(&part.mesh.positions, &part.mesh.indices),
            volume,
            (volume - expected).abs() / expected,
        ));
    }
    out
}

fn check(kind: &str, tolerance: f64) {
    let mut failures = Vec::new();
    for (z0, h) in HEIGHTS {
        let result = steps(kind, z0, h);
        if result.iter().any(|&(o, _, e)| o != 0 || e > tolerance) {
            failures.push((z0, h, result));
        }
    }
    assert!(failures.is_empty(), "{kind}: {failures:?}");
}

#[test]
fn prism_cutters_leave_a_closed_stator() {
    check("prism", 1e-6);
}

#[test]
fn faceted_cylinder_cutters_leave_a_closed_stator() {
    check("cylinder-n", 1e-4);
}

#[test]
fn true_cylinder_cutters_leave_a_closed_stator() {
    check("cylinder", 1e-4);
}
