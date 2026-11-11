//! Live receipt verification for the native app (`vcad_receipts_verify`,
//! `vcad_receipts_accept`): re-measure every persisted clearance assertion
//! (`Document::clearance_specs`) against the current geometry and classify
//! each as Holds / Stale / Violated — the native twin of the MCP server's
//! `build_receipt` + `verify_receipt` for `mech.clearance.*` claims.
//!
//! The app calls this after every edit, so nudging a part below an
//! assertion's `min_mm` turns its badge red on the next evaluation. The
//! result also carries a unified [`DesignReceipt`] (same claim ids, oracle,
//! and `details` payload the TypeScript server emits), so an MCP tool can
//! return it verbatim as structured output.
//!
//! Classification, per assertion (fail-closed, worst wins in the rollup):
//! - **Violated** — the measurement is below `min_mm` (allowed contact
//!   honoured, interpenetration never allowed), or a named group resolves to
//!   no part / the distance can't be measured. An assertion that can no
//!   longer be checked never reads green.
//! - **Stale** — it still holds, but measures a different distance than its
//!   accepted `baseline_mm`; or it is a swept assertion, which this native
//!   path does not re-sweep yet (gray, never green: re-verify with the MCP
//!   `verify_receipt`).
//! - **Holds** — it holds and matches its baseline (or has none yet).

use std::ffi::c_char;
use std::ptr;

use serde::Serialize;
use vcad_kernel::Solid;
use vcad_receipt::mechanical::{clearance_claims, ClearanceClaim, CONTACT_EPS_MM, DOMAIN};
use vcad_receipt::{DesignReceipt, OracleRef, ReceiptClaim};

use crate::analysis::{evaluate, parse_doc, parse_request};
use crate::clearance::keyed_bodies;
use crate::err;
use crate::json::{guard_ptr, json_out};

/// Claim-id prefix shared with the TS server and `vcad-receipt`.
pub const CLEARANCE_CLAIM_PREFIX: &str = "mech.clearance.";

/// Re-measured distances within this of the baseline are "same geometry".
/// Looser than the TS server's 1e-6 because the app re-tessellates on every
/// edit; 1 µm is still far below any manufacturable change.
pub const STALE_EPS_MM: f64 = 1e-3;

/// Per-assertion receipt status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum ReceiptStatus {
    /// Holds and matches its accepted baseline.
    Holds,
    /// Still holds but drifted from its baseline, or not re-checkable here.
    Stale,
    /// Fails, or can no longer be measured.
    Violated,
}

/// One re-verified assertion — what a badge draws.
#[derive(Debug, Clone, Serialize)]
pub struct ReceiptCheck {
    /// Assertion label (claim id is `mech.clearance.<label>`).
    pub label: String,
    /// Claim id.
    pub claim_id: String,
    /// Status.
    pub status: ReceiptStatus,
    /// `"<group_a> vs <group_b>"`.
    pub subject: String,
    /// Required minimum separation, mm.
    pub required_mm: f64,
    /// Measured minimum distance now, mm (negative = penetration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured_mm: Option<f64>,
    /// Accepted baseline, mm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_mm: Option<f64>,
    /// The closest pair of parts, by display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worst_pair: Option<[String; 2]>,
    /// Closest points on the worst pair (for drawing a leader in the viewport).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<[[f64; 3]; 2]>,
    /// Why it isn't Holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Whole-document verification.
#[derive(Debug, Clone, Serialize)]
pub struct ReceiptReport {
    /// Worst status across checks; `None` when the document asserts nothing.
    pub status: Option<ReceiptStatus>,
    /// One check per persisted assertion, in document order.
    pub checks: Vec<ReceiptCheck>,
    /// The unified receipt (machine-checkable structured output).
    pub receipt: DesignReceipt,
}

fn oracle() -> OracleRef {
    OracleRef::new("vcad-kernel/mesh-clearance", env!("CARGO_PKG_VERSION"))
}

struct Measured {
    distance: f64,
    intersecting: bool,
    pair: [String; 2],
    points: [[f64; 3]; 2],
}

fn holds(m: &Measured, min_mm: f64, allow_contact: bool) -> bool {
    if m.intersecting && m.distance < -CONTACT_EPS_MM {
        return false;
    }
    if m.intersecting && !allow_contact {
        return false;
    }
    m.distance >= min_mm || (allow_contact && m.distance.abs() <= CONTACT_EPS_MM)
}

/// Re-verify every persisted clearance assertion of `doc`.
pub fn verify(doc: &vcad_ir::Document) -> Result<ReceiptReport, String> {
    let specs = &doc.clearance_specs;
    if specs.is_empty() {
        return Ok(ReceiptReport {
            status: None,
            checks: Vec::new(),
            receipt: DesignReceipt::with_claims(Vec::new()),
        });
    }
    let scene = evaluate(doc)?;
    let bodies = keyed_bodies(doc, &scene);
    let select = |ids: &[String]| -> Result<Vec<usize>, String> {
        let mut out = Vec::new();
        for id in ids {
            let hit: Vec<usize> = bodies
                .iter()
                .enumerate()
                .filter(|(_, b)| b.keys.iter().any(|k| k == id))
                .map(|(i, _)| i)
                .collect();
            if hit.is_empty() {
                return Err(format!("part \"{id}\" is not in the document"));
            }
            out.extend(hit);
        }
        Ok(out)
    };
    let measure = |a: &[String], b: &[String]| -> Result<Measured, String> {
        let (ga, gb) = (select(a)?, select(b)?);
        let mut best: Option<Measured> = None;
        for &i in &ga {
            for &j in &gb {
                if i == j {
                    continue;
                }
                let solid = |k: usize| -> Result<&Solid, String> {
                    bodies[k]
                        .solid
                        .as_ref()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| format!("part \"{}\" has no solid", bodies[k].name))
                };
                let Some(c) = solid(i)?.clearance(solid(j)?) else {
                    return Err(format!(
                        "distance between \"{}\" and \"{}\" could not be measured",
                        bodies[i].name, bodies[j].name
                    ));
                };
                if best.as_ref().is_none_or(|m| c.distance < m.distance) {
                    best = Some(Measured {
                        distance: c.distance,
                        intersecting: c.intersecting,
                        pair: [bodies[i].name.clone(), bodies[j].name.clone()],
                        points: [c.point_a, c.point_b],
                    });
                }
            }
        }
        best.ok_or_else(|| "groups share every part; nothing to measure".to_string())
    };

    let mut checks = Vec::with_capacity(specs.len());
    let mut claims: Vec<ReceiptClaim> = Vec::with_capacity(specs.len());
    for spec in specs {
        let claim_id = format!("{CLEARANCE_CLAIM_PREFIX}{}", spec.label);
        let subject = format!("{} vs {}", spec.group_a.join("+"), spec.group_b.join("+"));
        let description = format!("clearance \"{}\" at least {} mm", spec.label, spec.min_mm);
        let allow_contact = spec.allow_contact == Some(true);
        let mut check = ReceiptCheck {
            label: spec.label.clone(),
            claim_id: claim_id.clone(),
            status: ReceiptStatus::Violated,
            subject: subject.clone(),
            required_mm: spec.min_mm,
            measured_mm: None,
            baseline_mm: spec.baseline_mm,
            worst_pair: None,
            points: None,
            reason: None,
        };
        let m = match measure(&spec.group_a, &spec.group_b) {
            Ok(m) => m,
            Err(e) => {
                check.reason = Some(e.clone());
                claims.push(
                    ReceiptClaim::unverifiable(&claim_id, DOMAIN, description, oracle(), e)
                        .with_subject(subject),
                );
                checks.push(check);
                continue;
            }
        };
        let ok = holds(&m, spec.min_mm, allow_contact);
        check.measured_mm = Some(m.distance);
        check.points = Some(m.points);
        check.worst_pair = Some(m.pair.clone());
        let swept = spec.sweep.as_ref().is_some_and(|s| !s.is_empty());
        (check.status, check.reason) = if !ok {
            (
                ReceiptStatus::Violated,
                Some(if m.intersecting {
                    format!("\"{}\" and \"{}\" interpenetrate", m.pair[0], m.pair[1])
                } else {
                    format!(
                        "measured {:.3} mm is below the required {} mm",
                        m.distance, spec.min_mm
                    )
                }),
            )
        } else if swept {
            (
                ReceiptStatus::Stale,
                Some(
                    "holds at the current pose; the swept range is not re-checked live — run verify_receipt"
                        .into(),
                ),
            )
        } else if spec
            .baseline_mm
            .is_some_and(|b| (b - m.distance).abs() > STALE_EPS_MM)
        {
            (
                ReceiptStatus::Stale,
                Some("geometry changed since the receipt was accepted, but it still holds".into()),
            )
        } else {
            (ReceiptStatus::Holds, None)
        };
        let mut claim = ClearanceClaim::new(
            &spec.label,
            spec.group_a.clone(),
            spec.group_b.clone(),
            spec.min_mm,
            m.distance,
        );
        if allow_contact {
            claim = claim.with_allow_contact();
        }
        claim.holds = ok;
        claim.poses_checked = Some(1);
        claims.extend(clearance_claims(&[claim], &oracle()));
        checks.push(check);
    }
    Ok(ReceiptReport {
        status: checks.iter().map(|c| c.status).max(),
        checks,
        receipt: DesignReceipt::with_claims(claims),
    })
}

/// Accept the current measurement as the baseline for `label` (every
/// assertion when `None`). Only assertions that currently hold are
/// re-baselined — accepting never launders a violation into green.
pub fn accept(doc: &mut vcad_ir::Document, label: Option<&str>) -> Result<usize, String> {
    let report = verify(doc)?;
    let mut n = 0;
    for (spec, check) in doc.clearance_specs.iter_mut().zip(&report.checks) {
        if label.is_some_and(|l| l != spec.label) || check.status == ReceiptStatus::Violated {
            continue;
        }
        if let Some(m) = check.measured_mm {
            spec.baseline_mm = Some(m);
            n += 1;
        }
    }
    Ok(n)
}

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

    /// Box (root 1) at the origin, Wedge (root 3) placed `dx` along x, and
    /// one assertion: at least 2 mm between them.
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

    /// Move the Wedge, as a viewport drag writes it.
    fn nudge(d: &mut vcad_ir::Document, dx: f64) {
        let node = d.nodes.get_mut(&3).unwrap();
        if let vcad_ir::CsgOp::Translate { offset, .. } = &mut node.op {
            offset.x = dx;
        } else {
            panic!("node 3 is not a Translate");
        }
    }

    #[test]
    fn holding_assertion_reads_holds_with_receipt_claim() {
        let r = verify(&doc(13.0)).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Holds));
        let c = &r.checks[0];
        assert!((c.measured_mm.unwrap() - 3.0).abs() < 1e-3, "{c:?}");
        assert_eq!(
            c.worst_pair.as_ref().unwrap(),
            &["Box".to_string(), "Wedge".into()]
        );
        let claim = &r.receipt.claims[0];
        assert_eq!(claim.id, "mech.clearance.gap");
        assert_eq!(claim.verdict, vcad_receipt::ClaimVerdict::Pass);
        // details carries the re-verifiable ClearanceClaim, like the TS server.
        let d: ClearanceClaim = serde_json::from_str(claim.details.as_ref().unwrap()).unwrap();
        assert!(d.holds && d.required_mm == 2.0);
    }

    /// The M2 contract: an edit re-verifies, and nudging a part inside the
    /// assertion turns it Violated immediately; nudging within spec after
    /// accepting reads Stale; accepting again reads Holds.
    #[test]
    fn re_verify_on_edit_goes_red_then_stale_then_holds() {
        let mut d = doc(13.0);
        assert_eq!(accept(&mut d, None).unwrap(), 1);
        assert!((d.clearance_specs[0].baseline_mm.unwrap() - 3.0).abs() < 1e-3);
        assert_eq!(verify(&d).unwrap().status, Some(ReceiptStatus::Holds));

        nudge(&mut d, 11.0); // 1 mm gap < 2 mm required
        let r = verify(&d).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Violated), "{r:#?}");
        assert!(r.checks[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("below the required"));
        assert_eq!(
            r.receipt.claims[0].verdict,
            vcad_receipt::ClaimVerdict::Fail
        );
        // Accepting never launders a violation.
        assert_eq!(accept(&mut d, None).unwrap(), 0);

        nudge(&mut d, 6.0); // interpenetrating
        let r = verify(&d).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Violated));

        nudge(&mut d, 15.0); // 5 mm: holds, but drifted from the 3 mm baseline
        let r = verify(&d).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Stale), "{r:#?}");

        assert_eq!(accept(&mut d, Some("gap")).unwrap(), 1);
        assert_eq!(verify(&d).unwrap().status, Some(ReceiptStatus::Holds));
    }

    #[test]
    fn missing_part_fails_closed() {
        let mut d = doc(13.0);
        d.clearance_specs[0].group_b = vec!["ghost".into()];
        let r = verify(&d).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Violated));
        assert_eq!(
            r.receipt.claims[0].verdict,
            vcad_receipt::ClaimVerdict::Unverifiable
        );
    }

    #[test]
    fn no_assertions_is_no_status() {
        let mut d = doc(13.0);
        d.clearance_specs.clear();
        assert_eq!(verify(&d).unwrap().status, None);
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
