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

/// A drawn body plus every id a persisted assertion may use to name it:
/// the stringified root node id (plain parts), the instance id (assembly
/// instances), and the display name.
pub(crate) struct KeyedBody {
    /// Ids that select this body (root node id / instance id, then name).
    pub keys: Vec<String>,
    /// Display name.
    pub name: String,
    /// The posed solid, when it evaluated.
    pub solid: Option<Solid>,
}

/// [`bodies`], keeping the ids assertions refer to.
pub(crate) fn keyed_bodies(
    doc: &vcad_ir::Document,
    scene: &vcad_eval::EvaluatedScene,
) -> Vec<KeyedBody> {
    let roots = part_roots(doc, scene);
    let root_body = |i: usize| {
        let name = part_name(doc, roots[i], i);
        let mut keys: Vec<String> = roots[i].iter().map(|r| r.to_string()).collect();
        keys.push(name.clone());
        KeyedBody {
            keys,
            name,
            solid: scene.parts[i].solid.clone(),
        }
    };
    let (Some(instances), Some(defs)) = (doc.instances.as_ref(), scene.part_defs.as_ref()) else {
        return (0..scene.parts.len()).map(root_body).collect();
    };
    if instances.is_empty() {
        return (0..scene.parts.len()).map(root_body).collect();
    }
    let world = vcad_eval::solve_forward_kinematics(doc);
    let mut out: Vec<KeyedBody> = instances
        .iter()
        .filter_map(|inst| {
            let def = defs.iter().find(|d| d.id == inst.part_def_id)?;
            let solid =
                def.solid
                    .as_ref()
                    .map(|s| match world.get(&inst.id).or(inst.transform.as_ref()) {
                        Some(t) => place(s, t),
                        None => s.clone(),
                    });
            let name = inst.name.clone().unwrap_or_else(|| inst.id.clone());
            Some(KeyedBody {
                keys: vec![inst.id.clone(), name.clone()],
                name,
                solid,
            })
        })
        .collect();
    let def_roots: std::collections::HashSet<vcad_ir::NodeId> = doc
        .part_defs
        .as_ref()
        .map(|m| m.values().map(|d| d.root).collect())
        .unwrap_or_default();
    out.extend(
        (0..scene.parts.len())
            .filter(|&i| roots[i].is_none_or(|r| !def_roots.contains(&r)))
            .map(root_body),
    );
    out
}

/// Apply `T · Rz · Ry · Rx · S` (the convention `vcad_eval::kinematics`
/// emits) to a solid. One call per axis so the order is explicit rather than
/// left to `Solid::rotate`'s composite.
fn place(s: &Solid, t: &vcad_ir::Transform3D) -> Solid {
    let (sc, r, tr) = (t.scale, t.rotation, t.translation);
    let mut out = s.clone();
    if (sc.x, sc.y, sc.z) != (1.0, 1.0, 1.0) {
        out = out.scale(sc.x, sc.y, sc.z);
    }
    if r.x != 0.0 {
        out = out.rotate(r.x, 0.0, 0.0);
    }
    if r.y != 0.0 {
        out = out.rotate(0.0, r.y, 0.0);
    }
    if r.z != 0.0 {
        out = out.rotate(0.0, 0.0, r.z);
    }
    out.translate(tr.x, tr.y, tr.z)
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
    let mut out = Vec::new();
    for (a, b) in pairs {
        let (Some(sa), Some(sb)) = (solids[a].as_ref(), solids[b].as_ref()) else {
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
