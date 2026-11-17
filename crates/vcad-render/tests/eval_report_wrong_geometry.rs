//! The render path (`vcad eval-json`, `vcad-render --eval-json`, MCP
//! `render_view`) must report geometry the kernel knows is wrong, the way
//! `vcad_eval::EvaluatedScene::warnings` does — and must never let the mesh
//! cache launder it: a cache hit carries no construction history, so a
//! known-wrong root or part definition that got cached would come back
//! warning-free on the next run.
//!
//! The repro is the counterbored clamp block from
//! `crates/vcad-eval/tests/channel_cut_across_counterbored_block.rs`, with
//! `VCAD_NO_MISSED_CUT_SWAP=1` keeping the condemned (uncut) analytic result.
//! One test per file: it sets that process-wide env knob.

use std::rc::Rc;

use vcad_eval::cache::{MemoryMeshCache, RootMeshCache};
use vcad_render::report::{eval_report, EvalReport};
use vcad_render::{render_svg_str, with_root_cache};

const SEGMENTS: u32 = 32;

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

/// The block as a scene root.
const AS_ROOT: &str = r#"
[let cutters
  [union channel [union bore-a [union bore-s [union cut-a [union cut-s
    [union bolts [union pin shaft-pin]]]]]]]]
[root [difference cutters body] "aluminum"]
"#;

/// The same block as an assembly part definition with one instance.
const AS_INSTANCE: &str = r#"
[let cutters
  [union channel [union bore-a [union bore-s [union cut-a [union cut-s
    [union bolts [union pin shaft-pin]]]]]]]]
[let blk [difference cutters body]]
[assembly
  #[[part "block" blk "aluminum"]]
  #[[instance "b1" "block" 0 0 0]]
  #[]
  "b1"]
"#;

fn doc_json(tail: &str) -> String {
    let doc = vcad_loon::eval_vcad(&format!("{PREAMBLE}{tail}"), None).expect("loon evaluates");
    serde_json::to_string(&doc).expect("serialize")
}

fn report(raw: &str, cache: &Rc<MemoryMeshCache>) -> EvalReport {
    with_root_cache(cache.clone() as Rc<dyn RootMeshCache>, SEGMENTS, || {
        eval_report(raw).expect("eval_report")
    })
}

fn assert_warned(name: &str, run: &str, r: &EvalReport) {
    assert_eq!(r.parts_failed, 0, "{name}/{run}: {r:#?}");
    assert_eq!(r.parts_warned, 1, "{name}/{run}: {r:#?}");
    let part = &r.parts[0];
    assert!(part.ok, "{name}/{run}: a part with geometry stays ok");
    assert!(
        part.warnings.iter().any(|w| w.contains("cut was missed")),
        "{name}/{run}: known-wrong geometry reported clean: {part:#?}"
    );
}

#[test]
fn known_wrong_geometry_is_reported_and_never_cached() {
    std::env::set_var("VCAD_NO_MISSED_CUT_SWAP", "1");
    for (name, tail) in [("root", AS_ROOT), ("instance", AS_INSTANCE)] {
        let raw = doc_json(tail);
        let cache = Rc::new(MemoryMeshCache::new());

        // Cold and warm: the warm run would lose the warning if the cold
        // one had cached the wrong mesh.
        assert_warned(name, "cold", &report(&raw, &cache));
        assert_warned(name, "warm", &report(&raw, &cache));

        // The render path (stand-in instances, placed-instance cache) must
        // not store it either.
        with_root_cache(cache.clone() as Rc<dyn RootMeshCache>, SEGMENTS, || {
            render_svg_str(&raw, 1.0).expect("render")
        });
        assert_eq!(
            cache.stats().stored,
            0,
            "{name}: a known-wrong mesh went into the cache"
        );
    }
    std::env::remove_var("VCAD_NO_MISSED_CUT_SWAP");

    // Control: with the missed-cut replacement on, the same documents build
    // clean and DO get cached (so the zero above is the gate, not a cache
    // that never stores).
    for (name, tail) in [("root", AS_ROOT), ("instance", AS_INSTANCE)] {
        let raw = doc_json(tail);
        let cache = Rc::new(MemoryMeshCache::new());
        let r = report(&raw, &cache);
        assert_eq!(r.parts_warned, 0, "{name}: control warned: {r:#?}");
        assert!(r.parts[0].warnings.is_empty());
        assert!(cache.stats().stored > 0, "{name}: control stored nothing");
    }
}
