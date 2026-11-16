//! Dose reports and the systematic uncertainty band.
//!
//! Every reported dose carries **two** uncertainties, kept apart because
//! they mean different things:
//!
//! - `rse` — Monte Carlo statistical relative standard error (shrinks
//!   with histories);
//! - `band_factor` — a multiplicative systematic band B: the model
//!   claims the true value lies in [value/B, value·B]. It is composed
//!   in log-quadrature from named terms ([`BandTerm`]): source-yield
//!   constant, source geometry, per-slab attenuation-data uncertainty
//!   (u·τ, so thick shields automatically widen the band), and the
//!   Kramers spectral-tip sensitivity when the transmitted dose comes
//!   mostly from the top 10% of the spectrum. Running more histories
//!   cannot shrink it; only measurement can.

use serde::{Deserialize, Serialize};
use vcad_kernel_neutronics::tally::Estimate;

use crate::transport::{DetectorResult, PathConfig};

/// One named contribution to the systematic band (natural-log width).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BandTerm {
    /// What it covers.
    pub name: String,
    /// ln of its multiplicative factor.
    pub ln_factor: f64,
}

impl BandTerm {
    /// From a multiplicative factor ≥ 1.
    pub fn factor(name: &str, f: f64) -> Self {
        BandTerm {
            name: name.to_string(),
            ln_factor: f.ln(),
        }
    }
}

/// Compose band terms in log-quadrature.
pub fn compose(terms: &[BandTerm]) -> f64 {
    terms.iter().map(|t| t.ln_factor * t.ln_factor).sum::<f64>().sqrt().exp()
}

/// Attenuation-data band terms for one detector of one path: u_k·τ_k
/// per slab (material μ uncertainty × dose-weighted optical depth),
/// plus 5% of the air-gap τ.
pub fn data_terms(cfg: &PathConfig, det: &DetectorResult) -> Vec<BandTerm> {
    let mut out: Vec<BandTerm> = cfg
        .slabs
        .iter()
        .zip(&det.tau_per_slab)
        .map(|(s, tau)| BandTerm {
            name: format!("attenuation data: {} {} mm (tau {:.2})", s.material.name, s.thickness_mm, tau),
            ln_factor: s.material.mu_rel_uncertainty * tau,
        })
        .collect();
    out.push(BandTerm {
        name: format!("attenuation data: air gap (tau {:.3})", det.tau_air),
        ln_factor: 0.05 * det.tau_air,
    });
    out
}

/// Kramers spectral-tip term: when > half the transmitted H*(10) comes
/// from the top 10% of the spectrum, the linear-to-endpoint shape
/// assumption is load-bearing — ×/÷ 1.5.
pub fn tip_term(tip_fraction: f64) -> Option<BandTerm> {
    (tip_fraction > 0.5).then(|| BandTerm::factor("Kramers spectral tip (dose from top 10% of spectrum)", 1.5))
}

/// A named dose component (for the breakdown table).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    /// Component name.
    pub name: String,
    /// H*(10) rate contribution, µSv/h.
    pub h10_usv_h: f64,
}

/// A priced detector: the unit every claim is built from.
#[derive(Debug, Clone, PartialEq)]
pub struct DoseReport {
    /// Detector label.
    pub label: String,
    /// Distance as specified (meaning stated in `geometry_note`), mm.
    pub distance_mm: f64,
    /// H*(10) rate, µSv/h.
    pub h10_usv_h: Estimate,
    /// Air kerma rate, µGy/h.
    pub kerma_ugy_h: Estimate,
    /// Systematic band terms.
    pub band: Vec<BandTerm>,
    /// Dose buildup (total / uncollided) of the dominant component.
    pub buildup_h10: f64,
    /// Breakdown by component.
    pub components: Vec<Component>,
    /// What the distance means and how the source was placed.
    pub geometry_note: String,
}

impl DoseReport {
    /// Composed band factor.
    pub fn band_factor(&self) -> f64 {
        compose(&self.band)
    }

    /// Lower edge of the band on H*(10), µSv/h.
    pub fn h10_low(&self) -> f64 {
        self.h10_usv_h.mean / self.band_factor()
    }

    /// Upper edge of the band on H*(10), µSv/h — the number a safety
    /// case should use.
    pub fn h10_high(&self) -> f64 {
        self.h10_usv_h.mean * self.band_factor()
    }
}

/// Sum estimates whose errors may be correlated: means add, absolute
/// sigmas add **linearly** (the conservative bound).
pub fn sum_conservative(parts: &[Estimate]) -> Estimate {
    let mean: f64 = parts.iter().map(|e| e.mean).sum();
    let sigma: f64 = parts.iter().map(|e| if e.mean == 0.0 { 0.0 } else { e.abs_sigma() }).sum();
    Estimate {
        mean,
        rse: if mean > 0.0 { sigma / mean } else { f64::INFINITY },
        batches: parts.iter().map(|e| e.batches).min().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_quadrature() {
        let b = compose(&[BandTerm::factor("a", 2.0), BandTerm::factor("b", 2.0)]);
        assert!((b - 2f64.powf(2f64.sqrt())).abs() < 1e-12);
        assert_eq!(compose(&[]), 1.0);
    }

    #[test]
    fn conservative_sum() {
        let a = Estimate { mean: 1.0, rse: 0.1, batches: 10 };
        let b = Estimate { mean: 3.0, rse: 0.1, batches: 8 };
        let s = sum_conservative(&[a, b]);
        assert!((s.mean - 4.0).abs() < 1e-12);
        assert!((s.rse - 0.1).abs() < 1e-12);
        assert_eq!(s.batches, 8);
    }
}
