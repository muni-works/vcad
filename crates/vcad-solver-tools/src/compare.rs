//! `compare_measurements`: bind bench readings to a predicted claim set
//! through the crates' own `receipt::compare` (the verdict is theirs, not
//! recomputed here) and return each row's acceptance window so a plot can
//! draw exactly the interval the verdict used.

use serde::Deserialize;
use serde_json::{json, Value};
use vcad_kernel_neutronics as nk;
use vcad_kernel_particle as pk;
use vcad_receipt::{DesignReceipt, ReceiptVerdict};

use crate::ToolError;

/// Default multiplicative acceptance band when a reading omits one.
pub const DEFAULT_BAND: f64 = 2.0;

#[derive(Deserialize)]
struct Reading {
    name: String,
    value: f64,
    #[serde(default)]
    uncertainty: f64,
    #[serde(default)]
    instrument: String,
    #[serde(default)]
    band_factor: Option<f64>,
}

#[derive(Deserialize)]
struct Args {
    claim_set: Value,
    measurements: Vec<Reading>,
}

fn verdict_str(v: ReceiptVerdict) -> &'static str {
    match v {
        ReceiptVerdict::Pass => "pass",
        ReceiptVerdict::Provisional => "provisional",
        ReceiptVerdict::Fail => "fail",
        ReceiptVerdict::Unverifiable => "unverifiable",
    }
}

fn check_readings(rs: &[Reading], names: &[String]) -> Result<(), ToolError> {
    for r in rs {
        if !names.contains(&r.name) {
            return Err(ToolError::Invalid(format!(
                "measurement {:?} matches no claim; claims in this set: {}",
                r.name,
                names.join(", ")
            )));
        }
        let band = r.band_factor.unwrap_or(DEFAULT_BAND);
        let band_ok = band.is_finite() && band >= 1.0;
        if !band_ok {
            return Err(ToolError::Invalid(format!(
                "measurement {:?}: band_factor must be ≥ 1 (got {band})",
                r.name
            )));
        }
        let reading_ok = r.value.is_finite() && r.uncertainty >= 0.0;
        if !reading_ok {
            return Err(ToolError::Invalid(format!(
                "measurement {:?}: value must be finite and uncertainty ≥ 0",
                r.name
            )));
        }
    }
    Ok(())
}

/// `compare_measurements`.
pub fn compare(args: &Value) -> Result<Value, ToolError> {
    let a: Args =
        serde_json::from_value(args.clone()).map_err(|e| ToolError::Invalid(format!("{e}")))?;
    let schema = a.claim_set["schema"].as_str().unwrap_or("").to_string();
    if a.measurements.is_empty() {
        return Err(ToolError::Invalid(
            "at least one measurement is required".into(),
        ));
    }
    match schema.as_str() {
        pk::receipt::CLAIM_SCHEMA => particle(a),
        nk::receipt::CLAIM_SCHEMA => neutronics(a),
        other => Err(ToolError::Invalid(format!(
            "claim_set schema {other:?} is not one compare_measurements binds (expected {} or {}); pass the `claim_set` object from the simulation result unchanged",
            pk::receipt::CLAIM_SCHEMA,
            nk::receipt::CLAIM_SCHEMA
        ))),
    }
}

/// Rows with the window each verdict used, plus both rollups.
fn finish(
    domain: &str,
    report: Value,
    rows: Vec<Value>,
    measured: Vec<vcad_receipt::ReceiptClaim>,
    unmeasured_predicted: Vec<vcad_receipt::ReceiptClaim>,
) -> Value {
    let measured_verdict = DesignReceipt::with_claims(measured.clone()).verdict();
    let mut all = measured.clone();
    all.extend(unmeasured_predicted);
    let receipt_verdict = DesignReceipt::with_claims(all).verdict();
    let holds = rows.iter().filter(|r| r["verdict"] == "Holds").count();
    let violated = rows.iter().filter(|r| r["verdict"] == "Violated").count();
    let unmeasured = rows.iter().filter(|r| r["verdict"] == "Unmeasured").count();
    json!({
        "domain": domain,
        "report": report,
        "rows": rows,
        "counts": {"holds": holds, "violated": violated, "unmeasured": unmeasured},
        "measured_verdict": verdict_str(measured_verdict),
        "receipt_verdict": verdict_str(receipt_verdict),
        "receipt_claims": serde_json::to_value(&measured).unwrap_or(Value::Null),
        "rule": "Holds when measured ∈ [predicted/band − σ_meas (− σ_MC), predicted·band + σ_meas (+ σ_MC)]; Unmeasured never passes. measured_verdict rolls up the bound readings alone; receipt_verdict adds every unmeasured prediction (Provisional while any remain).",
    })
}

fn particle(a: Args) -> Result<Value, ToolError> {
    let set: pk::receipt::ClaimSet = serde_json::from_value(a.claim_set)
        .map_err(|e| ToolError::Invalid(format!("bad particle claim_set: {e}")))?;
    let names: Vec<String> = set.claims.iter().map(|c| c.name.clone()).collect();
    check_readings(&a.measurements, &names)?;
    let ms: Vec<pk::receipt::Measurement> = a
        .measurements
        .iter()
        .map(|r| pk::receipt::Measurement {
            name: r.name.clone(),
            value: r.value,
            uncertainty: r.uncertainty,
            instrument: r.instrument.clone(),
            band_factor: r.band_factor.unwrap_or(DEFAULT_BAND),
        })
        .collect();
    let report = pk::receipt::compare(&set, &ms).map_err(ToolError::Invalid)?;
    let rows = report
        .entries
        .iter()
        .map(|e| {
            let c = set.claims.iter().find(|c| c.name == e.name);
            let m = ms.iter().find(|m| m.name == e.name);
            let mut row = json!({
                "name": e.name, "unit": c.map(|c| c.unit.clone()),
                "predicted": e.predicted, "measured": e.measured, "ratio": e.ratio,
                "verdict": format!("{:?}", e.verdict),
            });
            if let Some(m) = m {
                row["acceptance"] = json!([
                    e.predicted / m.band_factor - m.uncertainty,
                    e.predicted * m.band_factor + m.uncertainty
                ]);
                row["band_factor"] = json!(m.band_factor);
                row["uncertainty"] = json!(m.uncertainty);
                row["instrument"] = json!(m.instrument);
            }
            row
        })
        .collect();
    let measured = pk::receipt::comparison_claims(&report, &set, &ms);
    let unmeasured_set = pk::receipt::ClaimSet {
        claims: set
            .claims
            .iter()
            .filter(|c| !ms.iter().any(|m| m.name == c.name))
            .cloned()
            .collect(),
        ..set.clone()
    };
    Ok(finish(
        pk::receipt::RECEIPT_DOMAIN,
        serde_json::to_value(&report).unwrap_or(Value::Null),
        rows,
        measured,
        pk::receipt::design_claims(&unmeasured_set),
    ))
}

fn neutronics(a: Args) -> Result<Value, ToolError> {
    let set: nk::receipt::ClaimSet = serde_json::from_value(a.claim_set)
        .map_err(|e| ToolError::Invalid(format!("bad neutronics claim_set: {e}")))?;
    let names: Vec<String> = set.claims.iter().map(|c| c.name.clone()).collect();
    check_readings(&a.measurements, &names)?;
    let ms: Vec<nk::receipt::Measurement> = a
        .measurements
        .iter()
        .map(|r| nk::receipt::Measurement {
            name: r.name.clone(),
            value: r.value,
            uncertainty: r.uncertainty,
            instrument: r.instrument.clone(),
            band_factor: r.band_factor.unwrap_or(DEFAULT_BAND),
        })
        .collect();
    let report = nk::receipt::compare(&set, &ms).map_err(ToolError::Invalid)?;
    let rows = report
        .entries
        .iter()
        .map(|e| {
            let c = set.claims.iter().find(|c| c.name == e.name);
            let m = ms.iter().find(|m| m.name == e.name);
            let mut row = json!({
                "name": e.name, "unit": c.map(|c| c.unit.clone()),
                "predicted": e.predicted, "measured": e.measured, "ratio": e.ratio,
                "verdict": format!("{:?}", e.verdict),
                "rse": c.map(|c| c.rse),
            });
            if let (Some(m), Some(c)) = (m, c) {
                let s = c.value.abs() * c.rse;
                row["acceptance"] = json!([
                    e.predicted / m.band_factor - m.uncertainty - s,
                    e.predicted * m.band_factor + m.uncertainty + s
                ]);
                row["band_factor"] = json!(m.band_factor);
                row["uncertainty"] = json!(m.uncertainty);
                row["instrument"] = json!(m.instrument);
            }
            row
        })
        .collect();
    let measured = nk::receipt::comparison_claims(&report, &set, &ms);
    let unmeasured_set = nk::receipt::ClaimSet {
        claims: set
            .claims
            .iter()
            .filter(|c| !ms.iter().any(|m| m.name == c.name))
            .cloned()
            .collect(),
        ..set.clone()
    };
    Ok(finish(
        nk::receipt::RECEIPT_DOMAIN,
        serde_json::to_value(&report).unwrap_or(Value::Null),
        rows,
        measured,
        nk::receipt::design_claims(&unmeasured_set),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn particle_set() -> Value {
        json!({
            "schema": "vcad.particle-claims/1",
            "provenance": {"grid": [77, 153], "sor_tol": 1e-6, "sor_sweeps": 100, "ensemble": 96,
                           "max_passes": 40, "survivor_fraction": 0.0, "max_energy_drift_rel": 0.1,
                           "channels": ["beam_on_background", "cx_survival", "fast_neutral"]},
            "operating_point": {"ion_current_a": 0.01, "d2_pressure_mtorr": 5.0, "temperature_k": 300.0},
            "claims": [
                {"name": "interception_fraction", "value": 0.78, "unit": "1", "basis": "predicted", "note": ""},
                {"name": "ddn_neutron_rate", "value": 2.5e4, "unit": "n/s", "basis": "predicted", "note": ""}
            ]
        })
    }

    #[test]
    fn verdicts_match_the_acceptance_window() {
        // Inside, outside, and right at the edge of each window: the
        // crate's verdict must agree with the window we hand the plot.
        for (value, band, sigma) in [
            (3.0e4, 2.0, 0.0),
            (6.0e4, 2.0, 0.0),
            (5.0e4, 2.0, 0.0),
            (1.0e4, 2.0, 3.0e3),
            (1.0e4, 2.0, 1.0e3),
        ] {
            let out = compare(&json!({
                "claim_set": particle_set(),
                "measurements": [{"name": "ddn_neutron_rate", "value": value,
                                  "uncertainty": sigma, "band_factor": band}]
            }))
            .unwrap();
            let row = out["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == "ddn_neutron_rate")
                .unwrap()
                .clone();
            let lo = row["acceptance"][0].as_f64().unwrap();
            let hi = row["acceptance"][1].as_f64().unwrap();
            let inside = (lo..=hi).contains(&value);
            assert_eq!(
                row["verdict"] == "Holds",
                inside,
                "{value} in [{lo}, {hi}] → {}",
                row["verdict"]
            );
            assert_eq!(
                out["measured_verdict"],
                if inside { "pass" } else { "fail" }
            );
            // interception is unmeasured, so the whole receipt can't pass.
            assert_ne!(out["receipt_verdict"], "pass");
            assert_eq!(out["counts"]["unmeasured"], 1);
        }
    }

    #[test]
    fn unknown_claim_lists_the_valid_names() {
        let e = compare(&json!({
            "claim_set": particle_set(),
            "measurements": [{"name": "neutron_rate", "value": 1.0}]
        }))
        .unwrap_err();
        let s = e.to_string();
        assert!(
            s.contains("ddn_neutron_rate") && s.contains("interception_fraction"),
            "{s}"
        );
    }

    #[test]
    fn unknown_schema_is_refused() {
        let e = compare(
            &json!({"claim_set": {"schema": "x"}, "measurements": [{"name": "a", "value": 1}]}),
        )
        .unwrap_err();
        assert!(e.to_string().contains("vcad.particle-claims/1"));
    }

    #[test]
    fn neutronics_dose_reading_binds() {
        let spec = json!({
            "layers": [
                {"material": "air", "thickness_mm": 100},
                {"material": "hdpe", "thickness_mm": 100},
                {"material": "air", "thickness_mm": 900}
            ],
            "source": {"rate_n_per_s": 1.0e6, "energy_ev": 2.45e6},
            "detectors": [{"label": "operator", "radius_mm": 500}],
            "run": {"histories_per_batch": 1000, "batches": 4, "seed": 3}
        });
        let sim =
            crate::with_quiet_hooks(|h| crate::neutronics::simulate(&json!({"spec": spec}), h))
                .unwrap();
        let dose = sim["detectors"][0]["dose_usv_per_h"].as_f64().unwrap();
        let out = compare(&json!({
            "claim_set": sim["claim_set"],
            "measurements": [{"name": "dose_rate:operator", "value": dose * 1.2,
                              "uncertainty": 0.05 * dose, "band_factor": 1.5, "instrument": "survey meter"}]
        }))
        .unwrap();
        assert_eq!(out["domain"], "neutronics");
        assert_eq!(out["measured_verdict"], "pass");
        assert_eq!(out["receipt_claims"].as_array().unwrap().len(), 1);
    }
}
