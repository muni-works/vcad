//! Detector response functions (M7): "how many counts per minute does
//! *this* instrument show at *this* position?"
//!
//! A response function R(E) is events per unit fluence at energy E
//! (units: event·cm²). The instrument's reading at a position is
//! ∫ φ(E)·R(E) dE, which the scene transport folds track-by-track at the
//! neutron's actual energy ([`crate::transport3d::DetectorTally`]) — no
//! spectrum binning in between. Every response carries its citation, its
//! caveat, and (for computed responses) its own statistical uncertainty
//! `table_rse`, which claims combine in quadrature with the transport
//! RSE so the published error bar covers both Monte Carlos.
//!
//! Instruments:
//!
//! - **Moderated ³He / BF₃ proportional counter** ([`ModeratedCounter`])
//!   — the response is *computed*, not quoted: the tube (gas cylinder)
//!   inside its HDPE moderator is built as a [`crate::scene::Scene`] and
//!   irradiated by a uniform isotropic fluence at each grid energy; R(E)
//!   = captures in the gas / Φ. Gas capture physics: ³He(n,p) 5333 b and
//!   ¹⁰B(n,α) 3837 b at 2200 m/s, 1/v (see [`crate::materials::HELIUM3`]).
//!   This is the standard way Bonner-sphere response matrices are made
//!   (by MCNP rather than a 5-group library — hence the design-grade
//!   caveat; validation against a published response matrix is the open
//!   M6 rung). Every capture is counted (`counting_efficiency` scales for
//!   pulse-height discrimination and wall effect; tube walls are not
//!   modeled).
//! - **Bubble dosimeter** ([`bubble_dosimeter`]) — superheated-drop
//!   detectors are built to read neutron dose equivalent above an energy
//!   threshold (Ing, Noulty & McLean, "Bubble detectors — a maturing
//!   technology", Radiat. Meas. 27 (1997) 1–11). Modeled as
//!   `sensitivity × H*(10)` above the threshold, zero below. The
//!   sensitivity (bubbles/µSv) is **per-detector**: each unit is
//!   individually calibrated by its maker, so it is a required input,
//!   never a default.
//! - **CR-39 with a polyethylene radiator** ([`cr39`]) — proton-recoil
//!   track detector, modeled the same way (tracks/cm² per mSv above a
//!   recoil threshold). Real CR-39 response varies ~2× across
//!   0.2–5 MeV and with the etch protocol; calibrate against a known
//!   source (ISO 21909-1 is the dosimetry-system standard).
//! - **Ideal rem meter** ([`rem_meter`]) — an instrument that follows
//!   H*(10) exactly at `counts_per_usv`. Real Andersson–Braun rem balls
//!   over-respond in the keV range; this is the yardstick, not a model of
//!   a specific meter.

use crate::materials::{bf3_gas, helium3_gas, Material};
use crate::scene::{Axis, Cell, Scene, Shape};
use crate::tally::Estimate;
use crate::transport3d::{run_scene, SceneConfig, SceneRunError, SceneSource};

/// How R(E) is evaluated.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseModel {
    /// Tabulated `(energy eV, events·cm²)`, log-log interpolated
    /// (linear-in-log-E where a value is zero), clamped at the ends.
    Tabulated(Vec<(f64, f64)>),
    /// `events_per_usv × H*(10) per fluence` above `threshold_ev`.
    DoseEquivalent {
        /// Events per µSv of H*(10).
        events_per_usv: f64,
        /// Below this energy the instrument is blind.
        threshold_ev: f64,
    },
}

/// An instrument response.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseFunction {
    /// Name (claims are labeled with it).
    pub name: String,
    /// What one event is ("count", "bubble", "track/cm2").
    pub event_unit: String,
    /// The model.
    pub model: ResponseModel,
    /// Relative statistical uncertainty of the response itself (0 for
    /// analytic models; max RSE over the grid for computed tables).
    pub table_rse: f64,
    /// Literature basis.
    pub citation: String,
    /// What the model leaves out.
    pub caveat: String,
}

impl ResponseFunction {
    /// R(E), events per unit fluence (event·cm²).
    pub fn at(&self, e_ev: f64) -> f64 {
        match &self.model {
            ResponseModel::DoseEquivalent {
                events_per_usv,
                threshold_ev,
            } => {
                if e_ev >= *threshold_ev {
                    crate::dose::h10_psv_cm2(e_ev) * 1.0e-6 * events_per_usv
                } else {
                    0.0
                }
            }
            ResponseModel::Tabulated(pts) => interp_loglog(pts, e_ev),
        }
    }
}

fn interp_loglog(pts: &[(f64, f64)], e: f64) -> f64 {
    if pts.is_empty() {
        return 0.0;
    }
    if e <= pts[0].0 {
        return pts[0].1;
    }
    let last = pts[pts.len() - 1];
    if e >= last.0 {
        return last.1;
    }
    let i = pts.partition_point(|p| p.0 < e).max(1);
    let (e0, r0) = pts[i - 1];
    let (e1, r1) = pts[i];
    let t = (e / e0).ln() / (e1 / e0).ln();
    if r0 > 0.0 && r1 > 0.0 {
        r0 * (r1 / r0).powf(t)
    } else {
        r0 + (r1 - r0) * t
    }
}

/// Bubble dosimeter with its calibrated `sensitivity_bubbles_per_usv`
/// and an energy threshold (vendors quote O(100 keV) for the
/// personal-neutron-dosimeter type; bracket it when it matters).
pub fn bubble_dosimeter(sensitivity_bubbles_per_usv: f64, threshold_ev: f64) -> ResponseFunction {
    ResponseFunction {
        name: "bubble".to_string(),
        event_unit: "bubble".to_string(),
        model: ResponseModel::DoseEquivalent {
            events_per_usv: sensitivity_bubbles_per_usv,
            threshold_ev,
        },
        table_rse: 0.0,
        citation: "Ing, Noulty & McLean, Radiat. Meas. 27 (1997) 1-11: superheated-drop \
                   detectors read dose equivalent above a threshold"
            .to_string(),
        caveat: format!(
            "sensitivity {sensitivity_bubbles_per_usv} bubbles/uSv is the unit's own \
             calibration; flat-in-H*(10) above {threshold_ev:.3e} eV, zero below; \
             temperature dependence not modeled"
        ),
    }
}

/// CR-39 + PE-radiator track detector: `tracks_per_cm2_per_msv` above
/// a recoil threshold.
pub fn cr39(tracks_per_cm2_per_msv: f64, threshold_ev: f64) -> ResponseFunction {
    ResponseFunction {
        name: "cr39".to_string(),
        event_unit: "track/cm2".to_string(),
        model: ResponseModel::DoseEquivalent {
            events_per_usv: tracks_per_cm2_per_msv * 1.0e-3,
            threshold_ev,
        },
        table_rse: 0.0,
        citation: "proton-recoil CR-39 dosimetry; system calibration per ISO 21909-1".to_string(),
        caveat: format!(
            "threshold-H*(10) proxy at {tracks_per_cm2_per_msv} tracks/cm2/mSv above \
             {threshold_ev:.3e} eV; real response varies ~2x over 0.2-5 MeV and with \
             etch protocol; intrinsic background tracks not subtracted"
        ),
    }
}

/// An ideal H*(10)-following instrument at `counts_per_usv`.
pub fn rem_meter(counts_per_usv: f64) -> ResponseFunction {
    ResponseFunction {
        name: "rem_meter".to_string(),
        event_unit: "count".to_string(),
        model: ResponseModel::DoseEquivalent {
            events_per_usv: counts_per_usv,
            threshold_ev: 0.0,
        },
        table_rse: 0.0,
        citation: "ideal H*(10) response (ICRP-74 curve); the yardstick real rem meters \
                   approximate"
            .to_string(),
        caveat: "real Andersson-Braun-type rem balls over-respond in the keV range".to_string(),
    }
}

/// Proportional-counter fill gas.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "gas", rename_all = "lowercase")]
pub enum CounterGas {
    /// ³He at a fill pressure.
    He3 {
        /// Fill pressure, atm (20 °C).
        pressure_atm: f64,
    },
    /// BF₃ at a fill pressure and ¹⁰B enrichment.
    Bf3 {
        /// Fill pressure, atm (20 °C).
        pressure_atm: f64,
        /// ¹⁰B atom fraction in boron (commercial ~0.96).
        b10_fraction: f64,
    },
}

impl CounterGas {
    fn material(&self) -> Material {
        match *self {
            CounterGas::He3 { pressure_atm } => helium3_gas(pressure_atm),
            CounterGas::Bf3 {
                pressure_atm,
                b10_fraction,
            } => bf3_gas(pressure_atm, b10_fraction),
        }
    }

    fn valid(&self) -> bool {
        match *self {
            CounterGas::He3 { pressure_atm } => pressure_atm.is_finite() && pressure_atm > 0.0,
            CounterGas::Bf3 {
                pressure_atm,
                b10_fraction,
            } => {
                pressure_atm.is_finite()
                    && pressure_atm > 0.0
                    && (0.0..=1.0).contains(&b10_fraction)
            }
        }
    }
}

/// A cylindrical proportional counter centered in a cylindrical HDPE
/// moderator of uniform thickness (sides and ends).
#[derive(Debug, Clone, PartialEq)]
pub struct ModeratedCounter {
    /// Fill gas.
    pub gas: CounterGas,
    /// Tube (gas) diameter, mm.
    pub tube_diameter_mm: f64,
    /// Active length, mm.
    pub active_length_mm: f64,
    /// Moderator thickness around the tube, mm (0 = bare tube).
    pub moderator_thickness_mm: f64,
    /// Moderator material (default HDPE).
    pub moderator: Material,
    /// Fraction of gas captures that register (pulse-height threshold,
    /// wall effect). 1.0 counts every capture.
    pub counting_efficiency: f64,
}

/// The energy grid (eV) a counter response is computed on: thermal,
/// through the epithermal and keV range, to the D-D line and the top of
/// the group structure.
pub const RESPONSE_GRID_EV: [f64; 10] = [
    0.0253, 1.0, 30.0, 1.0e3, 1.0e4, 1.0e5, 3.0e5, 1.0e6, 2.45e6, 3.0e6,
];

/// A computed response table.
#[derive(Debug, Clone)]
pub struct ComputedResponse {
    /// `(energy eV, R(E) cm²)` with batch statistics.
    pub points: Vec<(f64, Estimate)>,
}

/// Response computation failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseError {
    /// Bad dimensions / gas / efficiency.
    BadCounter(String),
    /// The underlying scene run refused.
    Run(SceneRunError),
    /// A grid energy scored no captures: no error bar, no table.
    NothingScored {
        /// The energy, eV.
        energy_ev: f64,
    },
    /// The response run truncated histories.
    Truncated(u64),
}

impl std::fmt::Display for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResponseError::BadCounter(m) => write!(f, "invalid counter: {m}"),
            ResponseError::Run(e) => write!(f, "response run failed: {e}"),
            ResponseError::NothingScored { energy_ev } => write!(
                f,
                "counter response at {energy_ev:e} eV scored no captures — raise histories"
            ),
            ResponseError::Truncated(n) => write!(f, "response run truncated {n} histories"),
        }
    }
}

impl std::error::Error for ResponseError {}

impl ModeratedCounter {
    /// A counter with an HDPE moderator and every capture counted.
    pub fn new(
        gas: CounterGas,
        tube_diameter_mm: f64,
        active_length_mm: f64,
        moderator_thickness_mm: f64,
    ) -> Self {
        ModeratedCounter {
            gas,
            tube_diameter_mm,
            active_length_mm,
            moderator_thickness_mm,
            moderator: crate::materials::hdpe(),
            counting_efficiency: 1.0,
        }
    }

    fn validate(&self) -> Result<(), ResponseError> {
        let pos = |v: f64| v.is_finite() && v > 0.0;
        if !self.gas.valid() {
            return Err(ResponseError::BadCounter("fill gas".into()));
        }
        if !pos(self.tube_diameter_mm) || !pos(self.active_length_mm) {
            return Err(ResponseError::BadCounter("tube dimensions".into()));
        }
        if !(self.moderator_thickness_mm.is_finite() && self.moderator_thickness_mm >= 0.0) {
            return Err(ResponseError::BadCounter("moderator thickness".into()));
        }
        if !(self.counting_efficiency > 0.0 && self.counting_efficiency <= 1.0) {
            return Err(ResponseError::BadCounter(
                "counting efficiency must be in (0, 1]".into(),
            ));
        }
        Ok(())
    }

    /// Radius (mm) of the fluence sphere that encloses the assembly.
    fn bounding_radius_mm(&self) -> f64 {
        let r = 0.5 * self.tube_diameter_mm + self.moderator_thickness_mm;
        let hz = 0.5 * self.active_length_mm + self.moderator_thickness_mm;
        (2.0 * r * r + hz * hz).sqrt() * 1.001
    }

    /// The counter as a scene: gas, moderator (if any), void world sphere.
    pub fn scene(&self) -> Result<Scene, ResponseError> {
        self.validate()?;
        let rt = 0.5 * self.tube_diameter_mm;
        let l = self.active_length_mm;
        let t = self.moderator_thickness_mm;
        let mut cells = vec![Cell::new(
            "gas",
            Shape::cylinder_mm([0.0, 0.0, -0.5 * l], Axis::Z, rt, l),
            self.gas.material(),
        )];
        if t > 0.0 {
            cells.push(Cell::new(
                "moderator",
                Shape::cylinder_mm([0.0, 0.0, -0.5 * l - t], Axis::Z, rt + t, l + 2.0 * t),
                self.moderator.clone(),
            ));
        }
        cells.push(Cell::new(
            "world",
            Shape::sphere_mm([0.0; 3], self.bounding_radius_mm()),
            Material::void(),
        ));
        Scene::new(cells).map_err(|e| ResponseError::Run(SceneRunError::Scene(e)))
    }

    /// Compute R(E) on [`RESPONSE_GRID_EV`]: `histories_per_batch ×
    /// batches` isotropic-fluence histories per grid energy.
    pub fn compute(
        &self,
        histories_per_batch: usize,
        batches: usize,
        seed: u64,
    ) -> Result<ComputedResponse, ResponseError> {
        self.compute_on(&RESPONSE_GRID_EV, histories_per_batch, batches, seed)
    }

    /// Compute R(E) on an explicit ascending energy grid (eV).
    pub fn compute_on(
        &self,
        grid_ev: &[f64],
        histories_per_batch: usize,
        batches: usize,
        seed: u64,
    ) -> Result<ComputedResponse, ResponseError> {
        let scene = self.scene()?;
        let rb_mm = self.bounding_radius_mm();
        let area_cm2 = std::f64::consts::PI * (rb_mm * 0.1).powi(2);
        let mut points = Vec::with_capacity(grid_ev.len());
        for (k, &e) in grid_ev.iter().enumerate() {
            let mut cfg = SceneConfig::new(
                scene.clone(),
                SceneSource::IsotropicFluence {
                    center_mm: [0.0; 3],
                    radius_mm: rb_mm,
                    energy_ev: e,
                },
                histories_per_batch,
                seed.wrapping_add(k as u64 * 0x9E37_79B9),
            );
            cfg.batches = batches;
            let r = run_scene(&cfg).map_err(ResponseError::Run)?;
            if r.truncated_histories > 0 {
                return Err(ResponseError::Truncated(r.truncated_histories));
            }
            let captures = r.absorbed_per_cell[0];
            if !captures.rse.is_finite() {
                return Err(ResponseError::NothingScored { energy_ev: e });
            }
            points.push((e, captures.scaled(area_cm2 * self.counting_efficiency)));
        }
        Ok(ComputedResponse { points })
    }
}

impl ComputedResponse {
    /// Freeze into a [`ResponseFunction`] (means tabulated; the worst
    /// grid RSE rides along as `table_rse`).
    pub fn to_response_function(&self, name: &str, counter: &ModeratedCounter) -> ResponseFunction {
        let table_rse = self.points.iter().map(|(_, e)| e.rse).fold(0.0, f64::max);
        let gas = match counter.gas {
            CounterGas::He3 { pressure_atm } => format!("3He {pressure_atm} atm"),
            CounterGas::Bf3 {
                pressure_atm,
                b10_fraction,
            } => format!("BF3 {pressure_atm} atm ({b10_fraction} 10B)"),
        };
        ResponseFunction {
            name: name.to_string(),
            event_unit: "count".to_string(),
            model: ResponseModel::Tabulated(
                self.points.iter().map(|(e, r)| (*e, r.mean)).collect(),
            ),
            table_rse,
            citation: "computed: isotropic-fluence MC through the counter geometry; gas capture \
                       3He(n,p) 5333 b / 10B(n,a) 3837 b at 2200 m/s, 1/v (Mughabghab 2006)"
                .to_string(),
            caveat: format!(
                "{gas}, {:.1} mm x {:.0} mm active, {:.0} mm {} moderator; tube wall and \
                 detector self-perturbation of the room field not modeled; counting \
                 efficiency {}; design-grade 5-group library",
                counter.tube_diameter_mm,
                counter.active_length_mm,
                counter.moderator_thickness_mm,
                counter.moderator.name,
                counter.counting_efficiency
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isotropic_fluence_normalization_matches_sphere_capture_formula() {
        // Uniform isotropic fluence Φ on a purely absorbing sphere of
        // radius a, Σa: capture probability per entering neutron
        // P = 1 − [1 − (1+2x)e^{−2x}]/(2x²), x = Σa·a (Case, de Hoffmann &
        // Placzek); entering current = πa²Φ. So R = πa²·P exactly.
        let a_mm = 50.0;
        let sigma = 0.1; // 1/cm → x = 0.5
        let rb_mm = 60.0;
        let scene = Scene::new(vec![
            Cell::new(
                "ball",
                Shape::sphere_mm([0.0; 3], a_mm),
                Material::pure_absorber(sigma),
            ),
            Cell::new("world", Shape::sphere_mm([0.0; 3], rb_mm), Material::void()),
        ])
        .unwrap();
        let mut cfg = SceneConfig::new(
            scene,
            SceneSource::IsotropicFluence {
                center_mm: [0.0; 3],
                radius_mm: rb_mm,
                energy_ev: 2.45e6,
            },
            20_000,
            17,
        );
        cfg.batches = 10;
        let r = run_scene(&cfg).unwrap();
        let area = std::f64::consts::PI * 6.0f64.powi(2);
        let resp = r.absorbed_per_cell[0].scaled(area);
        let x: f64 = sigma * 5.0;
        let p = 1.0 - (1.0 - (1.0 + 2.0 * x) * (-2.0 * x).exp()) / (2.0 * x * x);
        let expect = std::f64::consts::PI * 25.0 * p;
        assert!(
            resp.consistent_with(expect, 4.0),
            "R = {resp} vs analytic {expect}"
        );
    }

    #[test]
    fn bare_thin_tube_is_sigma_times_volume() {
        // Thin limit: R = Σa·V for a weak absorber (low-pressure ³He).
        let c = ModeratedCounter::new(CounterGas::He3 { pressure_atm: 0.2 }, 25.4, 50.0, 0.0);
        let sa = helium3_gas(0.2).sigma_a[crate::groups::THERMAL_GROUP];
        let v = std::f64::consts::PI * 1.27f64.powi(2) * 5.0;
        let resp = c.compute_on(&[0.0253], 20_000, 8, 3).unwrap();
        let (e, r) = resp.points[0];
        assert_eq!(e, 0.0253);
        // Σa·d̄ ≈ 0.027·1.9 = 5%: first-order self-shielding ~ −2.5%.
        let thin = sa * v;
        assert!(
            r.mean < thin * 1.02 && r.mean > thin * 0.92,
            "R_th = {r} vs Σa·V = {thin}"
        );
    }

    #[test]
    fn moderator_moves_response_to_fast_neutrons() {
        let bare = ModeratedCounter::new(CounterGas::He3 { pressure_atm: 4.0 }, 25.4, 100.0, 0.0);
        let modd = ModeratedCounter::new(CounterGas::He3 { pressure_atm: 4.0 }, 25.4, 100.0, 50.0);
        // A bare tube is blind to 2.45 MeV (σ ≈ 0.5 b vs 5333 b thermal);
        // 50 mm of HDPE turns a usable fraction of its thermal
        // sensitivity into fast sensitivity — the reason the tube is
        // moderated at all.
        let rb = bare.compute_on(&[0.0253], 4_000, 4, 9).unwrap();
        let rm = modd.compute(4_000, 4, 9).unwrap();
        let bare_th = rb.points[0].1.mean;
        let mod_fast = rm.points[8].1.mean; // 2.45 MeV
        assert!(
            mod_fast > 0.02 * bare_th,
            "moderated fast response {mod_fast} vs bare thermal {bare_th}"
        );
        let f = rm.to_response_function("he3", &modd);
        assert!(f.at(2.45e6) > 0.0 && f.table_rse.is_finite());
        // Interpolation hits the grid points exactly.
        assert!((f.at(1.0e6) - rm.points[7].1.mean).abs() < 1e-12 * rm.points[7].1.mean);
    }

    #[test]
    fn dose_threshold_models() {
        let b = bubble_dosimeter(3.0, 1.0e5);
        assert_eq!(b.at(5.0e4), 0.0);
        // 2.45 MeV: h ≈ 419 pSv·cm² → 3 bubbles/µSv × 4.19e-4 µSv·cm².
        assert!((b.at(2.45e6) - 3.0 * crate::dose::h10_psv_cm2(2.45e6) * 1e-6).abs() < 1e-15);
        let c = cr39(20.0, 2.0e5);
        assert!((c.at(2.45e6) / b.at(2.45e6) - (20.0e-3 / 3.0)).abs() < 1e-12);
        assert!(rem_meter(1.0).at(0.0253) > 0.0);
        assert!(
            ModeratedCounter::new(CounterGas::He3 { pressure_atm: -1.0 }, 25.0, 100.0, 10.0)
                .compute(10, 2, 1)
                .is_err()
        );
    }
}
