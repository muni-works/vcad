//! Tuned-instrument metadata: which parts are tuned bars, what pitch each
//! must ring at, what the solver predicted, and — once someone strikes the
//! real bar — what was measured.
//!
//! Written by the `design_instrument` MCP tool and by vcad.app's Tune panel.
//! A bar with no `measurement` is a *predicted* receipt claim (Provisional,
//! never Pass); a measurement flips it to *measured*, Pass when
//! `|cents_error| ≤ tolerance_cents` and Fail otherwise.

use serde::{Deserialize, Serialize};

/// A tuned instrument on the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct Instrument {
    /// Instrument kind, e.g. `"glockenspiel"`.
    pub kind: String,
    /// Tuning reference, e.g. `"12-TET, A4 = 440 Hz"`.
    pub tuning: String,
    /// Bar stock id (`aluminium_6061`, `brass`, `pla`).
    pub material: String,
    /// Bars, low to high.
    pub bars: Vec<InstrumentBar>,
}

/// One tuned bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct InstrumentBar {
    /// Note name, e.g. `"C6"`.
    pub note: String,
    /// Scene root / part name of the bar.
    pub part: String,
    /// Target fundamental, Hz.
    pub target_hz: f64,
    /// Solver-predicted fundamental, Hz.
    pub predicted_hz: f64,
    /// Acceptance tolerance, cents.
    pub tolerance_cents: f64,
    /// Bar length, mm.
    pub length_mm: f64,
    /// Bar width, mm.
    pub width_mm: f64,
    /// Bar thickness, mm.
    pub thickness_mm: f64,
    /// Undercut depth from the underside, mm.
    pub undercut_depth_mm: f64,
    /// Undercut span as a fraction of length, centred.
    pub undercut_span: f64,
    /// Peg hole diameter, mm.
    pub hole_dia_mm: f64,
    /// Fundamental's nodal points, mm from the bar's x=0 end.
    pub nodes_mm: Vec<f64>,
    /// The latest strike measurement, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub measurement: Option<BarMeasurement>,
}

/// A measured strike.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct BarMeasurement {
    /// Detected fundamental, Hz.
    pub measured_hz: f64,
    /// Measured vs target, cents.
    pub cents_error: f64,
    /// How it was measured, e.g. `"vcad.app Tune (mic, FFT + parabolic peak)"`.
    pub instrument: String,
    /// ISO-8601 timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub measured_at: Option<String>,
}

impl InstrumentBar {
    /// Measured and inside tolerance.
    pub fn passes(&self) -> Option<bool> {
        self.measurement
            .as_ref()
            .map(|m| m.cents_error.abs() <= self.tolerance_cents)
    }
}
