//! Neutron-capture gamma source from a neutronics run, and its dose.
//!
//! The neutron crate tallies track-length flux per region per group;
//! the capture rate of element i in region r is
//!
//! ```text
//!     Cᵢᵣ = S · Σ_g Nᵢ σ_a,i(g) φ_rg V_r        [captures / s]
//! ```
//!
//! with the neutron library's own σ_a and the material's own number
//! densities (the composition is restated here and checked against the
//! neutronics material's Σ_a at runtime — drift between the two crates
//! fails closed). Each capture emits the element's capture lines:
//!
//! | element | line | photons / capture | treatment |
//! |---|---|---|---|
//! | H | 2223 keV | 1.00 | transported (slab MC) |
//! | B | 478 keV (¹⁰B(n,α)⁷Li*) | 0.94 | transported |
//! | C | ≤ 4946 keV (Q) | 1 (bound) | unattenuated bound |
//! | O | ≤ 4143 keV (Q) | 1 (bound) | unattenuated bound |
//! | Pb | ≤ 7368 keV (²⁰⁷Pb Q) | 1 (bound) | unattenuated bound |
//!
//! Lines above the 3 MeV data window are **bounded**: one photon per
//! capture at the Q value, no attenuation, no buildup credit (B·e^{−μt}
//! ≤ 1 at MeV energies in these media, and splitting the cascade into
//! several lower-energy photons lowers H*(10) per capture). They are a
//! ≤1% term in polyethylene shields.
//!
//! Geometry (conservative, stated on every report): each region's
//! captures sit on a uniform spherical shell at the region's **outer**
//! radius (no self-shielding, nearest placement), seen from the detector
//! radius R through the non-air layers outside it at normal incidence
//! (slab transport for attenuation + buildup) with the exact shell
//! geometry factor. Air regions emit nothing here: ¹⁴N absorption is
//! mostly (n,p), and the small (n,γ) part (10.8 MeV cascade, outside
//! every bundled table) is a stated caveat. Materials outside the table
//! (concrete, validation fictions) fail closed.

use std::collections::BTreeMap;

use vcad_kernel_neutronics::geometry::Geometry;
use vcad_kernel_neutronics::groups::N_GROUPS;
use vcad_kernel_neutronics::materials::{self as nmat, ElementData};
use vcad_kernel_neutronics::spec::{evaluate as n_evaluate, ShieldSpec};
use vcad_kernel_neutronics::tally::Estimate;
use vcad_kernel_neutronics::transport::RunResult;

use crate::dose::{h10_psv_cm2, kerma_pgy_cm2, PSV_PER_S_TO_USV_PER_H};
use crate::materials::{self, Material};
use crate::report::{self, BandTerm, Component, DoseReport};
use crate::source::Spectrum;
use crate::spec::{Evaluation, RunSpec};
use crate::transport::{self, PathConfig, Slab, MIN_GAP_MM};
use crate::xsdata::DATA_MAX_KEV;

/// Systematic band on the neutron library's capture rates.
pub const NEUTRON_LIBRARY_BAND: f64 = 1.3;

/// Capture-gamma failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureError {
    /// Only spherical neutronics geometries are supported.
    NotSpherical,
    /// A region material with no capture-gamma table.
    UnsupportedEmitter(String),
    /// Restated composition disagrees with the neutronics material.
    CompositionDrift(String),
    /// A dominant flux group scored nothing.
    NothingScored {
        /// Region index.
        region: usize,
    },
    /// An emitting region extends to or beyond a detector.
    EmitterBeyondDetector {
        /// Detector label.
        label: String,
    },
    /// The neutronics spec failed.
    Neutronics(String),
    /// Photon transport failed.
    Transport(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::NotSpherical => write!(f, "capture gammas need a spherical neutronics geometry"),
            CaptureError::UnsupportedEmitter(m) => write!(f, "no capture-gamma table for material {m:?}"),
            CaptureError::CompositionDrift(m) => {
                write!(f, "restated composition of {m:?} disagrees with the neutronics library")
            }
            CaptureError::NothingScored { region } => {
                write!(f, "region {region}: dominant capture group scored nothing — run more neutron histories")
            }
            CaptureError::EmitterBeyondDetector { label } => {
                write!(f, "an emitting (non-air) region reaches detector {label:?}")
            }
            CaptureError::Neutronics(e) => write!(f, "neutronics: {e}"),
            CaptureError::Transport(e) => write!(f, "photon transport: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

struct Emitter {
    photon: Material,
    /// (element data, mass fraction, lines (keV, photons/capture))
    parts: Vec<(ElementData, f64, Vec<(f64, f64)>)>,
}

const H_LINES: &[(f64, f64)] = &[(2223.2, 1.0)];
const B_LINES: &[(f64, f64)] = &[(477.6, 0.94)];
const C_LINES: &[(f64, f64)] = &[(4946.3, 1.0)];
const O_LINES: &[(f64, f64)] = &[(4143.1, 1.0)];
const PB_LINES: &[(f64, f64)] = &[(7367.8, 1.0)];

/// Emitter table keyed by the neutronics material name. `Ok(None)` for
/// non-emitters (air, void).
fn emitter(name: &str) -> Result<Option<Emitter>, CaptureError> {
    let e = |photon: Material, parts: Vec<(ElementData, f64, &[(f64, f64)])>| {
        Some(Emitter {
            photon,
            parts: parts.into_iter().map(|(el, w, l)| (el, w, l.to_vec())).collect(),
        })
    };
    Ok(match name {
        "HDPE" => e(materials::hdpe(), vec![(nmat::HYDROGEN, 0.1437, H_LINES), (nmat::CARBON, 0.8563, C_LINES)]),
        "paraffin" => e(materials::paraffin(), vec![(nmat::HYDROGEN, 0.1486, H_LINES), (nmat::CARBON, 0.8514, C_LINES)]),
        "borated-HDPE-5%" => e(
            materials::borated_hdpe_5(),
            vec![
                (nmat::HYDROGEN, 0.95 * 0.1437, H_LINES),
                (nmat::CARBON, 0.95 * 0.8563, C_LINES),
                (nmat::BORON, 0.05, B_LINES),
            ],
        ),
        "water" => e(materials::water(), vec![(nmat::HYDROGEN, 0.1119, H_LINES), (nmat::OXYGEN, 0.8881, O_LINES)]),
        "lead" => e(materials::lead(), vec![(nmat::LEAD, 1.0, PB_LINES)]),
        "air" | "void" => None,
        other => return Err(CaptureError::UnsupportedEmitter(other.to_string())),
    })
}

/// One emitted line in one region.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureLine {
    /// Neutronics region index.
    pub region: usize,
    /// Element symbol.
    pub element: &'static str,
    /// Line energy, keV.
    pub energy_kev: f64,
    /// Emission rate, photons/s (± neutron-MC error).
    pub rate_per_s: Estimate,
}

/// Capture-gamma evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureEvaluation {
    /// Dose reports, one per detector.
    pub evaluation: Evaluation,
    /// Every emitted line.
    pub lines: Vec<CaptureLine>,
}

fn bounds_cm(g: &Geometry) -> Vec<f64> {
    let mut b = vec![0.0];
    for l in g.layers() {
        b.push(b.last().unwrap() + l.thickness_mm * 0.1);
    }
    b
}

/// Capture lines for every region of a spherical neutronics run.
pub fn capture_lines(
    geometry: &Geometry,
    result: &RunResult,
    source_rate_n_per_s: f64,
) -> Result<Vec<CaptureLine>, CaptureError> {
    if !matches!(geometry, Geometry::Sphere(_)) {
        return Err(CaptureError::NotSpherical);
    }
    let b = bounds_cm(geometry);
    let mut out = Vec::new();
    for (r, layer) in geometry.layers().iter().enumerate() {
        let Some(em) = emitter(&layer.material.name)? else {
            continue;
        };
        let rho = layer.material.density_g_cc;
        let vol = 4.0 / 3.0 * std::f64::consts::PI * (b[r + 1].powi(3) - b[r].powi(3));
        // Composition drift check against the neutronics Σ_a.
        for g in 0..N_GROUPS {
            let sa: f64 = em
                .parts
                .iter()
                .map(|(el, w, _)| rho * w * vcad_kernel_neutronics::constants::AVOGADRO / el.mass_amu * el.sigma_a_b[g] * 1e-24)
                .sum();
            let lib = layer.material.sigma_a[g];
            if (sa - lib).abs() > 1e-6 * lib.max(1e-30) {
                return Err(CaptureError::CompositionDrift(layer.material.name.clone()));
            }
        }
        for (el, w, lines) in &em.parts {
            let n_i = rho * w * vcad_kernel_neutronics::constants::AVOGADRO / el.mass_amu;
            let contrib: Vec<(f64, f64)> = (0..N_GROUPS)
                .map(|g| {
                    let f = result.flux_per_source[r][g];
                    (n_i * el.sigma_a_b[g] * 1e-24 * f.mean * vol * source_rate_n_per_s, f.rse)
                })
                .collect();
            let total: f64 = contrib.iter().map(|c| c.0).sum();
            if total <= 0.0 {
                continue;
            }
            // Conservative relative error: the worst rse among groups
            // carrying ≥ 1% of the capture rate.
            let mut rse: f64 = 0.0;
            for (c, e) in &contrib {
                if *c >= 0.01 * total {
                    if !e.is_finite() {
                        return Err(CaptureError::NothingScored { region: r });
                    }
                    rse = rse.max(*e);
                }
            }
            for (e_kev, y) in lines {
                out.push(CaptureLine {
                    region: r,
                    element: el.symbol,
                    energy_kev: *e_kev,
                    rate_per_s: Estimate {
                        mean: total * y,
                        rse,
                        batches: result.provenance.batches,
                    },
                });
            }
        }
    }
    Ok(out)
}

/// Price capture-gamma dose at detector regions of a spherical
/// neutronics run.
pub fn capture_gamma_reports(
    geometry: &Geometry,
    result: &RunResult,
    source_rate_n_per_s: f64,
    detectors: &[(String, usize)],
    run: &RunSpec,
) -> Result<CaptureEvaluation, CaptureError> {
    let lines = capture_lines(geometry, result, source_rate_n_per_s)?;
    let b = bounds_cm(geometry);
    let layers = geometry.layers();
    let mut reports = Vec::new();
    let mut truncated = 0;
    let mut scored = true;
    let mut provenance = None;
    for (label, det_region) in detectors {
        let r_det = 0.5 * (b[*det_region] + b[det_region + 1]);
        let mut h_parts = Vec::new();
        let mut k_parts = Vec::new();
        let mut comps: Vec<Component> = Vec::new();
        let mut dominant: Option<(f64, Vec<BandTerm>, f64)> = None;
        for line in &lines {
            let a = b[line.region + 1];
            if a + MIN_GAP_MM * 0.1 > r_det {
                return Err(CaptureError::EmitterBeyondDetector { label: label.clone() });
            }
            let z_cm = r_det - a;
            let g = z_cm * z_cm / (2.0 * a * r_det) * ((r_det + a) / (r_det - a)).ln();
            let (h, k, buildup, terms) = if line.energy_kev <= DATA_MAX_KEV {
                // Non-air layers between the emitting shell and the detector.
                let slabs: Vec<Slab> = layers[line.region + 1..*det_region]
                    .iter()
                    .filter_map(|l| match emitter(&l.material.name) {
                        Ok(Some(em)) => Some(Slab::new(em.photon, l.thickness_mm)),
                        _ => None,
                    })
                    .collect();
                let cfg = PathConfig {
                    histories_per_batch: run.histories_per_batch,
                    batches: run.batches,
                    seed: run.seed,
                    ..PathConfig::new(slabs, vec![z_cm * 10.0])
                };
                let spec = Spectrum::line(line.energy_kev, line.rate_per_s.mean);
                let res = transport::run(&spec, &cfg).map_err(|e| CaptureError::Transport(e.to_string()))?;
                truncated += res.truncated_histories;
                scored &= res.scatter_scored;
                provenance.get_or_insert(res.provenance.clone());
                let d = &res.detectors[0];
                let comb = |x: Estimate| Estimate {
                    mean: x.mean * g,
                    rse: (x.rse.powi(2) + line.rate_per_s.rse.powi(2)).sqrt(),
                    batches: x.batches.min(line.rate_per_s.batches),
                };
                (comb(d.h10_usv_h), comb(d.kerma_ugy_h), d.buildup_h10, report::data_terms(&cfg, d))
            } else {
                let phi = line.rate_per_s.mean * g / (4.0 * std::f64::consts::PI * z_cm * z_cm);
                let mk = |c: f64| Estimate {
                    mean: phi * c * PSV_PER_S_TO_USV_PER_H,
                    rse: line.rate_per_s.rse,
                    batches: line.rate_per_s.batches,
                };
                (mk(h10_psv_cm2(line.energy_kev)), mk(kerma_pgy_cm2(line.energy_kev)), 1.0, vec![])
            };
            comps.push(Component {
                name: format!(
                    "{} capture {:.0} keV from region {}{}",
                    line.element,
                    line.energy_kev,
                    line.region,
                    if line.energy_kev > DATA_MAX_KEV { " (unattenuated bound)" } else { "" }
                ),
                h10_usv_h: h.mean,
            });
            if dominant.as_ref().map_or(true, |d| h.mean > d.0) {
                dominant = Some((h.mean, terms, buildup));
            }
            h_parts.push(h);
            k_parts.push(k);
        }
        let (_, data, buildup) = dominant.unwrap_or((0.0, vec![], 1.0));
        let mut band = vec![BandTerm::factor(
            "neutronics design-estimate library (capture rates, +/-20-30%)",
            NEUTRON_LIBRARY_BAND,
        )];
        band.extend(data);
        reports.push(DoseReport {
            label: label.clone(),
            distance_mm: r_det * 10.0,
            h10_usv_h: report::sum_conservative(&h_parts),
            kerma_ugy_h: report::sum_conservative(&k_parts),
            band,
            buildup_h10: buildup,
            components: comps,
            geometry_note: format!(
                "detector shell at r = {:.0} mm; each region's captures on a shell at its outer radius \
                 (no self-shielding), normal-incidence slab attenuation through outer non-air layers",
                r_det * 10.0
            ),
        });
    }
    Ok(CaptureEvaluation {
        evaluation: Evaluation {
            reports,
            truncated_histories: truncated,
            scatter_scored: scored,
            provenance: provenance.unwrap_or_else(|| transport::PhotonProvenance {
                seed: run.seed,
                histories_per_batch: run.histories_per_batch,
                batches: run.batches,
                data: "none (bounded lines only)".into(),
                dose_coefficients: "ICRP 74 photon".into(),
                model: "unattenuated bound".into(),
            }),
            source: format!(
                "neutron-capture gammas, {source_rate_n_per_s:.3e} n/s source, neutron MC {}x{} seed {}",
                result.provenance.histories_per_batch, result.provenance.batches, result.provenance.seed
            ),
        },
        lines,
    })
}

/// End-to-end: run a neutronics [`ShieldSpec`] and price both its
/// neutron claims and the capture-gamma dose at the same detectors.
pub fn from_shield_spec(
    spec: &ShieldSpec,
    params: &BTreeMap<String, f64>,
    run: &RunSpec,
) -> Result<(vcad_kernel_neutronics::receipt::ClaimSet, CaptureEvaluation), CaptureError> {
    let resolved = spec.resolve(params).map_err(|e| CaptureError::Neutronics(e.to_string()))?;
    let (doses, result) = n_evaluate(spec, params).map_err(|e| CaptureError::Neutronics(e.to_string()))?;
    let neutron = vcad_kernel_neutronics::receipt::claims_from_run(
        spec,
        &resolved.detector_regions,
        &doses,
        &result,
        params,
    )
    .map_err(|e| CaptureError::Neutronics(e.to_string()))?;
    let cap = capture_gamma_reports(
        &resolved.config.geometry,
        &result,
        resolved.source_rate_n_per_s,
        &resolved.detector_regions,
        run,
    )?;
    Ok((neutron, cap))
}
