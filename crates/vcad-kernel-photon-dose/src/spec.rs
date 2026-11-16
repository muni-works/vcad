//! The `.vcad` seam: serde photon-dose schemas with named parameters.
//!
//! Two problem shapes, both fail-closed on resolve (unbound parameter,
//! unknown material, voltage outside the Kramers band, detector inside
//! the minimum gap, non-positive geometry — each an error, never a
//! default):
//!
//! - [`PathSpec`] — a generic point source (Kramers bremsstrahlung or a
//!   discrete line) behind a slab stack, detectors on axis.
//! - [`ChamberSpec`] — the IEC-fusor x-ray problem: electrons of energy
//!   e·V (cathode-emitted secondaries, reaching the grounded wall at full
//!   energy) load the inner surface of a spherical chamber **uniformly**;
//!   detectors look either through the steel wall or along a viewport
//!   axis.
//!
//! **Chamber source model** (`evaluate_chamber`). Let a be the chamber
//! radius, r the viewport clear radius, f = r²/4a² the viewport's share
//! of the wall area, S_Z the 4π Kramers spectrum for the full electron
//! current on a target of Z.
//!
//! - *Wall path*: all wall emission exits through one wall thickness
//!   (the chamber is vacuum). A uniform spherical shell source of radius
//!   a gives, unattenuated, φ = S/(8πaR)·ln((R+a)/(R−a)) at distance R
//!   from the center; the slab transport at the same distance from the
//!   wall supplies attenuation and buildup at **normal** incidence
//!   (oblique rays see more steel → conservative).
//! - *Viewport path*: two equal-geometry terms, both an equivalent point
//!   source at the viewport's inner face: electrons striking the glass
//!   itself (f·S_glass, times a `beaming_factor` for star-mode jets aimed
//!   at the port), and wall emission seen through the aperture — by
//!   radiance invariance the far-wall patch inside the aperture's solid
//!   angle contributes exactly f·S_steel at the viewport, independent of
//!   how far away that patch is. The viewport-axis detector also gets
//!   the wall-path dose at its radius (the flange around the port).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::materials::{self, Material};
use crate::report::{self, BandTerm, Component, DoseReport};
use crate::source::{SourceError, Spectrum, ThickTarget};
use crate::transport::{
    self, PathConfig, PathResult, Slab, TransportError, MIN_GAP_MM, TRANSPORT_CUT_KEV,
};

/// Bremsstrahlung spectrum bins used by the spec evaluators.
pub const SPECTRUM_BINS: usize = 120;
/// Systematic band on uniform-wall-loading geometry (star-mode jets,
/// non-uniform electron deposition).
pub const CHAMBER_GEOMETRY_BAND: f64 = 2.0;

/// A literal value or the name of a document parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    /// A literal number.
    Literal(f64),
    /// A parameter name bound at resolve time.
    Named(String),
}

impl From<f64> for ParamValue {
    fn from(v: f64) -> Self {
        ParamValue::Literal(v)
    }
}

impl ParamValue {
    /// Resolve against bindings (fail-closed).
    pub fn resolve(&self, params: &BTreeMap<String, f64>) -> Result<f64, SpecError> {
        match self {
            ParamValue::Literal(v) => Ok(*v),
            ParamValue::Named(n) => params
                .get(n)
                .copied()
                .ok_or_else(|| SpecError::UnboundParameter(n.clone())),
        }
    }
}

fn one() -> ParamValue {
    ParamValue::Literal(1.0)
}

/// One slab layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSpec {
    /// Library material name ([`materials::by_name`]).
    pub material: String,
    /// Thickness, mm.
    pub thickness_mm: ParamValue,
}

/// Monte Carlo parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSpec {
    /// Histories per batch.
    #[serde(default = "default_histories")]
    pub histories_per_batch: usize,
    /// Batches.
    #[serde(default = "default_batches")]
    pub batches: usize,
    /// Seed.
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_histories() -> usize {
    20_000
}
fn default_batches() -> usize {
    20
}
fn default_seed() -> u64 {
    20261116
}

impl Default for RunSpec {
    fn default() -> Self {
        RunSpec {
            histories_per_batch: default_histories(),
            batches: default_batches(),
            seed: default_seed(),
        }
    }
}

/// Source for a [`PathSpec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathSource {
    /// Thick-target bremsstrahlung.
    Bremsstrahlung {
        /// Tube voltage magnitude, kV.
        voltage_kv: ParamValue,
        /// Electron current on target, mA.
        electron_current_ma: ParamValue,
        /// Target material.
        target: String,
    },
    /// A discrete line.
    Line {
        /// Energy, keV.
        energy_kev: ParamValue,
        /// Emission rate into 4π, photons/s.
        rate_per_s: ParamValue,
    },
}

/// An on-axis detector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectorSpec {
    /// Label carried onto claims.
    pub label: String,
    /// Distance (meaning depends on the problem shape), mm.
    pub distance_mm: ParamValue,
}

/// A generic point-source-behind-slabs problem. Detector distance is
/// measured from the **source point**.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathSpec {
    /// Source.
    pub source: PathSource,
    /// Slabs from the source face outward.
    pub layers: Vec<LayerSpec>,
    /// Detectors.
    pub detectors: Vec<DetectorSpec>,
    /// MC parameters.
    #[serde(default)]
    pub run: RunSpec,
}

/// Which way a chamber detector looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChamberPath {
    /// Through the chamber wall.
    Wall,
    /// Along the viewport axis.
    Viewport,
}

/// A chamber detector; distance measured from the **outer face** of the
/// wall or of the viewport stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChamberDetector {
    /// Label.
    pub label: String,
    /// Path.
    pub path: ChamberPath,
    /// Distance from the outer face, mm.
    pub distance_mm: ParamValue,
}

/// A viewport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewportSpec {
    /// Clear (unobstructed) radius, mm.
    pub clear_radius_mm: ParamValue,
    /// Stack from the vacuum side outward; the first layer is the
    /// electron-struck window (its material is the bremsstrahlung target).
    pub layers: Vec<LayerSpec>,
}

/// The IEC-fusor x-ray problem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChamberSpec {
    /// Cathode voltage magnitude, kV.
    pub voltage_kv: ParamValue,
    /// Supply current, mA.
    pub current_ma: ParamValue,
    /// Fraction of the supply current that reaches the wall as electrons
    /// at full energy (1.0 = bound).
    pub electron_fraction: ParamValue,
    /// Chamber inner radius, mm.
    pub chamber_radius_mm: ParamValue,
    /// Chamber wall.
    pub wall: LayerSpec,
    /// Viewport (optional).
    #[serde(default)]
    pub viewport: Option<ViewportSpec>,
    /// Electron-loading multiplier on the viewport glass (star-mode jet
    /// aimed at the port; 1 = uniform).
    #[serde(default = "one")]
    pub beaming_factor: ParamValue,
    /// Detectors.
    pub detectors: Vec<ChamberDetector>,
    /// MC parameters.
    #[serde(default)]
    pub run: RunSpec,
}

/// Resolution / evaluation failures.
#[derive(Debug, Clone, PartialEq)]
pub enum SpecError {
    /// Unbound named parameter.
    UnboundParameter(String),
    /// Unknown material.
    UnknownMaterial(String),
    /// Non-positive or non-finite geometry value.
    BadValue {
        /// Which field.
        field: String,
        /// Value.
        value: f64,
    },
    /// No detectors.
    NoDetectors,
    /// A viewport detector with no viewport declared.
    NoViewport(String),
    /// Source failure.
    Source(SourceError),
    /// Transport failure.
    Transport(TransportError),
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::UnboundParameter(n) => write!(f, "unbound parameter {n:?}"),
            SpecError::UnknownMaterial(m) => write!(f, "unknown photon material {m:?}"),
            SpecError::BadValue { field, value } => {
                write!(f, "{field} = {value} must be positive and finite")
            }
            SpecError::NoDetectors => write!(f, "no detectors"),
            SpecError::NoViewport(l) => write!(
                f,
                "detector {l:?} looks through a viewport, but none is declared"
            ),
            SpecError::Source(e) => write!(f, "source: {e}"),
            SpecError::Transport(e) => write!(f, "transport: {e}"),
        }
    }
}

impl std::error::Error for SpecError {}

fn positive(field: &str, v: &ParamValue, p: &BTreeMap<String, f64>) -> Result<f64, SpecError> {
    let x = v.resolve(p)?;
    if x.is_finite() && x > 0.0 {
        Ok(x)
    } else {
        Err(SpecError::BadValue {
            field: field.to_string(),
            value: x,
        })
    }
}

fn material(name: &str) -> Result<Material, SpecError> {
    materials::by_name(name).ok_or_else(|| SpecError::UnknownMaterial(name.to_string()))
}

fn slabs(layers: &[LayerSpec], p: &BTreeMap<String, f64>) -> Result<Vec<Slab>, SpecError> {
    layers
        .iter()
        .map(|l| {
            Ok(Slab::new(
                material(&l.material)?,
                positive(&format!("{} thickness", l.material), &l.thickness_mm, p)?,
            ))
        })
        .collect()
}

fn config(slabs: Vec<Slab>, distances: Vec<f64>, run: &RunSpec) -> PathConfig {
    PathConfig {
        histories_per_batch: run.histories_per_batch,
        batches: run.batches,
        seed: run.seed,
        ..PathConfig::new(slabs, distances)
    }
}

/// What a spec evaluation returns: priced detectors + run bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    /// One report per detector, in spec order.
    pub reports: Vec<DoseReport>,
    /// Sum of truncated histories over every run.
    pub truncated_histories: u64,
    /// False if any scattering run scored nothing.
    pub scatter_scored: bool,
    /// Provenance of the (last) run.
    pub provenance: transport::PhotonProvenance,
    /// Human-readable source description.
    pub source: String,
}

fn source_terms(kind: &str) -> Vec<BandTerm> {
    match kind {
        "brems" => vec![BandTerm::factor(
            "Kramers thick-target yield k = 1.1e-9/V (published 0.9-1.4e-9) and spectral shape",
            crate::source::KRAMERS_BAND,
        )],
        _ => vec![],
    }
}

/// Evaluate a [`PathSpec`].
pub fn evaluate_path(spec: &PathSpec, p: &BTreeMap<String, f64>) -> Result<Evaluation, SpecError> {
    if spec.detectors.is_empty() {
        return Err(SpecError::NoDetectors);
    }
    let (spectrum, kind, desc) = match &spec.source {
        PathSource::Bremsstrahlung {
            voltage_kv,
            electron_current_ma,
            target,
        } => {
            let v = positive("voltage_kv", voltage_kv, p)?;
            let i = positive("electron_current_ma", electron_current_ma, p)?;
            let t = ThickTarget::new(v, i, material(target)?).map_err(SpecError::Source)?;
            (
                t.spectrum(TRANSPORT_CUT_KEV, SPECTRUM_BINS)
                    .map_err(SpecError::Source)?,
                "brems",
                format!(
                    "Kramers thick-target bremsstrahlung, {v} kV, {i} mA electrons on {target}"
                ),
            )
        }
        PathSource::Line {
            energy_kev,
            rate_per_s,
        } => {
            let e = positive("energy_kev", energy_kev, p)?;
            let r = positive("rate_per_s", rate_per_s, p)?;
            (
                Spectrum::line(e, r),
                "line",
                format!("{e} keV line, {r:.3e} photons/s"),
            )
        }
    };
    let s = slabs(&spec.layers, p)?;
    let dists = spec
        .detectors
        .iter()
        .map(|d| positive(&d.label, &d.distance_mm, p))
        .collect::<Result<Vec<_>, _>>()?;
    let cfg = config(s, dists, &spec.run);
    let res = transport::run(&spectrum, &cfg).map_err(SpecError::Transport)?;
    let reports = spec
        .detectors
        .iter()
        .zip(&res.detectors)
        .map(|(d, r)| {
            let mut band = source_terms(kind);
            band.extend(report::data_terms(&cfg, r));
            if kind == "brems" {
                band.extend(report::tip_term(r.tip_fraction));
            }
            DoseReport {
                label: d.label.clone(),
                distance_mm: r.distance_mm,
                h10_usv_h: r.h10_usv_h,
                kerma_ugy_h: r.kerma_ugy_h,
                band,
                buildup_h10: r.buildup_h10,
                components: vec![Component {
                    name: "source".into(),
                    h10_usv_h: r.h10_usv_h.mean,
                }],
                geometry_note:
                    "on-axis point detector; distance from the source point; infinite slabs".into(),
            }
        })
        .collect();
    Ok(Evaluation {
        reports,
        truncated_histories: res.truncated_histories,
        scatter_scored: res.scatter_scored,
        provenance: res.provenance,
        source: desc,
    })
}

/// Evaluate a [`ChamberSpec`] (see module docs for the source model).
pub fn evaluate_chamber(
    spec: &ChamberSpec,
    p: &BTreeMap<String, f64>,
) -> Result<Evaluation, SpecError> {
    if spec.detectors.is_empty() {
        return Err(SpecError::NoDetectors);
    }
    let v = positive("voltage_kv", &spec.voltage_kv, p)?;
    let i = positive("current_ma", &spec.current_ma, p)?;
    let fe = spec.electron_fraction.resolve(p)?;
    if !(fe > 0.0 && fe <= 1.0) {
        return Err(SpecError::Source(SourceError::BadElectronFraction(fe)));
    }
    let a_mm = positive("chamber_radius_mm", &spec.chamber_radius_mm, p)?;
    let beaming = positive("beaming_factor", &spec.beaming_factor, p)?;
    let wall_mat = material(&spec.wall.material)?;
    let wall_t = positive("wall thickness", &spec.wall.thickness_mm, p)?;
    let ie = i * fe;
    let wall_src = ThickTarget::new(v, ie, wall_mat.clone()).map_err(SpecError::Source)?;
    let s_wall = wall_src
        .spectrum(TRANSPORT_CUT_KEV, SPECTRUM_BINS)
        .map_err(SpecError::Source)?;

    let dists: Vec<(f64, ChamberPath)> = spec
        .detectors
        .iter()
        .map(|d| Ok((positive(&d.label, &d.distance_mm, p)?, d.path)))
        .collect::<Result<_, SpecError>>()?;

    // Viewport geometry (if any).
    let vp = match &spec.viewport {
        Some(vp) => {
            let r = positive("viewport clear_radius_mm", &vp.clear_radius_mm, p)?;
            if r >= a_mm {
                return Err(SpecError::BadValue {
                    field: "viewport clear_radius_mm (must be < chamber radius)".into(),
                    value: r,
                });
            }
            let s = slabs(&vp.layers, p)?;
            if s.is_empty() {
                return Err(SpecError::BadValue {
                    field: "viewport layers (need ≥ 1)".into(),
                    value: 0.0,
                });
            }
            Some((r, s))
        }
        None => None,
    };
    for (d, (_, path)) in spec.detectors.iter().zip(&dists) {
        if *path == ChamberPath::Viewport && vp.is_none() {
            return Err(SpecError::NoViewport(d.label.clone()));
        }
    }

    let mut truncated = 0;
    let mut scored = true;
    let mut provenance = None;

    // Wall path: one run at every detector radius (viewport detectors
    // need the wall term too). Slab distance from the source point =
    // wall thickness + gap (viewport detectors: gap + viewport stack −
    // wall, floored to the minimum gap — the flange term is a minor
    // component and this keeps it on the conservative side).
    let vp_stack: f64 = vp
        .as_ref()
        .map(|(_, s)| s.iter().map(|x| x.thickness_mm).sum())
        .unwrap_or(0.0);
    let wall_gaps: Vec<f64> = dists
        .iter()
        .map(|(d, path)| match path {
            ChamberPath::Wall => *d,
            ChamberPath::Viewport => (d + vp_stack - wall_t).max(MIN_GAP_MM),
        })
        .collect();
    let wall_cfg = config(
        vec![Slab::new(wall_mat.clone(), wall_t)],
        wall_gaps.iter().map(|g| wall_t + g).collect(),
        &spec.run,
    );
    let wall_res = transport::run(&s_wall, &wall_cfg).map_err(SpecError::Transport)?;
    truncated += wall_res.truncated_histories;
    scored &= wall_res.scatter_scored;
    provenance.get_or_insert(wall_res.provenance.clone());

    // Shell-source geometry factor: (shell φ) / (slab-point φ).
    let a = a_mm * 0.1;
    let shell = |z_mm: f64| {
        let z = z_mm * 0.1;
        let r = a + z; // detector radius from the chamber center (wall thickness inside z)
        z * z / (2.0 * a * r) * ((r + a) / (r - a)).ln()
    };

    // Viewport path run.
    let vp_run: Option<(PathResult, PathConfig, f64)> = match &vp {
        Some((r_vp, s)) => {
            let f = (r_vp / a_mm).powi(2) / 4.0;
            let glass =
                ThickTarget::new(v, ie, s[0].material.clone()).map_err(SpecError::Source)?;
            let s_glass = glass
                .spectrum(TRANSPORT_CUT_KEV, SPECTRUM_BINS)
                .map_err(SpecError::Source)?;
            let mut combined = s_wall.scaled(f);
            for (b, g) in combined.bins.iter_mut().zip(&s_glass.bins) {
                b.rate_per_s += g.rate_per_s * f * beaming;
            }
            let vdists: Vec<f64> = dists
                .iter()
                .filter(|(_, path)| *path == ChamberPath::Viewport)
                .map(|(d, _)| vp_stack + d)
                .collect();
            if vdists.is_empty() {
                None
            } else {
                let cfg = config(s.clone(), vdists, &spec.run);
                let res = transport::run(&combined, &cfg).map_err(SpecError::Transport)?;
                truncated += res.truncated_histories;
                scored &= res.scatter_scored;
                Some((res, cfg, f))
            }
        }
        None => None,
    };

    let mut reports = Vec::new();
    let mut vi = 0;
    for (k, (det, (d, path))) in spec.detectors.iter().zip(&dists).enumerate() {
        let wr = &wall_res.detectors[k];
        let g = shell(wall_t + wall_gaps[k]);
        let wall_h = wr.h10_usv_h.scaled(g);
        let wall_k = wr.kerma_ugy_h.scaled(g);
        let mut band = source_terms("brems");
        band.push(BandTerm::factor(
            "uniform electron loading of the chamber wall (star-mode jets concentrate it)",
            CHAMBER_GEOMETRY_BAND,
        ));
        match path {
            ChamberPath::Wall => {
                band.extend(report::data_terms(&wall_cfg, wr));
                band.extend(report::tip_term(wr.tip_fraction));
                reports.push(DoseReport {
                    label: det.label.clone(),
                    distance_mm: *d,
                    h10_usv_h: wall_h,
                    kerma_ugy_h: wall_k,
                    band,
                    buildup_h10: wr.buildup_h10,
                    components: vec![Component {
                        name: format!("wall bremsstrahlung through {wall_t} mm {}", wall_mat.name),
                        h10_usv_h: wall_h.mean,
                    }],
                    geometry_note: format!(
                        "{d} mm from the wall's outer face; uniform spherical shell source r = {a_mm} mm, \
                         normal-incidence slab attenuation (shell factor {g:.3})"
                    ),
                });
            }
            ChamberPath::Viewport => {
                let (res, cfg, f) = vp_run.as_ref().expect("viewport run exists");
                let r = &res.detectors[vi];
                vi += 1;
                // Bands of the dominant component.
                if r.h10_usv_h.mean >= wall_h.mean {
                    band.extend(report::data_terms(cfg, r));
                    band.extend(report::tip_term(r.tip_fraction));
                } else {
                    band.extend(report::data_terms(&wall_cfg, wr));
                    band.extend(report::tip_term(wr.tip_fraction));
                }
                let stack_desc: Vec<String> = cfg
                    .slabs
                    .iter()
                    .map(|s| format!("{} mm {}", s.thickness_mm, s.material.name))
                    .collect();
                reports.push(DoseReport {
                    label: det.label.clone(),
                    distance_mm: *d,
                    h10_usv_h: report::sum_conservative(&[r.h10_usv_h, wall_h]),
                    kerma_ugy_h: report::sum_conservative(&[r.kerma_ugy_h, wall_k]),
                    band,
                    buildup_h10: r.buildup_h10,
                    components: vec![
                        Component {
                            name: format!("viewport ({})", stack_desc.join(" + ")),
                            h10_usv_h: r.h10_usv_h.mean,
                        },
                        Component {
                            name: format!("flange/wall through {wall_t} mm {}", wall_mat.name),
                            h10_usv_h: wall_h.mean,
                        },
                    ],
                    geometry_note: format!(
                        "{d} mm from the viewport's outer face on its axis; viewport area share f = {f:.4} \
                         of the wall (glass-target + far-wall-through-aperture terms, beaming x{beaming})"
                    ),
                });
            }
        }
    }
    Ok(Evaluation {
        reports,
        truncated_histories: truncated,
        scatter_scored: scored,
        provenance: provenance.expect("wall run always happens"),
        source: format!(
            "IEC chamber: {v} kV, {i} mA supply x electron fraction {fe} = {ie} mA electrons at full \
             energy on {} (r = {a_mm} mm)",
            wall_mat.name
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chamber_json() -> &'static str {
        r#"{
          "voltage_kv": "kv",
          "current_ma": 10,
          "electron_fraction": 1.0,
          "chamber_radius_mm": 76,
          "wall": {"material": "ss304", "thickness_mm": 2.0},
          "viewport": {"clear_radius_mm": 19, "layers": [{"material": "borosilicate", "thickness_mm": 5}]},
          "detectors": [
            {"label": "camera", "path": "viewport", "distance_mm": 500},
            {"label": "bench", "path": "wall", "distance_mm": 1000}
          ],
          "run": {"histories_per_batch": 2000, "batches": 4, "seed": 3}
        }"#
    }

    #[test]
    fn unbound_parameter_fails_closed() {
        let spec: ChamberSpec = serde_json::from_str(chamber_json()).unwrap();
        assert_eq!(
            evaluate_chamber(&spec, &BTreeMap::new()).unwrap_err(),
            SpecError::UnboundParameter("kv".into())
        );
    }

    #[test]
    fn viewport_dominates_wall() {
        let spec: ChamberSpec = serde_json::from_str(chamber_json()).unwrap();
        let p = BTreeMap::from([("kv".to_string(), 40.0)]);
        let ev = evaluate_chamber(&spec, &p).unwrap();
        assert_eq!(ev.truncated_histories, 0);
        let cam = &ev.reports[0];
        let bench = &ev.reports[1];
        assert!(cam.h10_usv_h.mean > 0.0 && bench.h10_usv_h.mean > 0.0);
        // Glass leaks; 2 mm steel at 40 kV does not.
        assert!(cam.components[0].h10_usv_h > 3.0 * cam.components[1].h10_usv_h);
        assert!(cam.band_factor() >= 2.0);
    }

    #[test]
    fn bad_inputs_fail_closed() {
        let mut spec: ChamberSpec = serde_json::from_str(chamber_json()).unwrap();
        let p = BTreeMap::from([("kv".to_string(), 40.0)]);
        spec.wall.material = "kryptonite".into();
        assert!(matches!(
            evaluate_chamber(&spec, &p),
            Err(SpecError::UnknownMaterial(_))
        ));
        let mut spec: ChamberSpec = serde_json::from_str(chamber_json()).unwrap();
        spec.viewport = None;
        assert!(matches!(
            evaluate_chamber(&spec, &p),
            Err(SpecError::NoViewport(_))
        ));
        let spec: ChamberSpec = serde_json::from_str(chamber_json()).unwrap();
        let p = BTreeMap::from([("kv".to_string(), 300.0)]);
        assert!(matches!(
            evaluate_chamber(&spec, &p),
            Err(SpecError::Source(_))
        ));
    }
}
