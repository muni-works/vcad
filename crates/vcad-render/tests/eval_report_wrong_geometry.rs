//! The render path (`vcad eval-json`, `vcad-render --eval-json`, MCP
//! `render_view`) must report geometry the kernel knows is wrong, the way
//! `vcad_eval::EvaluatedScene::warnings` does — and must never let the mesh
//! cache launder it: a cache hit carries no construction history, so a
//! known-wrong root or part definition that got cached would come back
//! warning-free on the next run.
//!
//! The wrong geometry is forced with `VCAD_FORCE_MISSED_CUT=1`, which makes
//! the boolean pipeline condemn every difference as a missed cut. A real
//! missed-cut fixture goes stale the moment its kernel bug is fixed: the
//! counterbored clamp block this test first used (#1220) stopped producing
//! one once #1230 closed its cutter union, and the test went red on main.
//! One test per file: it sets that process-wide env knob.

use std::rc::Rc;

use vcad_eval::cache::{MemoryMeshCache, RootMeshCache};
use vcad_render::report::{eval_report, EvalReport};
use vcad_render::{render_svg_str, with_root_cache};

const SEGMENTS: u32 = 32;

/// A plate with a through hole: one cheap, real difference.
const PREAMBLE: &str = r#"
[let plate [cube 40 30 10]]
[let hole [translate 20 15 -1 [cylinder 5 12]]]
"#;

/// The plate as a scene root.
const AS_ROOT: &str = r#"
[root [difference hole plate] "aluminum"]
"#;

/// The same plate as an assembly part definition with one instance.
const AS_INSTANCE: &str = r#"
[let blk [difference hole plate]]
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
    std::env::set_var("VCAD_FORCE_MISSED_CUT", "1");
    for (name, tail) in [("root", AS_ROOT), ("instance", AS_INSTANCE)] {
        let raw = doc_json(tail);
        let cache = Rc::new(MemoryMeshCache::new());

        // Cold and warm: the warm run would lose the warning if the cold
        // one had cached the wrong mesh.
        assert_warned(name, "cold", &report(&raw, &cache));
        assert_warned(name, "warm", &report(&raw, &cache));

        // The render path's own store (placed instances, stand-ins) must
        // not take it either. Roots have no render-side store.
        if name == "instance" {
            with_root_cache(cache.clone() as Rc<dyn RootMeshCache>, SEGMENTS, || {
                render_svg_str(&raw, 1.0).expect("render")
            });
        }
        assert_eq!(
            cache.stats().stored,
            0,
            "{name}: a known-wrong mesh went into the cache"
        );
    }
    std::env::remove_var("VCAD_FORCE_MISSED_CUT");

    // Control: without the forced verdict, the same part builds
    // clean and DOES get cached, so the zeros above are the gate rather
    // than a cache that never stores.
    let cache = Rc::new(MemoryMeshCache::new());
    let r = report(&doc_json(AS_INSTANCE), &cache);
    assert_eq!(r.parts_warned, 0, "control warned: {r:#?}");
    assert!(cache.stats().stored > 0, "control stored nothing");
}
