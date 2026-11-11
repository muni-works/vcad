//! Shared plumbing for the analysis entry points (`dfm`, `fea`, `clearance`):
//! parse a document, evaluate it the way the app does, and line evaluated
//! parts up with the document roots and shell-ordinal face ids.

use std::collections::HashMap;
use std::ffi::c_char;

use vcad_ir::Document;
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

pub(crate) use vcad_receipt_verify::bodies::{evaluate, part_name, part_roots};

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
