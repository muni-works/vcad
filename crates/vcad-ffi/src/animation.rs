//! Timeline authoring support for the native keyframe editor
//! (`vcad_timeline_normalize`).
//!
//! The Swift editor mutates the document's `timeline` JSON directly (add /
//! move / delete keys). Before that JSON is committed it round-trips through
//! the canonical `vcad_ir::animation::Timeline` type here, so whatever the
//! native app writes is exactly what the kernel sequencer, the MCP `animate`
//! tool and the web evaluator read: keys sorted by time, times clamped into
//! `[0, durationS]`, coincident keys collapsed (last write wins), empty tracks
//! dropped, and the default ease elided on serialize.

use std::ffi::c_char;

use vcad_ir::animation::Timeline;

use crate::err;
use crate::json::{borrow_str, guard_ptr, json_out};

/// Keys closer than this (seconds) are the same key.
pub const KEY_EPSILON_S: f64 = 1e-6;

/// Canonicalize a timeline in place. Errors on a non-finite or non-positive
/// duration / fps, or a non-finite key.
pub fn normalize(tl: &mut Timeline) -> Result<(), String> {
    if !(tl.duration_s.is_finite() && tl.duration_s > 0.0) {
        return Err(format!(
            "timeline: durationS must be > 0 (got {})",
            tl.duration_s
        ));
    }
    if !(tl.fps.is_finite() && tl.fps > 0.0) {
        return Err(format!("timeline: fps must be > 0 (got {})", tl.fps));
    }
    for track in &mut tl.tracks {
        if track
            .keys
            .iter()
            .any(|k| !k.t.is_finite() || !k.value.is_finite())
        {
            return Err("timeline: key time/value must be finite".into());
        }
        for k in &mut track.keys {
            k.t = k.t.clamp(0.0, tl.duration_s);
        }
        // Stable sort, then keep the LAST of each run of coincident keys —
        // the most recent write wins, matching how the editor appends.
        track.keys.sort_by(|a, b| a.t.total_cmp(&b.t));
        let mut out: Vec<vcad_ir::animation::AnimKey> = Vec::with_capacity(track.keys.len());
        for k in track.keys.drain(..) {
            match out.last_mut() {
                Some(prev) if (k.t - prev.t).abs() < KEY_EPSILON_S => *prev = k,
                _ => out.push(k),
            }
        }
        track.keys = out;
    }
    tl.tracks.retain(|t| !t.keys.is_empty());
    Ok(())
}

/// Normalize a timeline (UTF-8 JSON of `vcad_ir::animation::Timeline`).
/// Returns the canonical JSON, or null + `vcad_last_error` when it does not
/// parse or is invalid. Free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_timeline_normalize(timeline_json: *const c_char) -> *mut c_char {
    crate::err::clear_error();
    guard_ptr("timeline normalize", || {
        let Some(text) = borrow_str("timeline", timeline_json) else {
            return std::ptr::null_mut();
        };
        let res = serde_json::from_str::<Timeline>(text)
            .map_err(|e| format!("timeline: parse: {e}"))
            .and_then(|mut tl| normalize(&mut tl).map(|_| tl));
        match res {
            Ok(tl) => json_out(&tl),
            Err(e) => {
                err::set_error(e);
                std::ptr::null_mut()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(v: serde_json::Value) -> Result<serde_json::Value, String> {
        let c = std::ffi::CString::new(v.to_string()).unwrap();
        let out = vcad_timeline_normalize(c.as_ptr());
        if out.is_null() {
            return Err(crate::err::last_error_string().unwrap_or_default());
        }
        let s = unsafe { std::ffi::CStr::from_ptr(out) }
            .to_str()
            .unwrap()
            .to_string();
        crate::json::vcad_string_free(out);
        Ok(serde_json::from_str(&s).unwrap())
    }

    #[test]
    fn sorts_clamps_dedups_and_drops_empty_tracks() {
        let out = run(serde_json::json!({
            "durationS": 2.0, "fps": 24.0,
            "tracks": [
                {"target": {"type": "Joint", "jointId": "j"}, "keys": [
                    {"t": 1.5, "value": 30.0, "ease": "ease-in-out"},
                    {"t": 0.0, "value": 0.0},
                    {"t": 1.5, "value": 45.0},
                    {"t": 9.0, "value": 90.0}
                ]},
                {"target": {"type": "Joint", "jointId": "empty"}, "keys": []}
            ]
        }))
        .unwrap();
        let tracks = out["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1);
        let keys = tracks[0]["keys"].as_array().unwrap();
        let ts: Vec<f64> = keys.iter().map(|k| k["t"].as_f64().unwrap()).collect();
        assert_eq!(ts, vec![0.0, 1.5, 2.0]);
        assert_eq!(keys[1]["value"], 45.0, "last write wins");
        assert!(keys[1].get("ease").is_none(), "linear ease elided");
    }

    #[test]
    fn rejects_bad_duration() {
        let e = run(serde_json::json!({"durationS": 0.0, "fps": 24.0, "tracks": []})).unwrap_err();
        assert!(e.contains("durationS"), "{e}");
    }
}
