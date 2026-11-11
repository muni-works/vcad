//! Fab jobs as tasks: print (slice → G-code) and mill (CAM → stock-sim
//! verify → G-code), each ending in a **fail-closed verify gate** and a
//! machine backend.
//!
//! # Safety contract
//!
//! This crate has **no network or serial backend, by construction**. The only
//! [`MachineBackend`] here is [`DryRunMachine`], which parses and "streams"
//! G-code into a simulated machine model and reports what it would have done.
//! Real dispatch to a Bambu P1S / Carvera Air lives in the Swift app behind its
//! own gates and an explicit human confirm; wiring it to these tasks is a
//! deliberate later step, not something an agent call can reach today.
//!
//! Every gate is evaluated and reported, even after one fails, so the agent
//! sees the full picture. Any failed gate fails the task — the backend is
//! never reached with unverified G-code.

use serde::Serialize;
use serde_json::{json, Value};
use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::Document;
use vcad_kernel::vcad_kernel_stocksim::{verify_stock_against_mesh, Stock, VerifyOptions};
use vcad_kernel_cam::post::{GrblPost, PostProcessor};
use vcad_kernel_cam::{CamSettings, Contour, Contour2D, Tool};
use vcad_kernel_tessellate::TriangleMesh;
use vcad_slicer::SliceSettings;
use vcad_slicer_gcode::{GcodeSettings, PrinterProfile};

use crate::task::{JobError, TaskCtx};

/// One verify gate's outcome.
#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    /// Stable gate id (`fits_bed`, `stock_verify`, …).
    pub name: &'static str,
    /// Whether it passed.
    pub pass: bool,
    /// Human-readable detail (measured vs limit).
    pub detail: String,
}

fn gate(name: &'static str, pass: bool, detail: impl Into<String>) -> Gate {
    Gate {
        name,
        pass,
        detail: detail.into(),
    }
}

/// Fail the task if any gate failed, carrying the full gate list.
fn enforce(gates: &[Gate], extra: Value) -> Result<(), JobError> {
    let failed: Vec<&str> = gates.iter().filter(|g| !g.pass).map(|g| g.name).collect();
    if failed.is_empty() {
        return Ok(());
    }
    Err(JobError::Failed {
        message: format!("verify gate failed closed: {}", failed.join(", ")),
        report: Some(json!({ "gates": gates, "detail": extra, "dispatched": false })),
    })
}

// ---------------------------------------------------------------------------
// G-code analysis + dry-run backend
// ---------------------------------------------------------------------------

/// Motion envelope of a G-code program (absolute-coordinate G0/G1/G2/G3).
#[derive(Debug, Clone, Serialize)]
pub struct GcodeEnvelope {
    /// Min `[x, y, z]` reached.
    pub min: [f64; 3],
    /// Max `[x, y, z]` reached.
    pub max: [f64; 3],
    /// Motion lines.
    pub moves: usize,
    /// Lines with a positive extrusion (`E`) word.
    pub extrusions: usize,
    /// Total non-comment lines.
    pub lines: usize,
}

/// Scan a program's motion envelope. Only absolute positioning is modelled;
/// a `G91` makes the envelope unknowable, which is reported as an error so the
/// caller fails closed.
pub fn gcode_envelope(code: &str) -> Result<GcodeEnvelope, String> {
    let mut pos = [f64::NAN; 3];
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let (mut moves, mut extrusions, mut lines) = (0, 0, 0);
    for raw in code.lines() {
        let line = raw.split(';').next().unwrap_or("");
        let line = match line.find('(') {
            Some(i) => &line[..i],
            None => line,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        lines += 1;
        let words: Vec<&str> = line.split_whitespace().collect();
        let has = |w: &str| words.iter().any(|x| x.eq_ignore_ascii_case(w));
        if has("G91") {
            return Err("relative positioning (G91) — envelope not checkable".into());
        }
        let motion = ["G0", "G00", "G1", "G01", "G2", "G02", "G3", "G03"]
            .iter()
            .any(|g| has(g));
        if !motion {
            continue;
        }
        moves += 1;
        for w in &words {
            let (axis, val) = w.split_at(1);
            let Ok(v) = val.parse::<f64>() else { continue };
            match axis {
                "X" | "x" => pos[0] = v,
                "Y" | "y" => pos[1] = v,
                "Z" | "z" => pos[2] = v,
                "E" | "e" if v > 0.0 => extrusions += 1,
                _ => {}
            }
        }
        for k in 0..3 {
            if pos[k].is_finite() {
                min[k] = min[k].min(pos[k]);
                max[k] = max[k].max(pos[k]);
            }
        }
    }
    Ok(GcodeEnvelope {
        min,
        max,
        moves,
        extrusions,
        lines,
    })
}

/// A machine that consumes verified G-code.
pub trait MachineBackend {
    /// Backend id reported in results.
    fn id(&self) -> &str;
    /// `true` for backends that cannot move hardware. Tasks refuse any
    /// backend that reports `false` — the only safe value in this crate.
    fn is_dry_run(&self) -> bool;
    /// Stream the program, reporting progress in `[from, 1]`.
    fn run(&self, code: &str, ctx: &TaskCtx, from: f64) -> Result<Value, JobError>;
}

/// Simulated machine: walks the program line by line, honouring cancel
/// between lines, and returns the run summary. Never touches I/O.
#[derive(Debug, Clone)]
pub struct DryRunMachine {
    /// Machine name for the report (e.g. "Bambu P1S (dry run)").
    pub name: String,
    /// Optional per-line delay so progress/cancel are observable in demos
    /// and tests; zero by default.
    pub line_delay: std::time::Duration,
}

impl DryRunMachine {
    /// A dry-run machine with the given display name.
    pub fn new(name: impl Into<String>) -> Self {
        DryRunMachine {
            name: name.into(),
            line_delay: std::time::Duration::ZERO,
        }
    }
}

impl MachineBackend for DryRunMachine {
    fn id(&self) -> &str {
        &self.name
    }
    fn is_dry_run(&self) -> bool {
        true
    }
    fn run(&self, code: &str, ctx: &TaskCtx, from: f64) -> Result<Value, JobError> {
        let total = code.lines().count().max(1);
        let stride = (total / 50).max(1);
        for (i, _line) in code.lines().enumerate() {
            ctx.checkpoint()?;
            if !self.line_delay.is_zero() {
                std::thread::sleep(self.line_delay);
            }
            if i % stride == 0 {
                let f = from + (1.0 - from) * (i as f64 / total as f64);
                ctx.progress(f, format!("dry run: line {i}/{total}"))?;
            }
        }
        Ok(json!({ "backend": self.name, "dryRun": true, "linesStreamed": total }))
    }
}

fn check_backend(b: &dyn MachineBackend) -> Result<(), JobError> {
    if b.is_dry_run() {
        Ok(())
    } else {
        Err(
            "refusing a non-dry-run backend: hardware dispatch is not reachable from vcad-atoms"
                .into(),
        )
    }
}

// ---------------------------------------------------------------------------
// Shared mesh helpers
// ---------------------------------------------------------------------------

fn doc_mesh(doc: &Document) -> Result<(Vec<f32>, Vec<u32>), String> {
    let scene = evaluate_document(doc, &EvalOptions::default()).map_err(|e| e.to_string())?;
    let mut v: Vec<f32> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    for part in &scene.parts {
        let base = (v.len() / 3) as u32;
        v.extend_from_slice(&part.mesh.positions);
        idx.extend(part.mesh.indices.iter().map(|i| i + base));
    }
    if idx.is_empty() {
        return Err("document has no geometry".into());
    }
    Ok((v, idx))
}

fn bounds(v: &[f32]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for c in v.as_chunks::<3>().0 {
        for k in 0..3 {
            lo[k] = lo[k].min(c[k] as f64);
            hi[k] = hi[k].max(c[k] as f64);
        }
    }
    (lo, hi)
}

fn translate(v: &mut [f32], d: [f64; 3]) {
    for c in v.as_chunks_mut::<3>().0 {
        for k in 0..3 {
            c[k] += d[k] as f32;
        }
    }
}

fn f(req: &Value, key: &str, default: f64) -> Result<f64, String> {
    match req.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite())
            .ok_or_else(|| format!("`{key}` must be a finite number")),
    }
}

// ---------------------------------------------------------------------------
// Print
// ---------------------------------------------------------------------------

fn printer(id: &str) -> Result<PrinterProfile, String> {
    Ok(match id {
        "bambu_p1s" => PrinterProfile::bambu_p1s(),
        "bambu_x1c" => PrinterProfile::bambu_x1c(),
        "bambu_a1" => PrinterProfile::bambu_a1(),
        "bambu_a1_mini" => PrinterProfile::bambu_a1_mini(),
        "prusa_mk4" => PrinterProfile::prusa_mk4(),
        other => return Err(format!("unknown printer `{other}`")),
    })
}

/// `fab_print`: slice the document for a printer, verify the G-code, then
/// stream it to a dry-run printer.
///
/// Request keys (all optional): `printer` (default `bambu_p1s`),
/// `layerHeight`, `infill` (0–1), `walls`, `printTemp`, `bedTemp`.
pub fn fab_print(
    doc: &Document,
    req: &Value,
    backend: &dyn MachineBackend,
    ctx: &TaskCtx,
) -> Result<Value, JobError> {
    check_backend(backend)?;
    let printer_id = req
        .get("printer")
        .and_then(Value::as_str)
        .unwrap_or("bambu_p1s");
    let profile = printer(printer_id)?;
    let mut settings = SliceSettings {
        nozzle_diameter: profile.nozzle_diameter,
        ..SliceSettings::default()
    };
    settings.layer_height = f(req, "layerHeight", settings.layer_height)?;
    settings.infill_density = f(req, "infill", settings.infill_density)?;
    settings.wall_count = f(req, "walls", settings.wall_count as f64)? as u32;

    ctx.progress(0.02, "evaluating document")?;
    let (mut verts, indices) = doc_mesh(doc)?;
    let (lo, hi) = bounds(&verts);
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    // Drop onto the bed, centred.
    translate(
        &mut verts,
        [
            profile.bed_x / 2.0 - (lo[0] + hi[0]) / 2.0,
            profile.bed_y / 2.0 - (lo[1] + hi[1]) / 2.0,
            -lo[2],
        ],
    );
    let mesh = TriangleMesh {
        vertices: verts,
        indices,
        normals: Vec::new(),
        face_kinds: Vec::new(),
        face_ids: Vec::new(),
    };

    ctx.progress(0.1, "slicing")?;
    let sliced = vcad_slicer::slice(&mesh, &settings).map_err(|e| e.to_string())?;
    ctx.progress(0.5, format!("sliced {} layers", sliced.layers.len()))?;
    let gcode = vcad_slicer_gcode::generate_gcode(
        &sliced,
        GcodeSettings {
            printer: profile.clone(),
            print_temp: f(req, "printTemp", 210.0)? as u32,
            bed_temp: f(req, "bedTemp", 60.0)? as u32,
            ..Default::default()
        },
    );
    ctx.progress(0.6, "verifying G-code")?;

    // ---- verify gates (all evaluated, then enforced) ----
    let env = gcode_envelope(&gcode);
    let mut gates = vec![
        gate(
            "fits_bed",
            size[0] <= profile.bed_x && size[1] <= profile.bed_y && size[2] <= profile.bed_z,
            format!(
                "part {:.1}×{:.1}×{:.1} mm vs bed {}×{}×{}",
                size[0], size[1], size[2], profile.bed_x, profile.bed_y, profile.bed_z
            ),
        ),
        gate(
            "has_layers",
            !sliced.layers.is_empty(),
            format!("{} layers", sliced.layers.len()),
        ),
    ];
    match &env {
        Ok(e) => {
            // Purge lines/priming may sit near the bed edge: allow 0..bed.
            let inside = e.min[0] >= -1e-6
                && e.min[1] >= -1e-6
                && e.max[0] <= profile.bed_x + 1e-6
                && e.max[1] <= profile.bed_y + 1e-6
                && e.min[2] >= -1e-6;
            gates.push(gate(
                "motion_within_bed",
                inside,
                format!(
                    "XY [{:.1},{:.1}]–[{:.1},{:.1}], Z min {:.2}",
                    e.min[0], e.min[1], e.max[0], e.max[1], e.min[2]
                ),
            ));
            let ztop = size[2] + settings.layer_height + 15.0; // end-of-print lift allowance
            gates.push(gate(
                "z_within_part",
                e.max[2] <= ztop.min(profile.bed_z + 1e-6),
                format!(
                    "Z max {:.2} vs limit {:.2}",
                    e.max[2],
                    ztop.min(profile.bed_z)
                ),
            ));
            gates.push(gate(
                "extrudes",
                e.extrusions > 0,
                format!("{} extruding moves", e.extrusions),
            ));
        }
        Err(msg) => gates.push(gate("gcode_parse", false, msg.clone())),
    }
    let summary = json!({
        "printer": profile.name,
        "layers": sliced.stats.layer_count,
        "printTimeS": sliced.stats.print_time_seconds,
        "filamentG": sliced.stats.filament_grams,
        "gcodeLines": gcode.lines().count(),
        "envelope": env.as_ref().ok(),
    });
    enforce(&gates, summary.clone())?;

    ctx.progress(0.65, format!("streaming to {}", backend.id()))?;
    let run = backend.run(&gcode, ctx, 0.65)?;
    Ok(json!({ "kind": "print", "gates": gates, "summary": summary, "run": run }))
}

// ---------------------------------------------------------------------------
// Mill
// ---------------------------------------------------------------------------

/// `fab_mill`: CAM an outside profile around the part on a Carvera-class
/// 3-axis mill, verify the cut in the stock simulator against the target
/// mesh, then stream to a dry-run mill.
///
/// The part is placed with its XY-min at `(margin, margin)` and its bottom on
/// the stock bottom (stock top = Z0). Stock defaults to the part's bounding
/// box plus `margin` on each side, so one profile pass clears it — the
/// stock-sim gate grades exactly that claim and fails closed on any gouge or
/// leftover.
///
/// Request keys (all optional): `toolDiameter` (3.175), `stepdown` (1.0),
/// `feed` (600), `plunge` (150), `rpm` (12000), `margin` (0.6 · tool, so corners clear),
/// `stockDepth` (part height), `tolerance` (0.15), `allowance` (0.25).
pub fn fab_mill(
    doc: &Document,
    req: &Value,
    backend: &dyn MachineBackend,
    ctx: &TaskCtx,
) -> Result<Value, JobError> {
    check_backend(backend)?;
    let dia = f(req, "toolDiameter", 3.175)?;
    if dia <= 0.0 || dia > 25.0 {
        return Err("toolDiameter must be in (0, 25] mm".into());
    }
    // Corners: the tool rounds them, so the stock corner sits margin·√2 from the
    // part corner — keep that under one tool diameter or the corner survives.
    let margin = f(req, "margin", 0.6 * dia)?;
    ctx.progress(0.02, "evaluating document")?;
    let (mut verts, indices) = doc_mesh(doc)?;
    let (lo, hi) = bounds(&verts);
    let part = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let depth = f(req, "stockDepth", part[2])?;
    let stock = [part[0] + 2.0 * margin, part[1] + 2.0 * margin, depth];
    // Part frame → stock frame: XY min at margin, bottom at -depth.
    translate(&mut verts, [margin - lo[0], margin - lo[1], -depth - lo[2]]);

    let settings = CamSettings {
        stepover: dia * 0.4,
        stepdown: f(req, "stepdown", 1.0)?,
        feed_rate: f(req, "feed", 600.0)?,
        plunge_rate: f(req, "plunge", 150.0)?,
        spindle_rpm: f(req, "rpm", 12000.0)?,
        safe_z: 5.0,
        retract_z: 5.0,
    };
    if settings.stepdown <= 0.0 || settings.feed_rate <= 0.0 {
        return Err("stepdown and feed must be positive".into());
    }
    let tool = Tool::FlatEndMill {
        diameter: dia,
        flute_length: depth + 2.0,
        flutes: 2,
    };
    ctx.progress(0.1, "generating toolpath")?;
    let path = Contour2D::outside(Contour::rectangle(margin, margin, part[0], part[1]), depth)
        .generate(&tool, &settings)
        .map_err(|e| e.to_string())?;
    let gcode = GrblPost::default().generate("vcad fab_mill", &tool, &path, &settings);

    ctx.progress(0.3, "simulating stock removal")?;
    let res = (stock[0].max(stock[1]).max(stock[2]) / 120.0).max(0.1);
    let mut sim = Stock::from_box([0.0, 0.0, -depth, stock[0], stock[1], 0.0], res);
    sim.subtract_toolpath(&tool, &path);
    ctx.progress(0.55, "grading cut against the model")?;
    let verify = verify_stock_against_mesh(
        &sim,
        &verts,
        &indices,
        &VerifyOptions {
            tolerance: f(req, "tolerance", 0.15)?,
            allowance: f(req, "allowance", 0.25)?,
            max_examples: 8,
        },
    );

    let env = gcode_envelope(&gcode);
    let r = dia / 2.0;
    let mut gates = vec![
        gate(
            "toolpath_nonempty",
            !path.segments.is_empty(),
            format!("{} segments", path.segments.len()),
        ),
        gate(
            "stock_verify",
            verify.pass,
            format!(
                "gouge {} / excess {} (tol {}, allowance {})",
                verify.gouge.pass, verify.excess.pass, verify.tolerance, verify.allowance
            ),
        ),
    ];
    match &env {
        Ok(e) => {
            gates.push(gate(
                "motion_within_stock",
                e.min[0] >= -r - 1e-6
                    && e.min[1] >= -r - 1e-6
                    && e.max[0] <= stock[0] + r + 1e-6
                    && e.max[1] <= stock[1] + r + 1e-6,
                format!(
                    "XY [{:.2},{:.2}]–[{:.2},{:.2}] vs stock {:.2}×{:.2} ± r",
                    e.min[0], e.min[1], e.max[0], e.max[1], stock[0], stock[1]
                ),
            ));
            gates.push(gate(
                "no_spoilboard_cut",
                e.min[2] >= -depth - 0.05,
                format!("Z min {:.3} vs stock bottom {:.3}", e.min[2], -depth),
            ));
            gates.push(gate(
                "retracts_above_stock",
                e.max[2] > 0.0,
                format!("Z max {:.2}", e.max[2]),
            ));
        }
        Err(msg) => gates.push(gate("gcode_parse", false, msg.clone())),
    }
    let summary = json!({
        "machine": "3-axis mill (Carvera-class), GRBL post",
        "stockMm": stock,
        "toolDiameter": dia,
        "simResolution": res,
        "stockVerify": verify,
        "gcodeLines": gcode.lines().count(),
        "envelope": env.as_ref().ok(),
    });
    enforce(&gates, summary.clone())?;

    ctx.progress(0.6, format!("streaming to {}", backend.id()))?;
    let run = backend.run(&gcode, ctx, 0.6)?;
    Ok(json!({ "kind": "mill", "gates": gates, "summary": summary, "run": run }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_tracks_absolute_motion_and_extrusion() {
        let e =
            gcode_envelope("G90\nG0 X1 Y2 Z3\nG1 X10 E0.5 ; wall\n(comment)\nG1 Y-1\n").unwrap();
        assert_eq!(e.min, [1.0, -1.0, 3.0]);
        assert_eq!(e.max, [10.0, 2.0, 3.0]);
        assert_eq!(e.extrusions, 1);
        assert_eq!(e.moves, 3);
    }

    #[test]
    fn relative_mode_fails_closed() {
        assert!(gcode_envelope("G91\nG1 X5\n").is_err());
    }

    struct Live;
    impl MachineBackend for Live {
        fn id(&self) -> &str {
            "live"
        }
        fn is_dry_run(&self) -> bool {
            false
        }
        fn run(&self, _: &str, _: &TaskCtx, _: f64) -> Result<Value, JobError> {
            panic!("must never be reached")
        }
    }

    #[test]
    fn non_dry_run_backend_is_refused() {
        let doc = Document::new();
        let err = fab_print(&doc, &json!({}), &Live, &TaskCtx::detached()).unwrap_err();
        assert!(matches!(err, JobError::Failed { .. }));
        let err = fab_mill(&doc, &json!({}), &Live, &TaskCtx::detached()).unwrap_err();
        assert!(matches!(err, JobError::Failed { .. }));
    }
}
