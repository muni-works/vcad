//! Shared plumbing for the analysis entry points (`dfm`, `fea`, `clearance`):
//! parse a document, evaluate it the way the app does, and line evaluated
//! parts up with the document roots and shell-ordinal face ids.

use std::collections::HashMap;
use std::ffi::c_char;

use vcad_eval::{evaluate_document_with_sheet_metal, EvalOptions, EvaluatedScene};
use vcad_ir::{Document, NodeId};
use vcad_kernel::vcad_kernel_primitives::BRepSolid;
use vcad_kernel::vcad_kernel_topo::FaceId;

use crate::json::borrow_str;

/// Parse a NUL-terminated IR JSON document.
pub(crate) fn parse_doc(what: &str, json: *const c_char) -> Result<Document, String> {
    let text = borrow_str(what, json).ok_or_else(|| format!("{what}: bad string"))?;
    Document::from_json(text).map_err(|e| format!("{what}: parse: {e}"))
}

/// Parse an optional JSON request (null / empty → `{}`).
pub(crate) fn parse_request(json: *const c_char) -> Result<serde_json::Value, String> {
    if json.is_null() {
        return Ok(serde_json::json!({}));
    }
    let text = borrow_str("request", json).ok_or("request: bad string")?;
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(text).map_err(|e| format!("request: {e}"))
}

/// Evaluate with the app's kernel-direct evaluator (sheet metal folded).
pub(crate) fn evaluate(doc: &Document) -> Result<EvaluatedScene, String> {
    evaluate_document_with_sheet_metal(doc, &EvalOptions::default())
        .map_err(|e| format!("evaluate: {e}"))
}

/// Root node of each evaluated part, when parts line up with the visible
/// roots (they do on a clean eval; a failed root breaks the alignment, and
/// then provenance is withheld rather than guessed).
pub(crate) fn part_roots(doc: &Document, scene: &EvaluatedScene) -> Vec<Option<NodeId>> {
    let visible: Vec<NodeId> = doc
        .roots
        .iter()
        .filter(|r| r.visible != Some(false))
        .map(|r| r.root)
        .collect();
    if visible.len() == scene.parts.len() {
        visible.into_iter().map(Some).collect()
    } else {
        vec![None; scene.parts.len()]
    }
}

/// Display name for part `i`: the first name on its root's chain of
/// Translate/Rotate/Scale wrappers (a placed primitive's root is an unnamed
/// Translate over the named feature — the name the Model tree shows), else
/// `Part i+1`.
pub(crate) fn part_name(doc: &Document, root: Option<NodeId>, i: usize) -> String {
    let mut cur = root;
    for _ in 0..64 {
        let Some(node) = cur.and_then(|r| doc.nodes.get(&r)) else {
            break;
        };
        if let Some(n) = node.name.as_ref().filter(|n| !n.is_empty()) {
            return n.clone();
        }
        cur = match &node.op {
            vcad_ir::CsgOp::Translate { child, .. }
            | vcad_ir::CsgOp::Rotate { child, .. }
            | vcad_ir::CsgOp::Scale { child, .. } => Some(*child),
            _ => None,
        };
    }
    format!("Part {}", i + 1)
}

/// `FaceId → shell ordinal` — the face id the viewport tags triangles with
/// (`vcad_scene_part_face_ids`, `vcad_topology_json`).
pub(crate) fn shell_ordinals(brep: &BRepSolid) -> HashMap<FaceId, u32> {
    let topo = &brep.topology;
    topo.shells[topo.solids[brep.solid_id].outer_shell]
        .faces
        .iter()
        .enumerate()
        .map(|(i, &f)| (f, i as u32))
        .collect()
}

/// Kernel face index (`topology.faces` iteration order, what DFM reports)
/// → shell ordinal. Faces outside the outer shell map to `None`.
pub(crate) fn kernel_index_to_ordinal(brep: &BRepSolid) -> Vec<Option<u32>> {
    let ord = shell_ordinals(brep);
    brep.topology
        .faces
        .iter()
        .map(|(fid, _)| ord.get(&fid).copied())
        .collect()
}
