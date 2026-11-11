//! The bodies a document draws, keyed by every id an assertion may use to
//! name them — shared by `vcad-ffi` (clearance, analysis) and the receipt
//! verifier.

use vcad_eval::{evaluate_document_with_sheet_metal, EvalOptions, EvaluatedScene};
use vcad_ir::{CsgOp, Document, NodeId};
use vcad_kernel::Solid;

/// Evaluate with the app's kernel-direct evaluator (sheet metal folded).
pub fn evaluate(doc: &Document) -> Result<EvaluatedScene, String> {
    evaluate_document_with_sheet_metal(doc, &EvalOptions::default())
        .map_err(|e| format!("evaluate: {e}"))
}

/// Root node of each evaluated part, when parts line up with the visible
/// roots (they do on a clean eval; a failed root breaks the alignment, and
/// then provenance is withheld rather than guessed).
pub fn part_roots(doc: &Document, scene: &EvaluatedScene) -> Vec<Option<NodeId>> {
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
pub fn part_name(doc: &Document, root: Option<NodeId>, i: usize) -> String {
    let mut cur = root;
    for _ in 0..64 {
        let Some(node) = cur.and_then(|r| doc.nodes.get(&r)) else {
            break;
        };
        if let Some(n) = node.name.as_ref().filter(|n| !n.is_empty()) {
            return n.clone();
        }
        cur = match &node.op {
            CsgOp::Translate { child, .. }
            | CsgOp::Rotate { child, .. }
            | CsgOp::Scale { child, .. } => Some(*child),
            _ => None,
        };
    }
    format!("Part {}", i + 1)
}

/// A drawn body plus every id a persisted assertion may use to name it:
/// the stringified root node id (plain parts), the instance id (assembly
/// instances), and the display name.
pub struct KeyedBody {
    /// Ids that select this body (root node id / instance id, then name).
    pub keys: Vec<String>,
    /// Display name.
    pub name: String,
    /// The posed solid, when it evaluated.
    pub solid: Option<Solid>,
}

/// The bodies the viewport draws, in render order, with the ids assertions
/// refer to. A plain document draws its visible roots; an assembly draws its
/// instances posed by forward kinematics, then the free roots.
pub fn keyed_bodies(doc: &Document, scene: &EvaluatedScene) -> Vec<KeyedBody> {
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
    let def_roots: std::collections::HashSet<NodeId> = doc
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
pub fn place(s: &Solid, t: &vcad_ir::Transform3D) -> Solid {
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
