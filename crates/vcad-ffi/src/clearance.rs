//! Clearance / interference for the native app (`vcad_clearance`): for every
//! pair of evaluated parts, the minimum separation (or deepest penetration)
//! and — when they interfere — the overlap volume and its mesh, the native
//! twin of the web `AnalyzePanel` + `ClashMesh`.
//!
//! Distance is the kernel's tessellation-based [`vcad_kernel::Solid::clearance`]
//! (chord error at the part's own segment count). Overlap volume is the volume
//! of the B-rep intersection, so a pair that only touches reports zero volume
//! while still being flagged as intersecting.

use std::ffi::c_char;
use std::ptr;

use serde::Serialize;
use serde_json::Value;

use crate::analysis::{evaluate, parse_doc, parse_request, part_name, part_roots};
use crate::err;
use crate::json::{guard_ptr, json_out};

/// Overlap below this volume (mm³) is contact, not interference.
pub const CONTACT_VOLUME_MM3: f64 = 1e-6;

/// The interference solid as a drawable mesh.
#[derive(Debug, Clone, Serialize)]
pub struct ClashMesh {
    /// Flat xyz, mm.
    pub positions: Vec<f32>,
    /// Triangle indices.
    pub indices: Vec<u32>,
}

/// One part pair.
#[derive(Debug, Clone, Serialize)]
pub struct PairResult {
    /// First part index.
    pub a: usize,
    /// Second part index.
    pub b: usize,
    /// First part name.
    pub a_name: String,
    /// Second part name.
    pub b_name: String,
    /// Signed distance, mm: separation when ≥ 0, negated penetration depth
    /// when the parts intersect.
    pub distance_mm: f64,
    /// True when the surfaces cross or one contains the other.
    pub intersecting: bool,
    /// Overlap volume, mm³ (0 when separate or merely touching).
    pub overlap_volume_mm3: f64,
    /// Closest / deepest points on A and B.
    pub point_a: [f64; 3],
    /// Point on B.
    pub point_b: [f64; 3],
    /// `"clear" | "contact" | "interference"`.
    pub status: &'static str,
    /// Interference mesh (only when requested and volume > 0).
    pub clash: Option<ClashMesh>,
}

/// Whole-document result.
#[derive(Debug, Clone, Serialize)]
pub struct ClearanceReport {
    /// Part names by index.
    pub parts: Vec<String>,
    /// Pairs, worst first (smallest signed distance).
    pub pairs: Vec<PairResult>,
    /// Pairs that interfere.
    pub interference_count: usize,
    /// Smallest separation among clear pairs, mm.
    pub min_clearance_mm: Option<f64>,
    /// Total overlap volume across pairs, mm³.
    pub total_overlap_mm3: f64,
    /// Parts without a solid (skipped).
    pub skipped_parts: Vec<String>,
}

/// Measure every pair of parts (or only `req.pairs: [[a, b], …]`).
pub fn check(doc: &vcad_ir::Document, req: &Value) -> Result<ClearanceReport, String> {
    let scene = evaluate(doc)?;
    let roots = part_roots(doc, &scene);
    let names: Vec<String> = (0..scene.parts.len())
        .map(|i| part_name(doc, roots[i], i))
        .collect();
    let with_mesh = req["include_mesh"].as_bool().unwrap_or(true);
    let pairs: Vec<(usize, usize)> = match req["pairs"].as_array() {
        Some(list) => list
            .iter()
            .map(|p| {
                let a = p[0].as_u64().ok_or("pairs: [[a, b], …]")? as usize;
                let b = p[1].as_u64().ok_or("pairs: [[a, b], …]")? as usize;
                if a >= names.len() || b >= names.len() || a == b {
                    return Err(format!(
                        "pair [{a}, {b}] out of range ({} parts)",
                        names.len()
                    ));
                }
                Ok((a, b))
            })
            .collect::<Result<_, String>>()?,
        None => (0..names.len())
            .flat_map(|a| (a + 1..names.len()).map(move |b| (a, b)))
            .collect(),
    };
    let skipped_parts = scene
        .parts
        .iter()
        .enumerate()
        .filter(|(_, p)| p.solid.as_ref().is_none_or(|s| s.is_empty()))
        .map(|(i, _)| names[i].clone())
        .collect();
    let mut out = Vec::new();
    for (a, b) in pairs {
        let (Some(sa), Some(sb)) = (scene.parts[a].solid.as_ref(), scene.parts[b].solid.as_ref())
        else {
            continue;
        };
        let Some(c) = sa.clearance(sb) else { continue };
        let (overlap, clash) = if c.intersecting {
            let x = sa.intersection(sb);
            let v = x.volume().max(0.0);
            let mesh = (with_mesh && v > CONTACT_VOLUME_MM3).then(|| {
                let m = x.to_mesh(32);
                ClashMesh {
                    positions: m.vertices,
                    indices: m.indices,
                }
            });
            (v, mesh)
        } else {
            (0.0, None)
        };
        let status = if overlap > CONTACT_VOLUME_MM3 {
            "interference"
        } else if c.intersecting || c.distance.abs() < 1e-6 {
            "contact"
        } else {
            "clear"
        };
        out.push(PairResult {
            a,
            b,
            a_name: names[a].clone(),
            b_name: names[b].clone(),
            distance_mm: c.distance,
            intersecting: c.intersecting,
            overlap_volume_mm3: overlap,
            point_a: c.point_a,
            point_b: c.point_b,
            status,
            clash,
        });
    }
    out.sort_by(|x, y| x.distance_mm.total_cmp(&y.distance_mm));
    let interference_count = out.iter().filter(|p| p.status == "interference").count();
    let min_clearance_mm = out
        .iter()
        .filter(|p| p.status == "clear")
        .map(|p| p.distance_mm)
        .reduce(f64::min);
    let total_overlap_mm3 = out.iter().map(|p| p.overlap_volume_mm3).sum();
    Ok(ClearanceReport {
        parts: names,
        pairs: out,
        interference_count,
        min_clearance_mm,
        total_overlap_mm3,
        skipped_parts,
    })
}

/// Clearance / interference over a document's parts. `request_json`
/// (nullable): `{"pairs"?: [[a, b], …], "include_mesh"?: bool}`. Returns
/// report JSON (free with `vcad_string_free`) or null with `vcad_last_error`.
#[no_mangle]
pub extern "C" fn vcad_clearance(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_clearance", || {
        let run = || -> Result<ClearanceReport, String> {
            let doc = parse_doc("document", doc_json)?;
            check(&doc, &parse_request(request_json)?)
        };
        match run() {
            Ok(r) => json_out(&r),
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
    use crate::json::take;
    use serde_json::json;
    use std::ffi::CString;

    fn doc(src: &str) -> vcad_ir::Document {
        vcad_loon::eval_vcad(src, None).unwrap()
    }

    #[test]
    fn interfering_boxes_report_overlap_volume() {
        // Two 10 mm cubes overlapping by 4 × 10 × 10 = 400 mm³.
        let d = doc(
            "[root [cube 10 10 10] \"steel\"]\n[root [translate 6 0 0 [cube 10 10 10]] \"steel\"]",
        );
        let r = check(&d, &json!({})).unwrap();
        assert_eq!(r.pairs.len(), 1, "{r:#?}");
        let p = &r.pairs[0];
        assert_eq!(p.status, "interference");
        assert!(
            (p.overlap_volume_mm3 - 400.0).abs() < 1.0,
            "{}",
            p.overlap_volume_mm3
        );
        // Coplanar side faces touch, so the kernel may report 0 depth here.
        assert!(p.distance_mm <= 0.0, "{}", p.distance_mm);
        assert!(p.clash.as_ref().is_some_and(|m| !m.indices.is_empty()));
        assert_eq!(r.interference_count, 1);
    }

    #[test]
    fn separated_boxes_report_clearance() {
        let d = doc(
            "[root [cube 10 10 10] \"steel\"]\n[root [translate 13 0 0 [cube 10 10 10]] \"steel\"]",
        );
        let r = check(&d, &json!({})).unwrap();
        let p = &r.pairs[0];
        assert_eq!(p.status, "clear");
        assert!((p.distance_mm - 3.0).abs() < 1e-3, "{}", p.distance_mm);
        assert_eq!(p.overlap_volume_mm3, 0.0);
        assert_eq!(
            r.min_clearance_mm.map(|v| (v * 1000.0).round()),
            Some(3000.0)
        );
    }

    #[test]
    fn c_abi_round_trip() {
        let d = doc(
            "[root [cube 10 10 10] \"steel\"]\n[root [translate 6 0 0 [cube 10 10 10]] \"steel\"]",
        );
        let dj = CString::new(serde_json::to_string(&d).unwrap()).unwrap();
        let out: Value =
            serde_json::from_str(&take(vcad_clearance(dj.as_ptr(), ptr::null()))).unwrap();
        assert_eq!(out["interference_count"], 1);
        let bad = CString::new(r#"{"pairs":[[0,7]]}"#).unwrap();
        assert!(vcad_clearance(dj.as_ptr(), bad.as_ptr()).is_null());
    }
}
