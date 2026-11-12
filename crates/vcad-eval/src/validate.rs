//! Pre-flight reference validation for documents.
//!
//! Walks every `NodeId` reference reachable from a `Document` and reports the
//! first dangling reference together with a human-readable path (e.g.
//! `nodes[47].Translate.child`, `roots[2].root`, `partDefs["arm"].root`).

use vcad_ir::{CsgOp, Document, NodeId};

use crate::EvalError;

/// Verify every `NodeId` reachable from `doc` resolves to a node in `doc.nodes`.
///
/// Returns the first dangling reference with its path, or `Ok(())` if all
/// references are valid. O(total refs) — one pass with pure HashMap lookups.
pub fn validate_document(doc: &Document) -> Result<(), EvalError> {
    for (i, entry) in doc.roots.iter().enumerate() {
        check_ref(doc, entry.root, || format!("roots[{i}].root"))?;
    }

    if let Some(part_defs) = &doc.part_defs {
        for (id, part_def) in part_defs {
            check_ref(doc, part_def.root, || format!("partDefs[{id:?}].root"))?;
            for (i, root) in part_def.colliders.iter().flatten().enumerate() {
                check_ref(doc, *root, || format!("partDefs[{id:?}].colliders[{i}]"))?;
            }
        }
    }

    for (node_id, node) in &doc.nodes {
        validate_op(doc, *node_id, &node.op)?;
    }

    if let Some(n) = find_cycle(doc) {
        return Err(EvalError::Cycle(n));
    }

    Ok(())
}

/// A node on a cycle of the node graph, if it has one. Iterative
/// three-colour DFS over `CsgOp::child_ids`, O(nodes + refs); dangling
/// references are ignored (that is `validate_document`'s other check).
pub fn find_cycle(doc: &Document) -> Option<NodeId> {
    use std::collections::HashMap;
    #[derive(Clone, Copy, PartialEq)]
    enum C {
        Grey,
        Black,
    }
    let mut colour: HashMap<NodeId, C> = HashMap::with_capacity(doc.nodes.len());
    for &start in doc.nodes.keys() {
        if colour.contains_key(&start) {
            continue;
        }
        // (node, its children, next child index)
        let mut stack: Vec<(NodeId, Vec<NodeId>, usize)> = Vec::new();
        colour.insert(start, C::Grey);
        stack.push((start, doc.nodes[&start].op.child_ids(), 0));
        while let Some((id, kids, i)) = stack.last_mut() {
            if *i == kids.len() {
                colour.insert(*id, C::Black);
                stack.pop();
                continue;
            }
            let k = kids[*i];
            *i += 1;
            match colour.get(&k) {
                Some(C::Grey) => return Some(k),
                Some(C::Black) => {}
                None => {
                    if let Some(node) = doc.nodes.get(&k) {
                        colour.insert(k, C::Grey);
                        stack.push((k, node.op.child_ids(), 0));
                    }
                }
            }
        }
    }
    None
}

fn validate_op(doc: &Document, node_id: NodeId, op: &CsgOp) -> Result<(), EvalError> {
    let op_name = csg_op_name(op);
    let at = |field: &str| format!("nodes[{node_id}].{op_name}.{field}");

    match op {
        CsgOp::Union { left, right }
        | CsgOp::Difference { left, right }
        | CsgOp::Intersection { left, right } => {
            check_ref(doc, *left, || at("left"))?;
            check_ref(doc, *right, || at("right"))?;
        }
        CsgOp::Translate { child, .. }
        | CsgOp::Rotate { child, .. }
        | CsgOp::Scale { child, .. }
        | CsgOp::Mirror { child, .. }
        | CsgOp::LinearPattern { child, .. }
        | CsgOp::CircularPattern { child, .. }
        | CsgOp::SweepRotate { child, .. }
        | CsgOp::Shell { child, .. }
        | CsgOp::Fillet { child, .. }
        | CsgOp::Chamfer { child, .. }
        | CsgOp::EdgeBlend { child, .. } => {
            check_ref(doc, *child, || at("child"))?;
        }
        CsgOp::SketchOnFace { sketch, target, .. } => {
            check_ref(doc, *sketch, || at("sketch"))?;
            check_ref(doc, *target, || at("target"))?;
        }
        CsgOp::Extrude { sketch, .. }
        | CsgOp::Revolve { sketch, .. }
        | CsgOp::Sweep { sketch, .. } => {
            check_ref(doc, *sketch, || at("sketch"))?;
        }
        CsgOp::Loft { sketches, .. } => {
            for (i, s) in sketches.iter().enumerate() {
                check_ref(doc, *s, || format!("nodes[{node_id}].Loft.sketches[{i}]"))?;
            }
        }
        CsgOp::Hull { children, .. } => {
            for (i, c) in children.iter().enumerate() {
                check_ref(doc, *c, || format!("nodes[{node_id}].Hull.children[{i}]"))?;
            }
        }
        // Leaf ops without NodeId references.
        CsgOp::Cube { .. }
        | CsgOp::Cylinder { .. }
        | CsgOp::Sphere { .. }
        | CsgOp::Cone { .. }
        | CsgOp::Torus { .. }
        | CsgOp::Wedge { .. }
        | CsgOp::Prism { .. }
        | CsgOp::SpurGear { .. }
        | CsgOp::Empty
        | CsgOp::Sketch2D { .. }
        | CsgOp::Text2D { .. }
        | CsgOp::ImportedMesh { .. }
        | CsgOp::StepImport { .. }
        | CsgOp::MeshImport { .. }
        | CsgOp::PcbBoard { .. }
        | CsgOp::EmbroideryPattern { .. }
        | CsgOp::PartInstance { .. }
        | CsgOp::SheetMetalBaseFlangeRect { .. }
        | CsgOp::SheetMetalBaseFlangePolygon { .. }
        | CsgOp::SheetMetalEdgeFlange { .. }
        | CsgOp::SheetMetalHem { .. }
        | CsgOp::SheetMetalJog { .. }
        | CsgOp::SheetMetalBendRelief { .. } => {}
    }

    Ok(())
}

fn check_ref<F: FnOnce() -> String>(
    doc: &Document,
    node_id: NodeId,
    path: F,
) -> Result<(), EvalError> {
    if doc.nodes.contains_key(&node_id) {
        Ok(())
    } else {
        Err(EvalError::MissingNodeAt {
            node_id,
            path: path(),
        })
    }
}

fn csg_op_name(op: &CsgOp) -> &'static str {
    match op {
        CsgOp::Cube { .. } => "Cube",
        CsgOp::Cylinder { .. } => "Cylinder",
        CsgOp::Sphere { .. } => "Sphere",
        CsgOp::Cone { .. } => "Cone",
        CsgOp::Torus { .. } => "Torus",
        CsgOp::Wedge { .. } => "Wedge",
        CsgOp::Prism { .. } => "Prism",
        CsgOp::SpurGear { .. } => "SpurGear",
        CsgOp::Mirror { .. } => "Mirror",
        CsgOp::Empty => "Empty",
        CsgOp::Union { .. } => "Union",
        CsgOp::Difference { .. } => "Difference",
        CsgOp::Intersection { .. } => "Intersection",
        CsgOp::Translate { .. } => "Translate",
        CsgOp::Rotate { .. } => "Rotate",
        CsgOp::Scale { .. } => "Scale",
        CsgOp::LinearPattern { .. } => "LinearPattern",
        CsgOp::CircularPattern { .. } => "CircularPattern",
        CsgOp::SweepRotate { .. } => "SweepRotate",
        CsgOp::Shell { .. } => "Shell",
        CsgOp::Fillet { .. } => "Fillet",
        CsgOp::Chamfer { .. } => "Chamfer",
        CsgOp::EdgeBlend { .. } => "EdgeBlend",
        CsgOp::Sketch2D { .. } => "Sketch2D",
        CsgOp::SketchOnFace { .. } => "SketchOnFace",
        CsgOp::Text2D { .. } => "Text2D",
        CsgOp::Extrude { .. } => "Extrude",
        CsgOp::Revolve { .. } => "Revolve",
        CsgOp::Sweep { .. } => "Sweep",
        CsgOp::Loft { .. } => "Loft",
        CsgOp::Hull { .. } => "Hull",
        CsgOp::ImportedMesh { .. } => "ImportedMesh",
        CsgOp::StepImport { .. } => "StepImport",
        CsgOp::MeshImport { .. } => "MeshImport",
        CsgOp::PcbBoard { .. } => "PcbBoard",
        CsgOp::EmbroideryPattern { .. } => "EmbroideryPattern",
        CsgOp::PartInstance { .. } => "PartInstance",
        CsgOp::SheetMetalBaseFlangeRect { .. } => "SheetMetalBaseFlangeRect",
        CsgOp::SheetMetalEdgeFlange { .. } => "SheetMetalEdgeFlange",
        CsgOp::SheetMetalHem { .. } => "SheetMetalHem",
        CsgOp::SheetMetalJog { .. } => "SheetMetalJog",
        CsgOp::SheetMetalBaseFlangePolygon { .. } => "SheetMetalBaseFlangePolygon",
        CsgOp::SheetMetalBendRelief { .. } => "SheetMetalBendRelief",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vcad_ir::{Node, PartDef, SceneEntry, Vec3};

    fn cube_node(id: NodeId) -> Node {
        Node {
            id,
            name: None,
            op: CsgOp::Cube {
                size: Vec3::new(1.0, 1.0, 1.0),
            },
        }
    }

    #[test]
    fn translate_missing_child_reports_path() {
        let mut doc = Document::new();
        doc.nodes.insert(
            5,
            Node {
                id: 5,
                name: None,
                op: CsgOp::Translate {
                    child: 0,
                    offset: Vec3::new(1.0, 2.0, 3.0),
                },
            },
        );

        let err = validate_document(&doc).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains('0'), "error should mention node id 0: {msg}");
        assert!(
            msg.contains("nodes[5].Translate.child"),
            "error should mention path: {msg}"
        );
    }

    #[test]
    fn root_missing_node_reports_path() {
        let mut doc = Document::new();
        doc.roots.push(SceneEntry {
            root: 999,
            material: "default".to_string(),
            visible: None,
        });

        let err = validate_document(&doc).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("999"), "error should mention 999: {msg}");
        assert!(
            msg.contains("roots[0].root"),
            "error should mention roots[0].root: {msg}"
        );
    }

    #[test]
    fn part_def_missing_root_reports_path() {
        let mut doc = Document::new();
        let mut pd = HashMap::new();
        pd.insert(
            "arm".to_string(),
            PartDef {
                id: "arm".to_string(),
                name: None,
                root: 42,
                default_material: None,
                inertial: None,
                colliders: None,
            },
        );
        doc.part_defs = Some(pd);

        let err = validate_document(&doc).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("42"), "error should mention 42: {msg}");
        assert!(
            msg.contains("partDefs[\"arm\"].root"),
            "error should mention partDefs[\"arm\"].root: {msg}"
        );
    }

    #[test]
    fn part_def_dangling_collider_reports_path() {
        // Collider roots are referenced only by `PartDef::colliders` — nothing
        // in `roots` or `nodes` points at them — so a dangling one would
        // otherwise surface as a physics-eval failure far from the cause.
        let mut doc = Document::new();
        doc.nodes.insert(
            1,
            Node {
                id: 1,
                name: None,
                op: CsgOp::Empty,
            },
        );
        let mut pd = HashMap::new();
        pd.insert(
            "arm".to_string(),
            PartDef {
                id: "arm".to_string(),
                name: None,
                root: 1,
                default_material: None,
                inertial: None,
                colliders: Some(vec![1, 777]),
            },
        );
        doc.part_defs = Some(pd);

        let err = validate_document(&doc).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("777"), "error should mention 777: {msg}");
        assert!(
            msg.contains("partDefs[\"arm\"].colliders[1]"),
            "error should mention the offending index: {msg}"
        );
    }

    #[test]
    fn loft_indexes_missing_sketch() {
        let mut doc = Document::new();
        doc.nodes.insert(1, cube_node(1));
        doc.nodes.insert(
            10,
            Node {
                id: 10,
                name: None,
                op: CsgOp::Loft {
                    sketches: vec![1, 7, 1],
                    closed: None,
                },
            },
        );

        let err = validate_document(&doc).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nodes[10].Loft.sketches[1]"),
            "error should mention Loft.sketches[1]: {msg}"
        );
        assert!(msg.contains('7'), "error should mention node id 7: {msg}");
    }

    #[test]
    fn valid_document_passes() {
        let mut doc = Document::new();
        doc.nodes.insert(1, cube_node(1));
        doc.roots.push(SceneEntry {
            root: 1,
            material: "default".to_string(),
            visible: None,
        });
        assert!(validate_document(&doc).is_ok());
    }

    /// Fuzz finding F5 (docs/qa/fuzz-2026-11-12.md): `Translate { child: self }`
    /// hung the importer forever. Cycles must be reported, not walked.
    #[test]
    fn cycles_are_rejected() {
        let doc: Document = serde_json::from_str(
            r#"{"version":"0.1","nodes":{
                "1":{"id":1,"name":null,"op":{"type":"Cube","size":{"x":1,"y":1,"z":1}}},
                "2":{"id":2,"name":null,"op":{"type":"Translate","child":2,"offset":{"x":0,"y":0,"z":0}}},
                "3":{"id":3,"name":null,"op":{"type":"Union","left":1,"right":4}},
                "4":{"id":4,"name":null,"op":{"type":"Rotate","child":3,"angles":{"x":0,"y":0,"z":0}}}},
              "materials":{},"part_materials":{},"roots":[{"root":3,"material":"default"}]}"#,
        )
        .unwrap();
        assert!(find_cycle(&doc).is_some());
        assert!(matches!(validate_document(&doc), Err(EvalError::Cycle(_))));
    }
}
