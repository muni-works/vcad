//! The `.vcad` seam for room-scale scenes (M7): a serde schema for a
//! [`crate::scene::Scene`] + source + detectors-with-instruments + dose
//! mesh, with named parameters.
//!
//! Same contract as the 1D [`crate::spec::ShieldSpec`]: every scalar is
//! a [`ParamValue`] (literal or a named document parameter), resolution
//! is fail-closed (unbound name, unknown material, out-of-structure
//! energy, a cell poking out of the world, a source outside it, an
//! instrument without its calibration — each is an error, never a
//! default), and results come back as [`Estimate`]s.
//!
//! Instruments ride on detectors: a detector is a virtual tally sphere
//! at a position; each instrument declared on it is folded there
//! ([`crate::response`]). Moderated-counter responses are computed once
//! per distinct counter design (shared across detectors) before the room
//! run, and their own MC uncertainty is carried into the claims.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::groups::{group_of_energy_ev, THERMAL_GROUP};
use crate::materials;
use crate::response::{
    bubble_dosimeter, cr39, rem_meter, CounterGas, ModeratedCounter, ResponseError,
    ResponseFunction,
};
use crate::scene::{Axis, Cell, Scene, Shape};
use crate::spec::{ParamValue, RunSpec, SpecError};
use crate::tally::Estimate;
use crate::transport3d::{
    run_scene, DetectorTally, MeshTally, SceneConfig, SceneResult, SceneRunError, SceneSource,
};

type P3 = [ParamValue; 3];

/// A convex primitive in mm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShapeSpec {
    /// Axis-aligned box by corners.
    Box {
        /// Lower corner, mm.
        min_mm: P3,
        /// Upper corner, mm.
        max_mm: P3,
    },
    /// Axis-aligned box by center and half-extents.
    BoxCentered {
        /// Center, mm.
        center_mm: P3,
        /// Half-extents, mm.
        half_mm: P3,
    },
    /// Sphere.
    Sphere {
        /// Center, mm.
        center_mm: P3,
        /// Radius, mm.
        radius_mm: ParamValue,
    },
    /// Cylinder along a principal axis from its base disc.
    Cylinder {
        /// Base-disc center, mm.
        base_mm: P3,
        /// Axis ("x" | "y" | "z").
        axis: Axis,
        /// Radius, mm.
        radius_mm: ParamValue,
        /// Length along the axis, mm.
        height_mm: ParamValue,
    },
}

/// One material cell (priority order; the last cell is the world).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellSpec {
    /// Unique label.
    pub name: String,
    /// Library material (see [`materials::by_name`]).
    pub material: String,
    /// The primitive.
    pub shape: ShapeSpec,
}

/// Isotropic point source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneSourceSpec {
    /// Position, mm.
    pub position_mm: P3,
    /// Emission rate, n/s.
    pub rate_n_per_s: ParamValue,
    /// Line energy, eV (D-D: 2.45e6).
    pub energy_ev: ParamValue,
}

fn default_counter_name() -> String {
    "he3_counter".into()
}
fn default_bubble_name() -> String {
    "bubble".into()
}
fn default_cr39_name() -> String {
    "cr39".into()
}
fn default_rem_name() -> String {
    "rem_meter".into()
}
fn one() -> f64 {
    1.0
}
fn default_bubble_threshold() -> f64 {
    1.0e5
}
fn default_cr39_threshold() -> f64 {
    2.0e5
}
fn default_resp_hist() -> usize {
    10_000
}
fn default_resp_batches() -> usize {
    10
}

/// An instrument at a detector position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstrumentSpec {
    /// Moderated ³He/BF₃ proportional counter (response computed by MC).
    ModeratedCounter {
        /// Claim label.
        #[serde(default = "default_counter_name")]
        name: String,
        /// Fill gas, e.g. `{"gas": "he3", "pressure_atm": 4}`.
        gas: CounterGas,
        /// Tube diameter, mm.
        tube_diameter_mm: f64,
        /// Active length, mm.
        active_length_mm: f64,
        /// HDPE moderator thickness, mm.
        moderator_thickness_mm: f64,
        /// Fraction of captures that register (default 1).
        #[serde(default = "one")]
        counting_efficiency: f64,
        /// Histories per batch per grid energy for the response table.
        #[serde(default = "default_resp_hist")]
        response_histories_per_batch: usize,
        /// Batches for the response table.
        #[serde(default = "default_resp_batches")]
        response_batches: usize,
    },
    /// Bubble dosimeter — sensitivity is the unit's own calibration.
    Bubble {
        /// Claim label.
        #[serde(default = "default_bubble_name")]
        name: String,
        /// Bubbles per µSv (required: printed on each detector).
        sensitivity_bubbles_per_usv: f64,
        /// Energy threshold, eV (default 100 keV).
        #[serde(default = "default_bubble_threshold")]
        threshold_ev: f64,
    },
    /// CR-39 track detector with PE radiator.
    Cr39 {
        /// Claim label.
        #[serde(default = "default_cr39_name")]
        name: String,
        /// Tracks/cm² per mSv (required: from the system calibration).
        tracks_per_cm2_per_msv: f64,
        /// Recoil threshold, eV (default 200 keV).
        #[serde(default = "default_cr39_threshold")]
        threshold_ev: f64,
    },
    /// Ideal H*(10)-following rem meter.
    RemMeter {
        /// Claim label.
        #[serde(default = "default_rem_name")]
        name: String,
        /// Counts per µSv.
        counts_per_usv: f64,
    },
}

impl InstrumentSpec {
    fn name(&self) -> &str {
        match self {
            InstrumentSpec::ModeratedCounter { name, .. }
            | InstrumentSpec::Bubble { name, .. }
            | InstrumentSpec::Cr39 { name, .. }
            | InstrumentSpec::RemMeter { name, .. } => name,
        }
    }
}

fn default_det_radius() -> ParamValue {
    ParamValue::Literal(50.0)
}

/// A detector position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneDetectorSpec {
    /// Label ("he3_0.5m", "operator", "door").
    pub label: String,
    /// Center, mm.
    pub position_mm: P3,
    /// Tally-sphere radius, mm (default 50).
    #[serde(default = "default_det_radius")]
    pub radius_mm: ParamValue,
    /// Instruments folded here (none = dose and fluence only).
    #[serde(default)]
    pub instruments: Vec<InstrumentSpec>,
}

/// The serializable room-scale problem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneSpec {
    /// Cells in priority order; the last encloses the world.
    pub cells: Vec<CellSpec>,
    /// The source.
    pub source: SceneSourceSpec,
    /// Detector positions.
    #[serde(default)]
    pub detectors: Vec<SceneDetectorSpec>,
    /// Optional voxel dose map (bounds mm, literal).
    #[serde(default)]
    pub mesh: Option<MeshTally>,
    /// Monte Carlo parameters.
    #[serde(default)]
    pub run: RunSpec,
}

/// Scene resolution/evaluation failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum SceneSpecError {
    /// Parameter/material/energy/rate failures (shared vocabulary).
    Spec(SpecError),
    /// Scene, mesh, detector or source geometry rejected.
    Run(SceneRunError),
    /// An instrument's response could not be built.
    Response(String, ResponseError),
    /// An instrument calibration value is missing/invalid.
    BadInstrument(String),
    /// Two instruments on one detector share a name.
    DuplicateInstrument(String),
    /// Nothing to tally: no detectors and no mesh.
    NoTallies,
}

impl std::fmt::Display for SceneSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneSpecError::Spec(e) => write!(f, "{e}"),
            SceneSpecError::Run(e) => write!(f, "{e}"),
            SceneSpecError::Response(n, e) => write!(f, "instrument {n:?}: {e}"),
            SceneSpecError::BadInstrument(n) => write!(
                f,
                "instrument {n:?} needs a positive, finite calibration/threshold"
            ),
            SceneSpecError::DuplicateInstrument(n) => {
                write!(f, "duplicate instrument name {n:?} on one detector")
            }
            SceneSpecError::NoTallies => {
                write!(f, "scene has no detectors and no mesh — nothing to report")
            }
        }
    }
}

impl std::error::Error for SceneSpecError {}

impl From<SpecError> for SceneSpecError {
    fn from(e: SpecError) -> Self {
        SceneSpecError::Spec(e)
    }
}

/// A resolved, runnable scene.
#[derive(Debug, Clone)]
pub struct ResolvedScene {
    /// The transport config (responses already computed).
    pub config: SceneConfig,
    /// Source rate, n/s.
    pub source_rate_n_per_s: f64,
    /// Source energy, eV.
    pub source_energy_ev: f64,
}

fn r3(v: &P3, p: &BTreeMap<String, f64>) -> Result<[f64; 3], SpecError> {
    Ok([v[0].resolve(p)?, v[1].resolve(p)?, v[2].resolve(p)?])
}

impl ShapeSpec {
    fn resolve(&self, p: &BTreeMap<String, f64>) -> Result<Shape, SpecError> {
        Ok(match self {
            ShapeSpec::Box { min_mm, max_mm } => Shape::box_mm(r3(min_mm, p)?, r3(max_mm, p)?),
            ShapeSpec::BoxCentered { center_mm, half_mm } => {
                Shape::box_centered_mm(r3(center_mm, p)?, r3(half_mm, p)?)
            }
            ShapeSpec::Sphere {
                center_mm,
                radius_mm,
            } => Shape::sphere_mm(r3(center_mm, p)?, radius_mm.resolve(p)?),
            ShapeSpec::Cylinder {
                base_mm,
                axis,
                radius_mm,
                height_mm,
            } => Shape::cylinder_mm(
                r3(base_mm, p)?,
                *axis,
                radius_mm.resolve(p)?,
                height_mm.resolve(p)?,
            ),
        })
    }
}

fn pos_finite(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

impl SceneSpec {
    /// Resolve against parameter bindings (computing any moderated
    /// counter responses), fail-closed.
    pub fn resolve(&self, params: &BTreeMap<String, f64>) -> Result<ResolvedScene, SceneSpecError> {
        if self.detectors.is_empty() && self.mesh.is_none() {
            return Err(SceneSpecError::NoTallies);
        }
        let rate = self.source.rate_n_per_s.resolve(params)?;
        if !pos_finite(rate) {
            return Err(SpecError::BadSourceRate(rate).into());
        }
        let energy_ev = self.source.energy_ev.resolve(params)?;
        if group_of_energy_ev(energy_ev).is_none() {
            return Err(SpecError::EnergyOutsideStructure { energy_ev }.into());
        }
        let mut cells = Vec::with_capacity(self.cells.len());
        for c in &self.cells {
            let m = materials::by_name(&c.material)
                .ok_or_else(|| SpecError::UnknownMaterial(c.material.clone()))?;
            cells.push(Cell::new(c.name.clone(), c.shape.resolve(params)?, m));
        }
        let scene = Scene::new(cells).map_err(|e| SceneSpecError::Run(SceneRunError::Scene(e)))?;

        // Instruments → responses (moderated counters computed once per
        // distinct design).
        let mut counter_cache: Vec<(InstrumentSpec, ResponseFunction)> = Vec::new();
        let mut detectors = Vec::with_capacity(self.detectors.len());
        for d in &self.detectors {
            let mut names = std::collections::BTreeSet::new();
            let mut responses = Vec::new();
            for inst in &d.instruments {
                if !names.insert(inst.name().to_string()) {
                    return Err(SceneSpecError::DuplicateInstrument(inst.name().into()));
                }
                let resp = match inst {
                    InstrumentSpec::ModeratedCounter {
                        name,
                        gas,
                        tube_diameter_mm,
                        active_length_mm,
                        moderator_thickness_mm,
                        counting_efficiency,
                        response_histories_per_batch,
                        response_batches,
                    } => {
                        // Cache key ignores the label.
                        let key = InstrumentSpec::ModeratedCounter {
                            name: String::new(),
                            gas: *gas,
                            tube_diameter_mm: *tube_diameter_mm,
                            active_length_mm: *active_length_mm,
                            moderator_thickness_mm: *moderator_thickness_mm,
                            counting_efficiency: *counting_efficiency,
                            response_histories_per_batch: *response_histories_per_batch,
                            response_batches: *response_batches,
                        };
                        if let Some((_, r)) = counter_cache.iter().find(|(k, _)| *k == key) {
                            let mut r = r.clone();
                            r.name = name.clone();
                            r
                        } else {
                            let mut counter = ModeratedCounter::new(
                                *gas,
                                *tube_diameter_mm,
                                *active_length_mm,
                                *moderator_thickness_mm,
                            );
                            counter.counting_efficiency = *counting_efficiency;
                            let table = counter
                                .compute(
                                    *response_histories_per_batch,
                                    *response_batches,
                                    self.run.seed ^ 0x5EED_C0DE,
                                )
                                .map_err(|e| SceneSpecError::Response(name.clone(), e))?;
                            let r = table.to_response_function(name, &counter);
                            counter_cache.push((key, r.clone()));
                            r
                        }
                    }
                    InstrumentSpec::Bubble {
                        name,
                        sensitivity_bubbles_per_usv,
                        threshold_ev,
                    } => {
                        if !pos_finite(*sensitivity_bubbles_per_usv)
                            || !(threshold_ev.is_finite() && *threshold_ev >= 0.0)
                        {
                            return Err(SceneSpecError::BadInstrument(name.clone()));
                        }
                        let mut r = bubble_dosimeter(*sensitivity_bubbles_per_usv, *threshold_ev);
                        r.name = name.clone();
                        r
                    }
                    InstrumentSpec::Cr39 {
                        name,
                        tracks_per_cm2_per_msv,
                        threshold_ev,
                    } => {
                        if !pos_finite(*tracks_per_cm2_per_msv)
                            || !(threshold_ev.is_finite() && *threshold_ev >= 0.0)
                        {
                            return Err(SceneSpecError::BadInstrument(name.clone()));
                        }
                        let mut r = cr39(*tracks_per_cm2_per_msv, *threshold_ev);
                        r.name = name.clone();
                        r
                    }
                    InstrumentSpec::RemMeter {
                        name,
                        counts_per_usv,
                    } => {
                        if !pos_finite(*counts_per_usv) {
                            return Err(SceneSpecError::BadInstrument(name.clone()));
                        }
                        let mut r = rem_meter(*counts_per_usv);
                        r.name = name.clone();
                        r
                    }
                };
                responses.push(resp);
            }
            detectors.push(DetectorTally {
                label: d.label.clone(),
                position_mm: r3(&d.position_mm, params)?,
                radius_mm: d.radius_mm.resolve(params)?,
                responses,
            });
        }

        let mut config = SceneConfig::new(
            scene,
            SceneSource::Point {
                position_mm: r3(&self.source.position_mm, params)?,
                energy_ev,
            },
            self.run.histories_per_batch,
            self.run.seed,
        );
        config.batches = self.run.batches;
        config.mesh = self.mesh.clone();
        config.detectors = detectors;
        Ok(ResolvedScene {
            config,
            source_rate_n_per_s: rate,
            source_energy_ev: energy_ev,
        })
    }

    /// Resolve and run.
    pub fn evaluate(
        &self,
        params: &BTreeMap<String, f64>,
    ) -> Result<(ResolvedScene, SceneResult), SceneSpecError> {
        let resolved = self.resolve(params)?;
        let result = run_scene(&resolved.config).map_err(SceneSpecError::Run)?;
        Ok((resolved, result))
    }
}

/// A JSON-friendly estimate: `rse` is `null` when nothing was scored.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct EstimateJson {
    /// Mean.
    pub mean: f64,
    /// Relative standard error (`None` = nothing scored).
    pub rse: Option<f64>,
}

impl From<Estimate> for EstimateJson {
    fn from(e: Estimate) -> Self {
        EstimateJson {
            mean: e.mean,
            rse: e.rse.is_finite().then_some(e.rse),
        }
    }
}

/// One instrument's expected reading.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstrumentReading {
    /// Instrument name.
    pub name: String,
    /// Event unit ("count", "bubble", "track/cm2").
    pub event_unit: String,
    /// Events per minute.
    pub per_minute: f64,
    /// Events per hour.
    pub per_hour: f64,
    /// Combined RSE: room MC ⊕ response-table MC (`None` = nothing
    /// scored).
    pub rse: Option<f64>,
    /// Response citation.
    pub citation: String,
    /// Response caveat.
    pub caveat: String,
}

/// One detector's report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectorReport {
    /// Label.
    pub label: String,
    /// Position, mm.
    pub position_mm: [f64; 3],
    /// H*(10) rate, µSv/h.
    pub dose_usv_per_h: EstimateJson,
    /// Fluence rate per group, n/cm²/s (group 0 = source group).
    pub fluence_n_cm2_s: Vec<EstimateJson>,
    /// Instrument readings.
    pub instruments: Vec<InstrumentReading>,
}

/// The voxel dose-rate map.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeshReport {
    /// Mesh geometry.
    pub mesh: MeshTally,
    /// Dose rate per voxel, µSv/h (x fastest, then y, then z).
    pub dose_usv_per_h: Vec<f64>,
    /// RSE per voxel (`None` = nothing scored — not a zero).
    pub rse: Vec<Option<f64>>,
}

/// Per-cell capture rate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CellCaptures {
    /// Cell name.
    pub cell: String,
    /// Material.
    pub material: String,
    /// Absorptions per second at the source rate.
    pub captures_per_s: EstimateJson,
}

/// The full JSON report of a scene run at its source rate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SceneReport {
    /// Source rate, n/s.
    pub source_rate_n_per_s: f64,
    /// Detector reports.
    pub detectors: Vec<DetectorReport>,
    /// Dose map, if requested.
    pub mesh: Option<MeshReport>,
    /// Capture rate per cell (the capture-gamma source term).
    pub captures: Vec<CellCaptures>,
    /// Fraction of source neutrons escaping the world.
    pub leaked_fraction: EstimateJson,
    /// Analog balance deviation (≈1e-15 when the books balance).
    pub balance_max_dev: f64,
    /// Truncated histories (must be 0 for claims).
    pub truncated_histories: u64,
    /// Total histories.
    pub total_histories: u64,
}

/// Combine an MC estimate's RSE with a response table's RSE.
pub fn combined_rse(e: Estimate, table_rse: f64) -> f64 {
    (e.rse * e.rse + table_rse * table_rse).sqrt()
}

/// Build the report for a run at `rate` n/s.
pub fn report(resolved: &ResolvedScene, result: &SceneResult) -> SceneReport {
    let rate = resolved.source_rate_n_per_s;
    let detectors = result
        .detectors
        .iter()
        .zip(resolved.config.detectors.iter())
        .map(|(d, t)| DetectorReport {
            label: d.label.clone(),
            position_mm: t.position_mm,
            dose_usv_per_h: d.dose_rate_usv_per_h(rate).into(),
            fluence_n_cm2_s: d
                .fluence_per_source
                .iter()
                .map(|e| e.scaled(rate).into())
                .collect(),
            instruments: d
                .responses
                .iter()
                .zip(t.responses.iter())
                .map(|(r, f)| {
                    let rse = combined_rse(r.per_source, f.table_rse);
                    InstrumentReading {
                        name: r.name.clone(),
                        event_unit: r.event_unit.clone(),
                        per_minute: r.per_minute(rate).mean,
                        per_hour: r.per_hour(rate).mean,
                        rse: rse.is_finite().then_some(rse),
                        citation: f.citation.clone(),
                        caveat: f.caveat.clone(),
                    }
                })
                .collect(),
        })
        .collect();
    let mesh = result.mesh.as_ref().map(|m| {
        let d = m.dose_rate_usv_per_h(rate);
        MeshReport {
            mesh: m.mesh.clone(),
            dose_usv_per_h: d.iter().map(|e| e.mean).collect(),
            rse: d
                .iter()
                .map(|e| e.rse.is_finite().then_some(e.rse))
                .collect(),
        }
    });
    let captures = result
        .absorbed_per_cell
        .iter()
        .zip(resolved.config.scene.cells.iter())
        .map(|(a, c)| CellCaptures {
            cell: c.name.clone(),
            material: c.material.name.clone(),
            captures_per_s: a.scaled(rate).into(),
        })
        .collect();
    SceneReport {
        source_rate_n_per_s: rate,
        detectors,
        mesh,
        captures,
        leaked_fraction: result.leaked.into(),
        balance_max_dev: result.balance_max_dev,
        truncated_histories: result.truncated_histories,
        total_histories: result.total_histories,
    }
}

/// Thermal (< 0.5 eV) fluence rate at a detector, n/cm²/s.
pub fn thermal_fluence(result: &SceneResult, detector: usize, rate: f64) -> Estimate {
    result.detectors[detector].fluence_per_source[THERMAL_GROUP].scaled(rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room_json() -> &'static str {
        r#"{
          "cells": [
            {"name": "shield", "material": "hdpe",
             "shape": {"type": "sphere", "center_mm": [0,0,0], "radius_mm": "t"}},
            {"name": "room", "material": "air",
             "shape": {"type": "box_centered", "center_mm": [0,0,0], "half_mm": [1000,1000,1000]}},
            {"name": "walls", "material": "concrete",
             "shape": {"type": "box_centered", "center_mm": [0,0,0], "half_mm": [1200,1200,1200]}}
          ],
          "source": {"position_mm": [0,0,0], "rate_n_per_s": 1e6, "energy_ev": 2.45e6},
          "detectors": [{"label": "d", "position_mm": [500,0,0], "radius_mm": 50,
            "instruments": [
              {"type": "bubble", "sensitivity_bubbles_per_usv": 3},
              {"type": "rem_meter", "counts_per_usv": 10}
            ]}],
          "mesh": {"min_mm": [-1000,-1000,-100], "max_mm": [1000,1000,100], "shape": [10,10,1]},
          "run": {"histories_per_batch": 1000, "batches": 4, "seed": 3}
        }"#
    }

    #[test]
    fn json_resolves_runs_and_reports() {
        let spec: SceneSpec = serde_json::from_str(room_json()).unwrap();
        let mut p = BTreeMap::new();
        p.insert("t".to_string(), 100.0);
        let (res, out) = spec.evaluate(&p).unwrap();
        assert_eq!(out.truncated_histories, 0);
        assert!(out.balance_max_dev < 1e-12);
        let rep = report(&res, &out);
        let d = &rep.detectors[0];
        assert!(d.dose_usv_per_h.mean > 0.0);
        // Ideal rem meter at 10 counts/µSv reads exactly 10 × dose/60
        // per minute (same tracks, same h(E)).
        let rem = &d.instruments[1];
        assert!(
            (rem.per_minute - 10.0 * d.dose_usv_per_h.mean / 60.0).abs() < 1e-9 * rem.per_minute
        );
        assert_eq!(rep.mesh.as_ref().unwrap().dose_usv_per_h.len(), 100);
        assert_eq!(rep.captures.len(), 3);
        let js = serde_json::to_string(&rep).unwrap();
        assert!(js.contains("\"per_minute\""));
    }

    #[test]
    fn fail_closed_everywhere() {
        let base: SceneSpec = serde_json::from_str(room_json()).unwrap();
        let p = BTreeMap::new();
        assert!(matches!(
            base.resolve(&p),
            Err(SceneSpecError::Spec(SpecError::UnboundParameter(_)))
        ));
        let mut b = BTreeMap::new();
        b.insert("t".to_string(), 100.0);
        let mut s = base.clone();
        s.cells[0].material = "unobtainium".into();
        assert!(matches!(
            s.resolve(&b),
            Err(SceneSpecError::Spec(SpecError::UnknownMaterial(_)))
        ));
        let mut s = base.clone();
        s.source.energy_ev = ParamValue::Literal(14.1e6);
        assert!(matches!(
            s.resolve(&b),
            Err(SceneSpecError::Spec(
                SpecError::EnergyOutsideStructure { .. }
            ))
        ));
        let mut s = base.clone();
        b.insert("t".to_string(), 5000.0); // shield bigger than the room
        assert!(matches!(s.resolve(&b), Err(SceneSpecError::Run(_))));
        b.insert("t".to_string(), 100.0);
        s.detectors[0].instruments.push(InstrumentSpec::Bubble {
            name: "bubble".into(),
            sensitivity_bubbles_per_usv: 1.0,
            threshold_ev: 1e5,
        });
        assert!(matches!(
            s.resolve(&b),
            Err(SceneSpecError::DuplicateInstrument(_))
        ));
        let mut s = base.clone();
        s.detectors[0].instruments[0] = InstrumentSpec::Bubble {
            name: "bubble".into(),
            sensitivity_bubbles_per_usv: 0.0,
            threshold_ev: 1e5,
        };
        assert!(matches!(
            s.resolve(&b),
            Err(SceneSpecError::BadInstrument(_))
        ));
        let mut s = base;
        s.detectors.clear();
        s.mesh = None;
        assert_eq!(s.resolve(&b).unwrap_err(), SceneSpecError::NoTallies);
    }
}
