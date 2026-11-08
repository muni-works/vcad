//! Sheet metal for the native app (`vcad_sheet_metal_*`).
//!
//! Two entry points, both JSON in / JSON out (free with `vcad_string_free`):
//!
//! - [`vcad_sheet_metal_call`] — the authored-chain API the web app uses
//!   (`evaluate`, `check`, `cost`, `nest`, `nested_dxf`, `sequence`,
//!   `folded_step`, `materials`, `bend_table`, `shop_catalog`), routed to
//!   [`vcad_kernel::sheet_json`], the same implementation the WASM bundle
//!   wraps — so native and web cannot drift.
//! - [`vcad_sheet_metal_flatten`] — flat pattern from an ordinary solid in a
//!   document (an extruded bracket, an imported STEP). Adds a second,
//!   independent volume check against the kernel's own BRep volume, so the
//!   DXF is only trusted when the recovered sheet, the tessellated mesh and
//!   the analytic solid all agree.

use std::ffi::c_char;
use std::ptr;

use serde_json::{json, Value};
use vcad_kernel::sheet_json as sj;

use crate::analysis::{evaluate, parse_doc, parse_request, part_name, part_roots};
use crate::err;
use crate::json::{guard_ptr, into_c};

/// Default tolerance on |recovered − kernel| / kernel for the flatten gate.
pub(crate) const DEFAULT_KERNEL_VOLUME_TOL: f64 = 0.02;

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn json_field(v: &Value, key: &str) -> String {
    match v.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Route one chain-API request. `req.op` picks the call.
pub(crate) fn call(req: &Value) -> Result<String, String> {
    let op = str_field(req, "op");
    let chain = json_field(req, "chain");
    let out = match op {
        "evaluate" => sj::evaluate_sheet_metal_chain(&chain),
        "check" => sj::check_sheet_metal(&chain, &json_field(req, "shop")),
        "cost" => {
            let qty = req.get("quantity").and_then(Value::as_u64).unwrap_or(1) as u32;
            sj::cost_sheet_metal(&chain, &json_field(req, "rates"), qty.max(1))
        }
        "nest" => sj::nest_sheet_metal_parts(&json_field(req, "parts"), &json_field(req, "params")),
        "nested_dxf" => sj::nested_sheet_metal_dxf(&json_field(req, "placements")),
        "sequence" => sj::sheet_metal_sequence(&chain),
        "folded_step" => sj::sheet_metal_folded_step(&chain),
        "materials" => sj::get_sheet_metal_materials(),
        "bend_table" => sj::get_sheet_metal_bend_table(),
        "shop_catalog" => sj::get_sheet_metal_shop_catalog(str_field(req, "shop")),
        "" => return Err("sheet metal: missing `op`".into()),
        other => return Err(format!("sheet metal: unknown op `{other}`")),
    };
    Ok(out)
}

/// Flatten part `req.part` (default 0) of `doc`. See module docs.
pub(crate) fn flatten(doc: &vcad_ir::Document, req: &Value) -> Result<Value, String> {
    let scene = evaluate(doc)?;
    let index = req.get("part").and_then(Value::as_u64).unwrap_or(0) as usize;
    let part = scene.parts.get(index).ok_or_else(|| {
        format!(
            "flatten: part {index} out of range ({} parts)",
            scene.parts.len()
        )
    })?;
    let roots = part_roots(doc, &scene);
    let name = part_name(doc, roots.get(index).copied().flatten(), index);
    // Bend recognition needs the cylindrical faces finely faceted; the
    // viewport mesh is too coarse on small radii, so re-tessellate the BRep.
    let segments = req.get("segments").and_then(Value::as_u64).unwrap_or(128) as u32;
    let (positions, indices): (Vec<f64>, Vec<u32>) = match &part.solid {
        Some(solid) => {
            let m = solid.to_mesh(segments.clamp(16, 512));
            (m.vertices.iter().map(|&v| v as f64).collect(), m.indices)
        }
        None => (
            part.mesh.positions.iter().map(|&v| v as f64).collect(),
            part.mesh.indices.clone(),
        ),
    };
    let (positions, mut indices) = weld(&positions, &indices, 1e-5);
    // Outward winding: flatten reads inside/outside bend faces from normals.
    if signed_volume(&positions, &indices) < 0.0 {
        indices
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .for_each(|t| t.swap(1, 2));
    }
    let mut kreq = json!({
        "positions": positions,
        "indices": indices,
    });
    for key in [
        "material",
        "manualK",
        "shopProfile",
        "volumeTolFrac",
        "planeAngleTolDeg",
        "planeOffsetTol",
        "quantity",
        "rates",
    ] {
        if let Some(v) = req.get(key) {
            kreq[key] = v.clone();
        }
    }
    let mut out: Value = serde_json::from_str(&sj::flatten_solid_to_sheet_metal(&kreq.to_string()))
        .map_err(|e| format!("flatten: {e}"))?;
    out["part"] = json!(index);
    out["partName"] = json!(name);
    if out.get("error").is_some_and(|e| !e.is_null()) {
        return Ok(out);
    }
    // Independent volume gate against the analytic solid.
    let tol = req
        .get("kernelVolumeTol")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_KERNEL_VOLUME_TOL);
    let recovered = out["recovered_volume_mm3"].as_f64().unwrap_or(0.0);
    let kernel = part.solid.as_ref().map(|s| s.volume());
    let (err_frac, pass) = match kernel {
        Some(k) if k > 0.0 => {
            let e = (recovered - k).abs() / k;
            (Some(e), e <= tol)
        }
        // No analytic solid (imported mesh): the mesh round-trip check the
        // kernel already ran is the only evidence.
        _ => (None, true),
    };
    out["kernelVolumeCheck"] = json!({
        "kernelVolumeMm3": kernel,
        "recoveredVolumeMm3": recovered,
        "errorFrac": err_frac,
        "tolerance": tol,
        "pass": pass,
    });
    if !pass {
        // Fail closed: never hand out a DXF that disagrees with the solid.
        out["dxf"] = json!("");
        out["error"] = json!(format!(
            "flat pattern volume {recovered:.1} mm³ disagrees with the solid's {:.1} mm³ ({:.2}% > {:.2}%)",
            kernel.unwrap_or(0.0),
            err_frac.unwrap_or(0.0) * 100.0,
            tol * 100.0
        ));
    }
    Ok(out)
}

fn signed_volume(p: &[f64], idx: &[u32]) -> f64 {
    let v = |i: u32| {
        [
            p[i as usize * 3],
            p[i as usize * 3 + 1],
            p[i as usize * 3 + 2],
        ]
    };
    idx.as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// Merge coincident vertices (tessellation emits one vertex ring per face;
/// flatten's wall/bend recognition walks shared edges).
fn weld(pos: &[f64], idx: &[u32], tol: f64) -> (Vec<f64>, Vec<u32>) {
    use std::collections::HashMap;
    let mut map: HashMap<[i64; 3], u32> = HashMap::new();
    let mut out = Vec::with_capacity(pos.len());
    let mut remap = Vec::with_capacity(pos.len() / 3);
    for c in pos.as_chunks::<3>().0.iter() {
        let key = [
            (c[0] / tol).round() as i64,
            (c[1] / tol).round() as i64,
            (c[2] / tol).round() as i64,
        ];
        let id = *map.entry(key).or_insert_with(|| {
            out.extend_from_slice(c);
            (out.len() / 3 - 1) as u32
        });
        remap.push(id);
    }
    let mut tris = Vec::with_capacity(idx.len());
    for t in idx.as_chunks::<3>().0.iter() {
        let (a, b, c) = (
            remap[t[0] as usize],
            remap[t[1] as usize],
            remap[t[2] as usize],
        );
        if a != b && b != c && a != c {
            tris.extend([a, b, c]);
        }
    }
    (out, tris)
}

fn finish(result: Result<String, String>) -> *mut c_char {
    match result {
        Ok(s) => into_c(s),
        Err(e) => {
            err::set_error(e);
            ptr::null_mut()
        }
    }
}

/// Chain-API request (`{"op": ..., ...}`) → result JSON. The kernel's own
/// `error` field carries modeling failures; null + `vcad_last_error` only for
/// a malformed request.
#[no_mangle]
pub extern "C" fn vcad_sheet_metal_call(request_json: *const c_char) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_sheet_metal_call", || {
        finish(parse_request(request_json).and_then(|r| call(&r)))
    })
}

/// Flat pattern from a solid in `doc_json`. Request: `{"part": 0,
/// "material": "al-soft", "shopProfile": "sendcutsend", "kernelVolumeTol":
/// 0.02}` (all optional).
#[no_mangle]
pub extern "C" fn vcad_sheet_metal_flatten(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_sheet_metal_flatten", || {
        finish((|| {
            let doc = parse_doc("doc", doc_json)?;
            let req = parse_request(request_json)?;
            Ok(flatten(&doc, &req)?.to_string())
        })())
    })
}

#[cfg(test)]
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;

    /// An L-bracket modelled as an ordinary solid: an L section (2 mm wall,
    /// 2 mm inside bend radius) sketched in XZ and extruded 40 mm along Y —
    /// never touched by the sheet-metal ops.
    pub(crate) fn bracket_doc() -> vcad_ir::Document {
        // Polyline section: the bend arcs as 24 facets each (what the
        // kernel's own flatten tests feed it).
        let arc = |c: [f64; 2], r: f64, a0: f64, a1: f64| -> Vec<[f64; 2]> {
            (0..=24)
                .map(|k| {
                    let a = a0 + (a1 - a0) * k as f64 / 24.0;
                    [c[0] + r * a.cos(), c[1] + r * a.sin()]
                })
                .collect()
        };
        let h = std::f64::consts::FRAC_PI_2;
        let mut pts: Vec<[f64; 2]> = vec![[0.0, 0.0]];
        pts.extend(arc([40.0, 4.0], 4.0, -h, 0.0));
        pts.extend([[44.0, 34.0], [42.0, 34.0]]);
        pts.extend(arc([40.0, 4.0], 2.0, 0.0, -h));
        pts.push([0.0, 2.0]);
        let segments: Vec<Value> = (0..pts.len())
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                json!({"type":"Line","start":{"x":a[0],"y":a[1]},"end":{"x":b[0],"y":b[1]}})
            })
            .collect();
        let doc = json!({
            "version":"0.1",
            "nodes":{
                "1":{"id":1,"name":"section","op":{"type":"Sketch2D",
                    "origin":{"x":0,"y":0,"z":0},"x_dir":{"x":1,"y":0,"z":0},
                    "y_dir":{"x":0,"y":0,"z":1},"segments":segments}},
                "2":{"id":2,"name":"bracket","op":{"type":"Extrude","sketch":1,
                    "direction":{"x":0,"y":40,"z":0}}}
            },
            "materials":{},
            "part_materials":{},
            "roots":[{"root":2,"material":"default"}]
        });
        vcad_ir::Document::from_json(&doc.to_string()).expect("bracket doc")
    }

    /// Analytic volume of [`bracket_doc`]: two legs + a quarter annulus.
    pub(crate) const BRACKET_VOLUME: f64 =
        (40.0 * 2.0 + 30.0 * 2.0 + std::f64::consts::FRAC_PI_4 * (16.0 - 4.0)) * 40.0;

    #[test]
    fn chain_api_routes_to_shared_impl() {
        let r = call(&json!({"op":"materials"})).unwrap();
        assert!(r.starts_with('['));
        let chain = json!([
            {"type":"BaseFlangeRect","width":60,"depth":40,"thickness":2,"material":"Al-soft"},
            {"type":"EdgeFlange","panelId":0,"edgeIndex":0,"length":30,"angle":1.5707963267948966,"radius":2.0,"direction":"Up"}
        ]);
        let v: Value =
            serde_json::from_str(&call(&json!({"op":"evaluate","chain":chain})).unwrap()).unwrap();
        assert!(v["error"].is_null(), "{}", v["error"]);
        assert!(v["dxf"].as_str().unwrap().contains("CUT"));
        let c: Value =
            serde_json::from_str(&call(&json!({"op":"cost","chain":chain,"quantity":10})).unwrap())
                .unwrap();
        assert!(c["error"].is_null(), "{}", c["error"]);
        assert!(call(&json!({"op":"nope"})).is_err());
    }

    /// Gate: bracket solid → flat-pattern DXF whose recovered volume matches
    /// the kernel's BRep volume.
    #[test]
    fn bracket_flattens_to_dxf_matching_kernel_volume() {
        let doc = bracket_doc();
        let v = flatten(&doc, &json!({"material":"al-soft"})).unwrap();
        assert!(
            v["error"].is_null(),
            "flatten failed: {} bends={}",
            v["error"],
            v["bends"]
        );
        let check = &v["kernelVolumeCheck"];
        assert_eq!(check["pass"], true, "{check}");
        let k = check["kernelVolumeMm3"].as_f64().unwrap();
        assert!(
            (k - BRACKET_VOLUME).abs() / BRACKET_VOLUME < 0.01,
            "kernel volume {k}"
        );
        let dxf = v["dxf"].as_str().unwrap();
        assert!(
            dxf.contains("CUT") && dxf.contains("EOF"),
            "not a layered DXF"
        );
        assert_eq!(v["model"]["bend_count"].as_u64().unwrap(), 1);
    }

    #[test]
    fn non_sheet_solid_fails_closed() {
        let src = r#"{"version":"0.1","nodes":{"1":{"id":1,"op":{"type":"Cube","size":{"x":30,"y":30,"z":30}}}},
            "materials":{},"part_materials":{},"roots":[{"root":1,"material":"default"}]}"#;
        let doc = vcad_ir::Document::from_json(src).unwrap();
        let v = flatten(&doc, &json!({})).unwrap();
        assert!(!v["error"].is_null(), "a 30 mm cube is not sheet metal");
        assert_eq!(v["dxf"].as_str().unwrap_or(""), "");
    }
}
