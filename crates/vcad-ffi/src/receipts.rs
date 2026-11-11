//! C ABI for live receipts (`vcad_receipts_verify`, `vcad_receipts_accept`).
//! The verifier itself lives in `vcad-receipt-verify`, shared with `vcad-mcp`.

use std::ffi::c_char;
use std::ptr;

pub use vcad_receipt_verify::{accept, verify, ReceiptCheck, ReceiptReport, ReceiptStatus};

use crate::analysis::{parse_doc, parse_request};
use crate::err;
use crate::json::{guard_ptr, json_out};

/// Re-verify a document's persisted assertions. Returns [`ReceiptReport`]
/// JSON (free with `vcad_string_free`) or null with `vcad_last_error`.
#[no_mangle]
pub extern "C" fn vcad_receipts_verify(doc_json: *const c_char) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_receipts_verify", || {
        match parse_doc("document", doc_json).and_then(|d| verify(&d)) {
            Ok(r) => json_out(&r),
            Err(e) => {
                err::set_error(e);
                ptr::null_mut()
            }
        }
    })
}

/// Re-baseline holding assertions. `request_json` (nullable):
/// `{"label"?: string}`. Returns `{"document": Document, "accepted": n}`.
#[no_mangle]
pub extern "C" fn vcad_receipts_accept(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_receipts_accept", || {
        let run = || -> Result<serde_json::Value, String> {
            let mut doc = parse_doc("document", doc_json)?;
            let req = parse_request(request_json)?;
            let n = accept(&mut doc, req["label"].as_str())?;
            Ok(serde_json::json!({ "document": doc, "accepted": n }))
        };
        match run() {
            Ok(v) => json_out(&v),
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
    use crate::json::take;
    use serde_json::{json, Value};
    use std::ffi::CString;

    fn doc(dx: f64) -> vcad_ir::Document {
        serde_json::from_value(json!({
            "version": "0.1",
            "nodes": {
                "1": {"id": 1, "name": "Box", "op": {"type": "Cube", "size": {"x": 10, "y": 10, "z": 10}}},
                "2": {"id": 2, "name": "Wedge", "op": {"type": "Cube", "size": {"x": 10, "y": 10, "z": 10}}},
                "3": {"id": 3, "name": null, "op": {"type": "Translate", "child": 2, "offset": {"x": dx, "y": 0, "z": 0}}}
            },
            "materials": {},
            "part_materials": {},
            "roots": [{"root": 1, "material": "default"}, {"root": 3, "material": "default"}],
            "clearance_specs": [{"label": "gap", "group_a": ["1"], "group_b": ["Wedge"], "min_mm": 2.0}]
        }))
        .unwrap()
    }

    #[test]
    fn c_abi_round_trip() {
        let dj = CString::new(serde_json::to_string(&doc(11.0)).unwrap()).unwrap();
        let out: Value = serde_json::from_str(&take(vcad_receipts_verify(dj.as_ptr()))).unwrap();
        assert_eq!(out["status"], "Violated");
        assert_eq!(out["checks"][0]["label"], "gap");

        let dj = CString::new(serde_json::to_string(&doc(13.0)).unwrap()).unwrap();
        let out: Value =
            serde_json::from_str(&take(vcad_receipts_accept(dj.as_ptr(), ptr::null()))).unwrap();
        assert_eq!(out["accepted"], 1);
        assert!(out["document"]["clearance_specs"][0]["baseline_mm"].is_number());
    }
}
