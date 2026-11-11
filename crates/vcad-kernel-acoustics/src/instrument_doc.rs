//! Document-level instrument operations shared by the MCP tools and the Mac
//! app (via `vcad-ffi`): the `Document.instrument` metadata, the per-bar
//! receipt, recording a strike, and fabrication job requests.

use serde_json::{json, Value};
use vcad_ir::instrument::{BarMeasurement, Instrument as IrInstrument, InstrumentBar};

use crate::instrument::{match_bar, measured_claim, predicted_claim, BarMaterial, Instrument};
use crate::strike::cents;

/// Default window for matching a strike to a bar (a diatonic neighbour is
/// ≥ 100 cents away).
pub const MATCH_WINDOW_CENTS: f64 = 60.0;

/// Build the document metadata for a designed instrument.
pub fn metadata(inst: &Instrument) -> IrInstrument {
    IrInstrument {
        kind: "glockenspiel".into(),
        tuning: "12-TET, A4 = 440 Hz".into(),
        material: inst.spec.bar.material.id().into(),
        bars: inst
            .bars
            .iter()
            .map(|p| {
                let g = &p.bar.geometry;
                InstrumentBar {
                    note: p.bar.note.clone(),
                    part: p.part.clone(),
                    target_hz: p.bar.target_hz,
                    predicted_hz: p.bar.hz[0],
                    tolerance_cents: p.bar.tolerance_cents,
                    length_mm: g.length_mm,
                    width_mm: g.width_mm,
                    thickness_mm: g.thickness_mm,
                    undercut_depth_mm: g.undercut_depth_mm,
                    undercut_span: g.undercut_span,
                    hole_dia_mm: g.hole_dia_mm,
                    nodes_mm: p.bar.nodes_mm.clone(),
                    measurement: None,
                }
            })
            .collect(),
    }
}

/// The claim for one bar in its current state.
pub fn bar_claim(bar: &InstrumentBar) -> vcad_receipt::ReceiptClaim {
    match &bar.measurement {
        None => predicted_claim(&bar.note, bar.target_hz, bar.predicted_hz, &bar.part),
        Some(m) => measured_claim(
            &bar.note,
            bar.target_hz,
            bar.predicted_hz,
            m.measured_hz,
            bar.tolerance_cents,
            &bar.part,
            &m.instrument,
        ),
    }
}

/// The instrument's receipt: one claim per bar. Rolls up Provisional while
/// any bar is only predicted, Pass once every bar is measured in tolerance,
/// Fail if any measured bar is out.
pub fn receipt(meta: &IrInstrument) -> vcad_receipt::DesignReceipt {
    vcad_receipt::DesignReceipt::with_claims(meta.bars.iter().map(bar_claim).collect())
}

/// Receipt plus a compact per-bar status table, as JSON.
pub fn receipt_json(meta: &IrInstrument) -> Value {
    let r = receipt(meta);
    let bars: Vec<Value> = meta
        .bars
        .iter()
        .map(|b| {
            let state = match b.passes() {
                None => "predicted",
                Some(true) => "measured_pass",
                Some(false) => "measured_fail",
            };
            json!({
                "note": b.note,
                "part": b.part,
                "target_hz": b.target_hz,
                "predicted_hz": b.predicted_hz,
                "predicted_cents": cents(b.predicted_hz, b.target_hz),
                "measured_hz": b.measurement.as_ref().map(|m| m.measured_hz),
                "cents_error": b.measurement.as_ref().map(|m| m.cents_error),
                "tolerance_cents": b.tolerance_cents,
                "state": state,
            })
        })
        .collect();
    json!({
        "verdict": r.verdict(),
        "bars": bars,
        "receipt": r,
    })
}

/// Record a strike. `note` picks the bar explicitly; otherwise the nearest
/// target within [`MATCH_WINDOW_CENTS`] wins. Returns the bar index.
pub fn record_strike(
    meta: &mut IrInstrument,
    measured_hz: f64,
    note: Option<&str>,
    instrument: &str,
    measured_at: Option<String>,
) -> Result<usize, String> {
    if !(measured_hz.is_finite() && measured_hz > 0.0) {
        return Err("measured_hz must be a positive frequency".into());
    }
    let idx = match note {
        Some(n) => meta
            .bars
            .iter()
            .position(|b| b.note.eq_ignore_ascii_case(n) || b.part == n)
            .ok_or_else(|| format!("no bar {n:?} on this instrument"))?,
        None => {
            let targets: Vec<f64> = meta.bars.iter().map(|b| b.target_hz).collect();
            match_bar(measured_hz, &targets, MATCH_WINDOW_CENTS)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    let (n, c) = crate::instrument::nearest_note(measured_hz);
                    format!(
                        "{measured_hz:.1} Hz ({n} {c:+.0} c) is not within {MATCH_WINDOW_CENTS} cents of any bar"
                    )
                })?
        }
    };
    let bar = &mut meta.bars[idx];
    bar.measurement = Some(BarMeasurement {
        measured_hz,
        cents_error: cents(measured_hz, bar.target_hz),
        instrument: instrument.into(),
        measured_at,
    });
    Ok(idx)
}

/// `vcad_cam_job` requests that mill one bar from plate stock of the bar's
/// thickness (6061 for the default set): setup 1 drills the peg holes and
/// profiles the outline with holding tabs; setup 2, the bar flipped
/// upside-down in soft jaws, clears the undercut pocket across the full
/// width. Stock frame: top Z = 0, G54 at the stock's lower-left top corner.
pub fn cam_jobs(bar: &InstrumentBar, post: &str) -> Vec<Value> {
    let margin = 8.0;
    let t = bar.thickness_mm;
    let (sw, sh) = (bar.length_mm + 2.0 * margin, bar.width_mm + 2.0 * margin);
    let tools = json!([
        {"number": 1, "name": "3.175 mm 1F aluminium end mill", "type": "flat_endmill",
         "diameter": 3.175, "flutes": 1, "rpm": 18000, "feed": 600, "plunge": 150},
        {"number": 2, "name": "6 mm 2F aluminium end mill", "type": "flat_endmill",
         "diameter": 6.0, "flutes": 2, "rpm": 14000, "feed": 800, "plunge": 200}
    ]);
    let mut ops = Vec::new();
    for h in &bar.hole_positions() {
        ops.push(json!({
            "kind": "circle_pocket", "name": format!("peg hole @ {h:.1} mm"), "tool": 1,
            "cx": margin + h, "cy": margin + bar.width_mm / 2.0, "radius": bar.hole_dia_mm / 2.0,
            "depth": t, "stepdown": 0.5, "stepover": 1.0
        }));
    }
    let (x0, y0, x1, y1) = (
        margin,
        margin,
        margin + bar.length_mm,
        margin + bar.width_mm,
    );
    ops.push(json!({
        "kind": "contour_outside", "name": "bar outline", "tool": 1,
        "contour": [[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
        "depth": t, "stepdown": 0.5, "stepover": 1.0,
        "tabs": 2, "tabWidth": 4.0, "tabHeight": 1.0
    }));
    let setup1 = json!({
        "name": format!("{} setup 1 — holes + outline", bar.note),
        "stock": {"width": sw, "height": sh, "depth": t},
        "tools": tools, "operations": ops, "clearance": 5, "post": post,
    });
    let mut jobs = vec![setup1];
    if bar.undercut_depth_mm > 0.05 {
        let span = bar.undercut_span * bar.length_mm;
        let a = 0.5 * (bar.length_mm - span);
        jobs.push(json!({
            "name": format!("{} setup 2 — flipped, undercut", bar.note),
            "stock": {"width": bar.length_mm, "height": bar.width_mm, "depth": t},
            "tools": tools,
            "operations": [{
                "kind": "pocket", "name": "undercut", "tool": 2,
                "x": a, "y": 0.0, "width": span, "height": bar.width_mm,
                "depth": bar.undercut_depth_mm, "stepdown": 0.4, "stepover": 2.4
            }],
            "clearance": 5, "post": post,
        }));
    }
    jobs
}

/// Hole positions helper on the IR bar (holes sit on the nodes).
trait HolePositions {
    fn hole_positions(&self) -> Vec<f64>;
}
impl HolePositions for InstrumentBar {
    fn hole_positions(&self) -> Vec<f64> {
        if self.hole_dia_mm > 0.0 {
            self.nodes_mm.clone()
        } else {
            Vec::new()
        }
    }
}

/// Slicer request for printing the set (PLA bars + frame) through the
/// existing print flow (`vcad_print_call` / `predict_print`): dry run, the
/// G-code is written to `gcode_path` and never sent to a printer.
pub fn print_request(material: BarMaterial, gcode_path: &str) -> Value {
    json!({
        "op": "slice",
        "settings": {
            "layer_height": 0.2,
            "wall_count": if material == BarMaterial::Pla { 6 } else { 3 },
            "infill_density": if material == BarMaterial::Pla { 1.0 } else { 0.2 },
            "printer": "bambu-p1s",
            "material": "PLA",
        },
        "gcodePath": gcode_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{design_instrument, InstrumentSpec};
    use vcad_receipt::ReceiptVerdict;

    fn meta() -> IrInstrument {
        let inst =
            design_instrument(&InstrumentSpec::new("C6", BarMaterial::Aluminium6061).unwrap())
                .unwrap();
        metadata(&inst)
    }

    #[test]
    fn receipt_walks_provisional_to_pass_and_fail() {
        let mut m = meta();
        assert_ne!(receipt(&m).verdict(), ReceiptVerdict::Pass);
        // Strike every bar 2 cents sharp → all measured, receipt passes.
        let targets: Vec<f64> = m.bars.iter().map(|b| b.target_hz).collect();
        for t in &targets {
            let hz = t * 2f64.powf(2.0 / 1200.0);
            record_strike(&mut m, hz, None, "test", None).unwrap();
        }
        assert!(m.bars.iter().all(|b| b.passes() == Some(true)));
        assert_eq!(receipt(&m).verdict(), ReceiptVerdict::Pass);
        // Re-strike E6 20 cents flat → that bar fails, receipt fails.
        let e6 = targets[2] * 2f64.powf(-20.0 / 1200.0);
        let i = record_strike(&mut m, e6, None, "test", None).unwrap();
        assert_eq!(m.bars[i].note, "E6");
        assert_eq!(m.bars[i].passes(), Some(false));
        assert_eq!(receipt(&m).verdict(), ReceiptVerdict::Fail);
        let j = receipt_json(&m);
        assert_eq!(j["bars"][2]["state"], "measured_fail");
    }

    #[test]
    fn strike_off_every_bar_is_rejected() {
        let mut m = meta();
        // 3 kHz sits between F#7 and G7 — far from every bar of C6–C7.
        let err = record_strike(&mut m, 3000.0, None, "test", None).unwrap_err();
        assert!(err.contains("not within"));
        assert!(record_strike(&mut m, 1046.0, Some("Q9"), "t", None).is_err());
        assert_eq!(
            record_strike(&mut m, 1046.0, Some("bar_c6"), "t", None).unwrap(),
            0
        );
    }

    #[test]
    fn cam_jobs_cover_holes_outline_and_undercut() {
        let m = meta();
        let jobs = cam_jobs(&m.bars[0], "grbl");
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0]["operations"].as_array().unwrap().len(), 3);
        assert_eq!(jobs[1]["operations"][0]["kind"], "pocket");
        let d = jobs[1]["operations"][0]["depth"].as_f64().unwrap();
        assert!((d - m.bars[0].undercut_depth_mm).abs() < 1e-9);
    }
}
