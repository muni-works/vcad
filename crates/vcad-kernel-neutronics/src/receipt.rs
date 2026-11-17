//! Predicted-dose claims for the design receipt (M4).
//!
//! Emits `vcad.neutronics-claims/1`: per-detector ambient-dose-rate,
//! attenuation-factor, and thermal-flux claims, each carrying **both**
//! its Monte Carlo relative standard error and the full method
//! provenance (histories, batches, groups, energy model, library
//! version, seed) — a dose number without its uncertainty and its
//! recipe is not a claim, it is a rumor.
//!
//! Fail-closed rules, enforced at construction:
//! - a run with truncated histories cannot produce claims;
//! - a tally that scored nothing (RSE = ∞) cannot become a claim — an
//!   all-zero dose is a statistics floor, not a measured zero;
//! - claims always carry the library caveat list (design-estimate
//!   constants, no capture-gamma transport, free field). Whoever
//!   consumes the claim gets the caveats in the same JSON object,
//!   not in a README three repos away.
//!
//! [`compare`] binds bench measurements (survey-meter readings at the
//! detector positions) to claims with Holds / Violated / Unmeasured
//! verdicts, in the vocabulary of the repo's receipt system: an
//! unmeasured receipt never passes, a measurement matching no claim is
//! an error, and Violated is a publishable result about the model.
//! Registration of this family in `crates/vcad-receipt` + the MCP
//! surface is the flagged follow-up PR (cross-crate schema + TS
//! codegen), same staging as the particle crate's family.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::groups::THERMAL_GROUP;
use crate::spec::{evaluate, ShieldSpec, SpecError};
use crate::transport::{RunProvenance, RunResult};

/// Schema tag for this claim family.
pub const CLAIM_SCHEMA: &str = "vcad.neutronics-claims/1";

/// One predicted claim, with its statistical uncertainty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    /// Claim name (snake_case, detector-labeled).
    pub name: String,
    /// Value.
    pub value: f64,
    /// Relative standard error of the value (Monte Carlo, batch
    /// statistics).
    pub rse: f64,
    /// Unit ("uSv/h", "1", "n/cm2/s").
    pub unit: String,
    /// Claim basis — always "predicted" here.
    pub basis: String,
    /// Assumptions and caveats specific to this claim.
    pub note: String,
}

/// The source the claims are priced at.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SourceProvenance {
    /// Emission rate, n/s.
    pub rate_n_per_s: f64,
    /// Line energy, eV.
    pub energy_ev: f64,
}

/// The full claim set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimSet {
    /// Schema tag ([`CLAIM_SCHEMA`]).
    pub schema: String,
    /// Reproducibility provenance (seed, histories, batches, groups,
    /// energy model, library version).
    pub provenance: RunProvenance,
    /// Source description.
    pub source: SourceProvenance,
    /// The claims.
    pub claims: Vec<Claim>,
    /// Library-wide caveats, carried on every set.
    pub caveats: Vec<String>,
}

/// Claim-construction failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimError {
    /// The spec failed to resolve or run.
    Spec(SpecError),
    /// The run truncated histories — its books do not balance.
    TruncatedHistories(u64),
    /// A tally scored nothing; the claim cannot carry an error bar.
    NothingScored {
        /// The claim that could not be priced.
        name: String,
    },
    /// The 3D scene spec failed to resolve or run (M7).
    Scene(String),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Spec(e) => write!(f, "spec failed: {e}"),
            ClaimError::TruncatedHistories(n) => {
                write!(
                    f,
                    "{n} truncated histories — refusing to claim from an unbalanced run"
                )
            }
            ClaimError::NothingScored { name } => write!(
                f,
                "claim {name:?} scored nothing (RSE = ∞): a zero tally is a statistics \
                 floor, not a measured zero — run more histories"
            ),
            ClaimError::Scene(e) => write!(f, "scene failed: {e}"),
        }
    }
}

impl std::error::Error for ClaimError {}

fn standard_caveats() -> Vec<String> {
    vec![
        "design-estimate multigroup library (±20-30% group constants), not evaluated \
         nuclear data"
            .to_string(),
        "neutron dose only: capture gammas (H(n,g) 2.22 MeV) are not transported — \
         budget gamma dose separately"
            .to_string(),
        "free field: no room return; concrete walls add back-scatter".to_string(),
        "isotropic-in-CM elastic scattering; MeV p-wave forward hardening not modeled".to_string(),
    ]
}

/// Build the predicted claim set for a shield spec: runs the Monte
/// Carlo oracle via [`evaluate`] and prices every detector.
pub fn predicted_claims(
    spec: &ShieldSpec,
    params: &BTreeMap<String, f64>,
) -> Result<ClaimSet, ClaimError> {
    let resolved = spec.resolve(params).map_err(ClaimError::Spec)?;
    let (doses, result) = evaluate(spec, params).map_err(ClaimError::Spec)?;
    claims_from_run(spec, &resolved.detector_regions, &doses, &result, params)
}

/// Lower layer: price claims from an existing run (exposed so tests can
/// exercise the fail-closed paths directly).
pub fn claims_from_run(
    spec: &ShieldSpec,
    detector_regions: &[(String, usize)],
    doses: &[crate::spec::DetectorDose],
    result: &RunResult,
    params: &BTreeMap<String, f64>,
) -> Result<ClaimSet, ClaimError> {
    if result.truncated_histories > 0 {
        return Err(ClaimError::TruncatedHistories(result.truncated_histories));
    }
    let resolved = spec.resolve(params).map_err(ClaimError::Spec)?;
    let rate = resolved.source_rate_n_per_s;
    let energy_ev = match &spec.source.energy_ev {
        crate::spec::ParamValue::Literal(v) => *v,
        crate::spec::ParamValue::Named(n) => *params.get(n).expect("resolved above"),
    };
    let h_src = crate::dose::h10_psv_cm2(energy_ev);

    // Detector radii from the resolved geometry (region centers).
    let bounds: Vec<f64> = {
        let mut b = vec![0.0];
        for l in resolved.config.geometry.layers() {
            b.push(b.last().unwrap() + l.thickness_mm);
        }
        b
    };

    let mut claims = Vec::new();
    for (dose, (label, region)) in doses.iter().zip(detector_regions.iter()) {
        let d = dose.dose_usv_per_h;
        if !d.rse.is_finite() {
            return Err(ClaimError::NothingScored {
                name: format!("dose_rate:{label}"),
            });
        }
        claims.push(Claim {
            name: format!("dose_rate:{label}"),
            value: d.mean,
            rse: d.rse,
            unit: "uSv/h".to_string(),
            basis: "predicted".to_string(),
            note: "ambient dose equivalent H*(10), ICRP-74-style factors; neutron \
                   component only"
                .to_string(),
        });

        // Attenuation factor vs the bare analytic source at this radius
        // (same 1/4πr², same dose factor at the source line energy).
        let r_cm = 0.5 * (bounds[*region] + bounds[*region + 1]) * 0.1;
        let bare_usv_h =
            rate / (4.0 * std::f64::consts::PI * r_cm * r_cm) * h_src * 3600.0 * 1.0e-6;
        claims.push(Claim {
            name: format!("attenuation_factor:{label}"),
            value: bare_usv_h / d.mean,
            rse: d.rse,
            unit: "1".to_string(),
            basis: "predicted".to_string(),
            note: "bare analytic point-source dose at the detector radius divided by \
                   the shielded prediction"
                .to_string(),
        });

        // Thermal flux (activation-analysis feasibility channel).
        let th = result.flux_per_source[*region][THERMAL_GROUP];
        if th.rse.is_finite() {
            claims.push(Claim {
                name: format!("thermal_flux:{label}"),
                value: th.mean * rate,
                rse: th.rse,
                unit: "n/cm2/s".to_string(),
                basis: "predicted".to_string(),
                note: "below 0.5 eV; free-gas thermal motion neglected".to_string(),
            });
        }
        // (A thermal tally that scored nothing is simply omitted — the
        // dose claim above is the load-bearing one; an absent claim
        // reads as Unmeasured downstream, never as zero.)
    }
    claims.push(Claim {
        name: "absorbed_fraction".to_string(),
        value: result.absorbed.mean,
        rse: result.absorbed.rse,
        unit: "1".to_string(),
        basis: "predicted".to_string(),
        note: "fraction of source neutrons absorbed anywhere in the geometry".to_string(),
    });

    Ok(ClaimSet {
        schema: CLAIM_SCHEMA.to_string(),
        provenance: result.provenance.clone(),
        source: SourceProvenance {
            rate_n_per_s: rate,
            energy_ev,
        },
        claims,
        caveats: standard_caveats(),
    })
}

/// Voxels with RSE at or below this are "resolved" and eligible for the
/// dose-map maximum claim (the max over noisy voxels is biased high).
pub const DOSE_MAP_RESOLVED_RSE: f64 = 0.2;

fn scene_caveats() -> Vec<String> {
    vec![
        "design-estimate multigroup library (±20-30% group constants), not evaluated \
         nuclear data"
            .to_string(),
        "neutron dose only: capture gammas (H(n,g) 2.22 MeV) are not transported — the \
         per-cell capture rates are their source term; budget gamma dose separately"
            .to_string(),
        "room return is modeled for the declared scene only: outside the world cell is \
         vacuum (no earth, neighbouring rooms, or skyshine)"
            .to_string(),
        "detectors are virtual tally spheres: instrument self-perturbation of the field is \
         not modeled; responses are folded at the neutron's energy"
            .to_string(),
        "isotropic-in-CM elastic scattering; MeV p-wave forward hardening and inelastic \
         scattering not modeled"
            .to_string(),
    ]
}

/// Claims for a 3D scene run (M7): per-detector dose rate, thermal
/// fluence, and **expected instrument readings** (counts/min, bubbles/h,
/// tracks/cm²/h — RSE combines the room MC with the response-table MC),
/// the resolved dose-map maximum, per-cell capture rates, and leakage.
/// Fail-closed as everywhere: truncation or an unscored dose/instrument
/// tally refuses the whole set.
pub fn scene_claims(
    spec: &crate::scene_spec::SceneSpec,
    params: &BTreeMap<String, f64>,
) -> Result<ClaimSet, ClaimError> {
    let (resolved, result) = spec
        .evaluate(params)
        .map_err(|e| ClaimError::Scene(e.to_string()))?;
    scene_claims_from_run(&resolved, &result)
}

/// Lower layer of [`scene_claims`]: price an existing run.
pub fn scene_claims_from_run(
    resolved: &crate::scene_spec::ResolvedScene,
    result: &crate::transport3d::SceneResult,
) -> Result<ClaimSet, ClaimError> {
    if result.truncated_histories > 0 {
        return Err(ClaimError::TruncatedHistories(result.truncated_histories));
    }
    let rate = resolved.source_rate_n_per_s;
    let mut claims = Vec::new();
    for (d, t) in result.detectors.iter().zip(&resolved.config.detectors) {
        let dose = d.dose_rate_usv_per_h(rate);
        if !dose.rse.is_finite() {
            return Err(ClaimError::NothingScored {
                name: format!("dose_rate:{}", d.label),
            });
        }
        let at = format!(
            "at ({:.0}, {:.0}, {:.0}) mm, {:.0} mm tally sphere",
            t.position_mm[0], t.position_mm[1], t.position_mm[2], t.radius_mm
        );
        claims.push(Claim {
            name: format!("dose_rate:{}", d.label),
            value: dose.mean,
            rse: dose.rse,
            unit: "uSv/h".to_string(),
            basis: "predicted".to_string(),
            note: format!(
                "ambient dose equivalent H*(10), neutron only, room return included; {at}"
            ),
        });
        let th = d.fluence_per_source[THERMAL_GROUP].scaled(rate);
        if th.rse.is_finite() {
            claims.push(Claim {
                name: format!("thermal_flux:{}", d.label),
                value: th.mean,
                rse: th.rse,
                unit: "n/cm2/s".to_string(),
                basis: "predicted".to_string(),
                note: format!("below 0.5 eV; free-gas thermal motion neglected; {at}"),
            });
        }
        for (r, f) in d.responses.iter().zip(&t.responses) {
            let name = format!("count_rate:{}:{}", d.label, r.name);
            let rse = crate::scene_spec::combined_rse(r.per_source, f.table_rse);
            if !rse.is_finite() {
                return Err(ClaimError::NothingScored { name });
            }
            // Slow instruments (bubbles, tracks) are read per hour; live
            // counters per minute.
            let (value, unit) = match r.event_unit.as_str() {
                "count" => (r.per_minute(rate).mean, "counts/min".to_string()),
                u => (r.per_hour(rate).mean, format!("{u}/h")),
            };
            claims.push(Claim {
                name,
                value,
                rse,
                unit,
                basis: "predicted".to_string(),
                note: format!("{}; {}; {at}", f.citation, f.caveat),
            });
        }
    }
    if let Some(m) = &result.mesh {
        let d = m.dose_rate_usv_per_h(rate);
        let resolved_vox: Vec<(usize, &crate::tally::Estimate)> = d
            .iter()
            .enumerate()
            .filter(|(_, e)| e.rse <= DOSE_MAP_RESOLVED_RSE)
            .collect();
        let frac = resolved_vox.len() as f64 / d.len() as f64;
        claims.push(Claim {
            name: "dose_map_resolved_fraction".to_string(),
            value: frac,
            rse: 0.0,
            unit: "1".to_string(),
            basis: "predicted".to_string(),
            note: format!(
                "fraction of {} voxels with RSE <= {DOSE_MAP_RESOLVED_RSE}",
                d.len()
            ),
        });
        if let Some((i, e)) = resolved_vox
            .iter()
            .max_by(|a, b| a.1.mean.total_cmp(&b.1.mean))
        {
            let (nx, ny) = (m.mesh.shape[0], m.mesh.shape[1]);
            let c = m.mesh.center_mm(i % nx, (i / nx) % ny, i / (nx * ny));
            claims.push(Claim {
                name: "dose_map_max".to_string(),
                value: e.mean,
                rse: e.rse,
                unit: "uSv/h".to_string(),
                basis: "predicted".to_string(),
                note: format!(
                    "largest resolved-voxel H*(10) rate, voxel centered ({:.0}, {:.0}, {:.0}) mm",
                    c[0], c[1], c[2]
                ),
            });
        }
    }
    for (a, c) in result
        .absorbed_per_cell
        .iter()
        .zip(&resolved.config.scene.cells)
    {
        if a.rse.is_finite() {
            claims.push(Claim {
                name: format!("capture_rate:{}", c.name),
                value: a.mean * rate,
                rse: a.rse,
                unit: "1/s".to_string(),
                basis: "predicted".to_string(),
                note: format!(
                    "neutron absorptions per second in {} — the capture-gamma source term",
                    c.material.name
                ),
            });
        }
    }
    claims.push(Claim {
        name: "leaked_fraction".to_string(),
        value: result.leaked.mean,
        rse: result.leaked.rse,
        unit: "1".to_string(),
        basis: "predicted".to_string(),
        note: "fraction of source neutrons escaping the world cell".to_string(),
    });
    Ok(ClaimSet {
        schema: CLAIM_SCHEMA.to_string(),
        provenance: result.provenance.clone(),
        source: SourceProvenance {
            rate_n_per_s: rate,
            energy_ev: resolved.source_energy_ev,
        },
        claims,
        caveats: scene_caveats(),
    })
}

/// A bench measurement to bind against a claim (survey meter at a
/// detector position, foil activation, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// Claim name this measures (must match).
    pub name: String,
    /// Measured value, claim units.
    pub value: f64,
    /// One-sigma absolute uncertainty, same units.
    pub uncertainty: f64,
    /// Instrument provenance ("Ludlum 12-4 s/n …", "BD-PND badge …").
    pub instrument: String,
    /// Acceptance band, multiplicative: holds when measured/predicted ∈
    /// [1/band, band] after widening by both uncertainties. The library
    /// caveat (±20–30% constants) argues for bands ≥ 1.5 on absolute
    /// doses; attenuation *ratios* deserve tighter bands.
    pub band_factor: f64,
}

/// Verdict per claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Measurement inside the stated band.
    Holds,
    /// Measurement outside the stated band.
    Violated,
    /// No measurement bound (fail-closed: never silently passing).
    Unmeasured,
}

/// One comparison row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonEntry {
    /// Claim name.
    pub name: String,
    /// Predicted value.
    pub predicted: f64,
    /// Measured value.
    pub measured: Option<f64>,
    /// measured / predicted.
    pub ratio: Option<f64>,
    /// Verdict.
    pub verdict: Verdict,
}

/// The comparison report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonReport {
    /// Schema tag.
    pub schema: String,
    /// Rows, one per claim.
    pub entries: Vec<ComparisonEntry>,
    /// True only when every measured claim holds AND at least one
    /// measurement exists.
    pub all_hold: bool,
}

/// Bind measurements to claims. A measurement matching no claim is an
/// error (measuring nothing is a bookkeeping bug).
pub fn compare(
    claims: &ClaimSet,
    measurements: &[Measurement],
) -> Result<ComparisonReport, String> {
    for m in measurements {
        if !claims.claims.iter().any(|c| c.name == m.name) {
            return Err(format!("measurement {:?} matches no claim", m.name));
        }
    }
    let mut entries = Vec::with_capacity(claims.claims.len());
    let mut measured_any = false;
    let mut all_hold = true;
    for c in &claims.claims {
        let entry = match measurements.iter().find(|m| m.name == c.name) {
            None => ComparisonEntry {
                name: c.name.clone(),
                predicted: c.value,
                measured: None,
                ratio: None,
                verdict: Verdict::Unmeasured,
            },
            Some(m) => {
                measured_any = true;
                // Widen the band by both the measurement uncertainty and
                // the claim's own MC standard error.
                let sigma_claim = c.value.abs() * c.rse;
                let lo = c.value / m.band_factor - m.uncertainty - sigma_claim;
                let hi = c.value * m.band_factor + m.uncertainty + sigma_claim;
                let holds = (lo..=hi).contains(&m.value);
                if !holds {
                    all_hold = false;
                }
                ComparisonEntry {
                    name: c.name.clone(),
                    predicted: c.value,
                    measured: Some(m.value),
                    ratio: if c.value != 0.0 {
                        Some(m.value / c.value)
                    } else {
                        None
                    },
                    verdict: if holds {
                        Verdict::Holds
                    } else {
                        Verdict::Violated
                    },
                }
            }
        };
        entries.push(entry);
    }
    Ok(ComparisonReport {
        schema: "vcad.neutronics-compare/1".to_string(),
        entries,
        all_hold: all_hold && measured_any,
    })
}

/// Domain tag for neutronics claims in the unified [`vcad_receipt`] schema.
pub const RECEIPT_DOMAIN: &str = "neutronics";

/// The oracle reference for this crate's Monte Carlo transport solver.
pub fn oracle() -> vcad_receipt::OracleRef {
    vcad_receipt::OracleRef::new("vcad-kernel-neutronics/mc", env!("CARGO_PKG_VERSION"))
}

fn quantity(value: f64, unit: &str) -> vcad_receipt::ClaimQuantity {
    if unit == "1" {
        vcad_receipt::ClaimQuantity::bare(value)
    } else {
        vcad_receipt::ClaimQuantity::new(value, unit)
    }
}

/// Translate a predicted [`ClaimSet`] into unified-receipt claims.
///
/// Every claim lands with [`vcad_receipt::ClaimBasis::Predicted`] — the
/// transport ran for real, but the claims describe a shield that has not
/// been measured, so a receipt built from these **rolls up Provisional,
/// never Pass** (the same contract as `predict_physics`/`predict_print`).
/// The computed value rides in `measured` ("what the oracle computed");
/// run provenance and the per-claim Monte Carlo relative standard error
/// ride in `details`.
pub fn design_claims(set: &ClaimSet) -> Vec<vcad_receipt::ReceiptClaim> {
    let oracle = oracle();
    let provenance = format!(
        "mc {}x{} histories seed {}, {} groups, {}, library {}; source {:.3e} n/s @ {:.3e} eV",
        set.provenance.histories_per_batch,
        set.provenance.batches,
        set.provenance.seed,
        set.provenance.groups,
        set.provenance.energy_model,
        set.provenance.library,
        set.source.rate_n_per_s,
        set.source.energy_ev,
    );
    set.claims
        .iter()
        .map(|c| {
            vcad_receipt::ReceiptClaim::pass(
                format!("neutronics.{}", c.name),
                RECEIPT_DOMAIN,
                c.note.clone(),
                oracle.clone(),
            )
            .with_basis(vcad_receipt::ClaimBasis::Predicted)
            .with_measured(quantity(c.value, &c.unit))
            .with_details(format!("{provenance}; rse {:.4}", c.rse))
        })
        .collect()
}

/// Translate a bench [`ComparisonReport`] into unified-receipt claims —
/// the twin of `vcad_kernel_particle::receipt::comparison_claims`.
///
/// Only measured rows translate (unmeasured claims stay in the predicted
/// set): Holds → Pass, Violated → Fail, both with
/// [`vcad_receipt::ClaimBasis::Measured`], the prediction in `predicted`
/// and the survey reading in `measured`.
pub fn comparison_claims(
    report: &ComparisonReport,
    set: &ClaimSet,
    measurements: &[Measurement],
) -> Vec<vcad_receipt::ReceiptClaim> {
    let oracle = oracle();
    report
        .entries
        .iter()
        .filter_map(|e| {
            let measured = e.measured?;
            let m = measurements.iter().find(|m| m.name == e.name)?;
            let unit = set
                .claims
                .iter()
                .find(|c| c.name == e.name)
                .map(|c| c.unit.clone())
                .unwrap_or_else(|| "1".to_string());
            let base = match e.verdict {
                Verdict::Holds => vcad_receipt::ReceiptClaim::pass(
                    format!("neutronics.{}", e.name),
                    RECEIPT_DOMAIN,
                    format!(
                        "survey measurement of {} within band {}",
                        e.name, m.band_factor
                    ),
                    oracle.clone(),
                ),
                Verdict::Violated => vcad_receipt::ReceiptClaim::fail(
                    format!("neutronics.{}", e.name),
                    RECEIPT_DOMAIN,
                    format!(
                        "survey measurement of {} outside band {}",
                        e.name, m.band_factor
                    ),
                    oracle.clone(),
                ),
                Verdict::Unmeasured => return None,
            };
            Some(
                base.with_basis(vcad_receipt::ClaimBasis::Measured)
                    .with_predicted(quantity(e.predicted, &unit))
                    .with_measured(quantity(measured, &unit))
                    .with_details(format!("instrument: {}", m.instrument)),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ShieldSpec;
    use std::collections::BTreeMap;

    #[test]
    fn design_claims_ride_the_unified_receipt_as_provisional() {
        let spec: ShieldSpec = serde_json::from_str(
            r#"{
              "layers": [
                {"material": "air",  "thickness_mm": 100},
                {"material": "hdpe", "thickness_mm": 100},
                {"material": "air",  "thickness_mm": 400}
              ],
              "source": {"rate_n_per_s": 1.0e6, "energy_ev": 2.45e6},
              "detectors": [{"label": "operator", "radius_mm": 400}],
              "run": {"histories_per_batch": 500, "batches": 4, "seed": 7}
            }"#,
        )
        .unwrap();
        let set = predicted_claims(&spec, &BTreeMap::new()).unwrap();
        let claims = design_claims(&set);
        assert_eq!(claims.len(), set.claims.len());
        for c in &claims {
            assert!(c.id.starts_with("neutronics."));
            assert_eq!(c.domain, RECEIPT_DOMAIN);
            assert_eq!(c.basis, Some(vcad_receipt::ClaimBasis::Predicted));
            assert!(c.measured.is_some());
            let details = c.details.as_deref().unwrap_or("");
            assert!(details.contains("mc 500x4 histories seed 7"));
            assert!(details.contains("rse "));
        }
        let receipt = vcad_receipt::DesignReceipt::with_claims(claims);
        assert_eq!(
            receipt.verdict(),
            vcad_receipt::ReceiptVerdict::Provisional,
            "predicted neutronics claims must never read as verified"
        );
    }

    #[test]
    fn scene_claims_price_instruments_and_fail_closed() {
        let spec: crate::scene_spec::SceneSpec = serde_json::from_str(
            r#"{
              "cells": [
                {"name": "castle", "material": "hdpe",
                 "shape": {"type": "box_centered", "center_mm": [0,0,0], "half_mm": [150,150,150]}},
                {"name": "room", "material": "air",
                 "shape": {"type": "box_centered", "center_mm": [0,0,0], "half_mm": [1000,1000,1000]}},
                {"name": "walls", "material": "concrete",
                 "shape": {"type": "box_centered", "center_mm": [0,0,0], "half_mm": [1200,1200,1200]}}
              ],
              "source": {"position_mm": [0,0,0], "rate_n_per_s": 1e6, "energy_ev": 2.45e6},
              "detectors": [{"label": "op", "position_mm": [500,0,0],
                "instruments": [{"type": "bubble", "sensitivity_bubbles_per_usv": 3}]}],
              "mesh": {"min_mm": [-1000,-1000,-100], "max_mm": [1000,1000,100], "shape": [8,8,1]},
              "run": {"histories_per_batch": 2000, "batches": 4, "seed": 5}
            }"#,
        )
        .unwrap();
        let set = scene_claims(&spec, &BTreeMap::new()).unwrap();
        let get = |n: &str| set.claims.iter().find(|c| c.name == n).cloned();
        let dose = get("dose_rate:op").expect("dose claim");
        let bub = get("count_rate:op:bubble").expect("bubble claim");
        assert_eq!(bub.unit, "bubble/h");
        assert!(bub.value > 0.0 && bub.rse.is_finite());
        // Bubbles/h ≤ 3 bubbles/µSv × dose (threshold removes slow neutrons).
        assert!(bub.value <= 3.0 * dose.value * (1.0 + 1e-9));
        assert!(get("dose_map_max").is_some());
        assert!(get("capture_rate:castle").is_some());
        assert!(set.caveats.iter().any(|c| c.contains("room return")));
        let receipt = vcad_receipt::DesignReceipt::with_claims(design_claims(&set));
        assert_eq!(receipt.verdict(), vcad_receipt::ReceiptVerdict::Provisional);

        // Fail closed: a 1-history-per-batch run leaves the bubble
        // detector unscored → refuse the whole set.
        let mut thin = spec.clone();
        thin.run.histories_per_batch = 1;
        thin.run.batches = 2;
        assert!(scene_claims(&thin, &BTreeMap::new()).is_err());
    }

    #[test]
    fn comparison_claims_translate_holds_and_violated() {
        let spec: ShieldSpec = serde_json::from_str(
            r#"{
              "layers": [
                {"material": "air",  "thickness_mm": 100},
                {"material": "hdpe", "thickness_mm": 100},
                {"material": "air",  "thickness_mm": 400}
              ],
              "source": {"rate_n_per_s": 1.0e6, "energy_ev": 2.45e6},
              "detectors": [{"label": "operator", "radius_mm": 400}],
              "run": {"histories_per_batch": 500, "batches": 4, "seed": 7}
            }"#,
        )
        .unwrap();
        let set = predicted_claims(&spec, &BTreeMap::new()).unwrap();
        let dose = set
            .claims
            .iter()
            .find(|c| c.name == "dose_rate:operator")
            .unwrap()
            .value;
        let ms = vec![Measurement {
            name: "dose_rate:operator".into(),
            value: dose * 1.1,
            uncertainty: 0.0,
            instrument: "test meter".into(),
            band_factor: 1.5,
        }];
        let report = compare(&set, &ms).unwrap();
        assert!(report.all_hold);
        let claims = comparison_claims(&report, &set, &ms);
        assert_eq!(claims.len(), 1, "only measured rows translate");
        assert_eq!(claims[0].basis, Some(vcad_receipt::ClaimBasis::Measured));
        let receipt = vcad_receipt::DesignReceipt::with_claims(claims);
        assert_eq!(receipt.verdict(), vcad_receipt::ReceiptVerdict::Pass);

        let bad = vec![Measurement {
            value: dose * 10.0,
            ..ms[0].clone()
        }];
        let report = compare(&set, &bad).unwrap();
        assert!(!report.all_hold);
        let receipt =
            vcad_receipt::DesignReceipt::with_claims(comparison_claims(&report, &set, &bad));
        assert_eq!(receipt.verdict(), vcad_receipt::ReceiptVerdict::Fail);
    }
}
