//! Electronics co-design for the native app (`vcad_pcb_*`): the native twin of
//! the MCP ECAD tools (`import_kicad`, `run_drc`, `route_nets`, `fix_drc` /
//! `fab_prep`, `export_gerber`, `length_match_traces`, `check_enclosure_fit`,
//! `board_from_solid`, `solid_from_board`).
//!
//! Every entry point is pure JSON in / JSON out: the board is the IR `Pcb`
//! (camelCase, exactly what the document stores under `pcb` or a `PcbBoard`
//! node), so the app keeps owning the document and its undo stack. Results
//! that change the board hand the whole board back under `"pcb"`.
//!
//! The DRC numbers the app shows are the fab-prep ones: absolute violations,
//! and *route-attributable* violations measured against the same board with
//! every trace and via stripped. Absolute zero is not reachable on an imported
//! fixture (footprint-internal and placement findings are not routing's to
//! fix), so both are always reported — and Gerber export is gated on the
//! route-attributable count reaching zero, never on the absolute one being
//! quietly ignored.

use std::collections::BTreeSet;
use std::ffi::c_char;
use std::path::Path;
use std::ptr;

use serde::Serialize;
use serde_json::{json, Value};
use vcad_ecad_fabprep::{run_fab_prep, DrcDelta, FabPrepOptions, VerdictOptions};
use vcad_ecad_pcb::drc::{check_drc, DrcViolation};
use vcad_ecad_pcb::router::length_match::{
    match_lengths_runs, net_routed_length, LengthMatchOptions,
};
use vcad_ir::ecad::Pcb;
use vcad_kernel_enclosure as encl;

use crate::analysis::{evaluate, parse_doc, parse_request};
use crate::err;
use crate::json::{borrow_str, guard_ptr, json_out};

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

fn parse_pcb(json: *const c_char) -> Result<Pcb, String> {
    let text = borrow_str("pcb", json).ok_or("pcb: bad string")?;
    serde_json::from_str(text).map_err(|e| format!("pcb: parse: {e}"))
}

fn run<T: Serialize>(what: &str, f: impl FnOnce() -> Result<T, String>) -> *mut c_char {
    guard_ptr(what, || match f() {
        Ok(v) => json_out(&v),
        Err(e) => {
            err::set_error(format!("{what}: {e}"));
            ptr::null_mut()
        }
    })
}

fn stripped(pcb: &Pcb) -> Pcb {
    let mut s = pcb.clone();
    s.traces.clear();
    s.trace_arcs.clear();
    s.vias.clear();
    s
}

/// One DRC finding, flattened for the app.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Rule name (`clearance`, `short`, …).
    pub rule: String,
    /// `error` | `warning`.
    pub severity: String,
    /// Board position, mm.
    pub x: f64,
    /// Board position, mm.
    pub y: f64,
    /// Human-readable message.
    pub message: String,
    /// `intra_footprint` | `inter_component` | `routing`.
    pub provenance: String,
}

fn finding(v: &DrcViolation) -> Finding {
    let tag = |s: String| s.trim_matches('"').to_string();
    Finding {
        rule: vcad_ecad_fabprep::delta::rule_name(&v.rule),
        severity: tag(serde_json::to_string(&v.severity).unwrap_or_default()).to_lowercase(),
        x: v.position.x,
        y: v.position.y,
        message: v.message.clone(),
        provenance: tag(serde_json::to_string(&v.provenance).unwrap_or_default()),
    }
}

/// The DRC verdict the panel and the export gate both read.
#[derive(Debug, Clone, Serialize)]
pub struct DrcReport {
    /// Every violation on the board as it is.
    pub total: usize,
    /// Violations on the same board with all routing stripped (the floor the
    /// board arrived with).
    pub baseline_total: usize,
    /// Violations routing introduced (and has not waived). Gerber export
    /// requires zero.
    pub route_attributable: usize,
    /// Of those, the ones the strip-and-re-route loop can fix.
    pub route_fixable: usize,
    /// Nets still open — unconnected, islanded, or an SMD pad unstitched from
    /// its plane (`vcad_ecad_fabprep::open_nets`). Zero means the netlist is
    /// fully realized in copper.
    pub unrouted: usize,
    /// The open nets by name (sorted).
    pub open_nets: Vec<String>,
    /// True when `route_attributable == 0`.
    pub clean: bool,
    /// Findings, routing-attributable offenders first, capped.
    pub findings: Vec<Finding>,
    /// Per-rule table.
    pub rules: Vec<vcad_ecad_fabprep::RuleDelta>,
}

/// Max findings echoed (dense imported boards have thousands).
const FINDINGS_CAP: usize = 500;

/// Run DRC and the route-attributable delta.
pub fn drc_report(pcb: &Pcb, accepted: &BTreeSet<String>) -> DrcReport {
    let final_ = check_drc(pcb);
    let baseline = check_drc(&stripped(pcb));
    let delta = DrcDelta::compute(&baseline, &final_, accepted);
    let attributable = delta
        .route_attributable_total
        .saturating_sub(delta.route_attributable_accepted);
    // Was `rule_name(..).contains("unrouted")` — but rule names are the Debug
    // spelling (`UnconnectedNet`, `NetIslands`), so that never matched and the
    // app reported "0 unrouted" on every board.
    let open_nets = vcad_ecad_fabprep::open_nets(&final_);
    let unrouted = open_nets.len();
    let offender_msgs: BTreeSet<&str> =
        delta.offenders.iter().map(|o| o.message.as_str()).collect();
    let mut findings: Vec<Finding> = final_
        .iter()
        .filter(|v| offender_msgs.contains(v.message.as_str()))
        .chain(
            final_
                .iter()
                .filter(|v| !offender_msgs.contains(v.message.as_str())),
        )
        .take(FINDINGS_CAP)
        .map(finding)
        .collect();
    findings.dedup_by(|a, b| a.message == b.message && a.x == b.x && a.y == b.y);
    DrcReport {
        total: final_.len(),
        baseline_total: baseline.len(),
        route_attributable: attributable,
        route_fixable: delta.route_attributable_fixable,
        unrouted,
        open_nets,
        clean: attributable == 0,
        findings,
        rules: delta.rules,
    }
}

fn accepted_rules(req: &Value) -> BTreeSet<String> {
    req.get("accept")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

/// Parse a board file by extension (`.kicad_pcb`, `.brd`). Returns
/// `{pcb, warnings, footprints, nets, traces}`.
pub fn import_board(path: &Path) -> Result<Value, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let (pcb, warnings) = match ext.as_str() {
        "kicad_pcb" => vcad_ecad_symbols::kicad_pcb::parse_kicad_pcb_with_warnings(&text)
            .map_err(|e| format!("KiCad parse: {e}"))?,
        "brd" => (
            vcad_ecad_symbols::eagle_brd::parse_eagle_brd(&text)
                .map_err(|e| format!("Eagle parse: {e}"))?,
            Vec::new(),
        ),
        other => {
            return Err(format!(
                "unsupported board format .{other} (use .kicad_pcb or .brd)"
            ))
        }
    };
    Ok(json!({
        "pcb": pcb,
        "warnings": warnings,
        "footprints": pcb.footprints.len(),
        "nets": pcb.nets.len(),
        "traces": pcb.traces.len(),
        "vias": pcb.vias.len(),
    }))
}

/// `char *vcad_pcb_import(const char *path)`.
#[no_mangle]
pub extern "C" fn vcad_pcb_import(path: *const c_char) -> *mut c_char {
    run("vcad_pcb_import", || {
        let p = borrow_str("path", path).ok_or("bad path")?;
        import_board(Path::new(p))
    })
}

// ---------------------------------------------------------------------------
// DRC / route / fix
// ---------------------------------------------------------------------------

/// `char *vcad_pcb_drc(const char *pcb_json, const char *request_json)`.
/// request `{"accept"?: [rule]}` → [`DrcReport`].
#[no_mangle]
pub extern "C" fn vcad_pcb_drc(
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_drc", || {
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        Ok(drc_report(&pcb, &accepted_rules(&req)))
    })
}

fn verdict_opts(req: &Value) -> VerdictOptions {
    let mut v = VerdictOptions::default();
    if let Some(b) = req.get("budget").and_then(Value::as_u64) {
        v.budget = b as usize;
    }
    if let Some(c) = req.get("max_cluster").and_then(Value::as_u64) {
        v.max_cluster = c as usize;
    }
    v.allow_blind_vias = req
        .get("allow_blind_vias")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    v
}

/// Route every unrouted connection. Returns `{pcb, verdict, drc}`.
pub fn autoroute(mut pcb: Pcb, req: &Value) -> Value {
    let verdict = vcad_ecad_fabprep::verdict::route_remaining(&mut pcb, verdict_opts(req));
    let finished = vcad_ecad_fabprep::verdict::finish_remaining(&mut pcb, 150_000);
    let drc = drc_report(&pcb, &accepted_rules(req));
    json!({ "pcb": pcb, "verdict": verdict, "finished": finished, "drc": drc })
}

/// `char *vcad_pcb_autoroute(const char *pcb_json, const char *request_json)`.
#[no_mangle]
pub extern "C" fn vcad_pcb_autoroute(
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_autoroute", || {
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        Ok(autoroute(pcb, &req))
    })
}

fn fab_opts(req: &Value) -> FabPrepOptions {
    let mut o = FabPrepOptions {
        calibrate_rules: false,
        ..Default::default()
    };
    o.calibrate_rules = req
        .get("calibrate")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    o.route_remaining = req.get("route").and_then(Value::as_bool).unwrap_or(true);
    if let Some(r) = req.get("max_rounds").and_then(Value::as_u64) {
        o.max_rounds = r as usize;
    }
    o.accept_rules = accepted_rules(req).into_iter().collect();
    o.allow_incomplete = req
        .get("allow_incomplete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    o.verdict = verdict_opts(req);
    o
}

/// Fix DRC: the fab-prep strip-and-re-route loop (plus opt-in calibration and
/// the dangling-copper prune). Returns `{pcb, converged, headline, report, drc}`.
pub fn fix_drc(mut pcb: Pcb, req: &Value) -> Value {
    let outcome = run_fab_prep(&mut pcb, &fab_opts(req));
    let drc = drc_report(&pcb, &accepted_rules(req));
    json!({
        "pcb": pcb,
        "converged": outcome.report.converged,
        "headline": outcome.report.headline(),
        "summary": vcad_ecad_fabprep::render::summary(&outcome.report),
        "report": outcome.report,
        "drc": drc,
    })
}

/// `char *vcad_pcb_fix_drc(const char *pcb_json, const char *request_json)`.
/// request `{"calibrate"?, "route"?, "max_rounds"?, "accept"?}`.
#[no_mangle]
pub extern "C" fn vcad_pcb_fix_drc(
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_fix_drc", || {
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        Ok(fix_drc(pcb, &req))
    })
}

// ---------------------------------------------------------------------------
// Gerber export (clean-DRC gate)
// ---------------------------------------------------------------------------

/// Write Gerbers + Excellon drills + BOM + pick-and-place into `dir` — only
/// when the board has zero route-attributable DRC violations. Fail-closed:
/// a dirty board writes nothing and returns an error naming the count.
pub fn export_gerbers(pcb: &Pcb, dir: &Path, req: &Value) -> Result<Value, String> {
    let drc = drc_report(pcb, &accepted_rules(req));
    if !drc.clean {
        return Err(format!(
            "refused: {} route-attributable DRC violation(s) ({} total, {} on the unrouted floor) — \
             run Fix DRC first; no fabrication files were written",
            drc.route_attributable, drc.total, drc.baseline_total
        ));
    }
    // Geometrically clean is not the circuit: refuse a board with open nets
    // unless the caller explicitly opts in (same contract as fab-prep).
    let allow_incomplete = req
        .get("allow_incomplete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if drc.unrouted > 0 && !allow_incomplete {
        return Err(format!(
            "refused: board NOT FULLY ROUTED — {} net(s) open ({}) — route them first, or pass \
             allow_incomplete; no fabrication files were written",
            drc.unrouted,
            drc.open_nets.join(", ")
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let mut written = Vec::new();
    let mut put = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let p = dir.join(name);
        std::fs::write(&p, bytes).map_err(|e| format!("write {}: {e}", p.display()))?;
        written.push(name.to_string());
        Ok(())
    };
    let gerbers =
        vcad_ecad_export::gerber::generate_gerbers(pcb).map_err(|e| format!("gerbers: {e}"))?;
    let mut names: Vec<_> = gerbers.keys().cloned().collect();
    names.sort();
    for n in &names {
        put(n, gerbers[n].as_bytes())?;
    }
    for (n, c) in
        vcad_ecad_export::excellon::generate_drill_files(pcb).map_err(|e| format!("drill: {e}"))?
    {
        put(&n, c.as_bytes())?;
    }
    let mut bom = Vec::new();
    vcad_ecad_export::bom::write_bom(&mut bom, pcb).map_err(|e| format!("bom: {e}"))?;
    put("bom.csv", &bom)?;
    let mut pnp = Vec::new();
    vcad_ecad_export::pick_place::write_pick_place(&mut pnp, pcb)
        .map_err(|e| format!("pick&place: {e}"))?;
    put("pick_place.csv", &pnp)?;
    Ok(json!({ "dir": dir.display().to_string(), "files": written, "drc": drc }))
}

/// `char *vcad_pcb_export_gerbers(const char *pcb_json, const char *request_json)`.
/// request `{"dir": path, "accept"?}`.
#[no_mangle]
pub extern "C" fn vcad_pcb_export_gerbers(
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_export_gerbers", || {
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        let dir = req
            .get("dir")
            .and_then(Value::as_str)
            .ok_or("request.dir required")?;
        export_gerbers(&pcb, Path::new(dir), &req)
    })
}

// ---------------------------------------------------------------------------
// length tuning
// ---------------------------------------------------------------------------

/// Routed length of every net with copper, longest first.
pub fn net_lengths(pcb: &Pcb) -> Vec<(String, f64)> {
    let nets: BTreeSet<&str> = pcb.traces.iter().map(|t| t.net.as_str()).collect();
    let mut out: Vec<(String, f64)> = nets
        .into_iter()
        .map(|n| (n.to_string(), net_routed_length(pcb, n)))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// Meander the shorter nets of a group up to the target (default: longest).
/// Commits replacement copper for tuned nets. Returns `{pcb, result, drc}`.
pub fn length_match(mut pcb: Pcb, req: &Value) -> Result<Value, String> {
    let nets: Vec<String> = req
        .get("nets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if nets.is_empty() {
        return Err("request.nets: name at least one net".into());
    }
    let mut opts = LengthMatchOptions {
        target_length: req.get("target").and_then(Value::as_f64),
        ..Default::default()
    };
    if let Some(t) = req.get("tolerance").and_then(Value::as_f64) {
        opts.tolerance = t;
    }
    if let Some(a) = req.get("max_amplitude").and_then(Value::as_f64) {
        opts.max_amplitude = a;
    }
    // The tuner clears its meanders against what it can see; the DRC has the
    // last word. Shallower meanders if they break a rule, nothing if they
    // still do.
    let accepted = accepted_rules(req);
    let before = drc_report(&pcb, &accepted).route_attributable;
    let mut result = match_lengths_runs(&pcb, &nets, &opts);
    let mut tuned = None;
    for _ in 0..4 {
        let mut trial = pcb.clone();
        for n in result
            .nets
            .iter()
            .filter(|n| n.tuned && !n.new_traces.is_empty())
        {
            trial.traces.retain(|t| t.net != n.net);
            trial.traces.extend(n.new_traces.iter().cloned());
        }
        if drc_report(&trial, &accepted).route_attributable <= before {
            tuned = Some(trial);
            break;
        }
        opts.max_amplitude /= 2.0;
        result = match_lengths_runs(&pcb, &nets, &opts);
    }
    match tuned {
        Some(trial) => pcb = trial,
        None => {
            result.all_matched = false;
            for n in result.nets.iter_mut().filter(|n| n.tuned) {
                n.tuned = false;
                n.matched = false;
                n.length_after = n.length_before;
                n.new_traces.clear();
                n.skip_reason = Some("meanders would break a design rule here".into());
            }
        }
    }
    let drc = drc_report(&pcb, &accepted);
    Ok(json!({ "pcb": pcb, "result": result, "drc": drc }))
}

/// `char *vcad_pcb_length_match(const char *pcb_json, const char *request_json)`.
/// request `{"nets": [..], "target"?, "tolerance"?, "max_amplitude"?}`; with
/// no `nets`, returns `{"lengths": [[net, mm]]}` only.
#[no_mangle]
pub extern "C" fn vcad_pcb_length_match(
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_length_match", || {
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        if req.get("nets").is_none() {
            return Ok(json!({ "lengths": net_lengths(&pcb) }));
        }
        length_match(pcb, &req)
    })
}

// ---------------------------------------------------------------------------
// board ↔ enclosure
// ---------------------------------------------------------------------------

fn part_features(doc: &vcad_ir::Document, part: usize) -> Result<encl::EnclosureFeatures, String> {
    let scene = evaluate(doc)?;
    let p = scene
        .parts
        .get(part)
        .ok_or_else(|| format!("part {part} out of range ({} parts)", scene.parts.len()))?;
    let positions: Vec<f64> = p.mesh.positions.iter().map(|&v| v as f64).collect();
    Ok(encl::extract_enclosure_features(
        &positions,
        &p.mesh.indices,
    ))
}

fn board_outline(pcb: &Pcb) -> encl::BoardOutline {
    encl::BoardOutline {
        vertices: pcb
            .outline
            .vertices
            .iter()
            .map(|v| encl::Vec2 { x: v.x, y: v.y })
            .collect(),
        thickness: pcb.outline.thickness,
    }
}

/// Cross-domain fit: board vs the enclosure part's cavity, standoffs and wall
/// openings. Returns the kernel `EnclosureFitReport` plus the features found.
pub fn enclosure_fit(doc: &vcad_ir::Document, pcb: &Pcb, req: &Value) -> Result<Value, String> {
    let part = req.get("part").and_then(Value::as_u64).unwrap_or(0) as usize;
    let feats = part_features(doc, part)?;
    let cavity = feats
        .cavity
        .ok_or("the enclosure part has no open-top cavity (cavity check failed)")?;
    let lite: encl::PcbLite =
        serde_json::from_value(serde_json::to_value(pcb).map_err(|e| e.to_string())?)
            .map_err(|e| format!("pcb → fit view: {e}"))?;
    let outline = board_outline(pcb);
    let input = encl::EnclosureFitInput {
        mounting_holes: Some(encl::mounting_holes_from_pcb(&lite)),
        connectors: Some(encl::connectors_from_pcb(&lite, &outline)),
        outline,
        cavity,
        standoffs: Some(feats.standoffs.clone()),
        openings: Some(feats.openings.clone()),
        component_extents: None,
        placement: req
            .get("placement")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| format!("placement: {e}"))?,
        clearance: req.get("clearance").and_then(Value::as_f64),
        standoff_height: req.get("standoff_height").and_then(Value::as_f64),
        hole_tolerance: req.get("hole_tolerance").and_then(Value::as_f64),
    };
    let report = encl::check_enclosure_fit(&input);
    Ok(json!({ "report": report, "features": feats }))
}

/// `char *vcad_pcb_enclosure_fit(const char *doc_json, const char *pcb_json, const char *request_json)`.
/// request `{"part": index, "clearance"?, "placement"?}`.
#[no_mangle]
pub extern "C" fn vcad_pcb_enclosure_fit(
    doc_json: *const c_char,
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_enclosure_fit", || {
        let doc = parse_doc("doc", doc_json)?;
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        enclosure_fit(&doc, &pcb, &req)
    })
}

/// Seed a board from an enclosure part's cavity: inset outline + a mounting
/// hole over each standoff. Returns `{pcb, placement, derived}` — `pcb` is a
/// fresh two-layer board with that outline and NPTH mounting-hole footprints.
pub fn board_from_solid(doc: &vcad_ir::Document, req: &Value) -> Result<Value, String> {
    let part = req.get("part").and_then(Value::as_u64).unwrap_or(0) as usize;
    let feats = part_features(doc, part)?;
    let cavity = feats
        .cavity
        .ok_or("the part has no open-top cavity to fit a board into")?;
    let opts = encl::DeriveBoardOptions {
        clearance: req.get("clearance").and_then(Value::as_f64),
        thickness: req.get("thickness").and_then(Value::as_f64),
        hole_diameter: req.get("hole_diameter").and_then(Value::as_f64),
        standoff_height: None,
    };
    let derived = encl::derive_board_from_cavity(&cavity, &feats.standoffs, &opts);
    let verts: Vec<Value> = derived
        .outline
        .vertices
        .iter()
        .map(|v| json!({"x": v.x, "y": v.y}))
        .collect();
    let holes: Vec<Value> = derived
        .mounting_holes
        .iter()
        .enumerate()
        .map(|(i, h)| {
            json!({
                "ref": format!("H{}", i + 1), "value": "MountingHole",
                "footprintName": "MountingHole:MountingHole_NPTH",
                "position": {"x": h.x, "y": h.y}, "rotation": 0, "front": true,
                "pads": [{"number": "", "padType": "NPTH",
                          "shape": {"type": "Circle", "diameter": h.diameter},
                          "position": {"x": 0, "y": 0}, "drill": h.diameter, "layers": []}],
                "graphics": []
            })
        })
        .collect();
    let mut v = empty_board_json();
    v["outline"] = json!({"vertices": verts, "thickness": derived.outline.thickness});
    v["footprints"] = Value::Array(holes);
    // Round-trip so a footprint shape the IR rejects fails here, not later.
    let pcb: Pcb = serde_json::from_value(v).map_err(|e| format!("derived board: {e}"))?;
    Ok(json!({ "pcb": pcb, "placement": derived.placement, "derived": derived }))
}

/// `char *vcad_pcb_board_from_solid(const char *doc_json, const char *request_json)`.
#[no_mangle]
pub extern "C" fn vcad_pcb_board_from_solid(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_board_from_solid", || {
        let doc = parse_doc("doc", doc_json)?;
        let req = parse_request(request_json)?;
        board_from_solid(&doc, &req)
    })
}

/// A blank two-layer board (the app's "New PCB" default rules).
pub fn empty_board_json() -> Value {
    json!({
        "outline": {"vertices": [{"x":0,"y":0},{"x":80,"y":0},{"x":80,"y":50},{"x":0,"y":50}], "thickness": 1.6},
        "stackup": {"layers": [
            {"layer": "FCu", "copperThickness": 0.035, "dielectricThickness": 1.53},
            {"layer": "BCu", "copperThickness": 0.035}]},
        "nets": [], "footprints": [], "traces": [], "vias": [], "zones": [],
        "rules": {"defaultRules": {"name": "Default", "traceWidth": 0.25, "clearance": 0.2,
                                   "viaDiameter": 0.6, "viaDrill": 0.3},
                  "edgeClearance": 0.5, "holeToHole": 0.25, "minAnnularRing": 0.15, "minDrill": 0.3}
    })
}

fn next_id(doc: &Value) -> u64 {
    doc["nodes"]
        .as_object()
        .map(|m| {
            m.keys()
                .filter_map(|k| k.parse::<u64>().ok())
                .max()
                .unwrap_or(0)
                + 1
        })
        .unwrap_or(1)
}

fn add_node(doc: &mut Value, name: &str, op: Value) -> u64 {
    let id = next_id(doc);
    if !doc["nodes"].is_object() {
        doc["nodes"] = json!({});
    }
    doc["nodes"][id.to_string()] = json!({ "id": id, "name": name, "op": op });
    id
}

fn loop_segments(pts: &[[f64; 2]]) -> Vec<Value> {
    (0..pts.len())
        .map(|i| {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            json!({"type": "Line", "start": {"x": a[0], "y": a[1]}, "end": {"x": b[0], "y": b[1]}})
        })
        .collect()
}

fn signed_area(pts: &[[f64; 2]]) -> f64 {
    (0..pts.len())
        .map(|i| {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0
}

fn pts_of(vs: &[vcad_ir::Vec2]) -> Vec<[f64; 2]> {
    vs.iter().map(|v| [v.x, v.y]).collect()
}

/// Materialize the board as a solid part (outline extruded to thickness,
/// cutouts as real holes) into `doc`, bottom at `z` (default 0). Returns the
/// updated document.
pub fn solid_from_board(mut doc: Value, pcb: &Pcb, req: &Value) -> Result<Value, String> {
    let mut outer = pts_of(&pcb.outline.vertices);
    if outer.len() < 3 {
        return Err("board outline needs >= 3 vertices".into());
    }
    if pcb.outline.thickness.is_nan() || pcb.outline.thickness <= 0.0 {
        return Err("board thickness must be > 0".into());
    }
    if signed_area(&outer) < 0.0 {
        outer.reverse();
    }
    let name = req.get("name").and_then(Value::as_str).unwrap_or("board");
    let dx = req.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let dy = req.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let z = req.get("z").and_then(Value::as_f64).unwrap_or(0.0);
    let holes: Vec<Value> = pcb
        .outline
        .cutouts
        .iter()
        .map(|c| Value::Array(loop_segments(&pts_of(c))))
        .collect();
    let mut sketch = json!({
        "type": "Sketch2D",
        "origin": {"x": dx, "y": dy, "z": z},
        "x_dir": {"x": 1, "y": 0, "z": 0},
        "y_dir": {"x": 0, "y": 1, "z": 0},
        "segments": loop_segments(&outer),
    });
    if !holes.is_empty() {
        sketch["holes"] = Value::Array(holes);
    }
    let sk = add_node(&mut doc, &format!("{name} profile"), sketch);
    let ex = add_node(
        &mut doc,
        name,
        json!({"type": "Extrude", "sketch": sk, "direction": {"x": 0, "y": 0, "z": pcb.outline.thickness}}),
    );
    if !doc["materials"].is_object() {
        doc["materials"] = json!({});
    }
    let key = format!("pcb:{name}");
    doc["materials"][&key] = json!({
        "name": key, "color": [0.05, 0.35, 0.18], "metallic": 0.1, "roughness": 0.7, "density": 1900.0
    });
    if !doc["roots"].is_array() {
        doc["roots"] = json!([]);
    }
    doc["roots"]
        .as_array_mut()
        .unwrap()
        .push(json!({"root": ex, "material": key}));
    // Validate: the result must still parse as a document.
    vcad_ir::Document::from_json(&doc.to_string()).map_err(|e| format!("result document: {e}"))?;
    Ok(doc)
}

/// `char *vcad_pcb_solid_from_board(const char *doc_json, const char *pcb_json, const char *request_json)`.
/// request `{"name"?, "x"?, "y"?, "z"?}` → updated document JSON.
#[no_mangle]
pub extern "C" fn vcad_pcb_solid_from_board(
    doc_json: *const c_char,
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_solid_from_board", || {
        let text = borrow_str("doc", doc_json).ok_or("doc: bad string")?;
        let doc: Value = serde_json::from_str(text).map_err(|e| format!("doc: {e}"))?;
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        solid_from_board(doc, &pcb, &req)
    })
}

/// Generate an open-top enclosure sized to the board (outline AABB plus
/// clearance and wall), its pocket floor at z = 0 and the board resting on it. Adds the
/// part to `doc` and returns the updated document plus the board placement
/// (board-local → enclosure frame) the fit check should use.
pub fn enclosure_for_board(mut doc: Value, pcb: &Pcb, req: &Value) -> Result<Value, String> {
    let pts = pts_of(&pcb.outline.vertices);
    if pts.len() < 3 {
        return Err("board outline needs >= 3 vertices".into());
    }
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in &pts {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let clearance = req.get("clearance").and_then(Value::as_f64).unwrap_or(1.5);
    let wall = req.get("wall").and_then(Value::as_f64).unwrap_or(2.0);
    let floor = req.get("floor").and_then(Value::as_f64).unwrap_or(2.0);
    let depth = req.get("depth").and_then(Value::as_f64).unwrap_or(15.0);
    let name = req
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("enclosure");
    let (w, h) = (
        hi[0] - lo[0] + 2.0 * clearance,
        hi[1] - lo[1] + 2.0 * clearance,
    );
    let outer = add_node(
        &mut doc,
        &format!("{name} shell"),
        json!({"type": "Cube", "size": {"x": w + 2.0 * wall, "y": h + 2.0 * wall, "z": depth + floor}}),
    );
    let outer_t = add_node(
        &mut doc,
        &format!("{name} shell placed"),
        json!({"type": "Translate", "child": outer, "offset": {"x": -wall, "y": -wall, "z": -floor}}),
    );
    let pocket = add_node(
        &mut doc,
        &format!("{name} pocket"),
        json!({"type": "Cube", "size": {"x": w, "y": h, "z": depth + 1.0}}),
    );
    let diff = add_node(
        &mut doc,
        name,
        json!({"type": "Difference", "left": outer_t, "right": pocket}),
    );
    if !doc["roots"].is_array() {
        doc["roots"] = json!([]);
    }
    doc["roots"]
        .as_array_mut()
        .unwrap()
        .push(json!({"root": diff, "material": "default"}));
    vcad_ir::Document::from_json(&doc.to_string()).map_err(|e| format!("result document: {e}"))?;
    // Board-local (lo) maps to (clearance, clearance) on the pocket floor.
    let placement = json!({"offset": {"x": clearance - lo[0], "y": clearance - lo[1], "z": 0.0}, "rotationDeg": 0.0});
    let part = doc["roots"].as_array().map(|r| r.len() - 1).unwrap_or(0);
    Ok(json!({ "document": doc, "placement": placement, "part": part }))
}

/// `char *vcad_pcb_enclosure_for_board(const char *doc_json, const char *pcb_json, const char *request_json)`.
#[no_mangle]
pub extern "C" fn vcad_pcb_enclosure_for_board(
    doc_json: *const c_char,
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_enclosure_for_board", || {
        let text = borrow_str("doc", doc_json).ok_or("doc: bad string")?;
        let doc: Value = serde_json::from_str(text).map_err(|e| format!("doc: {e}"))?;
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        enclosure_for_board(doc, &pcb, &req)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ice_viz() -> Pcb {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/ice-viz/fab/ice-viz.kicad_pcb");
        let v = import_board(&p).expect("import ice-viz");
        assert!(v["footprints"].as_u64().unwrap() > 0);
        serde_json::from_value(v["pcb"].clone()).unwrap()
    }

    fn empty_doc() -> Value {
        json!({"version": "0.1", "nodes": {}, "materials": {}, "part_materials": {}, "roots": []})
    }

    /// The launch gate, end to end through the FFI functions the app calls:
    /// import a KiCad board, enclose it, fit-check, DRC to zero
    /// route-attributable, export Gerbers.
    #[test]
    fn kicad_board_to_gerbers_gate() {
        let pcb = ice_viz();

        // Enclosure around the board, then the cross-domain fit check.
        let enc = enclosure_for_board(empty_doc(), &pcb, &json!({})).unwrap();
        let doc = vcad_ir::Document::from_json(&enc["document"].to_string()).unwrap();
        // Auto-fit placement (centered on the detected cavity).
        let fit = enclosure_fit(&doc, &pcb, &json!({"part": enc["part"]})).unwrap();
        assert_eq!(fit["report"]["ok"], json!(true), "fit: {}", fit["report"]);

        // The board as a solid part lands in the same document.
        let with_board = solid_from_board(enc["document"].clone(), &pcb, &json!({})).unwrap();
        assert_eq!(with_board["roots"].as_array().unwrap().len(), 2);

        // DRC → fix → zero route-attributable.
        let before = drc_report(&pcb, &BTreeSet::new());
        let fixed = if before.clean {
            pcb.clone()
        } else {
            let r = fix_drc(pcb.clone(), &json!({}));
            serde_json::from_value(r["pcb"].clone()).unwrap()
        };
        let after = drc_report(&fixed, &BTreeSet::new());
        assert!(
            after.clean,
            "route-attributable after fix: {}",
            after.route_attributable
        );
        eprintln!(
            "GATE fit={} drc before: total {} attributable {} | after: total {} attributable {} floor {}",
            fit["report"]["summary"], before.total, before.route_attributable,
            after.total, after.route_attributable, after.baseline_total
        );

        // Export (gated).
        let dir = std::env::temp_dir().join(format!("vcad-ffi-gerber-{}", std::process::id()));
        // The ice-viz fixture is geometrically clean but not fully connected
        // under the kernel DRC (13 open nets, incl. GND/VBUS), so the default
        // export must refuse it; the explicit opt-in still ships.
        if after.unrouted > 0 {
            let e = export_gerbers(&fixed, &dir, &json!({})).unwrap_err();
            assert!(e.contains("NOT FULLY ROUTED"), "{e}");
            assert!(!dir.exists());
        }
        let out = export_gerbers(&fixed, &dir, &json!({"allow_incomplete": true})).unwrap();
        let files: Vec<String> = serde_json::from_value(out["files"].clone()).unwrap();
        assert!(
            files
                .iter()
                .any(|f| f.ends_with(".gbr") || f.contains("Cu")),
            "{files:?}"
        );
        assert!(files.iter().any(|f| f.ends_with(".drl")), "{files:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A routing-introduced short must block export and write nothing.
    #[test]
    fn export_refuses_dirty_board() {
        let mut pcb = ice_viz();
        let a = pcb
            .traces
            .iter()
            .find(|t| !t.net.is_empty())
            .cloned()
            .expect("a trace");
        let other = pcb
            .traces
            .iter()
            .find(|t| t.net != a.net && t.layer == a.layer)
            .cloned()
            .unwrap();
        let mut bridge = a.clone();
        bridge.end = other.start;
        pcb.traces.push(bridge);
        let r = drc_report(&pcb, &BTreeSet::new());
        assert!(r.route_attributable > 0);
        let dir =
            std::env::temp_dir().join(format!("vcad-ffi-gerber-dirty-{}", std::process::id()));
        let e = export_gerbers(&pcb, &dir, &json!({})).unwrap_err();
        assert!(e.starts_with("refused"), "{e}");
        assert!(!dir.exists());
    }

    /// The app's "unrouted" count used to match the substring "unrouted"
    /// against Debug rule names (`UnconnectedNet`), so it read 0 on every
    /// board. It must count the nets that are actually open — and export must
    /// refuse a board with any.
    #[test]
    fn unrouted_counts_open_nets_and_blocks_export() {
        use vcad_ir::ecad::{Footprint, Pad, PadShape, PadType, PcbLayer};
        let mut pcb: Pcb = serde_json::from_value(empty_board_json()).unwrap();
        let pad = |n: &str, x: f64| Pad {
            number: n.into(),
            pad_type: PadType::SMD,
            shape: PadShape::Rect {
                width: 0.8,
                height: 0.8,
            },
            position: vcad_ir::Vec2::new(x, 0.0),
            rotation: 0.0,
            drill: None,
            net: Some("SIG".into()),
            layers: vec![PcbLayer::FCu],
        };
        pcb.footprints.push(Footprint {
            reference: "R1".into(),
            value: "1k".into(),
            footprint_name: "R".into(),
            position: vcad_ir::Vec2::new(20.0, 20.0),
            rotation: 0.0,
            front: true,
            pads: vec![pad("1", 0.0), pad("2", 10.0)],
            graphics: vec![],
            model_3d: None,
            properties: Default::default(),
        });
        let r = drc_report(&pcb, &BTreeSet::new());
        assert_eq!(r.unrouted, 1, "{:?}", r.findings);
        assert_eq!(r.open_nets, vec!["SIG".to_string()]);
        let dir = std::env::temp_dir().join(format!("vcad-ffi-open-{}", std::process::id()));
        let e = export_gerbers(&pcb, &dir, &json!({})).unwrap_err();
        assert!(e.contains("NOT FULLY ROUTED"), "{e}");
        assert!(!dir.exists());

        // Autoroute's status now carries the true remaining count.
        let routed = autoroute(pcb, &json!({}));
        assert_eq!(routed["drc"]["unrouted"], json!(0), "{}", routed["verdict"]);
    }

    #[test]
    fn board_from_enclosure_round_trips() {
        let blank: Pcb = serde_json::from_value(empty_board_json()).unwrap();
        let enc = enclosure_for_board(empty_doc(), &blank, &json!({"clearance": 1.0})).unwrap();
        let doc = vcad_ir::Document::from_json(&enc["document"].to_string()).unwrap();
        let r = board_from_solid(&doc, &json!({"part": enc["part"], "clearance": 0.5})).unwrap();
        let derived: Pcb = serde_json::from_value(r["pcb"].clone()).unwrap();
        let xs: Vec<f64> = derived.outline.vertices.iter().map(|v| v.x).collect();
        let w = xs.iter().cloned().fold(f64::MIN, f64::max)
            - xs.iter().cloned().fold(f64::MAX, f64::min);
        // pocket = 80 + 2·1.0 = 82, inset 0.5 each side → 81.
        assert!((w - 81.0).abs() < 1.0, "derived width {w}");
    }

    #[test]
    fn length_match_reports_lengths() {
        let pcb = ice_viz();
        let l = net_lengths(&pcb);
        assert!(!l.is_empty() && l[0].1 > 0.0);
        let nets = vec![l[l.len() - 1].0.clone(), l[l.len() / 2].0.clone()];
        let r = length_match(pcb, &json!({"nets": nets, "tolerance": 0.5})).unwrap();
        assert!(r["result"]["nets"].as_array().unwrap().len() == 2);
    }
}

// ---------------------------------------------------------------------------
// Interactive routing
// ---------------------------------------------------------------------------

/// An open board answering live routing queries.
pub struct VcadPcbRouter(std::sync::Mutex<vcad_ecad_fabprep::verdict::InteractiveRouter>);

/// Index a board for interactive routing. Free with [`vcad_pcb_router_free`].
#[no_mangle]
pub extern "C" fn vcad_pcb_router_open(pcb_json: *const c_char) -> *mut VcadPcbRouter {
    match parse_pcb(pcb_json) {
        Ok(pcb) => Box::into_raw(Box::new(VcadPcbRouter(std::sync::Mutex::new(
            vcad_ecad_fabprep::verdict::InteractiveRouter::new(pcb),
        )))),
        Err(e) => {
            err::set_error(format!("vcad_pcb_router_open: {e}"));
            ptr::null_mut()
        }
    }
}

/// Free a router from [`vcad_pcb_router_open`].
#[no_mangle]
pub extern "C" fn vcad_pcb_router_free(router: *mut VcadPcbRouter) {
    if !router.is_null() {
        drop(unsafe { Box::from_raw(router) });
    }
}

#[derive(serde::Deserialize)]
struct RouteQuery {
    net: String,
    from: vcad_ir::Vec2,
    to: vcad_ir::Vec2,
    layer: Option<vcad_ir::ecad::PcbLayer>,
    budget: Option<usize>,
}

#[derive(serde::Deserialize)]
struct RouteCommit {
    net: String,
    #[serde(default)]
    moved: Vec<vcad_ecad_fabprep::verdict::TraceMove>,
    path: Vec<(vcad_ir::Vec2, vcad_ir::Vec2, vcad_ir::ecad::PcbLayer)>,
}

/// Route `{net, from, to, layer?, budget?}`; `{status, reason, path, vias, width}`.
#[no_mangle]
pub extern "C" fn vcad_pcb_router_route(
    router: *const VcadPcbRouter,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_router_route", || {
        if router.is_null() {
            return Err("null router".to_string());
        }
        let q: RouteQuery =
            serde_json::from_str(borrow_str("request", request_json).ok_or("bad request")?)
                .map_err(|e| e.to_string())?;
        let r = unsafe { &*router }
            .0
            .lock()
            .map_err(|_| "router poisoned")?;
        Ok(r.route(&q.net, q.from, q.to, q.layer, q.budget.unwrap_or(150_000)))
    })
}

/// Commit `{net, path}` (a `route` result's path); `{traces, vias}` to append.
#[no_mangle]
pub extern "C" fn vcad_pcb_router_commit(
    router: *const VcadPcbRouter,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_router_commit", || {
        if router.is_null() {
            return Err("null router".to_string());
        }
        let q: RouteCommit =
            serde_json::from_str(borrow_str("request", request_json).ok_or("bad request")?)
                .map_err(|e| e.to_string())?;
        let mut r = unsafe { &*router }
            .0
            .lock()
            .map_err(|_| "router poisoned")?;
        let (traces, vias) = r.commit(&q.net, &q.path, &q.moved)?;
        Ok(json!({ "traces": traces, "vias": vias, "moved": q.moved }))
    })
}

/// The ratsnest: `[{net, from, to}]`, one per connection still needed to join
/// each net's copper islands — the DRC's own connectivity, so the airwires and
/// the "open nets" count can never disagree.
#[no_mangle]
pub extern "C" fn vcad_pcb_ratsnest(pcb_json: *const c_char) -> *mut c_char {
    run("vcad_pcb_ratsnest", || {
        let pcb = parse_pcb(pcb_json)?;
        let layers: Vec<_> = pcb
            .stackup
            .layers
            .iter()
            .map(|l| l.layer)
            .filter(|l| l.is_copper())
            .collect();
        let rats =
            vcad_ecad_pcb::router::islands::island_connections(&pcb, &layers, &BTreeSet::new());
        let wires: Vec<Value> = rats
            .iter()
            .map(|r| json!({ "net": r.net, "from": r.from, "to": r.to }))
            .collect();
        Ok(json!({ "wires": wires }))
    })
}

/// Filled copper for every zone: `{zones: [{polygons, net, layer}]}`.
#[no_mangle]
pub extern "C" fn vcad_pcb_fill_zones(pcb_json: *const c_char) -> *mut c_char {
    run("vcad_pcb_fill_zones", || {
        let pcb = parse_pcb(pcb_json)?;
        Ok(json!({ "zones": vcad_ecad_pcb::copper_pour::fill_zones(&pcb) }))
    })
}

/// A KiCad library footprint ready to place: `{footprint}` for
/// `{id: "Library:Name", ref, position: {x,y}, copper: ["FCu", ...]}`.
#[no_mangle]
pub extern "C" fn vcad_pcb_library_footprint(request_json: *const c_char) -> *mut c_char {
    run("vcad_pcb_library_footprint", || {
        #[derive(serde::Deserialize)]
        struct Req {
            id: String,
            #[serde(rename = "ref")]
            reference: String,
            position: vcad_ir::Vec2,
            copper: Vec<vcad_ir::ecad::PcbLayer>,
        }
        let q: Req =
            serde_json::from_str(borrow_str("request", request_json).ok_or("bad request")?)
                .map_err(|e| e.to_string())?;
        let def = vcad_ecad_symbols::kicad_board::load_footprint(&q.id)?;
        let fp = vcad_ecad_symbols::kicad_board::place_footprint(
            &def,
            &q.id,
            &q.reference,
            "",
            q.position,
            0.0,
            &q.copper,
        );
        Ok(json!({ "footprint": fp }))
    })
}

/// Route a declared differential pair `{p, n}`: `{pcb, routed, reason}`.
/// The router goes straight, so a result that adds any route-attributable
/// violation is thrown away and the board returned untouched.
#[no_mangle]
pub extern "C" fn vcad_pcb_route_diff_pair(
    pcb_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_pcb_route_diff_pair", || {
        let pcb = parse_pcb(pcb_json)?;
        let req = parse_request(request_json)?;
        let net = |k: &str| {
            req.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or(format!("missing {k}"))
        };
        let (p, n) = (net("p")?, net("n")?);
        let accepted = accepted_rules(&req);
        let before = drc_report(&pcb, &accepted).route_attributable;
        // Coupled and straight when the channel is clear; otherwise each leg
        // finds its own legal way around what's there.
        let mut out = pcb.clone();
        let mut coupled = false;
        if let Some((rp, rn)) = vcad_ecad_pcb::router::diff_pair::route_diff_pair(&pcb, &p, &n) {
            let layer = vcad_ir::ecad::PcbLayer::FCu;
            let width = out.rules.default_rules.trace_width;
            for r in [&rp, &rn] {
                for (a, b) in &r.segments {
                    out.traces.push(vcad_ir::ecad::Trace {
                        start: *a,
                        end: *b,
                        width,
                        layer,
                        net: r.net.clone(),
                        source: None,
                    });
                }
            }
            coupled = drc_report(&out, &accepted).route_attributable <= before;
        }
        if !coupled {
            out = pcb.clone();
            if !vcad_ecad_fabprep::verdict::route_nets(&mut out, &[p.clone(), n.clone()], 150_000)
                || drc_report(&out, &accepted).route_attributable > before
            {
                return Ok(
                    json!({ "pcb": pcb, "routed": false, "reason": "No legal route for this pair." }),
                );
            }
        }
        Ok(json!({ "pcb": out, "routed": true, "coupled": coupled, "reason": null }))
    })
}

/// A KiCad library symbol's pins and default footprint:
/// `{pins, footprint}` for `{id: "Library:Symbol"}`.
#[no_mangle]
pub extern "C" fn vcad_pcb_library_symbol(request_json: *const c_char) -> *mut c_char {
    run("vcad_pcb_library_symbol", || {
        let req: Value =
            serde_json::from_str(borrow_str("request", request_json).ok_or("bad request")?)
                .map_err(|e| e.to_string())?;
        let id = req.get("id").and_then(Value::as_str).ok_or("missing id")?;
        let (lib, name) = id.split_once(':').ok_or("id must be Library:Symbol")?;
        let dirs = [
            "/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols",
            "/usr/share/kicad/symbols",
            "/usr/local/share/kicad/symbols",
        ];
        let path = dirs
            .iter()
            .map(|d| Path::new(d).join(format!("{lib}.kicad_sym")))
            .find(|p| p.exists())
            .ok_or_else(|| format!("symbol library {lib} not found"))?;
        let parsed = vcad_ecad_symbols::kicad_sym::parse_symbol_lib_file(&path.to_string_lossy())
            .map_err(|e| e.to_string())?;
        let sym = parsed
            .get(name)
            .ok_or_else(|| format!("{name} not in {lib}"))?;
        let footprint = sym
            .properties
            .iter()
            .find(|p| p.key == "Footprint")
            .map(|p| p.value.clone())
            .unwrap_or_default();
        let pins: Vec<Value> = sym
            .pins
            .iter()
            .map(|p| json!({ "number": p.number, "name": p.name, "pin_type": p.pin_type, "position": { "x": p.position.0, "y": -p.position.1 } }))
            .collect();
        Ok(json!({ "pins": pins, "footprint": footprint }))
    })
}
