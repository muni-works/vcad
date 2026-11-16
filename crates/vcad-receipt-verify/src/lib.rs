//! Live receipt verification (used by the Mac app via `vcad-ffi` and by
//! `vcad-mcp`): re-measure every persisted clearance assertion
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
//!
//! **Fit mode** (`ClearanceSpec::is_fit`: `max_mm` set, or
//! `fit: "interference"`): press / transition fits. The gap is the signed
//! analytic radial gap between a coaxial shaft/bore pair, measured from the
//! BRep (`vcad_eval::sim_extract::tolerance::signed_radial_gap`), and must
//! lie in `[min_mm, max_mm]` — interference down to a negative `min_mm` is
//! allowed. Geometry the analytic measurement can't handle reads Violated
//! with an **unverifiable** receipt claim, never Holds. Assertions without
//! either field keep the classic semantics above, unchanged.

#![warn(missing_docs)]

use serde::Serialize;
use vcad_kernel::Solid;
use vcad_receipt::mechanical::{clearance_claims, ClearanceClaim, CONTACT_EPS_MM, DOMAIN};
use vcad_receipt::{DesignReceipt, OracleRef, ReceiptClaim};

pub mod bodies;

use bodies::{evaluate, keyed_bodies};

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
    // Fit-mode assertions need BRep solids (the classic path's scene may
    // come from the mesh cache), so they get their own evaluation — only
    // when a document actually has one.
    let fit_scene = if specs.iter().any(|s| s.is_fit()) {
        Some(
            vcad_eval::evaluate_document(doc, &fit_eval_options())
                .map_err(|e| format!("evaluation for fit assertions failed: {e}")),
        )
    } else {
        None
    };
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
        if spec.is_fit() {
            let scene = fit_scene
                .as_ref()
                .expect("evaluated when a fit spec exists");
            let (check, claim) = verify_fit(doc, scene, spec, check);
            claims.push(claim);
            checks.push(check);
            continue;
        }
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

fn fit_eval_options() -> vcad_eval::EvalOptions {
    vcad_eval::EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
        on_root_ready: None,
    }
}

/// Fit-mode verification: signed analytic radial gap in `[min, max]`.
fn verify_fit(
    doc: &vcad_ir::Document,
    scene: &Result<vcad_eval::EvaluatedScene, String>,
    spec: &vcad_ir::ClearanceSpec,
    mut check: ReceiptCheck,
) -> (ReceiptCheck, ReceiptClaim) {
    let description = match spec.max_mm {
        Some(mx) => format!(
            "fit \"{}\" gap within [{}, {}] mm",
            spec.label, spec.min_mm, mx
        ),
        None => format!(
            "fit \"{}\" gap at least {} mm (interference allowed)",
            spec.label, spec.min_mm
        ),
    };
    // Fits are close by definition: candidates within 1 mm of the band.
    let window = 1.0 + spec.min_mm.abs().max(spec.max_mm.map_or(0.0, f64::abs));
    let measured = scene.as_ref().map_err(|e| e.clone()).and_then(|sc| {
        vcad_eval::sim_extract::tolerance::signed_radial_gap(
            doc,
            sc,
            &spec.label,
            &spec.group_a,
            &spec.group_b,
            window,
        )
        .map_err(|e| e.to_string())
    });
    let g = match measured {
        Ok(g) => g,
        Err(e) => {
            let why = format!("fit gap is not analytically measurable: {e}");
            check.reason = Some(why.clone());
            let claim =
                ReceiptClaim::unverifiable(&check.claim_id, DOMAIN, description, oracle(), why)
                    .with_subject(check.subject.clone());
            return (check, claim);
        }
    };
    let ok = g.gap_mm >= spec.min_mm && spec.max_mm.is_none_or(|mx| g.gap_mm <= mx);
    check.measured_mm = Some(g.gap_mm);
    check.worst_pair = Some(g.parts.clone());
    let swept = spec.sweep.as_ref().is_some_and(|s| !s.is_empty());
    (check.status, check.reason) = if !ok {
        (
            ReceiptStatus::Violated,
            Some(match spec.max_mm {
                Some(mx) => format!(
                    "fit gap {:.4} mm is outside [{}, {}] mm",
                    g.gap_mm, spec.min_mm, mx
                ),
                None => format!(
                    "fit gap {:.4} mm is below the allowed {} mm",
                    g.gap_mm, spec.min_mm
                ),
            }),
        )
    } else if swept {
        (
            ReceiptStatus::Stale,
            Some("fit holds at the current pose; the swept range is not re-checked live".into()),
        )
    } else if spec
        .baseline_mm
        .is_some_and(|b| (b - g.gap_mm).abs() > STALE_EPS_MM)
    {
        (
            ReceiptStatus::Stale,
            Some("geometry changed since the receipt was accepted, but it still holds".into()),
        )
    } else {
        (ReceiptStatus::Holds, None)
    };
    let mut cc = ClearanceClaim::new(
        &spec.label,
        spec.group_a.clone(),
        spec.group_b.clone(),
        spec.min_mm,
        g.gap_mm,
    );
    cc = cc.with_analytic_fit();
    if let Some(mx) = spec.max_mm {
        cc = cc.with_max(mx);
    }
    cc.holds = ok;
    cc.poses_checked = Some(1);
    let claim = clearance_claims(&[cc], &oracle())
        .pop()
        .expect("one assertion in, one claim out");
    (check, claim)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    // ---- fit mode ---------------------------------------------------------

    /// A Ø70 rim with a Ø60 × 6 socket at z 14–20 (root 4) and a lid whose
    /// Ø`2·lid_r` plug sits in the socket under a Ø70 flange (root 9).
    fn fit_doc(lid_r: f64, spec: serde_json::Value) -> vcad_ir::Document {
        let cyl = |id: u32, r: f64, h: f64| json!({"id": id, "name": null, "op": {"type": "Cylinder", "radius": r, "height": h, "segments": 0}});
        let tr = |id: u32, child: u32, z: f64| json!({"id": id, "name": null, "op": {"type": "Translate", "child": child, "offset": {"x": 0, "y": 0, "z": z}}});
        serde_json::from_value(json!({
            "version": "0.1",
            "nodes": {
                "1": cyl(1, 35.0, 20.0),
                "2": cyl(2, 30.0, 7.0),
                "3": tr(3, 2, 14.0),
                "4": {"id": 4, "name": "rim", "op": {"type": "Difference", "left": 1, "right": 3}},
                "5": cyl(5, lid_r, 6.5),
                "6": tr(6, 5, 14.0),
                "7": cyl(7, 35.0, 2.0),
                "8": tr(8, 7, 20.0),
                "9": {"id": 9, "name": "lid", "op": {"type": "Union", "left": 6, "right": 8}}
            },
            "materials": {},
            "part_materials": {},
            "roots": [{"root": 4, "material": "default"}, {"root": 9, "material": "default"}],
            "clearance_specs": [spec]
        }))
        .unwrap()
    }

    fn press(max: Option<f64>, fit: Option<&str>) -> serde_json::Value {
        let mut s =
            json!({"label": "lid-fit", "group_a": ["lid"], "group_b": ["rim"], "min_mm": -0.15});
        if let Some(m) = max {
            s["max_mm"] = json!(m);
        }
        if let Some(f) = fit {
            s["fit"] = json!(f);
        }
        s
    }

    /// The legacy contract, pinned: with neither `max_mm` nor `fit`, a
    /// negative `min_mm` does NOT allow interference — the interpenetrating
    /// press fit reads Violated exactly as before, and the spec serializes
    /// without the new keys.
    #[test]
    fn bare_negative_min_keeps_legacy_semantics() {
        let d = fit_doc(30.075, press(None, None));
        assert!(!d.clearance_specs[0].is_fit());
        let r = verify(&d).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Violated), "{r:#?}");
        assert!(r.checks[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("interpenetrate"));
        let v = serde_json::to_value(&d.clearance_specs[0]).unwrap();
        assert!(v.get("max_mm").is_none() && v.get("fit").is_none());
        // And the classic box/wedge receipts are untouched by fit support.
        assert_eq!(
            verify(&doc(13.0)).unwrap().status,
            Some(ReceiptStatus::Holds)
        );
    }

    #[test]
    fn press_fit_in_band_holds_with_the_analytic_gap() {
        for (max, fit) in [(Some(-0.01), None), (None, Some("interference"))] {
            let d = fit_doc(30.075, press(max, fit));
            let r = verify(&d).unwrap();
            assert_eq!(r.status, Some(ReceiptStatus::Holds), "{r:#?}");
            let c = &r.checks[0];
            assert!((c.measured_mm.unwrap() + 0.075).abs() < 1e-9, "{c:?}");
            assert_eq!(
                c.worst_pair.as_ref().unwrap(),
                &["lid".to_string(), "rim".into()]
            );
            let claim = &r.receipt.claims[0];
            assert_eq!(claim.verdict, vcad_receipt::ClaimVerdict::Pass);
            let det: ClearanceClaim =
                serde_json::from_str(claim.details.as_ref().unwrap()).unwrap();
            assert_eq!(det.max_mm, max);
            assert_eq!(det.analytic_fit, Some(true));
        }
    }

    #[test]
    fn press_fit_out_of_band_is_violated_both_ways() {
        // Too tight: 0.2 mm interference < −0.15.
        let r = verify(&fit_doc(30.2, press(Some(-0.01), None))).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Violated));
        assert!(r.checks[0].reason.as_ref().unwrap().contains("outside"));
        assert_eq!(
            r.receipt.claims[0].verdict,
            vcad_receipt::ClaimVerdict::Fail
        );
        // Too loose: 0.005 mm interference rattles (> −0.01).
        let r = verify(&fit_doc(30.005, press(Some(-0.01), None))).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Violated));
    }

    /// Geometry the analytic gap can't measure (box vs wedge: no coaxial
    /// cylinders) fails closed — unverifiable, never Pass.
    #[test]
    fn unmeasurable_fit_fails_closed_unverifiable() {
        let mut d = doc(13.0);
        d.clearance_specs[0].max_mm = Some(5.0);
        let r = verify(&d).unwrap();
        assert_eq!(r.status, Some(ReceiptStatus::Violated));
        assert!(r.checks[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("not analytically measurable"));
        assert_eq!(
            r.receipt.claims[0].verdict,
            vcad_receipt::ClaimVerdict::Unverifiable
        );
    }
}
