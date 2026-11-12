//! How parts sound, for vcad.app's mallet tool (`vcad_sound_call`).
//!
//! Ops (all take the document JSON):
//! - `modes` `{part, modes?, quality?: "preview"|"fine", material?}` →
//!   [`vcad_sound::PartModes`]: frequencies named as notes, plus an animation
//!   mesh (positions, indices) and per-mode displacement on it.
//! - `lock` `{part, mode?, target_hz, note?, drives, tolerance_cents?}` →
//!   `{document, report}`: the parameter solved so that mode rings at the
//!   target, and the lock stored on the document (replacing any lock on the
//!   same part + mode). The caller commits the document as one undo step.
//! - `unlock` `{part, mode?}` → `{document}` without that lock.
//! - `check` → `{locks: [{lock, hz, cents, holds}]}`: measure every stored
//!   lock against the current geometry without solving.

use std::ffi::c_char;
use std::ptr;

use serde_json::{json, Value};
use vcad_ir::Document;
use vcad_sound::{part_index, solve_pitch_lock, ModesRequest, PitchLock};

use crate::analysis::parse_request;
use crate::err;
use crate::json::{borrow_str, guard_ptr, into_c};

fn lock_from(req: &Value) -> Result<PitchLock, String> {
    serde_json::from_value(req.clone()).map_err(|e| format!("lock: {e}"))
}

/// The app's engine-materialized documents carry part names as feature
/// names, not on the root nodes; the caller passes them (`part_names`, in
/// visible-part order) and they fill any unnamed root.
fn apply_names(doc: &mut Document, req: &Value) {
    let Some(names) = req.get("part_names").and_then(Value::as_array) else {
        return;
    };
    let roots: Vec<_> = doc
        .roots
        .iter()
        .filter(|e| e.visible != Some(false))
        .map(|e| e.root)
        .collect();
    for (root, name) in roots.iter().zip(names) {
        if let (Some(node), Some(n)) = (doc.nodes.get_mut(root), name.as_str()) {
            if node.name.is_none() && !n.is_empty() {
                node.name = Some(n.to_string());
            }
        }
    }
}

pub(crate) fn call(doc: &Document, req: &Value) -> Result<Value, String> {
    call_with_scene(doc, None, req)
}

/// [`call`] with the document's evaluated scene already in hand (the
/// document runtime's): `modes` and `check` take part meshes from it instead
/// of evaluating the part again.
pub(crate) fn call_with_scene(
    doc: &Document,
    scene: Option<&vcad_eval::EvaluatedScene>,
    req: &Value,
) -> Result<Value, String> {
    let scene_mesh = |part: usize| {
        scene
            .and_then(|s| s.parts.get(part))
            .filter(|p| !p.mesh.indices.is_empty())
            .map(|p| (&p.mesh.positions, &p.mesh.indices))
    };
    let mut named = doc.clone();
    apply_names(&mut named, req);
    let doc = &named;
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");
    match op {
        "modes" => {
            let r: ModesRequest =
                serde_json::from_value(req.clone()).map_err(|e| format!("modes: {e}"))?;
            // The app passes the part's tessellation it already has; else
            // evaluate just that part.
            let pm = match req.get("mesh") {
                Some(m) => {
                    let positions: Vec<f32> = serde_json::from_value(m["positions"].clone())
                        .map_err(|e| format!("mesh.positions: {e}"))?;
                    let indices: Vec<u32> = serde_json::from_value(m["indices"].clone())
                        .map_err(|e| format!("mesh.indices: {e}"))?;
                    vcad_sound::part_modes_mesh(doc, &r, &positions, &indices)
                }
                None => match scene_mesh(r.part) {
                    Some((p, i)) => vcad_sound::part_modes_mesh(doc, &r, p, i),
                    None => vcad_sound::part_modes_alone(doc, &r),
                },
            }
            .map_err(|e| e.to_string())?;
            serde_json::to_value(pm).map_err(|e| e.to_string())
        }
        "lock" => {
            let lock = lock_from(req)?;
            if part_index(doc, &lock.part).is_none() {
                return Err(format!("no part named {:?}", lock.part));
            }
            let (mut solved, report) = solve_pitch_lock(doc, &lock).map_err(|e| e.to_string())?;
            solved
                .pitch_locks
                .retain(|l| !(l.part == lock.part && l.mode == lock.mode));
            solved.pitch_locks.push(lock);
            Ok(json!({ "document": solved, "report": report }))
        }
        "unlock" => {
            let part = req
                .get("part")
                .and_then(Value::as_str)
                .ok_or("unlock: part required")?;
            let mode = req.get("mode").and_then(Value::as_u64).unwrap_or(1) as usize;
            let mut d = doc.clone();
            d.pitch_locks
                .retain(|l| !(l.part == part && l.mode == mode));
            Ok(json!({ "document": d }))
        }
        "check" => {
            let mut out = Vec::new();
            let only = req.get("part").and_then(Value::as_str);
            for l in doc
                .pitch_locks
                .iter()
                .filter(|l| only.is_none_or(|p| p == l.part))
            {
                let entry = match part_index(doc, &l.part) {
                    None => json!({"lock": l, "error": format!("part {:?} is gone", l.part)}),
                    Some(idx) => {
                        let r = ModesRequest {
                            part: idx,
                            modes: l.mode,
                            quality: vcad_sound::Quality::Fine,
                            material: None,
                            shapes: false,
                        };
                        let modes = match scene_mesh(idx) {
                            Some((p, i)) => vcad_sound::part_modes_mesh(doc, &r, p, i),
                            None => vcad_sound::part_modes_alone(doc, &r),
                        };
                        match modes {
                            Ok(pm) => match pm.modes.get(l.mode - 1) {
                                Some(m) => {
                                    let c = 1200.0 * (m.hz / l.target_hz).log2();
                                    json!({"lock": l, "hz": m.hz, "cents": c, "holds": c.abs() <= l.tolerance_cents})
                                }
                                None => json!({"lock": l, "error": "mode not found"}),
                            },
                            Err(e) => json!({"lock": l, "error": e.to_string()}),
                        }
                    }
                };
                out.push(entry);
            }
            Ok(json!({ "locks": out }))
        }
        "" => Err("sound: missing `op`".into()),
        other => Err(format!("sound: unknown op `{other}`")),
    }
}

/// `{"op": "modes" | "lock" | "unlock" | "check", ...}` on a document → JSON.
/// Null + `vcad_last_error` on failure; free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_sound_call(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_sound_call", || {
        let run = || -> Result<String, String> {
            let text = borrow_str("doc", doc_json).ok_or("doc: bad string")?;
            let doc = Document::from_json(text).map_err(|e| format!("doc: parse: {e}"))?;
            let req = parse_request(request_json)?;
            Ok(call(&doc, &req)?.to_string())
        };
        match run() {
            Ok(s) => into_c(s),
            Err(e) => {
                err::set_error(e);
                ptr::null_mut()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        vcad_loon::eval_vcad(
            "[defparam len 120 :unit \"mm\" :min 40 :max 300]\n[let bar [cube len 25 6]]\n[root bar \"aluminum\"]\n",
            None,
        )
        .unwrap()
    }

    #[test]
    fn modes_then_lock_then_check() {
        let d = doc();
        let m = call(&d, &json!({"op": "modes", "part": 0, "modes": 3})).unwrap();
        assert_eq!(m["name"], "bar");
        assert!(m["modes"][0]["hz"].as_f64().unwrap() > 1000.0);
        assert!(
            m["positions"].as_array().unwrap().len() > 300,
            "animation mesh is refined"
        );
        let r = call(&d, &json!({"op": "lock", "part": "bar", "target_hz": 1567.98, "note": "G6", "drives": "len"})).unwrap();
        assert_eq!(r["report"]["converged"], true, "{}", r["report"]);
        let locked: Document = serde_json::from_value(r["document"].clone()).unwrap();
        assert_eq!(locked.pitch_locks.len(), 1);
        let c = call(&locked, &json!({"op": "check"})).unwrap();
        assert_eq!(c["locks"][0]["holds"], true, "{c}");
        let u = call(&locked, &json!({"op": "unlock", "part": "bar"})).unwrap();
        let unlocked: Document = serde_json::from_value(u["document"].clone()).unwrap();
        assert!(unlocked.pitch_locks.is_empty());
    }
}
