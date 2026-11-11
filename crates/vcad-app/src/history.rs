//! Feature history as the feature tree sees it: build state per feature
//! (from the materializer), plus references that no longer resolve.
//!
//! A broken reference is found by resolving it, not by guessing: a
//! sketch-on-face (`SketchOnFace`) whose face name and hint no longer pick
//! exactly one face of its target is reported with the failure and a ranked
//! list of planar faces the user can re-pick. Nothing is rebound silently.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use vcad_ir::{CsgOp, Document, NodeId};
use vcad_kernel::Solid;

use crate::document_api::DocumentApi;
use crate::materializer::{materialize, HistoryEntry, HistoryState};

/// One face a broken reference could be re-pointed at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceCandidate {
    /// Persistent face name (what the repaired reference stores).
    pub name: String,
    /// A point on the face's plane.
    pub point: [f64; 3],
    /// Outward unit normal.
    pub normal: [f64; 3],
    /// Lower is a better match for the lost reference.
    pub score: f64,
}

/// A reference inside a feature that no longer resolves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokenRef {
    /// The IR node holding the reference (a `SketchOnFace`).
    pub node: NodeId,
    /// `face`.
    pub kind: String,
    /// The stored face name, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Why it failed, in the resolver's words.
    pub message: String,
    /// Planar faces of the target, best match first.
    pub candidates: Vec<FaceCandidate>,
}

/// A history row plus what evaluation found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRow {
    /// Materializer state.
    #[serde(flatten)]
    pub entry: HistoryEntry,
    /// References that no longer resolve (empty when fine).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broken: Vec<BrokenRef>,
}

/// Max candidates offered per broken reference.
const MAX_CANDIDATES: usize = 8;

impl DocumentApi {
    /// Every modeling feature in history order, with its state and any
    /// broken references. Only built features with reference-bearing nodes
    /// are evaluated, so this stays cheap for plain parts.
    pub fn feature_history(&self) -> Vec<HistoryRow> {
        let result = materialize(self.crdt());
        let doc = &result.document;
        // Each reference is reported on the feature that holds it: a walk
        // stops at other features' roots (a cut doesn't re-report its tool).
        let roots: HashSet<NodeId> = result.history.iter().filter_map(|h| h.node).collect();
        result
            .history
            .into_iter()
            .map(|entry| {
                let broken = match (entry.state, entry.node) {
                    (HistoryState::Built, Some(node)) => broken_refs(doc, node, &roots),
                    _ => Vec::new(),
                };
                HistoryRow { entry, broken }
            })
            .collect()
    }
}

/// `SketchOnFace` nodes reachable from `root` whose face doesn't resolve.
fn broken_refs(doc: &Document, root: NodeId, other_roots: &HashSet<NodeId>) -> Vec<BrokenRef> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    let mut cache = HashMap::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) || (id != root && other_roots.contains(&id)) {
            continue;
        }
        let Some(node) = doc.nodes.get(&id) else {
            continue;
        };
        if let CsgOp::SketchOnFace { target, face, .. } = &node.op {
            if let Some(b) = check_face_ref(doc, id, *target, face, &mut cache) {
                out.push(b);
            }
        }
        stack.extend(node.op.child_ids());
    }
    out.sort_by_key(|b| b.node);
    out
}

fn check_face_ref(
    doc: &Document,
    node: NodeId,
    target: NodeId,
    face: &vcad_ir::FaceRef,
    cache: &mut HashMap<NodeId, Option<Solid>>,
) -> Option<BrokenRef> {
    let broken = |message: String, candidates| BrokenRef {
        node,
        kind: "face".into(),
        reference: face.name.clone(),
        message,
        candidates,
    };
    let solid = match vcad_eval::evaluate::evaluate_node(target, &doc.nodes, cache) {
        Ok(Some(s)) => s,
        // The target itself failing is its own feature's problem, not this
        // reference's.
        _ => return None,
    };
    let r = vcad_eval::convert::to_face_refs(std::slice::from_ref(face)).pop()?;
    let fid = match solid.resolve_face(r.name.as_deref(), r.point, r.normal) {
        Ok(f) => f,
        Err(e) => {
            // `resolve_face` reports through the shell error type.
            let msg = e.to_string();
            let msg = msg
                .strip_prefix("shell failed: ")
                .unwrap_or(&msg)
                .to_string();
            return Some(broken(msg, rank_faces(&solid, face)));
        }
    };
    if solid.face_plane(fid).is_none() {
        return Some(broken(
            "the face is no longer planar".into(),
            rank_faces(&solid, face),
        ));
    }
    None
}

/// Planar named faces of `solid`, best match for `lost` first: same tag as
/// the lost name, then normal agreement and distance from its hint point.
pub fn rank_faces(solid: &Solid, lost: &vcad_ir::FaceRef) -> Vec<FaceCandidate> {
    let Some(names) = solid.names() else {
        return Vec::new();
    };
    let lost_tag = lost
        .name
        .as_deref()
        .and_then(|n| n.split_once(':'))
        .map(|(_, rest)| rest.split('.').next().unwrap_or(rest).to_string());
    let v = |p: &vcad_ir::Vec3| [p.x, p.y, p.z];
    let hint_n = lost.normal.as_ref().map(v);
    let hint_p = lost.point.as_ref().map(v);
    let mut out: Vec<FaceCandidate> = names
        .faces
        .iter()
        .filter_map(|(fid, name)| {
            let (p, n) = solid.face_plane(*fid)?;
            let point = [p.x, p.y, p.z];
            let normal = [n.x, n.y, n.z];
            let mut score = 0.0;
            if lost_tag.as_deref() != Some(name.tag.as_str()) {
                score += 1.0;
            }
            if let Some(h) = hint_n {
                let len = (h[0] * h[0] + h[1] * h[1] + h[2] * h[2]).sqrt().max(1e-12);
                let dot = (h[0] * normal[0] + h[1] * normal[1] + h[2] * normal[2]) / len;
                score += 1.0 - dot; // 0 aligned … 2 opposite
            }
            if let Some(h) = hint_p {
                let d = (0..3).map(|i| (h[i] - point[i]) * normal[i]).sum::<f64>();
                score += d.abs() / 100.0;
            }
            Some(FaceCandidate {
                name: name.to_string(),
                point,
                normal,
                score,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(MAX_CANDIDATES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::DocCommand;
    use crate::feature::FeatureInput;
    use crate::materializer::{ROLLBACK_MARKER, SUPPRESSED};
    use vcad_crdt::{ReplicaId, Value};

    fn api() -> DocumentApi {
        DocumentApi::new(ReplicaId(1))
    }

    fn cube(s: f64) -> FeatureInput {
        FeatureInput::Cube {
            size_x: s,
            size_y: s,
            size_z: s,
        }
    }

    fn add(api: &mut DocumentApi, f: FeatureInput) -> String {
        api.apply(DocCommand::AddFeature {
            feature: f,
            name: None,
        })
        .unwrap()
        .created_feature_id
        .unwrap()
    }

    fn set(api: &mut DocumentApi, id: &str, key: &str, on: bool) {
        api.apply(DocCommand::SetParam {
            id: id.into(),
            key: key.into(),
            value: Value::Bool(on),
        })
        .unwrap();
    }

    fn states(api: &DocumentApi) -> Vec<(String, HistoryState)> {
        api.feature_history()
            .into_iter()
            .map(|r| (r.entry.id, r.entry.state))
            .collect()
    }

    fn total_volume(api: &DocumentApi) -> f64 {
        let doc = api.get_state().document;
        vcad_eval::evaluate_root_solids(&doc)
            .unwrap()
            .iter()
            .filter_map(|r| r.solid.as_ref().map(|s| s.volume()))
            .sum()
    }

    /// cube A (10), cube B (4) — cut: A − B.
    fn cut(api: &mut DocumentApi) -> [String; 3] {
        let a = add(api, cube(10.0));
        let b = add(api, cube(4.0));
        let c = add(
            api,
            FeatureInput::Boolean {
                boolean_type: crate::feature::BooleanType::Difference,
                input_a: a.clone(),
                input_b: b.clone(),
            },
        );
        [a, b, c]
    }

    #[test]
    fn suppressing_a_tool_body_skips_the_cut_and_restores_its_input() {
        let mut api = api();
        let [a, b, c] = cut(&mut api);
        assert!((total_volume(&api) - (1000.0 - 64.0)).abs() < 1e-6);
        set(&mut api, &b, SUPPRESSED, true);
        assert_eq!(
            states(&api),
            [
                (a.clone(), HistoryState::Built),
                (b.clone(), HistoryState::Suppressed),
                (c.clone(), HistoryState::Skipped),
            ]
        );
        let rows = api.feature_history();
        assert_eq!(rows[2].entry.skipped_by.as_deref(), Some(b.as_str()));
        assert!(rows[0].entry.root, "the uncut input is drawn again");
        // Only A is built: the cut is gone, not just hidden.
        assert!((total_volume(&api) - 1000.0).abs() < 1e-6);
        assert!(api.get_state().warnings.is_empty());
        set(&mut api, &b, SUPPRESSED, false);
        assert!((total_volume(&api) - (1000.0 - 64.0)).abs() < 1e-6);
    }

    #[test]
    fn rollback_marker_builds_a_prefix_and_new_features_land_at_it() {
        let mut api = api();
        let [a, b, c] = cut(&mut api);
        set(&mut api, &a, ROLLBACK_MARKER, true);
        assert_eq!(
            states(&api),
            [
                (a.clone(), HistoryState::Built),
                (b.clone(), HistoryState::PastMarker),
                (c.clone(), HistoryState::PastMarker),
            ]
        );
        // Prefix build equals a cold build of just the prefix.
        assert!((total_volume(&api) - 1000.0).abs() < 1e-6);
        // A feature added while rolled back goes in at the marker, which
        // moves onto it.
        let d = add(&mut api, cube(2.0));
        let rows = api.feature_history();
        let order: Vec<&str> = rows.iter().map(|r| r.entry.id.as_str()).collect();
        assert_eq!(order, [a.as_str(), d.as_str(), b.as_str(), c.as_str()]);
        assert!(rows[1].entry.marker && !rows[0].entry.marker);
        assert!((total_volume(&api) - 1008.0).abs() < 1e-6);
        // Roll to the end.
        set(&mut api, &d, ROLLBACK_MARKER, false);
        assert!(states(&api).iter().all(|(_, s)| *s == HistoryState::Built));
    }

    #[test]
    fn suppress_and_marker_survive_an_ir_round_trip() {
        let mut api = api();
        let [a, b, c] = cut(&mut api);
        let d = add(&mut api, cube(3.0));
        set(&mut api, &b, SUPPRESSED, true);
        set(&mut api, &c, ROLLBACK_MARKER, true);
        let before = states(&api);
        let ir = serde_json::to_value(api.get_state().document).unwrap();
        assert!(ir.get("history").is_some());

        let mut fresh = DocumentApi::new(ReplicaId(2));
        fresh.apply(DocCommand::ImportIr { document: ir }).unwrap();
        let after: Vec<HistoryState> = states(&fresh).into_iter().map(|(_, s)| s).collect();
        assert_eq!(
            after,
            before.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
            "order and states survive"
        );
        assert_eq!(
            after,
            [
                HistoryState::Built,
                HistoryState::Suppressed,
                HistoryState::Skipped,
                HistoryState::PastMarker
            ]
        );
        let _ = (a, d);
    }

    #[test]
    fn plain_documents_carry_no_history_block() {
        let mut api = api();
        cut(&mut api);
        assert!(api.get_state().document.history.is_none());
    }

    /// A box with a 10×10 pocket cut through a sketch attached to its top.
    fn pocket_doc(face: &str) -> serde_json::Value {
        let v = |x: f64, y: f64, z: f64| serde_json::json!({ "x": x, "y": y, "z": z });
        let p = |x: f64, y: f64| serde_json::json!({ "x": x, "y": y });
        let line = |a, b| serde_json::json!({ "type": "Line", "start": a, "end": b });
        serde_json::json!({
            "version": "0.1",
            "nodes": {
                "1": { "id": 1, "name": "box", "op": { "type": "Cube", "size": v(40.0, 40.0, 10.0) } },
                "2": { "id": 2, "name": "sketch", "op": { "type": "Sketch2D",
                    "origin": v(15.0, 15.0, 10.0), "x_dir": v(1.0, 0.0, 0.0), "y_dir": v(0.0, 1.0, 0.0),
                    "segments": [
                        line(p(0.0, 0.0), p(10.0, 0.0)), line(p(10.0, 0.0), p(10.0, 10.0)),
                        line(p(10.0, 10.0), p(0.0, 10.0)), line(p(0.0, 10.0), p(0.0, 0.0))
                    ] } },
                "3": { "id": 3, "name": "on top", "op": { "type": "SketchOnFace",
                    "sketch": 2, "target": 1, "face": { "name": face } } },
                "4": { "id": 4, "name": "tool", "op": { "type": "Extrude", "sketch": 3,
                    "direction": v(0.0, 0.0, -2.0) } },
                "5": { "id": 5, "name": "Cut", "op": { "type": "Difference", "left": 1, "right": 4 } }
            },
            "materials": {},
            "part_materials": {},
            "roots": [{ "root": 5, "material": "default" }]
        })
    }

    #[test]
    fn a_resolving_face_reference_is_not_broken() {
        let mut api = api();
        api.apply(DocCommand::ImportIr {
            document: pocket_doc("n1:top"),
        })
        .unwrap();
        let rows = api.feature_history();
        assert!(rows.len() >= 2, "{rows:?}");
        assert!(rows.iter().all(|r| r.broken.is_empty()), "{rows:?}");
    }

    #[test]
    fn a_lost_face_reference_is_reported_with_ranked_candidates() {
        let mut api = api();
        // `n1:roof` doesn't exist: the cube's faces are top/bottom/….
        api.apply(DocCommand::ImportIr {
            document: pocket_doc("n1:roof"),
        })
        .unwrap();
        let rows = api.feature_history();
        let broken: Vec<&BrokenRef> = rows.iter().flat_map(|r| &r.broken).collect();
        assert_eq!(broken.len(), 1, "{rows:?}");
        // Only the feature holding the reference is flagged.
        assert_eq!(rows.iter().filter(|r| !r.broken.is_empty()).count(), 1);
        let b = &broken[0];
        assert_eq!(b.kind, "face");
        assert!(b.reference.as_deref().unwrap().ends_with(":roof"));
        assert!(b.message.contains("lost"), "{}", b.message);
        assert_eq!(b.candidates.len(), 6, "a box has six planar named faces");
        assert!(b.candidates.iter().any(|c| c.name.ends_with(":top")));
    }

    #[test]
    fn ranking_prefers_the_same_tag_and_the_hinted_normal() {
        let mut api = api();
        api.apply(DocCommand::ImportIr {
            document: pocket_doc("n1:top"),
        })
        .unwrap();
        let doc = api.get_state().document;
        let cube = doc
            .nodes
            .values()
            .find(|n| matches!(n.op, CsgOp::Cube { .. }))
            .unwrap()
            .id;
        let solid = vcad_eval::evaluate::evaluate_node(cube, &doc.nodes, &mut HashMap::new())
            .unwrap()
            .unwrap();
        let by_tag = rank_faces(
            &solid,
            &vcad_ir::FaceRef {
                name: Some("n9:top".into()),
                point: None,
                normal: None,
            },
        );
        assert!(by_tag[0].name.ends_with(":top"), "{by_tag:?}");
        let by_normal = rank_faces(
            &solid,
            &vcad_ir::FaceRef {
                name: Some("n9:gone".into()),
                point: Some(vcad_ir::Vec3::new(0.0, 0.0, 0.0)),
                normal: Some(vcad_ir::Vec3::new(0.0, 0.0, -1.0)),
            },
        );
        assert!(by_normal[0].name.ends_with(":bottom"), "{by_normal:?}");
    }
}
