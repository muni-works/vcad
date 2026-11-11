//! Build the document a STEP import produces: one root per body, either a
//! lazy B-rep `StepImport` node (the bytes are registered with the kernel
//! under `path`) or a tessellated `ImportedMesh`, all in one material, with
//! the default steel/aluminum/default material table.

use serde_json::json;
use vcad_ir::{CsgOp, Document, MaterialDef, Node, SceneEntry};

/// One imported body.
#[derive(Debug, Clone)]
pub enum StepBody {
    /// Lazy B-rep reference to solid `solid_index` of the file at `path`.
    Brep {
        /// Path the STEP bytes are registered under.
        path: String,
        /// 0-based solid index within the file.
        solid_index: u32,
    },
    /// Tessellated body.
    Mesh {
        /// Flat vertex positions (mm).
        positions: Vec<f64>,
        /// Triangle indices.
        indices: Vec<u32>,
        /// Optional per-vertex normals.
        normals: Option<Vec<f64>>,
        /// Source label for display.
        source: String,
    },
}

/// The material table every STEP import starts with.
pub fn default_materials() -> std::collections::HashMap<String, MaterialDef> {
    let defs = json!({
        "steel": {"name": "Steel", "color": [0.6, 0.6, 0.65], "metallic": 0.9, "roughness": 0.3, "density": 7850.0},
        "aluminum": {"name": "Aluminum", "color": [0.8, 0.8, 0.85], "metallic": 0.9, "roughness": 0.2, "density": 2700.0},
        "default": {"name": "Default", "color": [0.8, 0.8, 0.8], "metallic": 0.0, "roughness": 0.5},
    });
    serde_json::from_value(defs).expect("static material table")
}

/// Build the import document. A single body is named `part_name`; several
/// are `part_name_1`, `part_name_2`, … . Node ids start at 1.
pub fn step_import_document(part_name: &str, material: &str, bodies: Vec<StepBody>) -> Document {
    let mut doc = Document {
        materials: default_materials(),
        ..Document::default()
    };
    let many = bodies.len() > 1;
    for (i, body) in bodies.into_iter().enumerate() {
        let id = i as u64 + 1;
        let name = if many {
            format!("{part_name}_{id}")
        } else {
            part_name.to_string()
        };
        let op = match body {
            StepBody::Brep { path, solid_index } => CsgOp::StepImport {
                path,
                solid_index: (solid_index != 0).then_some(solid_index),
            },
            StepBody::Mesh {
                positions,
                indices,
                normals,
                source,
            } => CsgOp::ImportedMesh {
                positions,
                indices,
                normals,
                source: Some(source),
            },
        };
        doc.nodes.insert(
            id,
            Node {
                id,
                name: Some(name.clone()),
                op,
            },
        );
        doc.roots.push(SceneEntry {
            root: id,
            material: material.to_string(),
            visible: None,
        });
        doc.part_materials.insert(name, material.to_string());
    }
    doc
}

/// Lay out a read STEP file as an IR document: one root per placed copy of
/// each solid (an assembly's screws each get their own root, at their own
/// pose), named after the assembly occurrence, coloured by the file's
/// `STYLED_ITEM`s. A pose becomes `Translate(Rotate(StepImport))`; the
/// identity pose is the bare `StepImport`.
pub fn step_assembly_document(
    read: &vcad_kernel::vcad_kernel_step::ReadResult,
    abs_path: &str,
    stem: &str,
) -> Document {
    use vcad_ir::Vec3;
    let index_of: std::collections::HashMap<u64, usize> = read
        .report
        .solids
        .iter()
        .enumerate()
        .map(|(i, r)| (r.solid_id, i))
        .collect();
    // (solid index, transform, name). Solids no occurrence reaches (an
    // anchor-less file) sit once at their own coordinates.
    let mut placed: Vec<(usize, [[f64; 4]; 4], Option<String>)> = read
        .occurrences
        .iter()
        .filter_map(|o| {
            let i = *index_of.get(&o.solid_id)?;
            // A multi-body part: the reader named each body itself (its
            // product is shared), and that name is the useful one.
            let own = read.names.get(i).cloned().flatten();
            let name = match own {
                Some(n) if n != o.part_name => n,
                _ => o.instance_name.clone(),
            };
            Some((i, o.transform, Some(name)))
        })
        .collect();
    let reached: std::collections::HashSet<usize> = placed.iter().map(|p| p.0).collect();
    for i in 0..read.solids.len() {
        if !reached.contains(&i) {
            placed.push((i, IDENTITY, read.names.get(i).cloned().flatten()));
        }
    }

    let mut doc = Document::new();
    let mut next_id: u64 = 1;
    let mut add = |doc: &mut Document, name: Option<String>, op: CsgOp| {
        let id = next_id;
        next_id += 1;
        doc.nodes.insert(id, Node { id, name, op });
        id
    };
    let single = placed.len() == 1;
    let mut used_names: std::collections::HashMap<String, usize> = Default::default();
    for (n, (i, m, name)) in placed.iter().enumerate() {
        let base = name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if single {
                    stem.to_owned()
                } else {
                    format!("{stem} #{}", n + 1)
                }
            });
        // Repeated occurrence names ("R1", "R1") are told apart.
        let k = used_names.entry(base.clone()).or_insert(0);
        *k += 1;
        let label = if *k == 1 {
            base
        } else {
            format!("{base} ({k})")
        };
        let mut id = add(
            &mut doc,
            Some(label.clone()),
            CsgOp::StepImport {
                path: abs_path.to_owned(),
                solid_index: Some(*i as u32),
            },
        );
        if *m != IDENTITY {
            let (rx, ry, rz) = euler_xyz_deg(m);
            if rx.abs() > 1e-9 || ry.abs() > 1e-9 || rz.abs() > 1e-9 {
                id = add(
                    &mut doc,
                    None,
                    CsgOp::Rotate {
                        child: id,
                        angles: Vec3 {
                            x: rx,
                            y: ry,
                            z: rz,
                        },
                    },
                );
            }
            let t = Vec3 {
                x: m[0][3],
                y: m[1][3],
                z: m[2][3],
            };
            if t.x != 0.0 || t.y != 0.0 || t.z != 0.0 {
                id = add(
                    &mut doc,
                    None,
                    CsgOp::Translate {
                        child: id,
                        offset: t,
                    },
                );
            }
            // The outermost node carries the name the tree shows.
            if let Some(node) = doc.nodes.get_mut(&id) {
                node.name = Some(label);
            }
        }
        let material = match read.colors.get(*i).copied().flatten() {
            Some(c) => {
                let key = format!(
                    "step_{:02x}{:02x}{:02x}",
                    (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0).round() as u8
                );
                doc.materials
                    .entry(key.clone())
                    .or_insert_with(|| MaterialDef {
                        name: key.clone(),
                        color: c,
                        metallic: 0.0,
                        roughness: 0.5,
                        ..Default::default()
                    });
                key
            }
            None => "default".into(),
        };
        doc.roots.push(SceneEntry {
            root: id,
            material,
            visible: None,
        });
    }
    doc
}

const IDENTITY: [[f64; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Euler angles (degrees) for `CsgOp::Rotate`, which applies X, then Y,
/// then Z: R = Rz·Ry·Rx. Row-major `m`, rotation in the upper 3×3.
fn euler_xyz_deg(m: &[[f64; 4]; 4]) -> (f64, f64, f64) {
    let sy = -m[2][0];
    let (x, y, z) = if sy.abs() < 1.0 - 1e-12 {
        (m[2][1].atan2(m[2][2]), sy.asin(), m[1][0].atan2(m[0][0]))
    } else {
        // Gimbal lock: fold X into Z.
        (
            0.0,
            sy.signum() * std::f64::consts::FRAC_PI_2,
            (-m[0][1]).atan2(m[1][1]),
        )
    };
    (x.to_degrees(), y.to_degrees(), z.to_degrees())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brep_bodies_name_and_index() {
        let doc = step_import_document(
            "bracket",
            "aluminum",
            vec![
                StepBody::Brep {
                    path: "/x/a.step".into(),
                    solid_index: 0,
                },
                StepBody::Brep {
                    path: "/x/a.step".into(),
                    solid_index: 3,
                },
            ],
        );
        assert_eq!(doc.roots.len(), 2);
        assert_eq!(doc.roots[1].material, "aluminum");
        assert_eq!(doc.nodes[&1].name.as_deref(), Some("bracket_1"));
        assert_eq!(doc.part_materials["bracket_2"], "aluminum");
        match (&doc.nodes[&1].op, &doc.nodes[&2].op) {
            (
                CsgOp::StepImport {
                    solid_index: None, ..
                },
                CsgOp::StepImport {
                    solid_index: Some(3),
                    path,
                },
            ) => assert_eq!(path, "/x/a.step"),
            other => panic!("{other:?}"),
        }
        assert_eq!(doc.materials["steel"].density, Some(7850.0));
        assert_eq!(doc.materials.len(), 3);
    }

    #[test]
    fn single_mesh_body_keeps_the_part_name() {
        let doc = step_import_document(
            "plate",
            "steel",
            vec![StepBody::Mesh {
                positions: vec![0.0; 9],
                indices: vec![0, 1, 2],
                normals: None,
                source: "plate.step".into(),
            }],
        );
        assert_eq!(doc.nodes[&1].name.as_deref(), Some("plate"));
        assert!(matches!(
            &doc.nodes[&1].op,
            CsgOp::ImportedMesh { source: Some(s), .. } if s == "plate.step"
        ));
    }
}
