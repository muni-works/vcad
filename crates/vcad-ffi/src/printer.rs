//! Printing from the Mac app, end to end: orientation search, job prep
//! (the same centre → slice → lint → printcheck → `.gcode.3mf` pipeline as
//! `vcad print`), printer discovery and pairing, live status, the fail-closed
//! gated send, and pause / resume / stop. Ops are dispatched from
//! `vcad_print_call` (see `print.rs`).
//!
//! Every send goes through `vcad_slicer_bambu::link::print_gated`, which only
//! uploads after the gate issues a ticket, and the gate needs
//! `"confirm": true` in the request — the app sets it from its confirm sheet
//! and nowhere else.
//!
//! The printer named [`DRY_RUN`] is an in-process simulated P1S (no network)
//! used by tests and previews: it validates uploads against the gate ticket
//! like a real printer and advances progress on each status read.

use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use vcad_ir::Document;
use vcad_kernel_tessellate::TriangleMesh;
use vcad_slicer::orient;
use vcad_slicer_bambu::config::{self, PrinterEntry};
use vcad_slicer_bambu::gate::{GateInputs, PrintTicket};
use vcad_slicer_bambu::job::{self, JobSpec};
use vcad_slicer_bambu::link::{self, Control, FakePrinter, PrinterLink};
use vcad_slicer_bambu::{AmsSlot, AmsStatus, AmsUnit, BambuError, PrintState, PrinterStatus};
use vcad_slicer_gcode::GcodeSettings;

use crate::print::{bed_mesh, printer, settings_of, Settings};

/// Name of the simulated printer.
pub(crate) const DRY_RUN: &str = "dry-run";

// ---------------------------------------------------------------- dry run

struct DryRun {
    fake: FakePrinter,
    /// Percent added per status read while printing.
    step: f64,
    /// AMS mapping of the last started job (for tests).
    last_ams: Vec<u32>,
}

fn dry_status() -> PrinterStatus {
    let slot = |id: u8, kind: Option<&str>, color: &str, remaining: u8| AmsSlot {
        id,
        filament_type: kind.map(str::to_string),
        color: Some(color.to_string()),
        remaining: Some(remaining),
    };
    PrinterStatus {
        state: PrintState::Idle,
        nozzle_temp: 27.0,
        bed_temp: 26.0,
        firmware_version: Some("01.08.00.00 (dry run)".into()),
        loaded_filament: Some("PLA".into()),
        ams_status: Some(AmsStatus {
            units: vec![AmsUnit {
                id: 0,
                slots: vec![
                    slot(0, Some("PLA"), "1B1B1DFF", 82),
                    slot(1, Some("PETG"), "E5484DFF", 40),
                    slot(2, Some("PLA"), "ECECECFF", 12),
                    slot(3, None, "00000000", 0),
                ],
                humidity: Some(3),
                temperature: Some(24.0),
            }],
            active_unit: Some(0),
            active_slot: Some(0),
        }),
        ..Default::default()
    }
}

fn fresh_dry_run() -> DryRun {
    DryRun {
        fake: FakePrinter {
            status: dry_status(),
            jpeg: None,
            ..Default::default()
        },
        step: 25.0,
        last_ams: Vec::new(),
    }
}

static DRY: Mutex<Option<DryRun>> = Mutex::new(None);

fn with_dry<T>(f: impl FnOnce(&mut DryRun) -> T) -> T {
    let mut g = DRY.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(fresh_dry_run))
}

/// [`PrinterLink`] over the process-wide simulated printer.
struct DryRunLink;

impl PrinterLink for DryRunLink {
    fn status(&mut self) -> vcad_slicer_bambu::Result<PrinterStatus> {
        with_dry(|d| {
            let st = &mut d.fake.status;
            if st.state == PrintState::Printing {
                st.progress_percent = (st.progress_percent + d.step).min(100.0);
                st.layer_current =
                    ((st.progress_percent / 100.0) * st.layer_total as f64).round() as u32;
                st.time_remaining_min =
                    ((100.0 - st.progress_percent) / 100.0 * 41.0).round() as u32;
                if st.progress_percent >= 100.0 {
                    st.state = PrintState::Finished;
                    st.nozzle_target = 0.0;
                    st.bed_target = 0.0;
                }
            }
            d.fake.status()
        })
    }
    fn camera_jpeg(&mut self) -> vcad_slicer_bambu::Result<Vec<u8>> {
        with_dry(|d| d.fake.camera_jpeg())
    }
    fn upload(&mut self, name: &str, data: &[u8]) -> vcad_slicer_bambu::Result<()> {
        with_dry(|d| d.fake.upload(name, data))
    }
    fn file_size(&mut self, name: &str) -> vcad_slicer_bambu::Result<Option<u64>> {
        with_dry(|d| d.fake.file_size(name))
    }
    fn delete(&mut self, name: &str) -> vcad_slicer_bambu::Result<()> {
        with_dry(|d| d.fake.delete(name))
    }
    fn start_print(
        &mut self,
        name: &str,
        ticket: &PrintTicket,
        ams: &[u32],
    ) -> vcad_slicer_bambu::Result<()> {
        with_dry(|d| {
            d.fake.start_print(name, ticket, ams)?;
            d.last_ams = ams.to_vec();
            let st = &mut d.fake.status;
            st.state = PrintState::Printing;
            st.filename = Some(name.to_string());
            st.progress_percent = 0.0;
            st.layer_current = 0;
            st.layer_total = st.layer_total.max(1);
            st.nozzle_target = 220.0;
            st.bed_target = 55.0;
            st.nozzle_temp = 220.0;
            st.bed_temp = 55.0;
            Ok(())
        })
    }
    fn control(&mut self, action: Control) -> vcad_slicer_bambu::Result<()> {
        with_dry(|d| {
            d.fake.control(action)?;
            let st = &mut d.fake.status;
            match (action, &st.state) {
                (Control::Pause, PrintState::Printing) => st.state = PrintState::Paused,
                (Control::Resume, PrintState::Paused) => st.state = PrintState::Printing,
                (Control::Stop, PrintState::Printing | PrintState::Paused) => {
                    st.state = PrintState::Idle;
                    st.progress_percent = 0.0;
                    st.layer_current = 0;
                    st.filename = None;
                }
                (a, s) => {
                    return Err(BambuError::PrintError(format!(
                        "cannot {a:?} while printer is {s:?}"
                    )))
                }
            }
            Ok(())
        })
    }
}

/// Reset the simulated printer; optional overrides for tests.
fn dry_run_reset(req: &Value) -> Result<Value, String> {
    let mut d = fresh_dry_run();
    if let Some(f) = req.get("loadedFilament") {
        d.fake.status.loaded_filament = f.as_str().map(str::to_string);
    }
    if let Some(s) = req.get("step").and_then(Value::as_f64) {
        d.step = s;
    }
    if req.get("offline") == Some(&json!(true)) {
        d.fake.offline = true;
    }
    if req.get("busy") == Some(&json!(true)) {
        d.fake.status.state = PrintState::Printing;
    }
    *DRY.lock().unwrap_or_else(|e| e.into_inner()) = Some(d);
    Ok(json!({"ok": true}))
}

/// What the simulated printer was asked to do (tests).
fn dry_run_log() -> Value {
    with_dry(|d| {
        let calls: Vec<String> = d.fake.calls.iter().map(|c| format!("{c:?}")).collect();
        json!({"calls": calls, "printed": d.fake.printed(), "uploaded": d.fake.uploaded(),
               "ams": d.last_ams, "files": d.fake.files.keys().collect::<Vec<_>>()})
    })
}

// ---------------------------------------------------------------- printers

fn printers_path() -> Result<std::path::PathBuf, String> {
    config::default_path().ok_or_else(|| "no home directory for printers.toml".to_string())
}

fn saved_printers() -> Result<Vec<PrinterEntry>, String> {
    let path = printers_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    config::load(&path).map_err(|e| e.to_string())
}

/// A connected printer: the link plus the Bambu model id the gate checks.
fn open(name: &str) -> Result<(Box<dyn PrinterLink>, Option<&'static str>), String> {
    if name == DRY_RUN {
        return Ok((Box::new(DryRunLink), Some("C12")));
    }
    let entry = saved_printers()?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("no saved printer named `{name}`"))?;
    let model = entry.model_id();
    let lan = link::LanPrinter::new(entry).map_err(|e| e.to_string())?;
    Ok((Box::new(lan), model))
}

fn printer_name(req: &Value) -> Result<&str, String> {
    req.get("printer")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing `printer`".to_string())
}

fn list_printers(req: &Value) -> Result<Value, String> {
    let mut out: Vec<Value> = saved_printers()?
        .iter()
        .map(|p| json!({"name": p.name, "model": p.model, "ip": p.ip.to_string(), "serial": p.serial}))
        .collect();
    if req.get("includeDryRun") == Some(&json!(true)) {
        out.push(json!({"name": DRY_RUN, "model": "P1S", "ip": "simulated", "serial": "DRYRUN"}));
    }
    Ok(json!({ "printers": out }))
}

fn discover(req: &Value) -> Result<Value, String> {
    let ms = req
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(3000)
        .min(15000);
    let found = vcad_slicer_bambu::discover_printers(Duration::from_millis(ms))
        .map_err(|e| e.to_string())?;
    let saved: Vec<String> = saved_printers()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.serial)
        .collect();
    let list: Vec<Value> = found
        .iter()
        .map(|p| {
            json!({"ip": p.ip.to_string(), "serial": p.serial, "model": p.model,
                   "name": p.name, "firmware": p.firmware, "saved": saved.contains(&p.serial)})
        })
        .collect();
    Ok(json!({ "found": list }))
}

fn add_printer(req: &Value) -> Result<Value, String> {
    let s = |k: &str| {
        req.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let (name, model, ip, serial, code) = (
        s("name"),
        s("model"),
        s("ip"),
        s("serial"),
        s("access_code"),
    );
    if name.is_empty() || name == DRY_RUN {
        return Err("printer needs a name".into());
    }
    let ip: IpAddr = ip
        .parse()
        .map_err(|_| format!("`{ip}` is not an IP address"))?;
    if serial.is_empty() {
        return Err("printer needs its serial number".into());
    }
    if code.len() != 8 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("the access code is 8 characters (printer: Settings ▸ Network)".into());
    }
    let entry = PrinterEntry {
        name: name.clone(),
        model: if model.is_empty() {
            "P1S".into()
        } else {
            model
        },
        ip,
        serial,
        access_code: code,
    };
    config::save_entry(&printers_path()?, entry).map_err(|e| e.to_string())?;
    Ok(json!({"saved": name}))
}

fn hex_color(c: &Option<String>) -> Option<String> {
    c.as_ref()
        .filter(|s| s.len() >= 6)
        .map(|s| format!("#{}", &s[..6]))
}

/// Status as the app shows it: state, temps, progress, AMS slots.
fn status_json(st: &PrinterStatus) -> Value {
    let state = match &st.state {
        PrintState::Idle => "idle".to_string(),
        PrintState::Printing => "printing".to_string(),
        PrintState::Paused => "paused".to_string(),
        PrintState::Finished => "finished".to_string(),
        PrintState::Preparing => "preparing".to_string(),
        PrintState::Error(e) => format!("error: {e}"),
        PrintState::Unknown => "unknown".to_string(),
    };
    let slots: Vec<Value> = st
        .ams_status
        .iter()
        .flat_map(|a| a.units.iter())
        .flat_map(|u| {
            u.slots.iter().map(move |s| {
                json!({"tray": u.id as u32 * 4 + s.id as u32, "unit": u.id, "slot": s.id,
                       "type": s.filament_type, "color": hex_color(&s.color),
                       "remaining": s.remaining})
            })
        })
        .collect();
    json!({
        "state": state,
        "ready": matches!(st.state, PrintState::Idle | PrintState::Finished),
        "progress": st.progress_percent,
        "layer": st.layer_current, "layers": st.layer_total,
        "remainingMin": st.time_remaining_min,
        "nozzle": st.nozzle_temp, "nozzleTarget": st.nozzle_target,
        "bed": st.bed_temp, "bedTarget": st.bed_target,
        "file": st.filename, "firmware": st.firmware_version,
        "loadedFilament": st.loaded_filament,
        "ams": slots,
    })
}

fn printer_status(req: &Value) -> Result<Value, String> {
    let (mut link, model) = open(printer_name(req)?)?;
    let st = link.status().map_err(|e| e.to_string())?;
    let mut out = status_json(&st);
    out["modelId"] = json!(model);
    Ok(out)
}

fn camera(req: &Value) -> Result<Value, String> {
    let path = req.get("path").and_then(Value::as_str).unwrap_or("");
    if path.is_empty() {
        return Err("camera: missing `path`".into());
    }
    let (mut link, _) = open(printer_name(req)?)?;
    let jpeg = link.camera_jpeg().map_err(|e| e.to_string())?;
    std::fs::write(path, &jpeg).map_err(|e| format!("write {path}: {e}"))?;
    Ok(json!({"path": path, "bytes": jpeg.len()}))
}

fn control(req: &Value) -> Result<Value, String> {
    let action = req
        .get("action")
        .and_then(Value::as_str)
        .and_then(Control::parse)
        .ok_or("control: `action` must be pause, resume or stop")?;
    let (mut link, _) = open(printer_name(req)?)?;
    link.control(action).map_err(|e| e.to_string())?;
    Ok(json!({"ok": true}))
}

// ---------------------------------------------------------------- jobs

pub(crate) fn rotation_of(req: &Value) -> Result<Option<[[f64; 3]; 3]>, String> {
    match req.get("rotation") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let r: [[f64; 3]; 3] =
                serde_json::from_value(v.clone()).map_err(|e| format!("rotation: {e}"))?;
            if r.iter().flatten().any(|x| !x.is_finite()) {
                return Err("rotation: not finite".into());
            }
            Ok(Some(r))
        }
    }
}

/// Bambu model id for a slicer printer profile (`None` = gate refuses).
fn model_id_for_profile(name: &str) -> Option<String> {
    let short = name.trim_start_matches("Bambu Lab").trim();
    config::model_id_for(short).map(str::to_string)
}

fn spec(settings: &Settings, name: &str) -> JobSpec {
    let profile = printer(&settings.printer);
    JobSpec {
        name: name.to_string(),
        slice: settings.slice_settings(),
        printer_model_id: model_id_for_profile(&profile.name),
        gcode: GcodeSettings {
            printer: profile,
            print_temp: settings.print_temp,
            bed_temp: settings.bed_temp,
            ..Default::default()
        },
        filament_type: settings.material.to_ascii_uppercase(),
    }
}

fn posed_mesh(doc: &Document, req: &Value) -> Result<TriangleMesh, String> {
    let mut mesh = bed_mesh(doc, None)?;
    if let Some(r) = rotation_of(req)? {
        orient::rotate_mesh(&mut mesh, &r);
    }
    Ok(mesh)
}

fn job_name(req: &Value) -> String {
    let raw = req.get("name").and_then(Value::as_str).unwrap_or("part");
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if clean.is_empty() {
        "part".into()
    } else {
        clean
    }
}

fn findings(r: &vcad_printcheck::Report) -> Vec<Value> {
    r.failures()
        .map(|f| json!({"kind": format!("{:?}", f.kind), "message": f.message}))
        .collect()
}

/// Orientation search over the whole document (as it would sit on the plate).
fn orient_op(doc: &Document, req: &Value) -> Result<Value, String> {
    let settings = settings_of(req)?;
    let mesh = bed_mesh(doc, None)?;
    let max = req
        .get("max")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 8) as usize;
    let poses = orient::rank_orientations(&mesh, settings.support_angle, max);
    // Which poses' print time to estimate (slicing is the expensive part).
    let estimate = req.get("estimate") != Some(&json!(false));
    let mut out = Vec::new();
    for p in &poses {
        let mut v = json!({
            "rotation": p.rotation, "down": p.down,
            "supportFree": p.support_free(),
            "supportVolume": p.support_volume_mm3,
            "overhangArea": p.overhang_area_mm2,
            "overhangCount": p.overhang_triangles.len(),
            "height": p.height_mm, "contactArea": p.contact_area_mm2,
        });
        if estimate {
            let mut m = mesh.clone();
            orient::rotate_mesh(&mut m, &p.rotation);
            let profile = printer(&settings.printer);
            let mut verts = m.vertices.clone();
            if job::center_on_plate(&mut verts, &profile).is_ok() {
                m.vertices = verts;
                let mut s = settings.slice_settings();
                s.support_enabled = !p.support_free();
                if let Ok(r) = vcad_slicer::slice(&m, &s) {
                    v["printTimeSeconds"] = json!(r.stats.print_time_seconds);
                    v["filamentGrams"] = json!(r.stats.filament_grams);
                }
            } else {
                v["fits"] = json!(false);
            }
        }
        out.push(v);
    }
    // How the part sits as modeled, for "N faces overhang" in the UI.
    let modeled = orient::rank_orientations(&mesh, settings.support_angle, 64)
        .into_iter()
        .find(|o| o.down[2] < -0.9995);
    Ok(json!({
        "poses": out,
        "modeled": modeled.map(|o| json!({
            "supportFree": o.support_free(), "supportVolume": o.support_volume_mm3,
            "overhangArea": o.overhang_area_mm2, "overhangCount": o.overhang_triangles.len(),
        })),
    }))
}

/// Prepare (and optionally write) the exact archive a print would send.
fn prepare(doc: &Document, req: &Value) -> Result<Value, String> {
    let settings = settings_of(req)?;
    let mesh = posed_mesh(doc, req)?;
    let spec = spec(&settings, &job_name(req));
    let job = job::prepare(mesh.vertices, mesh.indices, &spec)?;
    let mut out = json!({
        "stats": job.stats, "layers": job.layers,
        "printcheckOk": job.printcheck.ok, "printcheck": findings(&job.printcheck),
        "lintOk": job.lint.ok(), "bytes": job.archive.len(),
        "printerModelId": spec.printer_model_id,
    });
    if let Some(p) = req
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        std::fs::write(p, &job.archive).map_err(|e| format!("write {p}: {e}"))?;
        out["path"] = json!(p);
    }
    if let Some(p) = req
        .get("gcodePath")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        std::fs::write(p, &job.gcode).map_err(|e| format!("write {p}: {e}"))?;
        out["gcodePath"] = json!(p);
    }
    Ok(out)
}

/// Filament type in a global AMS tray (`unit * 4 + slot`).
fn tray_filament(st: &PrinterStatus, tray: u32) -> Option<String> {
    st.ams_status
        .as_ref()?
        .units
        .iter()
        .find(|u| u.id as u32 == tray / 4)?
        .slots
        .iter()
        .find(|s| s.id as u32 == tray % 4)?
        .filament_type
        .clone()
}

/// Prepare, gate, and — only with `"confirm": true` — upload and start.
/// Without confirm this is a dry check: the gate runs as if confirmed and
/// the result says whether a real send would pass. Nothing is uploaded.
fn print(doc: &Document, req: &Value) -> Result<Value, String> {
    let confirm = req.get("confirm") == Some(&json!(true));
    let settings = settings_of(req)?;
    let mesh = posed_mesh(doc, req)?;
    let name = job_name(req);
    let job = job::prepare(mesh.vertices, mesh.indices, &spec(&settings, &name))?;
    if !job.printcheck.ok {
        return Err(format!(
            "printcheck failed: {}",
            findings(&job.printcheck)
                .iter()
                .filter_map(|f| f["message"].as_str().map(str::to_string))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    let (mut link, model_id) = open(printer_name(req)?)?;
    let st = link.status().map_err(|e| e.to_string())?;
    let tray = req.get("amsTray").and_then(Value::as_u64).map(|t| t as u32);
    let loaded = match tray {
        Some(t) => tray_filament(&st, t),
        None => st.loaded_filament.clone(),
    };
    let inputs = GateInputs {
        confirmed: true,
        printcheck_ok: Some(job.printcheck.ok),
        printer_model_id: model_id,
        loaded_filament: loaded.as_deref(),
        status: &st,
    };
    let file = format!("{name}.gcode.3mf");
    if !confirm {
        vcad_slicer_bambu::gate::check(&job.archive, 0, &inputs).map_err(|e| e.to_string())?;
        return Ok(json!({"sent": false, "gatesPass": true, "file": file}));
    }
    let ams: Vec<u32> = tray.into_iter().collect();
    let ticket = link::print_gated(link.as_mut(), &file, &job.archive, 0, &inputs, &ams)
        .map_err(|e| e.to_string())?;
    Ok(
        json!({"sent": true, "file": file, "md5": ticket.archive_md5(),
              "layers": job.layers, "stats": job.stats}),
    )
}

/// Ops that need no document.
pub(crate) fn call_nodoc(op: &str, req: &Value) -> Option<Result<Value, String>> {
    Some(match op {
        "printers" => list_printers(req),
        "discover" => discover(req),
        "add_printer" => add_printer(req),
        "printer_status" => printer_status(req),
        "camera" => camera(req),
        "control" => control(req),
        "dry_run_reset" => dry_run_reset(req),
        "dry_run_log" => Ok(dry_run_log()),
        _ => return None,
    })
}

/// Ops that need the document.
pub(crate) fn call_doc(op: &str, doc: &Document, req: &Value) -> Option<Result<Value, String>> {
    Some(match op {
        "orient" => orient_op(doc, req),
        "prepare" => prepare(doc, req),
        "print" => print(doc, req),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One mutex for every test touching the process-wide dry-run printer.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn doc() -> Document {
        Document::from_json(
            r#"{"version":"0.1","nodes":{"1":{"id":1,"name":"block","op":{"type":"Cube","size":{"x":20,"y":15,"z":10}}}},
                "materials":{},"part_materials":{},"roots":[{"root":1,"material":"default"}]}"#,
        )
        .unwrap()
    }

    fn settings() -> Value {
        json!({"printer": "bambu_p1s", "material": "PLA"})
    }

    #[test]
    fn orient_ranks_poses_with_estimates() {
        let v = orient_op(&doc(), &json!({"settings": settings()})).unwrap();
        let poses = v["poses"].as_array().unwrap();
        assert!(!poses.is_empty());
        assert_eq!(poses[0]["supportFree"], json!(true));
        assert!(poses[0]["printTimeSeconds"].as_f64().unwrap() > 0.0);
        assert_eq!(v["modeled"]["supportFree"], json!(true));
    }

    #[test]
    fn prepare_is_gate_ready_for_the_p1s() {
        let v = prepare(&doc(), &json!({"settings": settings(), "name": "blk"})).unwrap();
        assert_eq!(v["printcheckOk"], json!(true));
        assert_eq!(v["lintOk"], json!(true));
        assert_eq!(v["printerModelId"], json!("C12"));
    }

    #[test]
    fn print_without_confirm_never_uploads() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        dry_run_reset(&json!({})).unwrap();
        let v = print(&doc(), &json!({"settings": settings(), "printer": DRY_RUN})).unwrap();
        assert_eq!(v["sent"], json!(false));
        assert_eq!(v["gatesPass"], json!(true));
        let log = dry_run_log();
        assert_eq!(log["uploaded"], json!(false));
        assert_eq!(log["printed"], json!(false));
    }

    #[test]
    fn confirmed_print_runs_to_completion_with_pause_and_resume() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        dry_run_reset(&json!({"step": 50.0})).unwrap();
        let v = print(
            &doc(),
            &json!({"settings": settings(), "printer": DRY_RUN, "confirm": true, "amsTray": 0}),
        )
        .unwrap();
        assert_eq!(v["sent"], json!(true));
        assert_eq!(dry_run_log()["ams"], json!([0]));
        let st = printer_status(&json!({"printer": DRY_RUN})).unwrap();
        assert_eq!(st["state"], json!("printing"));
        control(&json!({"printer": DRY_RUN, "action": "pause"})).unwrap();
        let paused = printer_status(&json!({"printer": DRY_RUN})).unwrap();
        assert_eq!(paused["state"], json!("paused"));
        assert_eq!(paused["progress"], st["progress"], "paused print advanced");
        control(&json!({"printer": DRY_RUN, "action": "resume"})).unwrap();
        let done = printer_status(&json!({"printer": DRY_RUN})).unwrap();
        assert_eq!(done["state"], json!("finished"));
    }

    #[test]
    fn wrong_filament_in_the_chosen_tray_is_refused() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        dry_run_reset(&json!({})).unwrap();
        // Tray 1 holds PETG; the job is PLA.
        let e = print(
            &doc(),
            &json!({"settings": settings(), "printer": DRY_RUN, "confirm": true, "amsTray": 1}),
        )
        .unwrap_err();
        assert!(e.contains("PETG"), "{e}");
        assert_eq!(dry_run_log()["uploaded"], json!(false));
    }

    #[test]
    fn busy_printer_is_refused() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        dry_run_reset(&json!({"busy": true})).unwrap();
        let e = print(
            &doc(),
            &json!({"settings": settings(), "printer": DRY_RUN, "confirm": true}),
        )
        .unwrap_err();
        assert!(e.contains("not idle"), "{e}");
        assert_eq!(dry_run_log()["uploaded"], json!(false));
    }

    #[test]
    fn stop_returns_to_idle_and_bad_actions_fail() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        dry_run_reset(&json!({"step": 1.0})).unwrap();
        print(
            &doc(),
            &json!({"settings": settings(), "printer": DRY_RUN, "confirm": true}),
        )
        .unwrap();
        control(&json!({"printer": DRY_RUN, "action": "stop"})).unwrap();
        let st = printer_status(&json!({"printer": DRY_RUN})).unwrap();
        assert_eq!(st["state"], json!("idle"));
        assert!(control(&json!({"printer": DRY_RUN, "action": "resume"})).is_err());
        assert!(control(&json!({"printer": DRY_RUN, "action": "explode"})).is_err());
    }

    #[test]
    fn add_printer_validates_and_saves_owner_only() {
        let dir = std::env::temp_dir().join(format!("vcad-printers-{}", std::process::id()));
        let path = dir.join("printers.toml");
        let entry = |code: &str| PrinterEntry {
            name: "shop".into(),
            model: "P1S".into(),
            ip: "192.168.1.42".parse().unwrap(),
            serial: "01P00A000000000".into(),
            access_code: code.into(),
        };
        config::save_entry(&path, entry("12345678")).unwrap();
        config::save_entry(&path, entry("87654321")).unwrap();
        let all = config::load(&path).unwrap();
        assert_eq!(all.len(), 1, "same name replaces");
        assert_eq!(all[0].access_code, "87654321");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
        std::fs::remove_dir_all(dir).ok();
        // Request-level validation (no file touched).
        assert!(add_printer(
            &json!({"name": "x", "ip": "1.2.3.4", "serial": "s", "access_code": "123"})
        )
        .is_err());
        assert!(add_printer(
            &json!({"name": "x", "ip": "nope", "serial": "s", "access_code": "12345678"})
        )
        .is_err());
    }
}
