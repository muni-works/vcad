//! The claims ledger survives a Rust round trip of the document (the Mac app
//! saves through `vcad_ir::Document`; a field it didn't know would be dropped
//! silently, losing every frozen prediction and measurement).

use vcad_ir::Document;
use vcad_receipt::ledger::{AcceptanceBand, LEDGER_SCHEMA};

#[test]
fn ledger_round_trips_through_document() {
    let mut json = serde_json::to_value(Document::new()).unwrap();
    json["claims_ledger"] = serde_json::json!({
        "schema": LEDGER_SCHEMA,
        "entries": [{
            "claim": {
                "id": "thermal.t_max_c", "domain": "thermal", "description": "T_max",
                "oracle": {"id": "vcad-kernel-thermal/solve", "version": "0.11.0"},
                "verdict": "provisional", "basis": "predicted",
                "measured": {"value": 58.4, "unit": "C"}
            },
            "predicted": {"value": 58.4, "unit": "C"},
            "prediction_sha256": "aa",
            "document_fingerprint": "bb",
            "source_tool": "solve_thermal",
            "measurement": {
                "value": 62.0, "unit": "C", "uncertainty": 1.1,
                "instrument": "type-K", "band": {"kind": "factor", "band_factor": 1.25},
                "raw_sha256": "cc", "prediction_sha256": "aa"
            }
        }],
        "claim_sets": [{"schema": "vcad.thermal-claims/1", "json": "{}"}]
    });
    let doc: Document = serde_json::from_value(json).unwrap();
    let ledger = doc.claims_ledger.as_ref().expect("ledger parsed");
    let m = ledger.entries[0].measurement.as_ref().unwrap();
    assert_eq!(m.band, AcceptanceBand::Factor { band_factor: 1.25 });
    let back = serde_json::to_value(&doc).unwrap();
    let again: Document = serde_json::from_value(back.clone()).unwrap();
    assert_eq!(again.claims_ledger, doc.claims_ledger);
    assert_eq!(
        back["claims_ledger"]["entries"][0]["source_tool"],
        "solve_thermal"
    );

    // Absent ledger stays absent on the wire.
    let bare = serde_json::to_value(Document::new()).unwrap();
    assert!(bare.get("claims_ledger").is_none());
}
