//! `simulate_neutron_shield` over `vcad-kernel-neutronics`.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{json, Value};
use vcad_kernel_neutronics as nk;

use crate::{Hooks, ToolError};

/// Histories cap for an interactive run (error bars scale as 1/√N).
pub const MAX_HISTORIES: usize = 5_000_000;

#[derive(Deserialize)]
struct Args {
    spec: nk::spec::ShieldSpec,
    #[serde(default)]
    parameters: BTreeMap<String, f64>,
    #[serde(default)]
    budget_usv_per_h: Option<f64>,
    #[serde(default)]
    compare_bare: bool,
}

fn resolve_pv(v: &nk::spec::ParamValue, params: &BTreeMap<String, f64>) -> Result<f64, ToolError> {
    match v {
        nk::spec::ParamValue::Literal(x) => Ok(*x),
        nk::spec::ParamValue::Named(n) => params
            .get(n)
            .copied()
            .ok_or_else(|| ToolError::Invalid(format!("unbound parameter {n:?}"))),
    }
}

struct Run {
    detectors: Vec<Value>,
    doses: Vec<nk::spec::DetectorDose>,
    result: nk::transport::RunResult,
    resolved: nk::spec::ResolvedShield,
}

fn run(spec: &nk::spec::ShieldSpec, params: &BTreeMap<String, f64>) -> Result<Run, ToolError> {
    let total = spec
        .run
        .histories_per_batch
        .saturating_mul(spec.run.batches);
    if total > MAX_HISTORIES {
        return Err(ToolError::Invalid(format!(
            "too many histories: {total} (cap {MAX_HISTORIES}) — error bars scale as 1/sqrt(N)"
        )));
    }
    let resolved = spec
        .resolve(params)
        .map_err(|e| ToolError::Invalid(e.to_string()))?;
    let (doses, result) =
        nk::spec::evaluate(spec, params).map_err(|e| ToolError::Solver(e.to_string()))?;
    let mut detectors = Vec::with_capacity(doses.len());
    for (d, ds) in doses.iter().zip(&spec.detectors) {
        detectors.push(json!({
            "label": d.label,
            "radius_mm": resolve_pv(&ds.radius_mm, params)?,
            "dose_usv_per_h": d.dose_usv_per_h.mean,
            "rse": d.dose_usv_per_h.rse,
        }));
    }
    Ok(Run {
        detectors,
        doses,
        result,
        resolved,
    })
}

/// `simulate_neutron_shield`.
pub fn simulate(args: &Value, hooks: &Hooks) -> Result<Value, ToolError> {
    let a: Args =
        serde_json::from_value(args.clone()).map_err(|e| ToolError::Invalid(format!("{e}")))?;
    if hooks.cancelled() {
        return Err(ToolError::Cancelled);
    }
    let phases = if a.compare_bare { 2.0 } else { 1.0 };
    hooks.report(0.02, "shielded: transporting histories");
    let r = run(&a.spec, &a.parameters)?;
    let claim_set = nk::receipt::claims_from_run(
        &a.spec,
        &r.resolved.detector_regions,
        &r.doses,
        &r.result,
        &a.parameters,
    )
    .map_err(|e| ToolError::Solver(e.to_string()))?;
    let receipt_claims = nk::receipt::design_claims(&claim_set);
    let mut out = json!({
        "detectors": r.detectors,
        "absorbed": {"mean": r.result.absorbed.mean, "rse": r.result.absorbed.rse},
        "leaked_out": {"mean": r.result.leaked_out.mean, "rse": r.result.leaked_out.rse},
        "balance_max_dev": r.result.balance_max_dev,
        "total_histories": r.result.total_histories,
        "claim_set": serde_json::to_value(&claim_set).unwrap_or(Value::Null),
        "receipt_claims": serde_json::to_value(&receipt_claims).unwrap_or(Value::Null),
        "source_rate_n_per_s": r.resolved.source_rate_n_per_s,
        "layers": a.spec.layers.iter().map(|l| Ok(json!({
            "material": l.material,
            "thickness_mm": resolve_pv(&l.thickness_mm, &a.parameters)?,
        }))).collect::<Result<Vec<_>, ToolError>>()?,
        "scope": "neutron dose only (H*(10)); no capture gammas, no bremsstrahlung x-rays; spherical geometry; 5-group design-estimate library (±20–30%)",
    });

    if let Some(budget) = a.budget_usv_per_h {
        let rows: Vec<Value> = r
            .doses
            .iter()
            .map(|d| {
                let hi = d.dose_usv_per_h.mean * (1.0 + d.dose_usv_per_h.rse);
                json!({"label": d.label, "dose_usv_per_h": d.dose_usv_per_h.mean,
                       "upper_1sigma": hi, "pass": hi <= budget})
            })
            .collect();
        let pass = rows.iter().all(|r| r["pass"] == true);
        let worst = r
            .doses
            .iter()
            .max_by(|x, y| x.dose_usv_per_h.mean.total_cmp(&y.dose_usv_per_h.mean))
            .map(|d| json!({"label": d.label, "dose_usv_per_h": d.dose_usv_per_h.mean}));
        out["budget"] = json!({
            "usv_per_h": budget, "pass": pass, "worst": worst, "detectors": rows,
            "rule": "a detector passes when mean·(1 + rse) ≤ budget",
        });
    }

    if a.compare_bare {
        if hooks.cancelled() {
            return Err(ToolError::Cancelled);
        }
        hooks.report(0.5 / phases + 0.02, "bare source: transporting histories");
        let mut bare = a.spec.clone();
        for l in &mut bare.layers {
            l.material = "air".into();
        }
        let b = run(&bare, &a.parameters)?;
        out["bare_detectors"] = Value::Array(b.detectors);
    }
    hooks.report(1.0, "done");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::with_quiet_hooks;

    fn spec() -> Value {
        json!({
            "layers": [
                {"material": "air", "thickness_mm": 100},
                {"material": "hdpe", "thickness_mm": 150},
                {"material": "air", "thickness_mm": 3000}
            ],
            "source": {"rate_n_per_s": 5.0e6, "energy_ev": 2.45e6},
            "detectors": [
                {"label": "0.5 m", "radius_mm": 500},
                {"label": "1 m", "radius_mm": 1000},
                {"label": "2 m", "radius_mm": 2000}
            ],
            "run": {"histories_per_batch": 2000, "batches": 4, "seed": 7}
        })
    }

    #[test]
    fn shield_beats_bare_and_dose_falls_with_distance() {
        let out = with_quiet_hooks(|h| {
            simulate(
                &json!({"spec": spec(), "compare_bare": true, "budget_usv_per_h": 10.0}),
                h,
            )
        })
        .unwrap();
        let d: Vec<f64> = out["detectors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["dose_usv_per_h"].as_f64().unwrap())
            .collect();
        let b: Vec<f64> = out["bare_detectors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["dose_usv_per_h"].as_f64().unwrap())
            .collect();
        assert!(d[0] > d[1] && d[1] > d[2], "{d:?}");
        for (s, u) in d.iter().zip(&b) {
            assert!(s < u, "shielded {s} ≥ bare {u}");
        }
        assert_eq!(out["detectors"][1]["radius_mm"], 1000.0);
        assert!(out["budget"]["detectors"].as_array().unwrap().len() == 3);
        assert_eq!(out["claim_set"]["schema"], "vcad.neutronics-claims/1");
    }

    #[test]
    fn histories_are_capped() {
        let mut s = spec();
        s["run"] = json!({"histories_per_batch": 1_000_000, "batches": 10});
        let e = with_quiet_hooks(|h| simulate(&json!({"spec": s}), h)).unwrap_err();
        assert!(e.to_string().contains("too many histories"));
    }
}
