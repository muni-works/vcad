//! The claims ledger: every solver's frozen predictions, and later their
//! bench measurements, carried on one document.
//!
//! Solver tools (`solve_thermal`, `analyze_antenna`, `simulate_em`,
//! `analyze_tolerance_stackup`, …) are stateless: each returns its own
//! `vcad.<domain>-claims/1` set plus unified [`ReceiptClaim`]s. Attaching
//! those to a document appends [`LedgerEntry`]s here, **freezing** each
//! prediction: its numeric value and unit, a hash over the prediction
//! (`prediction_sha256`), and the fingerprint of the design it was priced on
//! (`document_fingerprint`). A later bench reading binds to one entry as a
//! [`LedgerMeasurement`] and is compared fail-closed with the domain's own
//! [`AcceptanceBand`] rule.
//!
//! Rows read [`LedgerRow::Holds`], [`LedgerRow::Violated`],
//! [`LedgerRow::Unmeasured`], or [`LedgerRow::Stale`]. Stale is the freeze
//! guarantee: once a claim has a measurement, a prediction whose hash no
//! longer matches what the measurement was compared against — or a design
//! whose fingerprint moved since the prediction was frozen — never silently
//! re-predicts. It reads Stale, which rolls up Unverifiable in a receipt.
//!
//! Hashes are opaque strings here: the producer (the MCP server) computes
//! them over one canonical serialization and this module only compares them,
//! so the two languages never have to agree on float formatting.

use serde::{Deserialize, Serialize};

use crate::{ClaimBasis, ClaimQuantity, ClaimValue, ClaimVerdict, ReceiptClaim};

/// Ledger schema identifier.
pub const LEDGER_SCHEMA: &str = "vcad.claims-ledger/1";

/// How a measurement is accepted against a prediction. The measurer owns
/// the band (instrument accuracy + design acceptance) — every domain crate
/// says so — but each domain fixes the *shape* of the rule.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub enum AcceptanceBand {
    /// Multiplicative band (thermal, EM, acoustics, flow): holds when
    /// `measured ∈ [p/band_factor − u, p·band_factor + u]`, where `u` is the
    /// measurement's one-sigma uncertainty.
    Factor {
        /// Band factor, ≥ 1.
        band_factor: f64,
    },
    /// Absolute half-width (antenna, tolerance): holds when
    /// `|measured − p| ≤ half_width + u + u_pred`, where `u_pred` is the
    /// prediction's own uncertainty when the claim family carries one
    /// (tolerance stackups do).
    Absolute {
        /// Acceptance half-width, in the claim's unit, ≥ 0.
        half_width: f64,
    },
}

/// Which band shape a domain's own `compare` uses, or `None` when the domain
/// has no measurement comparison (the caller must then name the shape).
pub fn domain_band_kind(domain: &str) -> Option<&'static str> {
    match domain {
        "thermal" | "em" | "acoustics" | "flow" => Some("factor"),
        "antenna" | "tolerance" => Some("absolute"),
        _ => None,
    }
}

impl AcceptanceBand {
    /// `"factor"` or `"absolute"`.
    pub fn kind(&self) -> &'static str {
        match self {
            AcceptanceBand::Factor { .. } => "factor",
            AcceptanceBand::Absolute { .. } => "absolute",
        }
    }

    /// Validate the band parameters (finite, in range).
    pub fn validate(&self) -> Result<(), String> {
        match *self {
            AcceptanceBand::Factor { band_factor } => {
                if !(band_factor.is_finite() && band_factor >= 1.0) {
                    return Err(format!(
                        "band_factor must be finite and >= 1 (got {band_factor})"
                    ));
                }
            }
            AcceptanceBand::Absolute { half_width } => {
                if !(half_width.is_finite() && half_width >= 0.0) {
                    return Err(format!(
                        "half_width must be finite and >= 0 (got {half_width})"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The acceptance interval `[lo, hi]` around `predicted`.
    pub fn interval(
        &self,
        predicted: f64,
        uncertainty: f64,
        prediction_uncertainty: f64,
    ) -> (f64, f64) {
        match *self {
            AcceptanceBand::Factor { band_factor } => {
                // Ordered so a negative prediction still yields lo <= hi.
                let a = predicted / band_factor;
                let b = predicted * band_factor;
                (a.min(b) - uncertainty, a.max(b) + uncertainty)
            }
            AcceptanceBand::Absolute { half_width } => {
                let w = half_width + uncertainty + prediction_uncertainty;
                (predicted - w, predicted + w)
            }
        }
    }

    /// Does `measured` hold against `predicted` under this band?
    pub fn holds(
        &self,
        predicted: f64,
        measured: f64,
        uncertainty: f64,
        prediction_uncertainty: f64,
    ) -> bool {
        let (lo, hi) = self.interval(predicted, uncertainty, prediction_uncertainty);
        measured.is_finite() && lo <= measured && measured <= hi
    }
}

/// One bench reading bound to one ledger entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct LedgerMeasurement {
    /// Measured value, in the prediction's unit.
    pub value: f64,
    /// Unit label (must match the frozen prediction's unit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub unit: Option<String>,
    /// One-sigma absolute uncertainty, same unit, ≥ 0.
    pub uncertainty: f64,
    /// Instrument provenance ("NanoVNA-H4 SOL-cal at SMA", "type-K TC epoxied").
    pub instrument: String,
    /// Acceptance band the measurer committed to.
    pub band: AcceptanceBand,
    /// SHA-256 (hex) of the raw instrument file, when one was imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub raw_sha256: Option<String>,
    /// Raw file name, for the audit trail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub raw_name: Option<String>,
    /// Typed importer that produced this reading (`"touchstone_s1p"`,
    /// `"rew_impedance"`, `"thermocouple_csv"`); absent for a hand entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub importer: Option<String>,
    /// RFC 3339 time of recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub recorded_at: Option<String>,
    /// The `prediction_sha256` this reading was compared against. If the
    /// entry's prediction hash moves, the row reads Stale.
    pub prediction_sha256: String,
}

/// One frozen prediction, plus its measurement once bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct LedgerEntry {
    /// The unified claim exactly as the solver emitted it (oracle id and
    /// version, basis, solver provenance in `details`).
    pub claim: ReceiptClaim,
    /// The frozen numeric prediction. Solver adapters put the computed value
    /// in `claim.measured` ("what the oracle computed"); freezing lifts it
    /// here so the comparison never has to guess which field holds it.
    pub predicted: ClaimQuantity,
    /// The prediction's own uncertainty, when the claim family carries one
    /// (tolerance stackups), same unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub prediction_uncertainty: Option<f64>,
    /// Hash over the frozen prediction (claim identity, oracle, value, unit,
    /// uncertainty), computed by the producer over one canonical form.
    pub prediction_sha256: String,
    /// Fingerprint of the design the prediction was priced on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub document_fingerprint: Option<String>,
    /// Native claim-set schema the claim came from (`vcad.thermal-claims/1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub claim_set_schema: Option<String>,
    /// Index into [`ClaimsLedger::claim_sets`] of the native set this claim
    /// came from (typed importers read domain context from it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub claim_set: Option<u32>,
    /// The tool that produced the claim (`"solve_thermal"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub source_tool: Option<String>,
    /// RFC 3339 time the prediction was frozen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub attached_at: Option<String>,
    /// The bound measurement, once recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub measurement: Option<LedgerMeasurement>,
}

/// A native claim set, kept verbatim (as JSON text) for importers that need
/// domain context the unified claim drops (antenna band + reference
/// impedance, tolerance uncertainties).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct NativeClaimSet {
    /// Schema tag, e.g. `vcad.antenna-claims/1`.
    pub schema: String,
    /// The claim set serialized as JSON.
    pub json: String,
}

/// The document-persisted ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct ClaimsLedger {
    /// [`LEDGER_SCHEMA`].
    pub schema: String,
    /// Entries, in attach order (the scorecard's row order).
    pub entries: Vec<LedgerEntry>,
    /// Native claim sets referenced by [`LedgerEntry::claim_set`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts-rs", ts(as = "Option<Vec<NativeClaimSet>>", optional))]
    pub claim_sets: Vec<NativeClaimSet>,
}

impl Default for ClaimsLedger {
    fn default() -> Self {
        Self {
            schema: LEDGER_SCHEMA.to_string(),
            entries: Vec::new(),
            claim_sets: Vec::new(),
        }
    }
}

/// Per-row verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub enum LedgerRow {
    /// Measured inside the band.
    Holds,
    /// Measured outside the band — a result about the model.
    Violated,
    /// No measurement yet. Never a pass.
    Unmeasured,
    /// Measured, but the prediction or the design moved since. Never a pass,
    /// never silently re-predicted.
    Stale,
}

/// Why a row is Stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub enum StaleReason {
    /// The prediction hash differs from the one the measurement was compared
    /// against (the prediction was edited or re-attached).
    PredictionChanged,
    /// The design fingerprint differs from the one the prediction was frozen on.
    DesignChanged,
}

/// Units that mean "dimensionless".
fn normalize_unit(u: Option<&str>) -> Option<&str> {
    match u.map(str::trim) {
        None | Some("") | Some("1") => None,
        Some(s) => Some(s),
    }
}

/// Do two unit labels name the same unit (dimensionless forms equal)?
pub fn same_unit(a: Option<&str>, b: Option<&str>) -> bool {
    normalize_unit(a) == normalize_unit(b)
}

impl LedgerEntry {
    /// The frozen numeric prediction, or `None` when it is not a number.
    pub fn predicted_value(&self) -> Option<f64> {
        match self.predicted.value {
            ClaimValue::Number(v) if v.is_finite() => Some(v),
            _ => None,
        }
    }

    /// Classify this row against the current prediction hash and the
    /// current design fingerprint (`None` when unknown — then the design
    /// check is skipped, never assumed to match a recorded fingerprint).
    ///
    /// `current_prediction_sha256` is the producer's hash of the entry as it
    /// is *now*; it equals `self.prediction_sha256` unless someone edited
    /// the entry without re-hashing it.
    pub fn row(
        &self,
        current_prediction_sha256: &str,
        current_document_fingerprint: Option<&str>,
    ) -> (LedgerRow, Option<StaleReason>) {
        let Some(m) = &self.measurement else {
            return (LedgerRow::Unmeasured, None);
        };
        if current_prediction_sha256 != self.prediction_sha256
            || m.prediction_sha256 != self.prediction_sha256
        {
            return (LedgerRow::Stale, Some(StaleReason::PredictionChanged));
        }
        if let (Some(frozen), Some(now)) = (
            self.document_fingerprint.as_deref(),
            current_document_fingerprint,
        ) {
            if frozen != now {
                return (LedgerRow::Stale, Some(StaleReason::DesignChanged));
            }
        }
        let Some(p) = self.predicted_value() else {
            // A non-numeric prediction cannot be banded: fail closed.
            return (LedgerRow::Stale, Some(StaleReason::PredictionChanged));
        };
        if m.band.holds(
            p,
            m.value,
            m.uncertainty,
            self.prediction_uncertainty.unwrap_or(0.0),
        ) {
            (LedgerRow::Holds, None)
        } else {
            (LedgerRow::Violated, None)
        }
    }

    /// The unified receipt claim for this row.
    ///
    /// - Unmeasured: the frozen claim as attached (a predicted pass stays
    ///   Provisional, never Pass).
    /// - Holds / Violated: basis `measured`, `predicted` = the frozen
    ///   prediction, `measured` = the bench reading.
    /// - Stale: Unverifiable, with the reason.
    pub fn receipt_claim(&self, row: LedgerRow, stale: Option<StaleReason>) -> ReceiptClaim {
        let mut c = self.claim.clone();
        match row {
            LedgerRow::Unmeasured => {
                if c.verdict == ClaimVerdict::Pass && c.basis.is_none() {
                    // Absent basis means verified; a frozen prediction is not.
                    c = c.with_basis(ClaimBasis::Predicted);
                }
            }
            LedgerRow::Holds | LedgerRow::Violated => {
                let m = self.measurement.as_ref().expect("measured row");
                c.verdict = if row == LedgerRow::Holds {
                    ClaimVerdict::Pass
                } else {
                    ClaimVerdict::Fail
                };
                c.basis = Some(ClaimBasis::Measured);
                c.predicted = Some(self.predicted.clone());
                c.measured = Some(ClaimQuantity {
                    value: ClaimValue::Number(m.value),
                    unit: m.unit.clone().or_else(|| self.predicted.unit.clone()),
                });
            }
            LedgerRow::Stale => {
                c.verdict = ClaimVerdict::Unverifiable;
                c.predicted = Some(self.predicted.clone());
                if let Some(m) = &self.measurement {
                    c.measured = Some(ClaimQuantity {
                        value: ClaimValue::Number(m.value),
                        unit: m.unit.clone().or_else(|| self.predicted.unit.clone()),
                    });
                }
                let why = match stale {
                    Some(StaleReason::DesignChanged) => {
                        "stale: the design changed since this prediction was frozen"
                    }
                    _ => "stale: the prediction changed since it was measured",
                };
                c.details = Some(why.to_string());
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DesignReceipt, OracleRef, ReceiptVerdict};

    fn entry(domain: &str, value: f64, unit: &str) -> LedgerEntry {
        let claim = ReceiptClaim::pass(
            format!("{domain}.x"),
            domain,
            "x",
            OracleRef::new(format!("vcad-kernel-{domain}/solve"), "0.11.0"),
        )
        .with_basis(ClaimBasis::Predicted)
        .with_measured(ClaimQuantity::new(value, unit));
        LedgerEntry {
            claim,
            predicted: ClaimQuantity::new(value, unit),
            prediction_uncertainty: None,
            prediction_sha256: "h1".into(),
            document_fingerprint: Some("doc1".into()),
            claim_set_schema: None,
            claim_set: None,
            source_tool: None,
            attached_at: None,
            measurement: None,
        }
    }

    fn measure(e: &mut LedgerEntry, value: f64, band: AcceptanceBand) {
        e.measurement = Some(LedgerMeasurement {
            value,
            unit: e.predicted.unit.clone(),
            uncertainty: 0.0,
            instrument: "bench".into(),
            band,
            raw_sha256: None,
            raw_name: None,
            importer: None,
            recorded_at: None,
            prediction_sha256: e.prediction_sha256.clone(),
        });
    }

    /// The band vectors shared with the TypeScript ledger
    /// (`packages/mcp/src/__tests__/claims-ledger.test.ts`), so the two
    /// implementations of the band math cannot drift.
    #[test]
    fn shared_band_vectors() {
        let raw = include_str!("../tests/ledger_band_vectors.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let cases = v["cases"].as_array().unwrap();
        assert!(cases.len() >= 8);
        for c in cases {
            let band: AcceptanceBand = serde_json::from_value(c["band"].clone()).unwrap();
            let got = band.holds(
                c["predicted"].as_f64().unwrap(),
                c["measured"].as_f64().unwrap(),
                c["uncertainty"].as_f64().unwrap(),
                c["prediction_uncertainty"].as_f64().unwrap_or(0.0),
            );
            assert_eq!(got, c["holds"].as_bool().unwrap(), "case {}", c["name"]);
        }
    }

    #[test]
    fn rows_hold_violate_and_stay_unmeasured() {
        let mut e = entry("thermal", 60.0, "C");
        assert_eq!(e.row("h1", Some("doc1")).0, LedgerRow::Unmeasured);
        measure(&mut e, 70.0, AcceptanceBand::Factor { band_factor: 1.25 });
        assert_eq!(e.row("h1", Some("doc1")).0, LedgerRow::Holds);
        measure(&mut e, 90.0, AcceptanceBand::Factor { band_factor: 1.25 });
        assert_eq!(e.row("h1", Some("doc1")).0, LedgerRow::Violated);
    }

    #[test]
    fn an_edited_prediction_or_design_reads_stale() {
        let mut e = entry("antenna", 2.44e9, "Hz");
        measure(&mut e, 2.45e9, AcceptanceBand::Absolute { half_width: 5e7 });
        assert_eq!(e.row("h1", Some("doc1")).0, LedgerRow::Holds);
        // Someone edits the frozen number without re-hashing it.
        assert_eq!(
            e.row("h2", Some("doc1")),
            (LedgerRow::Stale, Some(StaleReason::PredictionChanged))
        );
        // A re-attach re-hashes, but the measurement was taken against h1.
        e.prediction_sha256 = "h2".into();
        assert_eq!(
            e.row("h2", Some("doc1")),
            (LedgerRow::Stale, Some(StaleReason::PredictionChanged))
        );
        e.prediction_sha256 = "h1".into();
        // The design moved since the freeze.
        assert_eq!(
            e.row("h1", Some("doc2")),
            (LedgerRow::Stale, Some(StaleReason::DesignChanged))
        );
        // Unknown current fingerprint skips the design check, nothing more.
        assert_eq!(e.row("h1", None).0, LedgerRow::Holds);
    }

    #[test]
    fn rows_roll_up_fail_closed_in_a_receipt() {
        let unmeasured = entry("em", 1e-3, "H");
        let mut holds = entry("thermal", 60.0, "C");
        measure(
            &mut holds,
            62.0,
            AcceptanceBand::Factor { band_factor: 1.1 },
        );
        let claims = |es: &[&LedgerEntry]| {
            DesignReceipt::with_claims(
                es.iter()
                    .map(|e| {
                        let (r, s) = e.row("h1", Some("doc1"));
                        e.receipt_claim(r, s)
                    })
                    .collect(),
            )
        };
        // A predicted claim alone: Provisional, never Pass.
        assert_eq!(
            claims(&[&unmeasured]).verdict(),
            ReceiptVerdict::Provisional
        );
        // A measured hold: Pass on measured basis.
        let r = claims(&[&holds]);
        assert_eq!(r.verdict(), ReceiptVerdict::Pass);
        assert_eq!(r.claims[0].basis, Some(ClaimBasis::Measured));
        assert_eq!(r.claims[0].predicted, Some(ClaimQuantity::new(60.0, "C")));
        assert_eq!(r.claims[0].measured, Some(ClaimQuantity::new(62.0, "C")));
        // Mixed: still Provisional.
        assert_eq!(
            claims(&[&holds, &unmeasured]).verdict(),
            ReceiptVerdict::Provisional
        );
        // Violated fails the receipt.
        let mut violated = entry("acoustics", 45.0, "Hz");
        measure(
            &mut violated,
            60.0,
            AcceptanceBand::Factor { band_factor: 1.05 },
        );
        assert_eq!(claims(&[&holds, &violated]).verdict(), ReceiptVerdict::Fail);
        // Stale is unverifiable, never a pass.
        let stale = holds.receipt_claim(LedgerRow::Stale, Some(StaleReason::DesignChanged));
        assert_eq!(stale.verdict, ClaimVerdict::Unverifiable);
        assert_eq!(
            DesignReceipt::with_claims(vec![stale]).verdict(),
            ReceiptVerdict::Unverifiable
        );
    }

    #[test]
    fn domain_band_shapes_and_validation() {
        assert_eq!(domain_band_kind("thermal"), Some("factor"));
        assert_eq!(domain_band_kind("antenna"), Some("absolute"));
        assert_eq!(domain_band_kind("structure"), None);
        assert!(AcceptanceBand::Factor { band_factor: 0.9 }
            .validate()
            .is_err());
        assert!(AcceptanceBand::Absolute { half_width: -1.0 }
            .validate()
            .is_err());
        assert!(AcceptanceBand::Factor {
            band_factor: f64::NAN
        }
        .validate()
        .is_err());
        assert!(same_unit(Some("1"), None));
        assert!(!same_unit(Some("C"), Some("K")));
    }

    #[test]
    fn ledger_wire_shape_round_trips() {
        let mut e = entry("tolerance", 0.12, "mm");
        e.prediction_uncertainty = Some(0.01);
        measure(&mut e, 0.15, AcceptanceBand::Absolute { half_width: 0.02 });
        let ledger = ClaimsLedger {
            entries: vec![e],
            claim_sets: vec![NativeClaimSet {
                schema: "vcad.tolerance-claims/1".into(),
                json: "{}".into(),
            }],
            ..ClaimsLedger::default()
        };
        let json = serde_json::to_value(&ledger).unwrap();
        assert_eq!(json["schema"], LEDGER_SCHEMA);
        assert_eq!(
            json["entries"][0]["measurement"]["band"]["kind"],
            "absolute"
        );
        let back: ClaimsLedger = serde_json::from_value(json).unwrap();
        assert_eq!(back, ledger);
        // Empty claim_sets is omitted on the wire and defaults on read.
        let bare: ClaimsLedger =
            serde_json::from_value(serde_json::json!({"schema": LEDGER_SCHEMA, "entries": []}))
                .unwrap();
        assert!(bare.claim_sets.is_empty());
    }
}
