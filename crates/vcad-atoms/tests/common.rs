//! Shared toy documents.
#![allow(dead_code)]
use vcad_ir::{BindingKey, CsgOp, Document, Expr, MaterialDef, Node, Parameter, SceneEntry, Vec3};

fn base(op: CsgOp) -> Document {
    let mut doc = Document::new();
    doc.nodes.insert(
        1,
        Node {
            id: 1,
            name: Some("part".into()),
            op,
        },
    );
    doc.roots.push(SceneEntry {
        root: 1,
        material: "default".into(),
        visible: None,
    });
    doc.materials.insert(
        "default".into(),
        MaterialDef {
            name: "default".into(),
            color: [0.8; 3],
            metallic: 0.0,
            roughness: 0.5,
            density: None,
            friction: None,
            ..Default::default()
        },
    );
    doc
}

pub fn block(x: f64, y: f64, z: f64) -> Document {
    base(CsgOp::Cube {
        size: Vec3 { x, y, z },
    })
}

/// Cylinder with radius bound to `r` and height bound to `h`.
pub fn cylinder(r: f64, h: f64) -> Document {
    let mut doc = base(CsgOp::Cylinder {
        radius: 1.0,
        height: 1.0,
        segments: 48,
    });
    doc.parameters.insert("r".into(), Parameter::literal(r));
    doc.parameters.insert("h".into(), Parameter::literal(h));
    for (field, p) in [("radius", "r"), ("height", "h")] {
        doc.bindings.bind(
            BindingKey {
                node_id: 1,
                field_path: field.into(),
            },
            Expr::formula(p),
        );
    }
    doc
}
