//! Predicted photon-dose claims: `vcad.photon-claims/1`.
//!
//! Per detector: `h10_rate:<label>` (µSv/h), `air_kerma_rate:<label>`
//! (µGy/h), `buildup:<label>`. Every claim carries its Monte Carlo
//! relative standard error **and** its systematic band (value/B …
//! value·B, with the named terms that compose B), plus the run
//! provenance and the library-wide caveats in the same object.
//!
//! [`combined_claims`] sums a neutronics claim set
//! (`vcad.neutronics-claims/1`, neutron H*(10)) with photon reports at
//! the same labels into `total_dose_rate:<label>` — the number a safety
//! case actually needs, and the one a survey (neutron rem-ball + gamma
//! meter) binds against.
//!
//! Fail-closed at construction: truncated histories, a scattering run
//! that scored nothing, or an infinite-RSE dose cannot become claims.
//! [`compare`] binds survey readings with Holds / Violated / Unmeasured;
//! a measurement matching no claim is an error.

use serde::{Deserialize, Serialize};

use crate::report::{BandTerm, DoseReport};
use crate::spec::Evaluation;
use crate::transport::PhotonProvenance;

/// Schema tag.
pub const CLAIM_SCHEMA: &str = "vcad.photon-claims/1";
/// Domain tag in the unified receipt.
pub const RECEIPT_DOMAIN: &str = "photon";

/// One predicted claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    /// Name (`kind:label`).
    pub name: String,
    /// Central value.
    pub value: f64,
    /// Monte Carlo relative standard error.
    pub rse: f64,
    /// Systematic multiplicative band B.
    pub band_factor: f64,
    /// value / B.
    pub low: f64,
    /// value · B — use this for a safety case.
    pub high: f64,
    /// The named band terms.
    pub band_terms: Vec<BandTerm>,
    /// Unit.
    pub unit: String,
    /// Always "predicted".
    pub basis: String,
    /// Claim-specific note.
    pub note: String,
}

/// A claim set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimSet {
    /// [`CLAIM_SCHEMA`].
    pub schema: String,
    /// Run provenance.
    pub provenance: PhotonProvenance,
    /// Source description.
    pub source: String,
    /// Claims.
    pub claims: Vec<Claim>,
    /// Caveats carried on every set.
    pub caveats: Vec<String>,
}

/// Claim construction failures.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimError {
    /// Truncated histories.
    TruncatedHistories(u64),
    /// Scatter requested, nothing scored: buildup unresolved.
    ScatterUnresolved,
    /// A dose with no error bar.
    NothingScored(String),
    /// A label present in one set but not the other.
    MissingLabel(String),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::TruncatedHistories(n) => {
                write!(f, "{n} truncated histories — refusing to claim")
            }
            ClaimError::ScatterUnresolved => {
                write!(
                    f,
                    "scattering run scored nothing: buildup unresolved, run more histories"
                )
            }
            ClaimError::NothingScored(n) => write!(f, "claim {n:?} has no finite error bar"),
            ClaimError::MissingLabel(l) => {
                write!(f, "label {l:?} missing from one of the claim sets")
            }
        }
    }
}

impl std::error::Error for ClaimError {}

/// Library-wide caveats.
pub fn standard_caveats() -> Vec<String> {
    vec![
        "H*(10) is deep dose; for soft x-rays through thin windows the eye-lens/skin dose H'(0.07) \
         can exceed it — the air-kerma claim bounds H'(0.07) to within ~x1.6 (ICRP 74)"
            .into(),
        "infinite lateral slabs, normal incidence, no room return or skyshine (air attenuates but \
         does not scatter)"
            .into(),
        "coherent scatter and photo/pair absorption treated as removal: no fluorescence x-rays, \
         no 511 keV annihilation photons"
            .into(),
        "priced at the stated peak voltage and current; supply ripple and partial-energy electrons \
         lower the real dose"
            .into(),
        "predicted, not measured: an energy-compensated survey at every voltage step replaces \
         these numbers"
            .into(),
    ]
}

fn claims_from_report(r: &DoseReport, extra_note: &str) -> Result<Vec<Claim>, ClaimError> {
    let b = r.band_factor();
    let mk = |name: String, value: f64, rse: f64, unit: &str, note: String, band: bool| Claim {
        name,
        value,
        rse,
        band_factor: if band { b } else { 1.0 },
        low: if band { value / b } else { value },
        high: if band { value * b } else { value },
        band_terms: if band { r.band.clone() } else { vec![] },
        unit: unit.into(),
        basis: "predicted".into(),
        note,
    };
    for (n, e) in [("h10_rate", r.h10_usv_h), ("air_kerma_rate", r.kerma_ugy_h)] {
        if !e.rse.is_finite() || e.mean <= 0.0 {
            return Err(ClaimError::NothingScored(format!("{n}:{}", r.label)));
        }
    }
    let breakdown: Vec<String> = r
        .components
        .iter()
        .map(|c| format!("{} {:.3e}", c.name, c.h10_usv_h))
        .collect();
    Ok(vec![
        mk(
            format!("h10_rate:{}", r.label),
            r.h10_usv_h.mean,
            r.h10_usv_h.rse,
            "uSv/h",
            format!(
                "ambient dose equivalent H*(10), ICRP-74; {}; components (uSv/h): {}{extra_note}",
                r.geometry_note,
                breakdown.join("; ")
            ),
            true,
        ),
        mk(
            format!("air_kerma_rate:{}", r.label),
            r.kerma_ugy_h.mean,
            r.kerma_ugy_h.rse,
            "uGy/h",
            "air kerma (survey-meter calibration quantity; shallow-dose proxy)".into(),
            true,
        ),
        mk(
            format!("buildup:{}", r.label),
            r.buildup_h10,
            r.h10_usv_h.rse,
            "1",
            "total / uncollided H*(10) of the dominant component (scatter contribution)".into(),
            false,
        ),
    ])
}

/// Build the claim set for a spec evaluation (chamber, path, or
/// capture-gamma).
pub fn predicted_claims(ev: &Evaluation) -> Result<ClaimSet, ClaimError> {
    if ev.truncated_histories > 0 {
        return Err(ClaimError::TruncatedHistories(ev.truncated_histories));
    }
    if !ev.scatter_scored {
        return Err(ClaimError::ScatterUnresolved);
    }
    let mut claims = Vec::new();
    for r in &ev.reports {
        claims.extend(claims_from_report(r, "")?);
    }
    Ok(ClaimSet {
        schema: CLAIM_SCHEMA.into(),
        provenance: ev.provenance.clone(),
        source: ev.source.clone(),
        claims,
        caveats: standard_caveats(),
    })
}

/// Neutron + photon total dose per label. Means add; absolute MC
/// sigmas add linearly; the band edges add **edge-wise** —
/// high = N·b_n + P·b_p, low = N/b_n + P/b_p — which is exact for two
/// independently banded terms at their extremes (a dose-weighted
/// log-mixture would understate `high` whenever the bands differ).
/// The neutron band is the neutronics library's ×/÷1.3.
pub fn combined_claims(
    neutron: &vcad_kernel_neutronics::receipt::ClaimSet,
    photon_reports: &[&DoseReport],
    provenance: PhotonProvenance,
) -> Result<ClaimSet, ClaimError> {
    let mut claims = Vec::new();
    for r in photon_reports {
        let name = format!("dose_rate:{}", r.label);
        let n = neutron
            .claims
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| ClaimError::MissingLabel(r.label.clone()))?;
        if !n.rse.is_finite() || !r.h10_usv_h.rse.is_finite() {
            return Err(ClaimError::NothingScored(format!(
                "total_dose_rate:{}",
                r.label
            )));
        }
        let nb = crate::capture::NEUTRON_LIBRARY_BAND;
        let pb = r.band_factor();
        let total = n.value + r.h10_usv_h.mean;
        let sigma = n.value * n.rse + r.h10_usv_h.abs_sigma();
        let high = n.value * nb + r.h10_usv_h.mean * pb;
        let low = n.value / nb + r.h10_usv_h.mean / pb;
        let b = (high / total).max(total / low);
        let mut terms = vec![BandTerm::factor(
            "neutron dose: design-estimate library",
            nb,
        )];
        terms.extend(r.band.iter().cloned());
        claims.push(Claim {
            name: format!("total_dose_rate:{}", r.label),
            value: total,
            rse: sigma / total,
            band_factor: b,
            low,
            high,
            band_terms: terms,
            unit: "uSv/h".into(),
            basis: "predicted".into(),
            note: format!(
                "neutron {:.3e} (vcad.neutronics-claims/1) + photon {:.3e} uSv/h; band edges summed \
                 edge-wise from neutron x/{nb} and photon x/{pb:.2}",
                n.value,
                r.h10_usv_h.mean
            ),
        });
    }
    let mut caveats = standard_caveats();
    caveats.extend(neutron.caveats.iter().map(|c| format!("neutron: {c}")));
    Ok(ClaimSet {
        schema: CLAIM_SCHEMA.into(),
        provenance,
        source: "neutron + capture-gamma (+ x-ray if supplied) total".into(),
        claims,
        caveats,
    })
}

/// A survey reading bound to a claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// Claim name.
    pub name: String,
    /// Reading, claim units.
    pub value: f64,
    /// One-sigma absolute uncertainty.
    pub uncertainty: f64,
    /// Instrument provenance.
    pub instrument: String,
}

/// Verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Inside the band (widened by 2σ of both MC and measurement).
    Holds,
    /// Outside it.
    Violated,
    /// Not measured (never passes).
    Unmeasured,
}

/// One comparison row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonEntry {
    /// Claim.
    pub name: String,
    /// Predicted value.
    pub predicted: f64,
    /// Measured value.
    pub measured: Option<f64>,
    /// Verdict.
    pub verdict: Verdict,
}

/// Bind measurements. Holds iff the reading lies in
/// [low − 2σ, high + 2σ], σ combining the claim's MC error and the
/// reading's uncertainty.
pub fn compare(
    set: &ClaimSet,
    measurements: &[Measurement],
) -> Result<Vec<ComparisonEntry>, String> {
    for m in measurements {
        if !set.claims.iter().any(|c| c.name == m.name) {
            return Err(format!("measurement {:?} matches no claim", m.name));
        }
    }
    Ok(set
        .claims
        .iter()
        .map(|c| match measurements.iter().find(|m| m.name == c.name) {
            None => ComparisonEntry {
                name: c.name.clone(),
                predicted: c.value,
                measured: None,
                verdict: Verdict::Unmeasured,
            },
            Some(m) => {
                let s = ((c.value * c.rse).powi(2) + m.uncertainty.powi(2)).sqrt();
                let holds = m.value >= c.low - 2.0 * s && m.value <= c.high + 2.0 * s;
                ComparisonEntry {
                    name: c.name.clone(),
                    predicted: c.value,
                    measured: Some(m.value),
                    verdict: if holds {
                        Verdict::Holds
                    } else {
                        Verdict::Violated
                    },
                }
            }
        })
        .collect())
}

/// Oracle reference.
pub fn oracle() -> vcad_receipt::OracleRef {
    vcad_receipt::OracleRef::new("vcad-kernel-photon-dose/mc", env!("CARGO_PKG_VERSION"))
}

/// Translate into unified-receipt claims: always
/// [`vcad_receipt::ClaimBasis::Predicted`] → a receipt built from these
/// rolls up Provisional, never Pass.
pub fn design_claims(set: &ClaimSet) -> Vec<vcad_receipt::ReceiptClaim> {
    let oracle = oracle();
    let prov = format!(
        "mc {}x{} seed {}; {}; {}; {}; source: {}",
        set.provenance.histories_per_batch,
        set.provenance.batches,
        set.provenance.seed,
        set.provenance.model,
        set.provenance.data,
        set.provenance.dose_coefficients,
        set.source
    );
    set.claims
        .iter()
        .map(|c| {
            let q = if c.unit == "1" {
                vcad_receipt::ClaimQuantity::bare(c.value)
            } else {
                vcad_receipt::ClaimQuantity::new(c.value, &c.unit)
            };
            vcad_receipt::ReceiptClaim::pass(
                format!("photon.{}", c.name),
                RECEIPT_DOMAIN,
                c.note.clone(),
                oracle.clone(),
            )
            .with_basis(vcad_receipt::ClaimBasis::Predicted)
            .with_measured(q)
            .with_details(format!(
                "{prov}; rse {:.4}; band x/{:.3} [{:.4e}, {:.4e}]",
                c.rse, c.band_factor, c.low, c.high
            ))
        })
        .collect()
}
