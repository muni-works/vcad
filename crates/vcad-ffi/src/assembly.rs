//! Assembly checks for the native app: declarative mates re-verified against
//! the posed assembly (`vcad_assembly_check_mates`).
//!
//! Posing is `vcad_kernel_assembly::pose_document` — forward kinematics where
//! the document has a joint graph, instance transforms otherwise — the same
//! rule the viewport draws with, so a mate verdict and what is on screen
//! cannot disagree. A mate never moves anything; a failing one is a modelling
//! error to fix in the document.

use std::ffi::c_char;

use serde::Serialize;

use crate::analysis::parse_doc;
use crate::err;
use crate::json::{guard_ptr, json_out};

/// One mate's verdict.
#[derive(Debug, Clone, Serialize)]
pub struct MateVerdict {
    /// Mate id.
    pub id: String,
    /// Kind label (`coaxial`, `planar-offset`, `pattern-phase`).
    pub kind: String,
    /// Instance ids, in the mate's order.
    pub instances: [String; 2],
    /// Whether the assertion holds (false when `error` is set).
    pub pass: bool,
    /// Measured value.
    pub measured: f64,
    /// Asserted value.
    pub expected: f64,
    /// Tolerance used.
    pub tolerance: f64,
    /// `mm` or `deg`.
    pub unit: String,
    /// Human-readable explanation.
    pub detail: String,
    /// Set when the mate could not be checked at all (unknown instance, bad
    /// parameter) — the document is malformed, not merely wrong.
    pub error: Option<String>,
}

/// Check every mate in `doc`, one verdict per mate in document order. A mate
/// that cannot be checked yields a verdict with `error` set rather than
/// failing the whole report, so one typo doesn't hide the other verdicts.
pub fn check_document_mates(doc: &vcad_ir::Document) -> Result<Vec<MateVerdict>, String> {
    if doc.mates.is_empty() {
        return Ok(Vec::new());
    }
    let posed = vcad_kernel_assembly::pose_document(doc).map_err(|e| format!("pose: {e}"))?;
    Ok(doc
        .mates
        .iter()
        .map(|m| match vcad_kernel_assembly::check_mate(&posed, m) {
            Ok(c) => MateVerdict {
                id: c.id,
                kind: c.kind.to_string(),
                instances: [c.instances.0, c.instances.1],
                pass: c.pass,
                measured: c.measured,
                expected: c.expected,
                tolerance: c.tolerance,
                unit: c.unit.to_string(),
                detail: c.detail,
                error: None,
            },
            Err(e) => MateVerdict {
                id: m.id.clone(),
                kind: m.kind.label().to_string(),
                instances: [m.instance_a.clone(), m.instance_b.clone()],
                pass: false,
                measured: 0.0,
                expected: 0.0,
                tolerance: 0.0,
                unit: String::new(),
                detail: e.to_string(),
                error: Some(e.to_string()),
            },
        })
        .collect())
}

/// Check the mates of a `.vcad` document (UTF-8 JSON, NUL-terminated).
/// Returns a JSON array of verdicts (empty when the document has no mates);
/// null + `vcad_last_error` when the document doesn't parse or can't be posed.
/// Free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_assembly_check_mates(doc_json: *const c_char) -> *mut c_char {
    crate::err::clear_error();
    guard_ptr("assembly check mates", || {
        let res = parse_doc("assembly doc", doc_json).and_then(|d| check_document_mates(&d));
        match res {
            Ok(v) => json_out(&v),
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

    fn doc(offset_z: f64, mate_offset: f64) -> vcad_ir::Document {
        let json = serde_json::json!({
            "version": "0.1",
            "nodes": {
                "1": {"id": 1, "name": null, "op": {"type": "Cube", "size": {"x": 10.0, "y": 10.0, "z": 10.0}}}
            },
            "materials": {},
            "part_materials": {},
            "roots": [],
            "partDefs": {"cube": {"id": "cube", "root": 1}},
            "instances": [
                {"id": "a", "partDefId": "cube"},
                {"id": "b", "partDefId": "cube", "transform": {
                    "translation": {"x": 0.0, "y": 0.0, "z": offset_z},
                    "rotation": {"x": 0.0, "y": 0.0, "z": 0.0},
                    "scale": {"x": 1.0, "y": 1.0, "z": 1.0}}}
            ],
            "mates": [
                {"id": "stack", "instanceA": "a", "instanceB": "b",
                 "kind": {"type": "PlanarOffset", "axis": {"x": 0.0, "y": 0.0, "z": 1.0}, "offset": mate_offset}},
                {"id": "typo", "instanceA": "a", "instanceB": "nope",
                 "kind": {"type": "Coaxial", "axis": {"x": 0.0, "y": 0.0, "z": 1.0}}}
            ]
        });
        serde_json::from_value(json).expect("doc parses")
    }

    #[test]
    fn planar_offset_passes_and_fails_from_the_pose() {
        let ok = check_document_mates(&doc(10.0, 10.0)).unwrap();
        assert_eq!(ok.len(), 2);
        assert!(ok[0].pass, "{}", ok[0].detail);
        let bad = check_document_mates(&doc(12.0, 10.0)).unwrap();
        assert!(!bad[0].pass, "{}", bad[0].detail);
        assert!(
            (bad[0].measured - 12.0).abs() < 1e-6,
            "measured {}",
            bad[0].measured
        );
    }

    #[test]
    fn unknown_instance_is_an_error_verdict_not_a_failure_of_the_report() {
        let v = check_document_mates(&doc(10.0, 10.0)).unwrap();
        assert!(!v[1].pass);
        assert!(v[1].error.as_deref().unwrap().contains("nope"));
    }

    #[test]
    fn ffi_round_trip() {
        let s = serde_json::to_string(&doc(10.0, 10.0)).unwrap();
        let c = std::ffi::CString::new(s).unwrap();
        let out = vcad_assembly_check_mates(c.as_ptr());
        assert!(!out.is_null());
        let text = unsafe { std::ffi::CStr::from_ptr(out) }
            .to_str()
            .unwrap()
            .to_string();
        crate::json::vcad_string_free(out);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["pass"], true);
    }
}
