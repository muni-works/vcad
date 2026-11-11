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

use vcad_kernel::Solid;
use vcad_kernel_tessellate::{mesh_clearance, TriangleMesh};

use crate::analysis::{evaluate, parse_doc, parse_request};
use crate::err;
use crate::json::{guard_ptr, json_out};
use vcad_receipt_verify::bodies::keyed_bodies;

/// Overlap below this volume (mm³) is contact, not interference.
pub const CONTACT_VOLUME_MM3: f64 = 1e-6;

/// Above this many parts, unlisted pairs go through an AABB broadphase and
/// only near pairs are measured exactly (small documents keep every pair).
pub const BROADPHASE_MIN_PARTS: usize = 64;
/// Broadphase reach (mm) when the request gives no `near_mm`.
pub const DEFAULT_NEAR_MM: f64 = 5.0;

fn mesh_aabb(m: &TriangleMesh) -> Option<([f64; 3], [f64; 3])> {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for (i, &c) in m.vertices.iter().enumerate() {
        let k = i % 3;
        lo[k] = lo[k].min(c as f64);
        hi[k] = hi[k].max(c as f64);
    }
    (lo[0] <= hi[0]).then_some((lo, hi))
}

/// Lower bound on the distance between two boxes (0 when they overlap).
fn aabb_gap(a: &([f64; 3], [f64; 3]), b: &([f64; 3], [f64; 3])) -> f64 {
    let mut d2 = 0.0;
    for k in 0..3 {
        let g = (b.0[k] - a.1[k]).max(a.0[k] - b.1[k]).max(0.0);
        d2 += g * g;
    }
    d2.sqrt()
}

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

/// The bodies the viewport draws, `(name, solid)`, in render order. A plain
/// document draws its visible roots. An assembly draws its instances, each
/// part-def solid posed by forward kinematics (falling back to the
/// instance's own transform), followed by the free roots — visible roots
/// that are not some part def's root. Checking only the roots of an
/// assembly would test the unposed part defs, not what the user built.
fn bodies(
    doc: &vcad_ir::Document,
    scene: &vcad_eval::EvaluatedScene,
) -> Vec<(String, Option<Solid>)> {
    keyed_bodies(doc, scene)
        .into_iter()
        .map(|b| (b.name, b.solid))
        .collect()
}

/// Measure every pair of bodies (or only `req.pairs: [[a, b], …]`).
pub fn check(doc: &vcad_ir::Document, req: &Value) -> Result<ClearanceReport, String> {
    let scene = evaluate(doc)?;
    let (names, solids): (Vec<String>, Vec<Option<Solid>>) =
        bodies(doc, &scene).into_iter().unzip();
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
    let skipped_parts = solids
        .iter()
        .enumerate()
        .filter(|(_, s)| s.as_ref().is_none_or(|s| s.is_empty()))
        .map(|(i, _)| names[i].clone())
        .collect();
    // Tessellate each part once. `Solid::clearance` meshes both operands on
    // every call, so the all-pairs loop used to re-tessellate every part
    // 2·(n−1) times — minutes on a 24-link robot, a hang on a printer.
    let meshes: Vec<Option<TriangleMesh>> = solids
        .iter()
        .map(|s| {
            s.as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_mesh(s.segments()))
        })
        .collect();
    let boxes: Vec<Option<([f64; 3], [f64; 3])>> = meshes
        .iter()
        .map(|m| m.as_ref().and_then(mesh_aabb))
        .collect();
    // Broadphase: in a big assembly the far pairs are noise (a frame rail
    // vs a hotend screw), and exact distance on all n² of them never ends.
    // Only pairs whose boxes come within `near_mm` get the exact check.
    let broadphase = req["pairs"].is_null() && names.len() > BROADPHASE_MIN_PARTS;
    let near_mm = req["near_mm"].as_f64().unwrap_or(DEFAULT_NEAR_MM);
    let mut out = Vec::new();
    for (a, b) in pairs {
        let (Some(sa), Some(sb)) = (solids[a].as_ref(), solids[b].as_ref()) else {
            continue;
        };
        let (Some(ma), Some(mb)) = (meshes[a].as_ref(), meshes[b].as_ref()) else {
            continue;
        };
        if broadphase {
            match (boxes[a], boxes[b]) {
                (Some(ba), Some(bb)) if aabb_gap(&ba, &bb) <= near_mm => {}
                _ => continue,
            }
        }
        let Some(c) = mesh_clearance(ma, mb) else {
            continue;
        };
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
    fn large_assembly_broadphase_keeps_near_pairs_only() {
        // A row of 70 cubes 12 mm apart (2 mm gaps) plus one far cube that
        // overlaps the last row cube's neighbour-free space at x = 5000.
        let mut src = String::new();
        for i in 0..70 {
            src.push_str(&format!(
                "[root [translate {} 0 0 [cube 10 10 10]] \"steel\"]\n",
                i * 12
            ));
        }
        src.push_str("[root [translate 5000 0 0 [cube 10 10 10]] \"steel\"]\n");
        src.push_str("[root [translate 5005 0 0 [cube 10 10 10]] \"steel\"]\n");
        let d = doc(&src);
        let r = check(&d, &json!({})).unwrap();
        assert_eq!(r.parts.len(), 72);
        // 69 neighbour gaps + the one far interfering pair; never all 2556.
        assert_eq!(r.pairs.len(), 70, "{}", r.pairs.len());
        assert_eq!(r.interference_count, 1);
        assert!((r.min_clearance_mm.unwrap() - 2.0).abs() < 1e-3);
        // A wider reach pulls in the next ring of pairs (14 mm apart).
        let wide = check(&d, &json!({"near_mm": 15.0, "include_mesh": false})).unwrap();
        assert_eq!(wide.pairs.len(), 70 + 68);
        // Explicit pairs bypass the broadphase.
        let one = check(&d, &json!({"pairs": [[0, 71]]})).unwrap();
        assert_eq!(one.pairs.len(), 1);
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

    /// Two named 10 mm cubes, the second placed by an unnamed Translate —
    /// exactly what the app's "Add Cube, then click" writes.
    fn placed_doc(dx: f64) -> vcad_ir::Document {
        serde_json::from_value(json!({
            "version": "0.1",
            "nodes": {
                "1": {"id": 1, "name": "Box", "op": {"type": "Cube", "size": {"x": 10, "y": 10, "z": 10}}},
                "2": {"id": 2, "name": "Wedge", "op": {"type": "Cube", "size": {"x": 10, "y": 10, "z": 10}}},
                "3": {"id": 3, "name": null, "op": {"type": "Translate", "child": 2, "offset": {"x": dx, "y": 0, "z": 0}}}
            },
            "materials": {},
            "part_materials": {},
            "roots": [{"root": 1, "material": "default"}, {"root": 3, "material": "default"}]
        }))
        .unwrap()
    }

    #[test]
    fn pairs_use_feature_names_through_transform_wrappers() {
        let r = check(&placed_doc(13.0), &json!({})).unwrap();
        assert_eq!(r.parts, vec!["Box", "Wedge"], "{r:#?}");
        assert_eq!(r.pairs[0].a_name, "Box");
        assert_eq!(r.pairs[0].b_name, "Wedge");
    }

    #[test]
    fn assembly_checks_posed_instances_by_instance_name() {
        // One part def (the Box root), two instances: "Box 1" at the origin
        // and "Box 2" moved +6 in x — 400 mm³ of overlap that only exists in
        // the posed assembly. The Wedge root is not a part def: a free root.
        let mut d = placed_doc(100.0);
        let v: Value = json!({
            "partDefs": {"pd1": {"id": "pd1", "name": "Box", "root": 1}},
            "instances": [
                {"id": "i1", "partDefId": "pd1", "name": "Box 1"},
                {"id": "i2", "partDefId": "pd1", "name": "Box 2",
                 "transform": {"translation": {"x": 6, "y": 0, "z": 0},
                               "rotation": {"x": 0, "y": 0, "z": 0},
                               "scale": {"x": 1, "y": 1, "z": 1}}}
            ]
        });
        d.part_defs = serde_json::from_value(v["partDefs"].clone()).unwrap();
        d.instances = serde_json::from_value(v["instances"].clone()).unwrap();
        let r = check(&d, &json!({})).unwrap();
        assert_eq!(r.parts, vec!["Box 1", "Box 2", "Wedge"], "{r:#?}");
        let worst = &r.pairs[0];
        assert_eq!(
            (worst.a_name.as_str(), worst.b_name.as_str()),
            ("Box 1", "Box 2")
        );
        assert_eq!(worst.status, "interference");
        assert!(
            (worst.overlap_volume_mm3 - 400.0).abs() < 1.0,
            "{}",
            worst.overlap_volume_mm3
        );
        assert_eq!(r.interference_count, 1);
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
