//! Document fuzzing: random — and deliberately malformed — command
//! sequences through `DocumentApi` and `vcad_eval`, checked against:
//!
//! - no panic (every call runs under `catch_unwind`) and no hang (each
//!   evaluation runs on a worker thread with a deadline);
//! - every envelope is valid JSON with a boolean `ok`;
//! - undoing every step returns to the initial document, redoing every step
//!   returns to the final one;
//! - `save` → `load` reproduces the document exactly;
//! - evaluated meshes are finite, index in range, and a part with a BRep
//!   solid has non-negative volume.
//!
//! Seeded and reproducible; `VCAD_FUZZ_CASES` raises the case count.
//! Findings live in `docs/qa/fuzz-2026-11-12.md`.

use serde_json::{json, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::time::Duration;
use vcad_app::DocumentApi;
use vcad_crdt::ReplicaId;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// Mostly sane numbers, sometimes hostile ones.
    fn num(&mut self) -> Value {
        match self.below(12) {
            0 => json!(0.0),
            1 => json!(-1.0),
            2 => json!(1e300),
            3 => json!(-1e-300),
            4 => json!(1e-9),
            5 => json!("12"),
            6 => Value::Null,
            _ => json!((self.below(20_000) as f64 / 1000.0) - 2.0),
        }
    }
}

fn cases() -> u64 {
    std::env::var("VCAD_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
}

fn ids(api: &DocumentApi) -> Vec<String> {
    let mut v: Vec<String> = api
        .get_state()
        .parts
        .iter()
        .map(|p| p.id().to_string())
        .collect();
    v.extend(["999:999".into(), "".into(), "0:0".into(), "x".into()]);
    v
}

fn v3(rng: &mut Rng) -> Value {
    json!({"x": rng.num(), "y": rng.num(), "z": rng.num()})
}

/// `(is_tool, name_or_command, args)`.
fn step(rng: &mut Rng, api: &DocumentApi) -> (bool, String, String) {
    let ids = ids(api);
    let pick = |rng: &mut Rng| ids[rng.below(ids.len() as u64) as usize].clone();
    let t = |n: &str, a: Value| (true, n.to_string(), a.to_string());
    let c = |v: Value| (false, v.to_string(), String::new());
    match rng.below(24) {
        0 => t(
            "create",
            json!({"type": "cube", "params": {"size": v3(rng)}}),
        ),
        1 => t(
            "create",
            json!({"type": "cylinder", "params": {"radius": rng.num(), "height": rng.num()}}),
        ),
        2 => t(
            "create",
            json!({"type": "sphere", "params": {"radius": rng.num()}}),
        ),
        3 => t(
            "create",
            json!({"type": "cone", "params": {"radius_bottom": rng.num(), "radius_top": rng.num(), "height": rng.num()}}),
        ),
        4 => t(
            "create",
            json!({"type": "translate", "params": {"child": pick(rng), "offset": v3(rng)}}),
        ),
        5 => t(
            "create",
            json!({"type": "rotate", "params": {"child": pick(rng), "angles": v3(rng)}}),
        ),
        6 => t(
            "create",
            json!({"type": "scale", "params": {"child": pick(rng), "factor": v3(rng)}}),
        ),
        7 => t(
            "create",
            json!({"type": (["union", "difference", "intersection"][rng.below(3) as usize]), "params": {"left": pick(rng), "right": pick(rng)}}),
        ),
        8 => t(
            "create",
            json!({"type": (["fillet", "chamfer", "shell"][rng.below(3) as usize]), "parent_part_id": pick(rng), "params": {"radius": rng.num(), "distance": rng.num(), "thickness": rng.num()}}),
        ),
        9 => t(
            "linear_pattern",
            json!({"child": pick(rng), "direction": v3(rng), "count": rng.num(), "spacing": rng.num()}),
        ),
        10 => t(
            "circular_pattern",
            json!({"child": pick(rng), "axis_origin": v3(rng), "axis_dir": v3(rng), "count": rng.num(), "angle_deg": rng.num()}),
        ),
        11 => t(
            "set_material",
            json!({"part_id": pick(rng), "material": (["steel", "", "nope"][rng.below(3) as usize])}),
        ),
        12 => t("delete", json!({"part_id": pick(rng)})),
        13 => c(json!({"type": "rename", "id": pick(rng), "name": "R\u{0}é"})),
        14 => c(json!({"type": "set_visible", "id": pick(rng), "visible": rng.below(2) == 0})),
        15 => c(json!({"type": "reorder", "id": pick(rng), "index": rng.below(6)})),
        16 => c(
            json!({"type": "set_translation", "id": pick(rng), "offset": [rng.num(), rng.num(), rng.num()]}),
        ),
        17 => c(
            json!({"type": "set_scale", "id": pick(rng), "factor": [rng.num(), rng.num(), rng.num()]}),
        ),
        18 => c(json!({"type": "duplicate", "ids": [pick(rng), pick(rng)]})),
        19 => c(json!({"type": "batch", "commands": [
            {"type": "set_material", "id": pick(rng), "material": "brass"},
            {"type": "delete_feature", "id": pick(rng)},
            {"type": "set_rotation", "id": pick(rng), "angles": [rng.num(), 0, 0]}
        ]})),
        20 => c(json!({"type": "set_param", "id": pick(rng), "key": "radius", "value": rng.num()})),
        // Malformed envelopes: wrong types, missing fields, garbage.
        21 => (
            false,
            [
                "",
                "{",
                "null",
                "[]",
                "{\"type\":5}",
                "{\"type\":\"batch\",\"commands\":{}}",
            ][rng.below(6) as usize]
                .into(),
            String::new(),
        ),
        22 => (
            true,
            ["create", "delete", "nope", ""][rng.below(4) as usize].into(),
            [
                "",
                "{",
                "null",
                "{\"type\":\"cube\",\"params\":7}",
                "{\"part_id\":[]}",
            ][rng.below(5) as usize]
                .into(),
        ),
        _ => c(json!({"type": (["undo", "redo"][rng.below(2) as usize])})),
    }
}

fn run(api: &mut DocumentApi, s: &(bool, String, String), ctx: &str) -> Value {
    let raw = catch_unwind(AssertUnwindSafe(|| {
        if s.0 {
            api.apply_tool_envelope(&s.1, &s.2)
        } else {
            api.apply_json_envelope(&s.1)
        }
    }))
    .unwrap_or_else(|_| panic!("{ctx}: PANIC on {s:?}"));
    let v: Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{ctx}: bad envelope {e}: {raw}"));
    assert!(v["ok"].is_boolean(), "{ctx}: envelope without ok: {raw}");
    v
}

#[test]
fn hostile_sequences_never_panic_and_history_round_trips() {
    for seed in 1..=cases() {
        // Each seed on a worker with a deadline: a hang is a finding too.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let r = catch_unwind(|| hostile_seed(seed));
            let _ = tx.send(r);
        });
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => std::panic::resume_unwind(e),
            Err(_) => {
                panic!("seed {seed}: HUNG >120s (rerun with VCAD_FUZZ_TRACE=1 to see the steps)")
            }
        }
    }
}

fn hostile_seed(seed: u64) {
    let trace = std::env::var_os("VCAD_FUZZ_TRACE").is_some();
    {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut api = DocumentApi::new(ReplicaId(1));
        let initial = api.state_hash();
        let mut changed = 0usize;
        for i in 0..16 {
            let s = step(&mut rng, &api);
            if s.1.contains("undo") || s.1.contains("redo") {
                continue; // keep history linear for the round-trip check
            }
            let before = api.state_hash();
            if trace {
                eprintln!("seed {seed} step {i}: {s:?}");
            }
            run(&mut api, &s, &format!("seed {seed} step {i}"));
            if api.state_hash() != before {
                changed += 1;
            }
        }
        let fin = api.state_hash();

        // save → load is exact.
        let bytes = api.save();
        let loaded = DocumentApi::load(&bytes, ReplicaId(1))
            .unwrap_or_else(|e| panic!("seed {seed}: load failed: {e}"));
        assert_eq!(loaded.state_hash(), fin, "seed {seed}: save/load drifted");

        // undo everything → initial, redo everything → final.
        let mut undos = 0;
        while api.can_undo() && undos < 1000 {
            run(
                &mut api,
                &(false, r#"{"type":"undo"}"#.into(), String::new()),
                "undo",
            );
            undos += 1;
        }
        assert_eq!(
            api.state_hash(),
            initial,
            "seed {seed}: undo-all ≠ initial ({changed} edits, {undos} undos)"
        );
        for _ in 0..undos {
            run(
                &mut api,
                &(false, r#"{"type":"redo"}"#.into(), String::new()),
                "redo",
            );
        }
        assert_eq!(api.state_hash(), fin, "seed {seed}: redo-all ≠ final");
    }
}

/// Byte-level garbage into `load`, `from_ir` and `restore_snapshot` fails
/// closed, never panics.
#[test]
fn corrupt_files_fail_closed() {
    let mut api = DocumentApi::new(ReplicaId(1));
    run(
        &mut api,
        &(
            true,
            "create".into(),
            json!({"type": "cube", "params": {"size": {"x": 1, "y": 2, "z": 3}}}).to_string(),
        ),
        "seed",
    );
    let good = api.save();
    let ir = serde_json::to_string(&api.get_state().document).unwrap();
    let mut rng = Rng(0xfeed);
    for case in 0..cases() * 4 {
        let mut b = good.clone();
        let mut j = ir.clone().into_bytes();
        for _ in 0..1 + rng.below(8) {
            match rng.below(3) {
                0 if !b.is_empty() => {
                    let k = rng.below(b.len() as u64) as usize;
                    b[k] = rng.next() as u8;
                }
                1 if !b.is_empty() => {
                    b.truncate(rng.below(b.len() as u64) as usize);
                }
                _ if !j.is_empty() => {
                    let k = rng.below(j.len() as u64) as usize;
                    j[k] = b"{}[]\":,0-e9 nulltrue"[rng.below(20) as usize];
                }
                _ => {}
            }
        }
        let js = String::from_utf8_lossy(&j).into_owned();
        catch_unwind(AssertUnwindSafe(|| {
            let _ = DocumentApi::load(&b, ReplicaId(1));
            let _ = DocumentApi::from_ir(&js, ReplicaId(1));
            let mut a = DocumentApi::new(ReplicaId(1));
            let _ = a.restore_snapshot(&b);
            let _ = a.import_ir(&js);
        }))
        .unwrap_or_else(|_| panic!("case {case}: PANIC loading corrupt input (json: {js})"));
    }
}

/// Evaluation of every fuzzed document terminates, never panics, and
/// produces finite, well-indexed meshes and non-negative solid volumes.
#[test]
fn evaluation_of_fuzzed_documents_is_sane() {
    let opts = || vcad_eval::EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    };
    for seed in 1..=(cases() / 4).max(10) {
        let mut rng = Rng(seed.wrapping_mul(0xa076_1d64_78bd_642f) | 1);
        let mut api = DocumentApi::new(ReplicaId(1));
        for i in 0..8 {
            let s = step(&mut rng, &api);
            run(&mut api, &s, &format!("seed {seed} step {i}"));
        }
        let doc = api.get_state().document;
        let (tx, rx) = mpsc::channel();
        let d2 = doc.clone();
        std::thread::spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| {
                let scene = match vcad_eval::evaluate_document(&d2, &opts()) {
                    Ok(s) => s,
                    Err(_) => return Ok(()),
                };
                for p in &scene.parts {
                    let m = &p.mesh;
                    if m.positions.iter().any(|x| !x.is_finite()) {
                        return Err("non-finite vertex".to_string());
                    }
                    let nv = (m.positions.len() / 3) as u32;
                    if m.indices.len() % 3 != 0 || m.indices.iter().any(|&k| k >= nv) {
                        return Err("index out of range".to_string());
                    }
                    if let Some(sol) = &p.solid {
                        let v = sol.volume();
                        if !(v.is_finite() && v >= -1e-6) {
                            return Err(format!("solid volume {v}"));
                        }
                    }
                }
                Ok(())
            }));
            let _ = tx.send(r);
        });
        let doc_json = serde_json::to_string(&doc).unwrap();
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => panic!("seed {seed}: {e}\n{doc_json}"),
            Ok(Err(_)) => panic!("seed {seed}: evaluation PANICKED\n{doc_json}"),
            Err(_) => panic!("seed {seed}: evaluation HUNG >60s\n{doc_json}"),
        }
    }
}

/// Fuzz finding F5: a `.vcad` whose node graph has a cycle hung `from_ir`
/// (and `import_ir`) forever in the migrator. It must fail closed, fast.
#[test]
fn cyclic_documents_fail_closed_without_hanging() {
    let ir = r#"{"version":"0.1","nodes":{
        "1":{"id":1,"name":null,"op":{"type":"Cube","size":{"x":1,"y":1,"z":1}}},
        "4":{"id":4,"name":null,"op":{"type":"Translate","child":4,"offset":{"x":0,"y":0,"z":0}}},
        "7":{"id":7,"name":null,"op":{"type":"Union","left":1,"right":4}}},
      "materials":{},"part_materials":{},"roots":[{"root":7,"material":"default"}]}"#;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let opened = DocumentApi::from_ir(ir, ReplicaId(1)).is_ok();
        let mut api = DocumentApi::new(ReplicaId(1));
        api.import_ir(ir);
        let _ = tx.send((opened, api.get_state().parts.len()));
    });
    let (opened, parts) = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("from_ir/import_ir hung on a cyclic document");
    assert!(!opened, "from_ir accepted a cyclic document");
    assert_eq!(parts, 0, "import_ir imported a cyclic document");
}
