//! 3D printing for the native app (`vcad_print_call`): printer profiles,
//! printability analysis + recommended settings, instant cost estimate,
//! slicing with layer previews and print-time / filament prediction, G-code,
//! and 3MF export (plain, or Bambu `.gcode.3mf` with the sliced G-code
//! embedded). Mirrors the web app's `PrintPanel` / `SlicerSettings` surface,
//! on the same `vcad-slicer*` crates the WASM bundle uses.
//!
//! One entry point, `{"op": ...}` request plus the document JSON; result JSON
//! freed with `vcad_string_free`. File outputs (`gcodePath`, `path`) are
//! written by Rust so large buffers never cross the boundary.

use std::ffi::c_char;
use std::ptr;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use vcad_ir::Document;
use vcad_kernel_tessellate::TriangleMesh;
use vcad_slicer::{InfillPattern, SliceSettings};
use vcad_slicer_gcode::{GcodeSettings, PrinterProfile};

use crate::analysis::{evaluate, parse_request};
use crate::err;
use crate::json::{borrow_str, guard_ptr, into_c};

/// Wire form of the slicer settings (same field names as the web app's
/// `SlicerSettings`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub layer_height: f64,
    pub first_layer_height: f64,
    pub nozzle_diameter: f64,
    pub line_width: f64,
    pub wall_count: u32,
    pub infill_density: f64,
    /// grid | lines | triangles | honeycomb | gyroid
    pub infill_pattern: String,
    pub support_enabled: bool,
    pub support_angle: f64,
    pub printer: String,
    pub material: String,
    pub print_temp: u32,
    pub bed_temp: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layer_height: 0.2,
            first_layer_height: 0.25,
            nozzle_diameter: 0.4,
            line_width: 0.45,
            wall_count: 3,
            infill_density: 0.15,
            infill_pattern: "grid".into(),
            support_enabled: false,
            support_angle: 45.0,
            printer: "generic".into(),
            material: "PLA".into(),
            print_temp: 210,
            bed_temp: 60,
        }
    }
}

impl Settings {
    fn validate(&self) -> Result<(), String> {
        let ok = |v: f64, lo: f64, hi: f64| v.is_finite() && v >= lo && v <= hi;
        if !ok(self.layer_height, 0.04, 1.0)
            || !ok(self.first_layer_height, 0.04, 1.0)
            || !ok(self.nozzle_diameter, 0.1, 2.0)
            || !ok(self.line_width, 0.1, 3.0)
            || !ok(self.infill_density, 0.0, 1.0)
            || !ok(self.support_angle, 0.0, 90.0)
            || self.wall_count > 20
        {
            return Err("print settings out of range".into());
        }
        Ok(())
    }

    pub(crate) fn slice_settings(&self) -> SliceSettings {
        SliceSettings {
            layer_height: self.layer_height,
            first_layer_height: self.first_layer_height,
            nozzle_diameter: self.nozzle_diameter,
            line_width: self.line_width,
            wall_count: self.wall_count,
            infill_density: self.infill_density,
            infill_pattern: match self.infill_pattern.to_ascii_lowercase().as_str() {
                "lines" => InfillPattern::Lines,
                "triangles" => InfillPattern::Triangles,
                "honeycomb" => InfillPattern::Honeycomb,
                "gyroid" => InfillPattern::Gyroid,
                _ => InfillPattern::Grid,
            },
            support_enabled: self.support_enabled,
            support_angle: self.support_angle,
        }
    }
}

fn profile_id(name: &str) -> String {
    // The short ids `printer()` and the app use for the common printers, so
    // a listed id round-trips (`bambu_p1s`, not `bambu_lab_p1s`).
    match name {
        "Bambu Lab X1 Carbon" => return "bambu_x1c".into(),
        "Bambu Lab P1S" => return "bambu_p1s".into(),
        "Bambu Lab A1" => return "bambu_a1".into(),
        "Bambu Lab A1 Mini" => return "bambu_a1_mini".into(),
        "Creality Ender 3" => return "ender3".into(),
        "Prusa MK4" => return "prusa_mk4".into(),
        "Voron 2.4 (350mm)" => return "voron_24".into(),
        _ => {}
    }
    name.to_lowercase()
        .replace(' ', "_")
        .replace(['(', ')'], "")
}

/// Resolve a printer id (as listed by `profiles`, or the web's short ids).
pub(crate) fn printer(id: &str) -> PrinterProfile {
    match id {
        "bambu_x1c" => return PrinterProfile::bambu_x1c(),
        "bambu_p1s" => return PrinterProfile::bambu_p1s(),
        "bambu_a1" => return PrinterProfile::bambu_a1(),
        "bambu_a1_mini" => return PrinterProfile::bambu_a1_mini(),
        "ender3" => return PrinterProfile::ender3(),
        "prusa_mk4" => return PrinterProfile::prusa_mk4(),
        "voron_24" => return PrinterProfile::voron_24(),
        _ => {}
    }
    PrinterProfile::all_profiles()
        .into_iter()
        .find(|p| profile_id(&p.name) == id)
        .unwrap_or_else(PrinterProfile::generic)
}

fn printer_params(p: &PrinterProfile) -> vcad_slicer::smart_defaults::PrinterParams {
    vcad_slicer::smart_defaults::PrinterParams {
        nozzle_diameter: p.nozzle_diameter,
        bed_x: p.bed_x,
        bed_y: p.bed_y,
        bed_z: p.bed_z,
    }
}

/// Every visible part merged into one mesh, dropped onto the bed (min Z = 0)
/// and centred on it.
pub(crate) fn bed_mesh(doc: &Document, bed: Option<(f64, f64)>) -> Result<TriangleMesh, String> {
    let scene = evaluate(doc)?;
    let mut vertices: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for part in &scene.parts {
        let base = (vertices.len() / 3) as u32;
        vertices.extend_from_slice(&part.mesh.positions);
        indices.extend(part.mesh.indices.iter().map(|i| i + base));
    }
    if indices.is_empty() {
        return Err("print: document has no geometry".into());
    }
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for c in vertices.as_chunks::<3>().0.iter() {
        for k in 0..3 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    }
    let (cx, cy) = match bed {
        Some((bx, by)) => (bx as f32 / 2.0, by as f32 / 2.0),
        None => ((hi[0] - lo[0]) / 2.0, (hi[1] - lo[1]) / 2.0),
    };
    let shift = [
        cx - (lo[0] + hi[0]) / 2.0,
        cy - (lo[1] + hi[1]) / 2.0,
        -lo[2],
    ];
    for c in vertices.as_chunks_mut::<3>().0.iter_mut() {
        for k in 0..3 {
            c[k] += shift[k];
        }
    }
    Ok(TriangleMesh {
        vertices,
        indices,
        normals: Vec::new(),
        face_kinds: Vec::new(),
        face_ids: Vec::new(),
    })
}

pub(crate) fn settings_of(req: &Value) -> Result<Settings, String> {
    let s: Settings = match req.get("settings") {
        Some(v) if !v.is_null() => {
            serde_json::from_value(v.clone()).map_err(|e| format!("settings: {e}"))?
        }
        _ => Settings::default(),
    };
    s.validate()?;
    Ok(s)
}

fn material(name: &str) -> vcad_slicer::cost::Material {
    match name.to_ascii_uppercase().as_str() {
        "PETG" => vcad_slicer::cost::Material::petg(),
        "ABS" => vcad_slicer::cost::Material::abs(),
        "TPU" => vcad_slicer::cost::Material::tpu(),
        _ => vcad_slicer::cost::Material::pla(),
    }
}

fn profiles() -> Value {
    let list: Vec<Value> = PrinterProfile::all_profiles()
        .into_iter()
        .map(|p| {
            json!({"id": profile_id(&p.name), "name": p.name, "bed_x": p.bed_x,
                   "bed_y": p.bed_y, "bed_z": p.bed_z, "nozzle_diameter": p.nozzle_diameter})
        })
        .collect();
    json!({ "profiles": list, "defaults": Settings::default() })
}

/// Printability analysis of part `req.part` (default 0): geometric analysis,
/// recommended settings, DFM warnings (face ids = kernel face indices) and an
/// instant volume-based cost estimate.
fn analyze(doc: &Document, req: &Value) -> Result<Value, String> {
    let settings = settings_of(req)?;
    let scene = evaluate(doc)?;
    let index = req.get("part").and_then(Value::as_u64).unwrap_or(0) as usize;
    let part = scene
        .parts
        .get(index)
        .ok_or_else(|| format!("print: part {index} out of range"))?;
    let profile = printer(&settings.printer);
    let params = printer_params(&profile);
    let volume = part.solid.as_ref().map(|s| s.volume()).unwrap_or(0.0);
    let mut out = json!({ "part": index, "volume_mm3": volume });
    if let Some(solid) = &part.solid {
        if let Some(brep) = solid.brep() {
            let analysis =
                vcad_slicer::analyze::analyze_for_printing(brep, volume, solid.surface_area());
            let rec = vcad_slicer::smart_defaults::recommend_settings(&analysis, &params);
            out["analysis"] = serde_json::to_value(&analysis).map_err(|e| e.to_string())?;
            out["recommended"] = serde_json::to_value(&rec).map_err(|e| e.to_string())?;
            out["dfm"] = serde_json::to_value(vcad_slicer::dfm::check_printability(brep, &params))
                .map_err(|e| e.to_string())?;
        }
    }
    let cost = vcad_slicer::cost::estimate_cost_from_volume(
        volume,
        settings.infill_density,
        settings.wall_count,
        settings.line_width,
        &material(&settings.material),
    );
    out["cost"] = serde_json::to_value(&cost).map_err(|e| e.to_string())?;
    Ok(out)
}

type Ring = Vec<[f64; 2]>;

fn rings<'a, I: IntoIterator<Item = &'a [vcad_kernel_math::Point2]>>(it: I) -> Vec<Ring> {
    it.into_iter()
        .map(|pts| pts.iter().map(|p| [p.x, p.y]).collect())
        .collect()
}

/// Slice the document. Returns stats, a bounded set of layer previews and —
/// when `gcodePath` is set — writes the G-code there.
fn slice(doc: &Document, req: &Value) -> Result<Value, String> {
    let settings = settings_of(req)?;
    let profile = printer(&settings.printer);
    let mut mesh = bed_mesh(doc, Some((profile.bed_x, profile.bed_y)))?;
    // Preview the pose the user picked in Orient.
    if let Some(r) = crate::printer::rotation_of(req)? {
        vcad_slicer::orient::rotate_mesh(&mut mesh, &r);
        let mut v = mesh.vertices.clone();
        if vcad_slicer_bambu::job::center_on_plate(&mut v, &profile).is_ok() {
            mesh.vertices = v;
        }
    }
    let result =
        vcad_slicer::slice(&mesh, &settings.slice_settings()).map_err(|e| e.to_string())?;
    let stats = &result.stats;
    let size = [
        stats.bounds_max[0] - stats.bounds_min[0],
        stats.bounds_max[1] - stats.bounds_min[1],
        stats.bounds_max[2] - stats.bounds_min[2],
    ];
    let fits = size[0] <= profile.bed_x && size[1] <= profile.bed_y && size[2] <= profile.bed_z;
    // Previews: every layer up to 400, else an even stride.
    let max_preview = req
        .get("maxPreviewLayers")
        .and_then(Value::as_u64)
        .unwrap_or(400)
        .max(1) as usize;
    let stride = result.layers.len().div_ceil(max_preview).max(1);
    let previews: Vec<Value> = result
        .layers
        .iter()
        .step_by(stride)
        .map(|l| {
            json!({
                "z": l.z,
                "index": l.index,
                "outer": rings(l.outer_perimeters.iter().map(|p| p.points.as_slice())),
                "inner": rings(l.inner_perimeters.iter().map(|p| p.points.as_slice())),
                "infill": rings(l.infill.iter().map(|p| p.points.as_slice())),
            })
        })
        .collect();
    let mut out = json!({
        "stats": stats,
        "printer": profile.name,
        "bed": [profile.bed_x, profile.bed_y, profile.bed_z],
        "fitsBed": fits,
        "layers": previews,
        "cost": vcad_slicer::cost::estimate_cost_from_volume(
            mesh_volume(&mesh), settings.infill_density, settings.wall_count,
            settings.line_width, &material(&settings.material)),
    });
    let gcode_path = req.get("gcodePath").and_then(Value::as_str).unwrap_or("");
    let want_3mf = req.get("threeMfPath").and_then(Value::as_str).unwrap_or("");
    if !gcode_path.is_empty()
        || !want_3mf.is_empty()
        || req.get("returnGcode") == Some(&json!(true))
    {
        let gcode = vcad_slicer_gcode::generate_gcode(
            &result,
            GcodeSettings {
                printer: profile.clone(),
                print_temp: settings.print_temp,
                bed_temp: settings.bed_temp,
                ..Default::default()
            },
        );
        out["gcodeBytes"] = json!(gcode.len());
        out["gcodeLines"] = json!(gcode.lines().count());
        if !gcode_path.is_empty() {
            std::fs::write(gcode_path, &gcode).map_err(|e| format!("write {gcode_path}: {e}"))?;
            out["gcodePath"] = json!(gcode_path);
        }
        if !want_3mf.is_empty() {
            let bytes = three_mf(&mesh, &settings, req, Some(gcode.clone().into_bytes()))?;
            std::fs::write(want_3mf, &bytes).map_err(|e| format!("write {want_3mf}: {e}"))?;
            out["threeMfPath"] = json!(want_3mf);
            out["threeMfBytes"] = json!(bytes.len());
        }
        if req.get("returnGcode") == Some(&json!(true)) {
            out["gcode"] = json!(gcode);
        }
    }
    Ok(out)
}

fn mesh_volume(m: &TriangleMesh) -> f64 {
    let v = |i: u32| {
        let i = i as usize * 3;
        [
            m.vertices[i] as f64,
            m.vertices[i + 1] as f64,
            m.vertices[i + 2] as f64,
        ]
    };
    m.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum::<f64>()
        .abs()
}

fn three_mf(
    mesh: &TriangleMesh,
    settings: &Settings,
    req: &Value,
    gcode: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    use vcad_slicer_bambu::{PrintSettings, ThreeMfModel};
    let name = req
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("vcad part");
    let mut model = ThreeMfModel::new(
        name.to_string(),
        mesh.vertices.clone(),
        mesh.indices.clone(),
    );
    let d = PrintSettings::default();
    model.settings = PrintSettings {
        layer_height: settings.layer_height,
        first_layer_height: settings.first_layer_height,
        wall_count: settings.wall_count,
        infill_density: settings.infill_density,
        print_temp: settings.print_temp,
        bed_temp: settings.bed_temp,
        filament_type: if settings.material.is_empty() {
            d.filament_type
        } else {
            settings.material.clone()
        },
    };
    if let Some(g) = gcode {
        model = model.with_gcode(g);
    }
    model.to_bytes().map_err(|e| format!("3MF: {e}"))
}

/// Unsliced 3MF (mesh + print settings) to `req.path`.
fn export_3mf(doc: &Document, req: &Value) -> Result<Value, String> {
    let settings = settings_of(req)?;
    let path = req.get("path").and_then(Value::as_str).unwrap_or("");
    if path.is_empty() {
        return Err("export_3mf: missing `path`".into());
    }
    let profile = printer(&settings.printer);
    let mesh = bed_mesh(doc, Some((profile.bed_x, profile.bed_y)))?;
    let bytes = three_mf(&mesh, &settings, req, None)?;
    std::fs::write(path, &bytes).map_err(|e| format!("write {path}: {e}"))?;
    Ok(json!({"path": path, "bytes": bytes.len(), "triangles": mesh.indices.len() / 3}))
}

pub(crate) fn call(doc: Option<&Document>, req: &Value) -> Result<Value, String> {
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");
    if op == "profiles" {
        return Ok(profiles());
    }
    if let Some(r) = crate::printer::call_nodoc(op, req) {
        return r;
    }
    let doc = doc.ok_or("print: this op needs a document")?;
    if let Some(r) = crate::printer::call_doc(op, doc, req) {
        return r;
    }
    match op {
        "analyze" => analyze(doc, req),
        "slice" => slice(doc, req),
        "export_3mf" => export_3mf(doc, req),
        "" => Err("print: missing `op`".into()),
        other => Err(format!("print: unknown op `{other}`")),
    }
}

/// `{"op": "profiles" | "analyze" | "slice" | "export_3mf", ...}`. `doc_json`
/// may be null for `profiles`. Null + `vcad_last_error` on failure.
#[no_mangle]
pub extern "C" fn vcad_print_call(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_print_call", || {
        let run = || -> Result<String, String> {
            let doc = if doc_json.is_null() {
                None
            } else {
                let text = borrow_str("doc", doc_json).ok_or("doc: bad string")?;
                Some(Document::from_json(text).map_err(|e| format!("doc: parse: {e}"))?)
            };
            let req = parse_request(request_json)?;
            Ok(call(doc.as_ref(), &req)?.to_string())
        };
        match run() {
            Ok(s) => into_c(s),
            Err(e) => {
                err::set_error(e);
                ptr::null_mut()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_doc() -> Document {
        Document::from_json(
            r#"{"version":"0.1","nodes":{"1":{"id":1,"name":"block","op":{"type":"Cube","size":{"x":20,"y":15,"z":10}}}},
            "materials":{},"part_materials":{},"roots":[{"root":1,"material":"default"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn profiles_list_printers() {
        let v = call(None, &json!({"op":"profiles"})).unwrap();
        let list = v["profiles"].as_array().unwrap();
        assert!(list.len() >= 5);
        // Every listed id resolves back to the same profile.
        for p in list {
            let id = p["id"].as_str().unwrap();
            assert_eq!(printer(id).name, p["name"].as_str().unwrap(), "{id}");
        }
        assert!(list.iter().any(|p| p["id"] == "bambu_p1s"));
    }

    /// Gate: a part is analysed, sliced (time + filament predicted), and
    /// exported for print as G-code, a sliced `.gcode.3mf` and a plain 3MF.
    #[test]
    fn part_is_exported_for_print() {
        let doc = block_doc();
        let dir = std::env::temp_dir().join(format!("vcad-print-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = call(Some(&doc), &json!({"op":"analyze"})).unwrap();
        assert!((a["volume_mm3"].as_f64().unwrap() - 3000.0).abs() < 1.0);
        assert!(a["cost"].is_object());

        let g = dir.join("block.gcode");
        let t = dir.join("block.gcode.3mf");
        let s = call(
            Some(&doc),
            &json!({"op":"slice","settings":{"printer":"bambu_p1s"},
                    "gcodePath": g.to_str().unwrap(), "threeMfPath": t.to_str().unwrap()}),
        )
        .unwrap();
        let layers = s["stats"]["layer_count"].as_u64().unwrap();
        assert!(
            (45..=55).contains(&layers),
            "10 mm at 0.2 mm ≈ 50 layers, got {layers}"
        );
        assert!(s["stats"]["print_time_seconds"].as_f64().unwrap() > 0.0);
        assert!(s["stats"]["filament_grams"].as_f64().unwrap() > 0.0);
        assert_eq!(s["fitsBed"], true);
        let gcode = std::fs::read_to_string(&g).unwrap();
        assert!(gcode.contains("G1") && gcode.lines().count() > 100);
        let zip = std::fs::read(&t).unwrap();
        assert_eq!(&zip[..2], b"PK", "3MF is a zip");

        let p = dir.join("block.3mf");
        let e = call(
            Some(&doc),
            &json!({"op":"export_3mf","path": p.to_str().unwrap()}),
        )
        .unwrap();
        assert_eq!(e["triangles"], 12);
        assert_eq!(&std::fs::read(&p).unwrap()[..2], b"PK");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
