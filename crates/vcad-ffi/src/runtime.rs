//! The document runtime over the C ABI (`docs/runtime-design.md`).
//!
//! A `VcadEngine` *is* a runtime (`vcad_runtime::Runtime`): its blocking
//! `vcad_engine_*` calls and the fire-and-forget intents sent here share one
//! thread and one inbox. Intents come back as `VcadFrame`s through the sink
//! installed with `vcad_doc_runtime_set_sink`, delivered on the runtime
//! thread; the host owns each frame and frees it with `vcad_frame_free`.

use std::ffi::{c_char, c_void, CString};
use std::ptr;

use vcad_runtime::{Envelope, Frame};

use crate::engine::VcadEngine;
use crate::{err, VcadScene};

/// One published frame (owned by the host once delivered).
pub struct VcadFrame {
    frame: Frame,
    state: CString,
    info: CString,
}

/// Frame sink: called on the runtime thread with an owned frame.
pub type VcadFrameFn = extern "C" fn(ctx: *mut c_void, frame: *mut VcadFrame);

struct Ctx(usize);

/// Install the frame sink (`cb` null clears it). `ctx` is passed back
/// verbatim and must stay valid until the sink is cleared or the engine is
/// freed — `vcad_engine_free` guarantees no callback runs after it returns.
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_set_sink(
    engine: *const VcadEngine,
    cb: Option<VcadFrameFn>,
    ctx: *mut c_void,
) {
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return;
    };
    let Some(cb) = cb else {
        engine.rt.set_sink(None);
        return;
    };
    let ctx = Ctx(ctx as usize);
    engine.rt.set_sink(Some(Box::new(move |frame: Frame| {
        let info = serde_json::json!({
            "errors": frame.errors,
            "endedGestures": frame.ended_gestures,
            "gestureOpen": frame.gesture_open,
            "evalError": frame.eval_error,
            "stateHash": frame.state_hash,
            "history": frame.history_json,
            "partKeys": frame.part_keys,
        })
        .to_string();
        let boxed = Box::new(VcadFrame {
            state: CString::new(frame.state.clone()).unwrap_or_default(),
            info: CString::new(info).unwrap_or_default(),
            frame,
        });
        cb(ctx.0 as *mut c_void, Box::into_raw(boxed));
    })));
}

/// Queue an intent envelope (`{"seq":N,"intent":{"type":…}}`, UTF-8 JSON).
/// Never blocks. False on a bad envelope (see `vcad_last_error`) or a
/// stopped runtime.
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_send(
    engine: *const VcadEngine,
    json: *const u8,
    len: usize,
) -> bool {
    err::clear_error();
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return false;
    };
    if json.is_null() {
        err::set_error("vcad_doc_runtime_send: null json");
        return false;
    }
    let bytes = unsafe { std::slice::from_raw_parts(json, len) };
    match serde_json::from_slice::<Envelope>(bytes) {
        Ok(env) => engine.rt.send(env),
        Err(e) => {
            err::set_error(format!("vcad_doc_runtime_send: {e}"));
            false
        }
    }
}

/// The runtime's current revision (after everything queued so far).
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_rev(engine: *const VcadEngine) -> u64 {
    unsafe { engine.as_ref() }
        .and_then(|e| e.rt.call(|c| c.rev()).ok())
        .unwrap_or(0)
}

/// The shared undo history (`vcad_runtime::History`), one JSON op per call:
///
/// - `{"op":"list"}`
/// - `{"op":"push_engine","label"}` — one engine history group was applied
/// - `{"op":"mark"}` → `"mark"`: remember the current state
/// - `{"op":"push_mark","mark","label"}` — land the mark as one snapshot entry
/// - `{"op":"restore_mark","mark"}` — put the document back to it (a failed edit)
/// - `{"op":"drop_mark","mark"}`
/// - `{"op":"undo"}` / `{"op":"redo"}` → `"label"` (null: nothing to do)
/// - `{"op":"truncate","depth"}`, `{"op":"rename_top","label"}`,
///   `{"op":"top_is_snapshot"}`, `{"op":"clear_redo"}`, `{"op":"clear"}`
///
/// Every reply carries `{"ok":true,"undo":[labels],"redo":[labels]}` (bottom
/// to top); ops that change the document add `"state"`, the state envelope.
/// Errors: `{"ok":false,"error":…}`. Free with `vcad_engine_string_free`.
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_history(
    engine: *const VcadEngine,
    op_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return ptr::null_mut();
    };
    let reply = match crate::analysis::parse_request(op_json) {
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
        Ok(req) => engine
            .rt
            .call(move |c| history_op(c, &req))
            .unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": e })),
    };
    CString::new(reply.to_string()).map_or(ptr::null_mut(), CString::into_raw)
}

fn history_op(c: &mut vcad_runtime::Core, req: &serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Value};
    let label = req
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("Edit")
        .to_string();
    let mark = req.get("mark").and_then(Value::as_u64).unwrap_or(0);
    let mut out = json!({ "ok": true });
    let mut changed = false;
    let res: Result<(), String> = match req.get("op").and_then(Value::as_str).unwrap_or("") {
        "list" => Ok(()),
        "push_engine" => {
            c.history.push_engine(&label);
            Ok(())
        }
        "mark" => {
            out["mark"] = json!(c.history.mark(&c.api));
            Ok(())
        }
        "push_mark" => c.history.push_mark(mark, &label),
        "restore_mark" => {
            changed = true;
            c.history.restore_mark(mark, &mut c.api)
        }
        "drop_mark" => {
            c.history.drop_mark(mark);
            Ok(())
        }
        op @ ("undo" | "redo") => {
            let r = if op == "undo" {
                c.history.undo(&mut c.api)
            } else {
                c.history.redo(&mut c.api)
            };
            r.map(|l| {
                changed = l.is_some();
                out["label"] = json!(l);
            })
        }
        "truncate" => {
            c.history
                .truncate(req.get("depth").and_then(Value::as_u64).unwrap_or(0) as usize);
            Ok(())
        }
        "rename_top" => {
            out["renamed"] = json!(c.history.rename_top(&label));
            Ok(())
        }
        "top_is_snapshot" => {
            out["snapshot"] = json!(c.history.top_is_snapshot());
            Ok(())
        }
        "clear_redo" => {
            c.history.clear_redo();
            Ok(())
        }
        "clear" => {
            c.history.clear();
            Ok(())
        }
        other => Err(format!("history: unknown op `{other}`")),
    };
    if let Err(e) = res {
        return json!({ "ok": false, "error": e });
    }
    let labels = c.history.labels();
    out["undo"] = json!(labels.undo);
    out["redo"] = json!(labels.redo);
    if changed {
        c.bump();
        out["state"] = serde_json::from_str(&c.api.state_envelope()).unwrap_or(Value::Null);
    }
    out
}

/// Push a snapshot the host holds (bytes of `vcad_engine_save`) as an undo
/// entry. Returns the `vcad_doc_runtime_history` list reply.
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_history_push_snapshot(
    engine: *const VcadEngine,
    label: *const c_char,
    bytes: *const u8,
    len: usize,
) -> *mut c_char {
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return ptr::null_mut();
    };
    let label = crate::json::borrow_str("label", label)
        .unwrap_or("Edit")
        .to_string();
    let snap = if bytes.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec()
    };
    let reply = engine
        .rt
        .call(move |c| {
            c.history.push_snapshot(&label, snap);
            history_op(c, &serde_json::json!({ "op": "list" }))
        })
        .unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": e }));
    CString::new(reply.to_string()).map_or(ptr::null_mut(), CString::into_raw)
}

/// The evaluated scene of the runtime's current state, through its
/// incremental cache — for a host that needs B-reps before the next frame
/// lands. Blocks. Null on failure (see `vcad_last_error`); free with
/// `vcad_scene_free`.
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_scene(engine: *const VcadEngine) -> *mut VcadScene {
    err::clear_error();
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return ptr::null_mut();
    };
    match engine.rt.call(|c| c.evaluate_current()) {
        Ok(Ok((inner, doc))) => Box::into_raw(Box::new(VcadScene {
            inner,
            doc: Some(doc),
        })),
        Ok(Err(e)) | Err(e) => {
            err::set_error(e);
            ptr::null_mut()
        }
    }
}

/// `vcad_sound_call` on the runtime's current document, using its evaluated
/// meshes (no document JSON from the host, no part re-evaluation for
/// `modes`/`check`). Only the snapshot is taken on the runtime thread; the
/// solve runs on the calling thread, so call it off the UI thread. Free with
/// `vcad_string_free`; null + `vcad_last_error` on failure.
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_sound(
    engine: *const VcadEngine,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return ptr::null_mut();
    };
    let run = || -> Result<String, String> {
        let req = crate::analysis::parse_request(request_json)?;
        let (doc, scene) = engine.rt.call(|c| match c.evaluate_current() {
            Ok((scene, doc)) => (doc, Some(scene)),
            Err(_) => (c.api.get_state().document, None),
        })?;
        Ok(crate::sound::call_with_scene(&doc, scene.as_ref(), &req)?.to_string())
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Ok(s)) => CString::new(s).map_or(ptr::null_mut(), CString::into_raw),
        Ok(Err(e)) => {
            err::set_error(e);
            ptr::null_mut()
        }
        Err(_) => {
            err::set_error("vcad_doc_runtime_sound panicked");
            ptr::null_mut()
        }
    }
}

/// Directory relative `MeshImport` paths resolve against when frames are
/// evaluated (empty clears it).
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_set_base_dir(
    engine: *const VcadEngine,
    dir: *const u8,
    len: usize,
) {
    let Some(e) = (unsafe { engine.as_ref() }) else {
        return;
    };
    let dir = (!dir.is_null() && len > 0)
        .then(|| std::str::from_utf8(unsafe { std::slice::from_raw_parts(dir, len) }).ok())
        .flatten()
        .map(std::path::PathBuf::from);
    let _ = e.rt.call(move |c| c.base_dir = dir);
}

/// Revision the frame describes.
#[no_mangle]
pub extern "C" fn vcad_frame_rev(frame: *const VcadFrame) -> u64 {
    unsafe { frame.as_ref() }.map_or(0, |f| f.frame.rev)
}

/// Last intent `seq` folded into the frame.
#[no_mangle]
pub extern "C" fn vcad_frame_acked_seq(frame: *const VcadFrame) -> u64 {
    unsafe { frame.as_ref() }.map_or(0, |f| f.frame.acked_seq)
}

/// The state envelope (`vcad_engine_state` shape), borrowed from the frame.
#[no_mangle]
pub extern "C" fn vcad_frame_state(frame: *const VcadFrame) -> *const c_char {
    unsafe { frame.as_ref() }.map_or(ptr::null(), |f| f.state.as_ptr())
}

/// `{"errors":[…],"endedGestures":[…],"gestureOpen":bool,"evalError":…,"stateHash":…}`,
/// borrowed from the frame.
#[no_mangle]
pub extern "C" fn vcad_frame_info(frame: *const VcadFrame) -> *const c_char {
    unsafe { frame.as_ref() }.map_or(ptr::null(), |f| f.info.as_ptr())
}

/// The materialized IR document as JSON bytes, borrowed from the frame.
#[no_mangle]
pub extern "C" fn vcad_frame_document_json(
    frame: *const VcadFrame,
    out_len: *mut usize,
) -> *const u8 {
    let Some(f) = (unsafe { frame.as_ref() }) else {
        return ptr::null();
    };
    if !out_len.is_null() {
        unsafe { *out_len = f.frame.document_json.len() };
    }
    f.frame.document_json.as_ptr()
}

/// Move the evaluated scene out of the frame (null if there is none or it
/// was already taken). Free with `vcad_scene_free`.
#[no_mangle]
pub extern "C" fn vcad_frame_take_scene(frame: *mut VcadFrame) -> *mut VcadScene {
    let Some(f) = (unsafe { frame.as_mut() }) else {
        return ptr::null_mut();
    };
    match f.frame.scene.take() {
        Some(inner) => Box::into_raw(Box::new(VcadScene {
            inner,
            doc: Some(f.frame.document.clone()),
        })),
        None => ptr::null_mut(),
    }
}

/// Free a frame.
#[no_mangle]
pub extern "C" fn vcad_frame_free(frame: *mut VcadFrame) {
    if !frame.is_null() {
        unsafe { drop(Box::from_raw(frame)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    static FRAMES: Mutex<Option<mpsc::Sender<usize>>> = Mutex::new(None);

    extern "C" fn sink(_ctx: *mut c_void, frame: *mut VcadFrame) {
        if let Some(tx) = FRAMES.lock().unwrap().as_ref() {
            tx.send(frame as usize).unwrap();
        }
    }

    #[test]
    fn engine_calls_and_intents_share_one_runtime() {
        let (tx, rx) = mpsc::channel();
        *FRAMES.lock().unwrap() = Some(tx);
        let engine = crate::engine::vcad_engine_new(1);
        vcad_doc_runtime_set_sink(engine, Some(sink), ptr::null_mut());
        let doc = CString::new(
            r#"{"version":"0.1","nodes":{"1":{"id":1,"op":{"type":"Cube","size":{"x":10,"y":10,"z":10}}}},
            "materials":{},"part_materials":{},"parameters":{"w":{"value":10.0}},
            "roots":[{"root":1,"material":"a"}]}"#,
        )
        .unwrap();
        let st = crate::engine::vcad_engine_load_ir(engine, doc.as_ptr());
        assert!(!st.is_null());
        crate::engine::vcad_engine_string_free(st);
        let rev0 = vcad_doc_runtime_rev(engine);

        let intent = br#"{"seq":1,"intent":{"type":"set_doc_param","name":"w","value":20,"gesture":{"id":5,"start":true}}}"#;
        assert!(vcad_doc_runtime_send(engine, intent.as_ptr(), intent.len()));
        assert!(!vcad_doc_runtime_send(engine, b"{".as_ptr(), 1));
        // The load published a scene frame first (blocking edits publish once
        // the inbox is idle); skip to the intent's frame.
        let frame = loop {
            let f = rx.recv_timeout(Duration::from_secs(30)).unwrap() as *mut VcadFrame;
            if vcad_frame_acked_seq(f) == 1 {
                break f;
            }
            vcad_frame_free(f);
        };
        assert!(vcad_frame_rev(frame) > rev0);
        assert_eq!(vcad_frame_acked_seq(frame), 1);
        let mut len = 0;
        let bytes = vcad_frame_document_json(frame, &mut len);
        let v: serde_json::Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(bytes, len) }).unwrap();
        assert_eq!(v["parameters"]["w"]["value"], 20.0);
        let scene = vcad_frame_take_scene(frame);
        assert!(!scene.is_null());
        assert_eq!(crate::vcad_scene_part_count(scene), 1);
        crate::vcad_scene_free(scene);
        assert!(vcad_frame_take_scene(frame).is_null());
        vcad_frame_free(frame);

        // The gesture is one history entry; undo through the C ABI, ordered
        // after the intent.
        let op = |j: &str| {
            let c = CString::new(j).unwrap();
            let raw = vcad_doc_runtime_history(engine, c.as_ptr());
            let v: serde_json::Value =
                serde_json::from_str(unsafe { std::ffi::CStr::from_ptr(raw) }.to_str().unwrap())
                    .unwrap();
            crate::engine::vcad_engine_string_free(raw);
            v
        };
        assert_eq!(op(r#"{"op":"list"}"#)["undo"], serde_json::json!(["Edit"]));
        let u = op(r#"{"op":"undo"}"#);
        assert_eq!(u["label"], "Edit");
        assert_eq!(
            u["state"]["result"]["document"]["parameters"]["w"]["value"], 10.0,
            "{u}"
        );
        assert_eq!(u["redo"], serde_json::json!(["Edit"]));
        let r = op(r#"{"op":"redo"}"#);
        assert_eq!(
            r["state"]["result"]["document"]["parameters"]["w"]["value"],
            20.0
        );
        assert!(op(r#"{"op":"redo"}"#)["label"].is_null());
        assert_eq!(op(r#"{"op":"bogus"}"#)["ok"], false);

        // Sound on the runtime's document + meshes, no host JSON.
        let req = CString::new(r#"{"op":"modes","part":0,"modes":3,"quality":"preview"}"#).unwrap();
        let out = vcad_doc_runtime_sound(engine, req.as_ptr());
        assert!(!out.is_null(), "{:?}", crate::err::last_error_string());
        let v: serde_json::Value =
            serde_json::from_str(unsafe { std::ffi::CStr::from_ptr(out) }.to_str().unwrap())
                .unwrap();
        crate::json::vcad_string_free(out);
        assert!(v["modes"][0]["hz"].as_f64().unwrap() > 0.0, "{v}");
        crate::engine::vcad_engine_free(engine);
    }
}
