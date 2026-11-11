//! Feature builders (`vcad_feature_append`): high-level modeling requests →
//! IR nodes in a `.vcad` document.
//!
//! The native app keeps the document as IR JSON (the same format the web app,
//! the MCP server and `vcad_engine_load_ir` speak). A builder takes that
//! document plus one feature request and returns the document with the new
//! nodes appended — so everything these produce saves, reloads, undoes and
//! evaluates exactly like web-authored geometry. Evaluate the result with
//! `vcad_scene_from_json`.
//!
//! Request shape: `{"kind": "...", ...}` — see [`Feature`]. Profiles are the
//! object `vcad_sketch_profile_json` emits.
//!
//! Root bookkeeping: when a feature consumes nodes that are scene roots
//! (`child`, `target`, `left`/`right`), the first consumed root is retargeted
//! to the new node and the other consumed roots are dropped — the usual CAD
//! "the hole replaces the body" behavior. A feature that consumes no root
//! (a fresh revolve, a cone) is added as a new root.

use std::collections::BTreeSet;
use std::ffi::c_char;
use std::ptr;

use serde::{Deserialize, Serialize};
use vcad_ir::{
    CsgOp, Document, EdgeQuery, Node, NodeId, PathCurve, SceneEntry, SketchSegment2D,
    TextAlignment, Vec3,
};

use crate::err;
use crate::json::{borrow_str, guard_ptr, json_out};

/// A closed 2D profile on a plane — the shape of `vcad_sketch_profile_json`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileIn {
    origin: Vec3,
    x_dir: Vec3,
    y_dir: Vec3,
    segments: Vec<SketchSegment2D>,
    #[serde(default)]
    holes: Option<Vec<Vec<SketchSegment2D>>>,
    /// Attach the sketch to a face of `target`, so it follows that face when
    /// upstream geometry changes (IR [`CsgOp::SketchOnFace`]). The stored
    /// frame stays as the placement inside the face's plane.
    #[serde(default)]
    on_face: Option<OnFaceIn>,
}

/// Where an attached sketch lives: a node and a face of its solid.
#[derive(Debug, Clone, Deserialize)]
pub struct OnFaceIn {
    target: NodeId,
    face: vcad_ir::FaceRef,
}

fn default_segments() -> u32 {
    64
}
fn default_font() -> String {
    "sans-serif".into()
}

/// Boolean flavor for [`Feature::Boolean`].
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoolOp {
    /// left ∪ right
    Union,
    /// left − right
    Difference,
    /// left ∩ right
    Intersection,
}

/// One modeling request. Node references (`child`, `target`, …) are IR node
/// ids already in the document.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Feature {
    /// Sketch2D + Extrude.
    Extrude {
        profile: ProfileIn,
        direction: Vec3,
        #[serde(default)]
        twist_angle: Option<f64>,
        #[serde(default)]
        scale_end: Option<f64>,
    },
    /// Extrude the profile and subtract it from `target`.
    CutExtrude {
        target: NodeId,
        profile: ProfileIn,
        direction: Vec3,
    },
    /// Extrude along `direction` exactly up to the plane of a picked face
    /// (`face_point`, `face_normal` — from `vcad_solid_pick`). With `target`,
    /// the result is unioned onto it (or subtracted when `cut`).
    ExtrudeToFace {
        #[serde(default)]
        target: Option<NodeId>,
        profile: ProfileIn,
        direction: Vec3,
        face_point: Vec3,
        face_normal: Vec3,
        #[serde(default)]
        cut: bool,
    },
    /// Sketch2D + Revolve about an axis (angle in degrees, 360 = full).
    Revolve {
        profile: ProfileIn,
        axis_origin: Vec3,
        axis_dir: Vec3,
        angle_deg: f64,
    },
    /// Sketch2D + Sweep along a path (Line / Helix / Cylindrical).
    Sweep {
        profile: ProfileIn,
        path: PathCurve,
        #[serde(default)]
        twist_angle: Option<f64>,
        #[serde(default)]
        scale_start: Option<f64>,
        #[serde(default)]
        scale_end: Option<f64>,
    },
    /// One Sketch2D per profile + Loft through them, in order.
    Loft {
        profiles: Vec<ProfileIn>,
        #[serde(default)]
        closed: Option<bool>,
    },
    /// Linear pattern of `child`.
    LinearPattern {
        child: NodeId,
        direction: Vec3,
        count: u32,
        spacing: f64,
    },
    /// Circular pattern of `child` (angle_deg = total sweep).
    CircularPattern {
        child: NodeId,
        axis_origin: Vec3,
        axis_dir: Vec3,
        count: u32,
        angle_deg: f64,
    },
    /// Mirror `child` across a plane; `keep_original` unions it with the copy.
    Mirror {
        child: NodeId,
        plane_origin: Vec3,
        plane_normal: Vec3,
        #[serde(default)]
        keep_original: bool,
    },
    /// Non-uniform scale about the origin.
    Scale { child: NodeId, factor: Vec3 },
    /// Text2D extruded by `depth` along the text plane's normal.
    Text {
        origin: Vec3,
        x_dir: Vec3,
        y_dir: Vec3,
        text: String,
        #[serde(default = "default_font")]
        font: String,
        height: f64,
        depth: f64,
        #[serde(default)]
        alignment: TextAlignment,
    },
    /// Cone / frustum primitive on +Z.
    Cone {
        radius_bottom: f64,
        radius_top: f64,
        height: f64,
        #[serde(default = "default_segments")]
        segments: u32,
    },
    /// Per-edge blend: `shape` 1 = fillet, 0 = chamfer; `size` = radius/leg.
    /// `edges` is an IR EdgeQuery — e.g. a `selector` from `vcad_topology_json`.
    EdgeBlend {
        child: NodeId,
        edges: EdgeQuery,
        size: f64,
        shape: f64,
    },
    /// Closed hollow shell (open-face shells live on `vcad_solid_shell_open`
    /// until the IR grows a removed-faces field).
    Shell { child: NodeId, thickness: f64 },
    /// Union / difference / intersection of two nodes.
    Boolean {
        op: BoolOp,
        left: NodeId,
        right: NodeId,
    },
}

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(flatten)]
    feature: Feature,
    /// Material for a newly added root (default "default").
    #[serde(default)]
    material: Option<String>,
    /// Display name for the result node.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize)]
struct Response<'a> {
    document: &'a Document,
    /// The node that carries the feature's result.
    node_id: NodeId,
    /// Every node added, in creation order.
    added: Vec<NodeId>,
}

struct Builder<'a> {
    doc: &'a mut Document,
    next: NodeId,
    added: Vec<NodeId>,
    consumed: BTreeSet<NodeId>,
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    Vec3::new(a.x - b.x, a.y - b.y, a.z - b.z)
}
fn dot(a: Vec3, b: Vec3) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z
}
fn scale(a: Vec3, s: f64) -> Vec3 {
    Vec3::new(a.x * s, a.y * s, a.z * s)
}

impl<'a> Builder<'a> {
    fn new(doc: &'a mut Document) -> Self {
        let next = doc.nodes.keys().copied().max().map_or(1, |m| m + 1);
        Self {
            doc,
            next,
            added: Vec::new(),
            consumed: BTreeSet::new(),
        }
    }

    fn node(&mut self, op: CsgOp) -> NodeId {
        let id = self.next;
        self.next += 1;
        self.doc.nodes.insert(id, Node { id, name: None, op });
        self.added.push(id);
        id
    }

    fn use_node(&mut self, id: NodeId) -> Result<NodeId, String> {
        if !self.doc.nodes.contains_key(&id) {
            return Err(format!("node {id} is not in the document"));
        }
        self.consumed.insert(id);
        Ok(id)
    }

    fn sketch(&mut self, p: &ProfileIn) -> Result<NodeId, String> {
        if p.segments.is_empty() {
            return Err("profile has no segments".into());
        }
        let sketch = self.node(CsgOp::Sketch2D {
            origin: p.origin,
            x_dir: p.x_dir,
            y_dir: p.y_dir,
            segments: p.segments.clone(),
            holes: p.holes.clone(),
        });
        let Some(on) = &p.on_face else {
            return Ok(sketch);
        };
        if !self.doc.nodes.contains_key(&on.target) {
            return Err(format!("node {} is not in the document", on.target));
        }
        if on.face.name.is_none() && on.face.point.is_none() {
            return Err("on_face needs a face name or point".into());
        }
        Ok(self.node(CsgOp::SketchOnFace {
            sketch,
            target: on.target,
            face: on.face.clone(),
        }))
    }

    fn extrude(&mut self, p: &ProfileIn, direction: Vec3) -> Result<NodeId, String> {
        if dot(direction, direction) <= 0.0 {
            return Err("extrude direction is zero".into());
        }
        let sketch = self.sketch(p)?;
        Ok(self.node(CsgOp::Extrude {
            sketch,
            direction,
            twist_angle: None,
            scale_end: None,
        }))
    }

    fn build(&mut self, f: &Feature) -> Result<NodeId, String> {
        Ok(match f {
            Feature::Extrude {
                profile,
                direction,
                twist_angle,
                scale_end,
            } => {
                let sketch = self.sketch(profile)?;
                self.node(CsgOp::Extrude {
                    sketch,
                    direction: *direction,
                    twist_angle: *twist_angle,
                    scale_end: *scale_end,
                })
            }
            Feature::CutExtrude {
                target,
                profile,
                direction,
            } => {
                let left = self.use_node(*target)?;
                let right = self.extrude(profile, *direction)?;
                self.node(CsgOp::Difference { left, right })
            }
            Feature::ExtrudeToFace {
                target,
                profile,
                direction,
                face_point,
                face_normal,
                cut,
            } => {
                let dn = dot(*direction, *face_normal);
                if dn.abs() < 1e-9 {
                    return Err("extrude direction is parallel to the target face".into());
                }
                let depth = dot(sub(*face_point, profile.origin), *face_normal) / dn;
                if depth.is_nan() || depth <= 1e-9 {
                    return Err(format!(
                        "target face is behind the profile (depth {depth:.4})"
                    ));
                }
                let tool = self.extrude(profile, scale(*direction, depth))?;
                match target {
                    None => tool,
                    Some(t) => {
                        let left = self.use_node(*t)?;
                        self.node(if *cut {
                            CsgOp::Difference { left, right: tool }
                        } else {
                            CsgOp::Union { left, right: tool }
                        })
                    }
                }
            }
            Feature::Revolve {
                profile,
                axis_origin,
                axis_dir,
                angle_deg,
            } => {
                let sketch = self.sketch(profile)?;
                self.node(CsgOp::Revolve {
                    sketch,
                    axis_origin: *axis_origin,
                    axis_dir: *axis_dir,
                    angle_deg: *angle_deg,
                })
            }
            Feature::Sweep {
                profile,
                path,
                twist_angle,
                scale_start,
                scale_end,
            } => {
                let sketch = self.sketch(profile)?;
                self.node(CsgOp::Sweep {
                    sketch,
                    path: path.clone(),
                    twist_angle: *twist_angle,
                    scale_start: *scale_start,
                    scale_end: *scale_end,
                    orientation: None,
                    path_segments: None,
                    arc_segments: None,
                })
            }
            Feature::Loft { profiles, closed } => {
                if profiles.len() < 2 {
                    return Err("loft needs at least 2 profiles".into());
                }
                let sketches = profiles
                    .iter()
                    .map(|p| self.sketch(p))
                    .collect::<Result<_, _>>()?;
                self.node(CsgOp::Loft {
                    sketches,
                    closed: *closed,
                })
            }
            Feature::LinearPattern {
                child,
                direction,
                count,
                spacing,
            } => {
                let child = self.use_node(*child)?;
                self.node(CsgOp::LinearPattern {
                    child,
                    direction: *direction,
                    count: *count,
                    spacing: *spacing,
                })
            }
            Feature::CircularPattern {
                child,
                axis_origin,
                axis_dir,
                count,
                angle_deg,
            } => {
                let child = self.use_node(*child)?;
                self.node(CsgOp::CircularPattern {
                    child,
                    axis_origin: *axis_origin,
                    axis_dir: *axis_dir,
                    count: *count,
                    angle_deg: *angle_deg,
                })
            }
            Feature::Mirror {
                child,
                plane_origin,
                plane_normal,
                keep_original,
            } => {
                let child = self.use_node(*child)?;
                let m = self.node(CsgOp::Mirror {
                    child,
                    plane_origin: *plane_origin,
                    plane_normal: *plane_normal,
                });
                if *keep_original {
                    self.node(CsgOp::Union {
                        left: child,
                        right: m,
                    })
                } else {
                    m
                }
            }
            Feature::Scale { child, factor } => {
                let child = self.use_node(*child)?;
                self.node(CsgOp::Scale {
                    child,
                    factor: *factor,
                })
            }
            Feature::Text {
                origin,
                x_dir,
                y_dir,
                text,
                font,
                height,
                depth,
                alignment,
            } => {
                if text.trim().is_empty() {
                    return Err("text is empty".into());
                }
                let sketch = self.node(CsgOp::Text2D {
                    origin: *origin,
                    x_dir: *x_dir,
                    y_dir: *y_dir,
                    text: text.clone(),
                    font: font.clone(),
                    height: *height,
                    letter_spacing: None,
                    line_spacing: None,
                    alignment: *alignment,
                });
                let n = Vec3::new(
                    x_dir.y * y_dir.z - x_dir.z * y_dir.y,
                    x_dir.z * y_dir.x - x_dir.x * y_dir.z,
                    x_dir.x * y_dir.y - x_dir.y * y_dir.x,
                );
                let len = dot(n, n).sqrt();
                if len < 1e-12 {
                    return Err("text plane axes are parallel".into());
                }
                self.node(CsgOp::Extrude {
                    sketch,
                    direction: scale(n, depth / len),
                    twist_angle: None,
                    scale_end: None,
                })
            }
            Feature::Cone {
                radius_bottom,
                radius_top,
                height,
                segments,
            } => self.node(CsgOp::Cone {
                radius_bottom: *radius_bottom,
                radius_top: *radius_top,
                height: *height,
                segments: *segments,
            }),
            Feature::EdgeBlend {
                child,
                edges,
                size,
                shape,
            } => {
                let child = self.use_node(*child)?;
                self.node(CsgOp::EdgeBlend {
                    child,
                    edges: edges.clone(),
                    profile: vcad_ir::BlendProfile::Constant {
                        size: *size,
                        shape: *shape,
                    },
                })
            }
            Feature::Shell { child, thickness } => {
                let child = self.use_node(*child)?;
                self.node(CsgOp::Shell {
                    child,
                    thickness: *thickness,
                    open_faces: Vec::new(),
                })
            }
            Feature::Boolean { op, left, right } => {
                let left = self.use_node(*left)?;
                let right = self.use_node(*right)?;
                self.node(match op {
                    BoolOp::Union => CsgOp::Union { left, right },
                    BoolOp::Difference => CsgOp::Difference { left, right },
                    BoolOp::Intersection => CsgOp::Intersection { left, right },
                })
            }
        })
    }

    fn retarget_roots(&mut self, result: NodeId, material: Option<String>) {
        let mut placed = false;
        self.doc.roots.retain_mut(|r| {
            if !self.consumed.contains(&r.root) {
                return true;
            }
            if placed {
                return false;
            }
            r.root = result;
            placed = true;
            true
        });
        if !placed {
            self.doc.roots.push(SceneEntry {
                root: result,
                material: material.unwrap_or_else(|| "default".into()),
                visible: None,
            });
        }
    }
}

/// Apply one feature request to `doc` in place; returns (result node, added).
pub fn append_feature(
    doc: &mut Document,
    request_json: &str,
) -> Result<(NodeId, Vec<NodeId>), String> {
    let req: Request = serde_json::from_str(request_json).map_err(|e| format!("request: {e}"))?;
    let mut b = Builder::new(doc);
    let result = b.build(&req.feature)?;
    if let Some(name) = req.name {
        if let Some(n) = b.doc.nodes.get_mut(&result) {
            n.name = Some(name);
        }
    }
    b.retarget_roots(result, req.material);
    Ok((result, b.added))
}

/// Append one feature to an IR document.
///
/// `doc_json`: a `.vcad` IR document (null or empty → a fresh document).
/// `feature_json`: `{"kind": "revolve" | "extrude" | "cut_extrude" |
/// "extrude_to_face" | "sweep" | "loft" | "linear_pattern" |
/// "circular_pattern" | "mirror" | "scale" | "text" | "cone" | "edge_blend" |
/// "shell" | "boolean", ...fields, "material"?, "name"?}`.
///
/// Returns `{"document": <Document>, "node_id": N, "added": [..]}`, or null
/// with `vcad_last_error` on malformed input / unknown node ids. The builder
/// does not evaluate; run the document through `vcad_scene_from_json` to get
/// geometry (and kernel-level errors). Free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_feature_append(
    doc_json: *const c_char,
    feature_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    let doc_text = if doc_json.is_null() {
        ""
    } else {
        let Some(t) = borrow_str("document", doc_json) else {
            return ptr::null_mut();
        };
        t
    };
    let Some(feature) = borrow_str("feature", feature_json) else {
        return ptr::null_mut();
    };
    guard_ptr("feature", || {
        let mut doc = if doc_text.trim().is_empty() {
            Document::new()
        } else {
            match Document::from_json(doc_text) {
                Ok(d) => d,
                Err(e) => {
                    err::set_error(format!("document: {e}"));
                    return ptr::null_mut();
                }
            }
        };
        match append_feature(&mut doc, feature) {
            Ok((node_id, added)) => json_out(&Response {
                document: &doc,
                node_id,
                added,
            }),
            Err(e) => {
                err::set_error(format!("feature: {e}"));
                ptr::null_mut()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;
    use std::ffi::CString;
    use vcad_eval::{evaluate_document, EvalOptions};

    fn append(doc: Option<&str>, feature: serde_json::Value) -> (String, NodeId) {
        let d = doc.map(|d| CString::new(d).unwrap());
        let f = CString::new(feature.to_string()).unwrap();
        let out = take(vcad_feature_append(
            d.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            f.as_ptr(),
        ));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        (v["document"].to_string(), v["node_id"].as_u64().unwrap())
    }

    fn volumes(doc: &str) -> Vec<f64> {
        let doc = Document::from_json(doc).unwrap();
        let scene = evaluate_document(&doc, &EvalOptions::default()).unwrap();
        assert!(scene.failures.is_empty(), "{:?}", scene.failures);
        scene
            .parts
            .iter()
            .map(|p| p.solid.as_ref().expect("solid").volume())
            .collect()
    }

    fn xz_rect(x0: f64, x1: f64, z0: f64, z1: f64) -> serde_json::Value {
        // Sketch on the XZ plane (x_dir = X, y_dir = Z).
        let p = |x: f64, y: f64| serde_json::json!({"x": x, "y": y});
        serde_json::json!({
            "origin": {"x":0,"y":0,"z":0}, "x_dir": {"x":1,"y":0,"z":0}, "y_dir": {"x":0,"y":0,"z":1},
            "segments": [
                {"type":"Line","start":p(x0,z0),"end":p(x1,z0)},
                {"type":"Line","start":p(x1,z0),"end":p(x1,z1)},
                {"type":"Line","start":p(x1,z1),"end":p(x0,z1)},
                {"type":"Line","start":p(x0,z1),"end":p(x0,z0)},
            ]
        })
    }

    fn xy_square(z: f64, half: f64) -> serde_json::Value {
        let p = |x: f64, y: f64| serde_json::json!({"x": x, "y": y});
        serde_json::json!({
            "origin": {"x":0,"y":0,"z":z}, "x_dir": {"x":1,"y":0,"z":0}, "y_dir": {"x":0,"y":1,"z":0},
            "segments": [
                {"type":"Line","start":p(-half,-half),"end":p(half,-half)},
                {"type":"Line","start":p(half,-half),"end":p(half,half)},
                {"type":"Line","start":p(half,half),"end":p(-half,half)},
                {"type":"Line","start":p(-half,half),"end":p(-half,-half)},
            ]
        })
    }

    #[test]
    fn revolve_a_profile_into_a_tube() {
        // Rect r∈[5,10], z∈[0,20] revolved 360° about Z → π(10²−5²)·20.
        let (doc, _) = append(
            None,
            serde_json::json!({
                "kind": "revolve", "profile": xz_rect(5.0, 10.0, 0.0, 20.0),
                "axis_origin": {"x":0,"y":0,"z":0}, "axis_dir": {"x":0,"y":0,"z":1}, "angle_deg": 360.0
            }),
        );
        let v = volumes(&doc);
        let expect = std::f64::consts::PI * 75.0 * 20.0;
        assert_eq!(v.len(), 1);
        assert!(
            (v[0] - expect).abs() / expect < 0.02,
            "{} vs {expect}",
            v[0]
        );
    }

    #[test]
    fn loft_two_squares_into_a_frustum() {
        // 20×20 at z=0 → 10×10 at z=10: V = h/3 (A1 + A2 + √(A1A2)) = 10/3·(400+100+200).
        let (doc, _) = append(
            None,
            serde_json::json!({"kind": "loft", "profiles": [xy_square(0.0, 10.0), xy_square(10.0, 5.0)]}),
        );
        let v = volumes(&doc);
        let expect = 10.0 / 3.0 * 700.0;
        assert!(
            (v[0] - expect).abs() / expect < 0.02,
            "{} vs {expect}",
            v[0]
        );
    }

    #[test]
    fn cut_extrude_retargets_the_root_and_patterns_compose() {
        let (doc, base) = append(
            None,
            serde_json::json!({"kind": "extrude", "profile": xy_square(0.0, 10.0), "direction": {"x":0,"y":0,"z":10}}),
        );
        // Through-cut a 4×4 pocket (z from 10 down past 0).
        let (doc, cut) = append(
            Some(&doc),
            serde_json::json!({"kind": "cut_extrude", "target": base,
                "profile": xy_square(12.0, 2.0), "direction": {"x":0,"y":0,"z":-14}}),
        );
        let parsed = Document::from_json(&doc).unwrap();
        assert_eq!(parsed.roots.len(), 1);
        assert_eq!(parsed.roots[0].root, cut);
        let v = volumes(&doc);
        assert!((v[0] - (4000.0 - 160.0)).abs() < 2.0, "{v:?}");

        let (doc, _) = append(
            Some(&doc),
            serde_json::json!({"kind": "linear_pattern", "child": cut, "direction": {"x":1,"y":0,"z":0},
                "count": 2, "spacing": 30.0}),
        );
        let v = volumes(&doc);
        assert!((v[0] - 2.0 * 3840.0).abs() < 4.0, "{v:?}");
    }

    #[test]
    fn profile_on_face_attaches_the_sketch() {
        let (doc, base) = append(
            None,
            serde_json::json!({"kind": "extrude", "profile": xy_square(0.0, 10.0), "direction": {"x":0,"y":0,"z":4}}),
        );
        let mut boss = xy_square(4.0, 2.0);
        boss["on_face"] =
            serde_json::json!({"target": base, "face": {"name": format!("n{base}:end")}});
        let (doc, _) = append(
            Some(&doc),
            serde_json::json!({"kind": "extrude", "profile": boss, "direction": {"x":0,"y":0,"z":3}}),
        );
        let parsed = Document::from_json(&doc).unwrap();
        assert!(
            parsed
                .nodes
                .values()
                .any(|n| matches!(n.op, CsgOp::SketchOnFace { target, .. } if target == base)),
            "a SketchOnFace node targets the base"
        );
        let v = volumes(&doc);
        assert!(
            v.iter().any(|v| (v - 48.0).abs() < 0.5),
            "boss 4x4x3 evaluates: {v:?}"
        );
    }

    #[test]
    fn extrude_to_face_reaches_the_plane() {
        let (doc, _) = append(
            None,
            serde_json::json!({"kind": "extrude_to_face", "profile": xy_square(0.0, 5.0),
                "direction": {"x":0,"y":0,"z":1},
                "face_point": {"x":0,"y":0,"z":7.5}, "face_normal": {"x":0,"y":0,"z":-1}}),
        );
        let v = volumes(&doc);
        assert!((v[0] - 750.0).abs() < 1.0, "{v:?}");
    }

    #[test]
    fn primitives_and_transforms_build_valid_ir() {
        let (doc, cone) = append(
            None,
            serde_json::json!({"kind":"cone","radius_bottom":5.0,"radius_top":0.0,"height":12.0}),
        );
        let one = std::f64::consts::PI * 25.0 * 12.0 / 3.0;
        assert!((volumes(&doc)[0] - one).abs() / one < 0.03);

        // Mirror is checked on a box: mirroring a cone currently yields a
        // wrong-volume solid in the kernel (418 vs 314 mm³, orientation
        // looks un-flipped) — a kernel bug tracked separately.
        let (bdoc, base) = append(
            None,
            serde_json::json!({"kind": "extrude", "profile": xy_square(0.0, 5.0), "direction": {"x":0,"y":0,"z":4}}),
        );
        let (bdoc, _) = append(
            Some(&bdoc),
            serde_json::json!({"kind":"mirror","child":base,"plane_origin":{"x":0,"y":0,"z":-1},
                "plane_normal":{"x":0,"y":0,"z":1},"keep_original":true}),
        );
        let v = volumes(&bdoc);
        assert!((v[0] - 800.0).abs() < 1.0, "{v:?}");

        let (doc, scaled) = append(
            Some(&doc),
            serde_json::json!({"kind":"scale","child":cone,"factor":{"x":2,"y":2,"z":2}}),
        );
        assert!(Document::from_json(&doc)
            .unwrap()
            .nodes
            .contains_key(&scaled));

        // Text: only checked as IR (the FFI static lib ships without the builtin font).
        let (doc, text) = append(
            Some(&doc),
            serde_json::json!({"kind":"text","origin":{"x":0,"y":0,"z":0},"x_dir":{"x":1,"y":0,"z":0},
                "y_dir":{"x":0,"y":1,"z":0},"text":"vcad","height":5.0,"depth":1.0}),
        );
        let parsed = Document::from_json(&doc).unwrap();
        assert!(matches!(parsed.nodes[&text].op, CsgOp::Extrude { .. }));
    }

    #[test]
    fn bad_requests_fail_closed() {
        let f = CString::new(r#"{"kind":"shell","child":42,"thickness":1}"#).unwrap();
        assert!(vcad_feature_append(ptr::null(), f.as_ptr()).is_null());
        let f = CString::new(r#"{"kind":"warp_drive"}"#).unwrap();
        assert!(vcad_feature_append(ptr::null(), f.as_ptr()).is_null());
        let f = CString::new(r#"{"kind":"loft","profiles":[]}"#).unwrap();
        assert!(vcad_feature_append(ptr::null(), f.as_ptr()).is_null());
    }
}
