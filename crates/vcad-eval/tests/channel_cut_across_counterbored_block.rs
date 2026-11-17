//! Regression: a full-width cable channel cut across the z=0 face of a
//! counterbored clamp block came out UNCUT on the native (app) path while
//! the evaluation reported success.
//!
//! The part is one cube minus a union of cutters: two Z bores (the 16 mm one
//! wider than the 12 mm channel), two clamp slits, six X pinch-bolt drills
//! with counterbores, a cross pin, two M6 cross holes, and the channel. With
//! the counterbores present the analytic difference kept 127 733 mm³ — the
//! channel (≈3.4 cm³) all but missing, 198 open edges — although the
//! missed-cut probe had already condemned it. It was kept because the mesh
//! fallback (124 383 mm³, the right solid) had 53 open edges and only a
//! perfectly closed fallback was accepted. `inspect_cad` said
//! `failures: []`.
//!
//! That was the safety net. The cause was upstream: the cutter union was
//! open because `split_planar_face_by_multi_arc` (a circle crossing a planar
//! face 4+ times — here the r8 bore through the channel's top and bottom
//! faces) sampled its arcs on the sphere convention's raw grid while the
//! bore wall rides the canonical sag-adaptive grid. The last block below
//! pins that the analytic result is now right without the net.
//!
//! The true volume, 124 317 mm³, comes from grid-integrating the exact
//! implicit CSG (no kernel involved; 0.04 × 0.04 × 0.05 mm cells). The bug
//! is 2.7 % high, so a 0.5 % band separates right from wrong with room for
//! tessellation.
//!
//! One test per file: the loud-path check sets a process-wide env knob.

use vcad_eval::{evaluate_document, EvalOptions, EvaluatedMesh};

const TRUE_VOLUME: f64 = 124_317.0;

const PREAMBLE: &str = r#"
[defparam block_w 44.0 :unit "mm" :min 20 :max 60]
[defparam bore_axle 10.0 :unit "mm" :min 8 :max 12]
[defparam bore_shaft 16.0 :unit "mm" :min 14 :max 18]
[defparam depth_axle 44.0 :unit "mm" :min 15 :max 60]
[defparam depth_shaft 45.0 :unit "mm" :min 20 :max 60]
[defparam deck_offset 22.0 :unit "mm" :min 16 :max 40]
[defparam top_pad 20.0 :unit "mm" :min 13 :max 25]
[defparam bot_pad 22.0 :unit "mm" :min 16 :max 25]
[defparam cable_gap 10.0 :unit "mm" :min 0 :max 20]
[defparam cable_w 12.0 :unit "mm" :min 6 :max 15]
[defparam pin_from_tip 12.0 :unit "mm" :min 6 :max 40]
[defparam shaft_pin_d 6.6 :unit "mm" :min 3 :max 8]
[defparam shaft_pin_a 5.0 :unit "mm" :min 4 :max 40]
[defparam shaft_pin_b 35.0 :unit "mm" :min 6 :max 44]
[defparam slit 2.0 :unit "mm" :min 1 :max 3]

[let half [/ block_w 2]]
[let r10 [/ bore_axle 2]]
[let r16 [/ bore_shaft 2]]
[let total [+ cable_gap depth_shaft]]
[let height [+ bot_pad [+ deck_offset top_pad]]]

[let body [translate [- 0 half] [- 0 bot_pad] 0 [cube block_w height total]]]
[let bore-a [translate 0 deck_offset -1 [cylinder r10 [+ total 2]]]]
[let bore-s [translate 0 0 -1 [cylinder r16 [+ total 2]]]]
[let cut-a [translate [- 0 [/ slit 2]] deck_offset -1 [cube slit [+ top_pad 1] [+ total 2]]]]
[let cut-s [translate [- 0 [/ slit 2]] [- [- 0 bot_pad] 1] -1
             [cube slit [+ bot_pad 1] [+ total 2]]]]

[let xdrill [fn [r l] [rotate 0 90 0 [cylinder r l]]]]
; M5 pinch bolt: clearance + counterbore on +X, tap drill on -X
[let pinch [fn [y z]
  [union [translate 0 y z [xdrill 2.75 [+ half 1]]]
  [union [translate [- [- 0 half] 1] y z [xdrill 2.1 [+ half 1]]]
         [translate [- half 5.5] y z [xdrill 4.5 6.5]]]]]]
[let ya [+ deck_offset [+ r10 6.0]]]
[let ys [- 0 [+ r16 6.5]]]
[let bolts
  [union [pinch ya 6.0]
  [union [pinch ya [/ depth_axle 2]]
  [union [pinch ya [- depth_axle 6.0]]
  [union [pinch ys [+ cable_gap 7.0]]
  [union [pinch ys [+ cable_gap [/ depth_shaft 2]]]
         [pinch ys [- total 7.0]]]]]]]]
[let pin [translate [- [- 0 half] 1] deck_offset [- depth_axle pin_from_tip]
           [xdrill 2.0 [+ block_w 2]]]]
[let spin [fn [d]
  [translate [- [- 0 half] 1] 0 [+ cable_gap d] [xdrill [/ shaft_pin_d 2] [+ block_w 2]]]]]
[let shaft-pin [union [spin shaft_pin_a] [spin shaft_pin_b]]]
[let channel [translate [- [- 0 half] 1] [- 0 [/ cable_w 2]] -1
               [cube [+ block_w 2] cable_w [+ cable_gap 1]]]]
"#;

/// The cutters exactly as the reporting session authored them.
const AUTHORED: &str = r#"
[let cutters
  [union channel [union bore-a [union bore-s [union cut-a [union cut-s
    [union bolts [union pin shaft-pin]]]]]]]]
[root [difference cutters body] "aluminum"]
"#;

/// Same cutters, cut in two stages (everything else first, then the
/// channel). The evaluator folds this into the same single difference, so
/// it failed identically.
const STAGED: &str = r#"
[let pre [difference [union bore-a [union bore-s [union cut-a [union cut-s
  [union bolts [union pin shaft-pin]]]]]] body]]
[root [difference channel pre] "aluminum"]
"#;

fn signed_volume(m: &EvaluatedMesh) -> f64 {
    let p = |i: u32| {
        let i = i as usize * 3;
        [
            m.positions[i] as f64,
            m.positions[i + 1] as f64,
            m.positions[i + 2] as f64,
        ]
    };
    m.indices
        .chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0])
        })
        .sum::<f64>()
        / 6.0
}

fn evaluate(tail: &str) -> (f64, Vec<String>, Vec<String>) {
    let src = format!("{PREAMBLE}{tail}");
    let doc = vcad_loon::eval_vcad(&src, None).expect("loon evaluates");
    let scene = evaluate_document(&doc, &EvalOptions::default()).expect("document evaluates");
    assert_eq!(scene.parts.len(), 1);
    let fmt = |v: &[vcad_eval::RootFailure]| v.iter().map(|f| f.error.clone()).collect();
    (
        signed_volume(&scene.parts[0].mesh).abs(),
        fmt(&scene.failures),
        fmt(&scene.warnings),
    )
}

#[test]
fn channel_across_counterbored_block_is_cut_or_reported() {
    for (name, tail) in [("authored", AUTHORED), ("staged", STAGED)] {
        let (vol, failures, warnings) = evaluate(tail);
        let err = (vol - TRUE_VOLUME).abs() / TRUE_VOLUME;
        assert!(
            failures.is_empty(),
            "{name}: unexpected failures {failures:?}"
        );
        assert!(
            warnings.is_empty(),
            "{name}: the cut should now succeed, got warnings {warnings:?}"
        );
        assert!(
            err < 0.005,
            "{name}: volume {vol:.1} mm³ vs true {TRUE_VOLUME} mm³ ({:.2}% off) — \
             the channel was not cut",
            err * 100.0
        );
    }

    // The root cause is fixed, not just caught: the cutter union used to come
    // out open (152 unpaired edges — the box faces of the channel sampled the
    // r8 bore's rim on the sphere grid while the bore wall used the canonical
    // one), which is what made `body − union` miss the channel. With the
    // missed-cut replacement disabled the ANALYTIC difference must now be the
    // right solid on its own, with nothing to report.
    std::env::set_var("VCAD_NO_MISSED_CUT_SWAP", "1");
    let (vol, failures, warnings) = evaluate(AUTHORED);
    std::env::remove_var("VCAD_NO_MISSED_CUT_SWAP");
    let err = (vol - TRUE_VOLUME).abs() / TRUE_VOLUME;
    assert!(failures.is_empty(), "{failures:?}");
    assert!(
        warnings.is_empty(),
        "analytic path still needs the missed-cut net: {warnings:?}"
    );
    assert!(
        err < 0.001,
        "analytic difference {vol:.1} mm³ vs true {TRUE_VOLUME} mm³ ({:.3}% off)",
        err * 100.0
    );
}
