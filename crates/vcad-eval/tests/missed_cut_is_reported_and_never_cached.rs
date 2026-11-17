//! A difference the kernel condemns as a missed cut must reach
//! `EvaluatedScene::warnings` and must never be stored in the root cache
//! (a cache hit carries no construction history, so it would come back
//! warning-free). The verdict is forced with `VCAD_FORCE_MISSED_CUT=1`: a
//! real missed-cut fixture goes stale as soon as its kernel bug is fixed,
//! as the #1220 counterbored block did once #1230 landed.
//! One test per file: it sets that process-wide env knob.

use std::rc::Rc;

use vcad_eval::cache::{MemoryMeshCache, RootMeshCache};
use vcad_eval::{evaluate_document, EvalOptions, EvaluatedScene};

const SRC: &str = r#"
[let plate [cube 40 30 10]]
[let hole [translate 20 15 -1 [cylinder 5 12]]]
[root [difference hole plate] "aluminum"]
"#;

fn evaluate(cache: &Rc<MemoryMeshCache>) -> EvaluatedScene {
    let doc = vcad_loon::eval_vcad(SRC, None).expect("loon evaluates");
    let opts = EvalOptions {
        root_cache: Some(cache.clone() as Rc<dyn RootMeshCache>),
        ..EvalOptions::default()
    };
    evaluate_document(&doc, &opts).expect("document evaluates")
}

#[test]
fn missed_cut_is_reported_and_never_cached() {
    std::env::set_var("VCAD_FORCE_MISSED_CUT", "1");
    let cache = Rc::new(MemoryMeshCache::new());
    for run in ["cold", "warm"] {
        let scene = evaluate(&cache);
        assert!(scene.failures.is_empty(), "{run}: {:?}", scene.failures);
        assert_eq!(scene.parts.len(), 1, "{run}: the part keeps its geometry");
        assert!(
            scene
                .warnings
                .iter()
                .any(|w| w.error.contains("cut was missed")),
            "{run}: known-wrong geometry reported clean: {:?}",
            scene.warnings
        );
    }
    assert_eq!(cache.stats().stored, 0, "a known-wrong mesh was cached");
    std::env::remove_var("VCAD_FORCE_MISSED_CUT");

    // Control: the same part without the forced verdict is clean and cached,
    // so the zero above is the gate rather than a cache that never stores.
    let cache = Rc::new(MemoryMeshCache::new());
    let scene = evaluate(&cache);
    assert!(
        scene.warnings.is_empty(),
        "control warned: {:?}",
        scene.warnings
    );
    assert!(cache.stats().stored > 0, "control stored nothing");
}
