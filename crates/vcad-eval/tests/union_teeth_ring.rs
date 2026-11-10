//! A ring with flat-walled teeth unioned into its curved bore: the export mesh
//! must be closed, at the closed-form volume, wherever the part sits.

use std::collections::HashMap;
use std::f64::consts::PI;

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_loon::eval_vcad;

const R_BORE: f64 = 30.0;
const R_OUTER: f64 = 40.0;
const TOOTH_START: f64 = 18.0;
const HALF_WIDTH: f64 = 3.0;
const HEIGHT: f64 = 20.0;

/// One tooth's area inside the bore: `x` from `TOOTH_START` to the circle.
fn tooth_area() -> f64 {
    let (r, w) = (R_BORE, HALF_WIDTH);
    w * (r * r - w * w).sqrt() + r * r * (w / r).asin() - 2.0 * w * TOOTH_START
}

fn expected_volume(teeth: usize) -> f64 {
    (PI * (R_OUTER * R_OUTER - R_BORE * R_BORE) + teeth as f64 * tooth_area()) * HEIGHT
}

/// Edges not used equally in each direction, welding vertices to `step` mm.
fn open_edges(positions: &[f32], indices: &[u32], step: f64) -> usize {
    let key = |i: u32| {
        let v = &positions[i as usize * 3..i as usize * 3 + 3];
        v.iter()
            .map(|c| (f64::from(*c) / step).round() as i64)
            .collect::<Vec<_>>()
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

fn program(teeth: usize, x: f64, y: f64) -> String {
    format!(
        "[root [translate {x:?} {y:?} 0.0 [union \
           [difference [cylinder {R_BORE:?} {HEIGHT:?}] [cylinder {R_OUTER:?} {HEIGHT:?}]] \
           [circular-pattern 0 0 0 0 0 1 {teeth} 360 \
             [translate {TOOTH_START:?} {:?} 0.0 [cube 14.0 {:?} {HEIGHT:?}]]]]] \"aluminum\"]",
        -HALF_WIDTH,
        2.0 * HALF_WIDTH
    )
}

/// `(worst open edges over the weld steps, volume, relative volume error)`.
fn measure(teeth: usize, x: f64, y: f64) -> (usize, usize, f64, f64) {
    let doc = eval_vcad(&program(teeth, x, y), None).expect("loon eval");
    let scene = evaluate_document(&doc, &EvalOptions::default()).expect("evaluates");
    let part = &scene.parts[0];
    let open = [1e-3, 1e-4, 1e-5]
        .iter()
        .map(|s| open_edges(&part.mesh.positions, &part.mesh.indices, *s))
        .max()
        .unwrap_or(0);
    let volume = part.solid.as_ref().expect("solid").volume().abs();
    let expected = expected_volume(teeth);
    (
        open,
        part.mesh.indices.len() / 3,
        volume,
        (volume - expected).abs() / expected,
    )
}

fn check(cases: &[(usize, f64, f64)]) {
    let results: Vec<_> = cases
        .iter()
        .map(|&(n, x, y)| ((n, x, y), measure(n, x, y)))
        .collect();
    eprintln!("{results:#?}");
    let bad: Vec<_> = results
        .iter()
        .filter(|(_, (open, _, _, err))| *open != 0 || *err > 2e-3)
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn twelve_teeth_translated_is_closed() {
    check(&[(12, 140.0, 30.0)]);
}

#[test]
fn twelve_teeth_at_origin_is_closed() {
    check(&[(12, 0.0, 0.0)]);
}

#[test]
fn other_tooth_counts_are_closed() {
    check(&[
        (3, 140.0, 30.0),
        (5, 140.0, 30.0),
        (6, 0.0, 0.0),
        (7, 0.0, 0.0),
        (8, 140.0, 30.0),
        (9, 140.0, 30.0),
        (11, 0.0, 0.0),
        (16, 140.0, 30.0),
    ]);
}
