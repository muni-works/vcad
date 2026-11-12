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
    let Some(engine) = (unsafe { engine.as_ref() }) else { return };
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
pub extern "C" fn vcad_doc_runtime_send(engine: *const VcadEngine, json: *const u8, len: usize) -> bool {
    err::clear_error();
    let Some(engine) = (unsafe { engine.as_ref() }) else { return false };
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
    unsafe { engine.as_ref() }.and_then(|e| e.rt.call(|c| c.rev()).ok()).unwrap_or(0)
}

/// Undo (or, called again, redo) a whole gesture: swap the document with the
/// snapshot taken at the gesture's first intent. Returns the state envelope
/// (free with `vcad_engine_string_free`), or null when the gesture is unknown.
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_swap_gesture(engine: *const VcadEngine, gesture: u64) -> *mut c_char {
    err::clear_error();
    let Some(engine) = (unsafe { engine.as_ref() }) else { return ptr::null_mut() };
    match engine.rt.call(move |c| {
        c.swap_gesture(gesture)?;
        Ok::<_, String>(c.api.state_envelope())
    }) {
        Ok(Ok(s)) => CString::new(s).map_or(ptr::null_mut(), CString::into_raw),
        Ok(Err(e)) | Err(e) => {
            err::set_error(e);
            ptr::null_mut()
        }
    }
}

/// Drop a gesture's snapshot (its undo entry is gone).
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_forget_gesture(engine: *const VcadEngine, gesture: u64) {
    if let Some(e) = unsafe { engine.as_ref() } {
        let _ = e.rt.call(move |c| c.forget_gesture(gesture));
    }
}

/// Directory relative `MeshImport` paths resolve against when frames are
/// evaluated (empty clears it).
#[no_mangle]
pub extern "C" fn vcad_doc_runtime_set_base_dir(engine: *const VcadEngine, dir: *const u8, len: usize) {
    let Some(e) = (unsafe { engine.as_ref() }) else { return };
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
pub extern "C" fn vcad_frame_document_json(frame: *const VcadFrame, out_len: *mut usize) -> *const u8 {
    let Some(f) = (unsafe { frame.as_ref() }) else { return ptr::null() };
    if !out_len.is_null() {
        unsafe { *out_len = f.frame.document_json.len() };
    }
    f.frame.document_json.as_ptr()
}

/// Move the evaluated scene out of the frame (null if there is none or it
/// was already taken). Free with `vcad_scene_free`.
#[no_mangle]
pub extern "C" fn vcad_frame_take_scene(frame: *mut VcadFrame) -> *mut VcadScene {
    let Some(f) = (unsafe { frame.as_mut() }) else { return ptr::null_mut() };
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
        let frame = rx.recv_timeout(Duration::from_secs(30)).unwrap() as *mut VcadFrame;
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

        // Gesture undo through the C ABI, ordered after the intent.
        let undone = vcad_doc_runtime_swap_gesture(engine, 5);
        assert!(!undone.is_null());
        let s = unsafe { std::ffi::CStr::from_ptr(undone) }.to_str().unwrap().to_owned();
        crate::engine::vcad_engine_string_free(undone);
        assert!(s.contains(r#""w":{"value":10.0"#), "{s}");
        assert!(vcad_doc_runtime_swap_gesture(engine, 99).is_null());
        crate::engine::vcad_engine_free(engine);
    }
}
