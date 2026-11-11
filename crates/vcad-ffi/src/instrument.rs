//! Tuned-instrument calls for vcad.app's Tune panel (`vcad_instrument_call`).
//!
//! The document carries `instrument` metadata (written by the
//! `design_instrument` MCP tool). Ops:
//!
//! - `receipt` — per-bar status + unified receipt (predicted / measured).
//! - `record` `{measured_hz, note?, instrument?, measured_at?}` — match the
//!   strike to a bar (or the named one), store the measurement; returns the
//!   patched `document` plus the receipt. The caller commits it as one undo.
//! - `cam_jobs` `{note, post?}` — the `vcad_cam_job` requests that mill one
//!   bar (two setups).
//! - `mill` `{note, out_dir, post?}` — run those jobs and write G-code
//!   files. **Dry run**: files only, nothing is sent to a machine.
//! - `print` `{plate: "bars"|"frame", gcode_path, settings?}` — slice one
//!   plate through the existing print flow and write G-code. **Dry run**.

use std::ffi::c_char;
use std::ptr;

use serde_json::{json, Value};
use vcad_ir::Document;
use vcad_kernel_acoustics::instrument_doc;

use crate::analysis::parse_request;
use crate::err;
use crate::json::{borrow_str, guard_ptr, into_c};

fn meta(doc: &Document) -> Result<&vcad_ir::instrument::Instrument, String> {
    doc.instrument
        .as_ref()
        .ok_or_else(|| "instrument: this document has no tuned instrument (run design_instrument)".into())
}

fn bar<'a>(doc: &'a Document, note: &str) -> Result<&'a vcad_ir::instrument::InstrumentBar, String> {
    meta(doc)?
        .bars
        .iter()
        .find(|b| b.note.eq_ignore_ascii_case(note) || b.part == note)
        .ok_or_else(|| format!("instrument: no bar {note:?}"))
}

fn str_arg<'a>(req: &'a Value, key: &str) -> Option<&'a str> {
    req.get(key).and_then(Value::as_str)
}

/// Keep only scene roots whose node name passes `keep`.
fn plate(doc: &Document, keep: impl Fn(&str) -> bool) -> Document {
    let mut d = doc.clone();
    d.roots.retain(|r| {
        d.nodes
            .get(&r.root)
            .and_then(|n| n.name.as_deref())
            .map(&keep)
            .unwrap_or(false)
    });
    d
}

/// Dispatch one request.
pub(crate) fn call(doc: &mut Document, req: &Value) -> Result<Value, String> {
    let op = str_arg(req, "op").unwrap_or("");
    let post = str_arg(req, "post").unwrap_or("grbl");
    match op {
        "receipt" => Ok(instrument_doc::receipt_json(meta(doc)?)),
        "record" => {
            let hz = req
                .get("measured_hz")
                .and_then(Value::as_f64)
                .ok_or("record: measured_hz required")?;
            let label = str_arg(req, "instrument").unwrap_or("vcad.app Tune (microphone)");
            let at = str_arg(req, "measured_at").map(str::to_string);
            let note = str_arg(req, "note");
            let m = doc.instrument.as_mut().ok_or("instrument: none on this document")?;
            let idx = instrument_doc::record_strike(m, hz, note, label, at)?;
            let bar = m.bars[idx].clone();
            let receipt = instrument_doc::receipt_json(m);
            Ok(json!({
                "bar": idx,
                "note": bar.note,
                "claim": instrument_doc::bar_claim(&bar),
                "receipt": receipt,
                "document": doc,
            }))
        }
        "cam_jobs" => {
            let note = str_arg(req, "note").ok_or("cam_jobs: note required")?;
            Ok(json!({ "jobs": instrument_doc::cam_jobs(bar(doc, note)?, post) }))
        }
        "mill" => {
            let note = str_arg(req, "note").ok_or("mill: note required")?;
            let dir = str_arg(req, "out_dir").ok_or("mill: out_dir required")?;
            std::fs::create_dir_all(dir).map_err(|e| format!("mill: {dir}: {e}"))?;
            let b = bar(doc, note)?;
            let mut out = Vec::new();
            for (i, j) in instrument_doc::cam_jobs(b, post).into_iter().enumerate() {
                let r = crate::cam_job::run_job_value(&j)?;
                let path = format!("{dir}/{}-setup{}.nc", b.part, i + 1);
                let gcode = r.get("gcode").and_then(Value::as_str).unwrap_or("");
                std::fs::write(&path, gcode).map_err(|e| format!("mill: {path}: {e}"))?;
                out.push(json!({
                    "name": j["name"],
                    "path": path,
                    "gcode_lines": gcode.lines().count(),
                    "verification": r.get("verification").cloned().unwrap_or(Value::Null),
                }));
            }
            Ok(json!({ "dry_run": true, "setups": out }))
        }
        "print" => {
            let which = str_arg(req, "plate").unwrap_or("bars");
            let path = str_arg(req, "gcode_path").ok_or("print: gcode_path required")?;
            let parts: Vec<String> = meta(doc)?.bars.iter().map(|b| b.part.clone()).collect();
            let d = match which {
                "bars" => plate(doc, |n| parts.iter().any(|p| p == n)),
                "frame" => plate(doc, |n| n == "frame"),
                other => return Err(format!("print: unknown plate {other:?} (bars | frame)")),
            };
            if d.roots.is_empty() {
                return Err(format!("print: no {which} parts on this document"));
            }
            let mut r = json!({"op": "slice", "gcodePath": path, "maxPreviewLayers": 1});
            if let Some(s) = req.get("settings") {
                r["settings"] = s.clone();
            }
            let mut res = crate::print::call(Some(&d), &r)?;
            if let Some(o) = res.as_object_mut() {
                o.remove("layers");
                o.insert("dry_run".into(), json!(true));
                o.insert("plate".into(), json!(which));
            }
            Ok(res)
        }
        "" => Err("instrument: missing `op`".into()),
        other => Err(format!("instrument: unknown op `{other}`")),
    }
}

/// `{"op": "receipt" | "record" | "cam_jobs" | "mill" | "print", ...}` on a
/// document → JSON. Null + `vcad_last_error` on failure. Free with
/// `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_instrument_call(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_instrument_call", || {
        let run = || -> Result<String, String> {
            let text = borrow_str("doc", doc_json).ok_or("doc: bad string")?;
            let mut doc = Document::from_json(text).map_err(|e| format!("doc: parse: {e}"))?;
            let req = parse_request(request_json)?;
            Ok(call(&mut doc, &req)?.to_string())
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
    use vcad_kernel_acoustics::instrument::{design_instrument, BarMaterial, InstrumentSpec};

    fn doc(material: BarMaterial) -> Document {
        let inst = design_instrument(&InstrumentSpec::new("C6", material).unwrap()).unwrap();
        let mut d = vcad_loon::eval_vcad(&inst.loon, None).expect("instrument loon evaluates");
        d.instrument = Some(instrument_doc::metadata(&inst));
        d
    }

    #[test]
    fn record_flips_receipt() {
        let mut d = doc(BarMaterial::Aluminium6061);
        let r = call(&mut d, &json!({"op": "receipt"})).unwrap();
        assert_eq!(r["bars"][0]["state"], "predicted");
        let r = call(&mut d, &json!({"op": "record", "measured_hz": 1047.0})).unwrap();
        assert_eq!(r["note"], "C6");
        assert_eq!(r["receipt"]["bars"][0]["state"], "measured_pass");
        assert_eq!(r["claim"]["basis"], "measured");
        // The patched document round-trips with the measurement.
        let back: Document = serde_json::from_value(r["document"].clone()).unwrap();
        assert!(back.instrument.unwrap().bars[0].measurement.is_some());
    }

    #[test]
    fn mill_dry_run_writes_verified_gcode() {
        let mut d = doc(BarMaterial::Aluminium6061);
        let dir = std::env::temp_dir().join(format!("vcad-glock-mill-{}", std::process::id()));
        let r = call(
            &mut d,
            &json!({"op": "mill", "note": "C6", "out_dir": dir.to_str().unwrap()}),
        )
        .unwrap();
        let setups = r["setups"].as_array().unwrap();
        assert_eq!(setups.len(), 2);
        for s in setups {
            let p = s["path"].as_str().unwrap();
            let g = std::fs::read_to_string(p).unwrap();
            assert!(g.lines().count() > 50, "{p}: {} lines", g.lines().count());
            assert!(g.contains("G1"));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn print_dry_run_slices_both_plates() {
        let mut d = doc(BarMaterial::Pla);
        for plate in ["bars", "frame"] {
            let path = std::env::temp_dir().join(format!("vcad-glock-{plate}-{}.gcode", std::process::id()));
            let r = call(
                &mut d,
                &json!({"op": "print", "plate": plate, "gcode_path": path.to_str().unwrap(),
                        "settings": {"printer": "bambu_p1s"}}),
            )
            .unwrap();
            assert_eq!(r["fitsBed"], true, "{plate}: {}", r["stats"]);
            assert!(std::fs::metadata(&path).unwrap().len() > 10_000);
            let _ = std::fs::remove_file(path);
        }
    }
}
