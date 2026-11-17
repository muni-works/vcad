//! A URDF `<visual>` with an oblique `rpy` lands in the same place whether
//! its geometry is a `<box>` (BRep under `CsgOp::Rotate`) or a `<mesh>` of
//! the same box (`MeshImport` under the same `Rotate`), and both match the
//! URDF convention R = Rz(yaw)·Ry(pitch)·Rx(roll).
//!
//! Before the fix the reader copied rpy straight into `Rotate` (which is
//! Rx·Ry·Rz), and the evaluator applied Rotate as Rz·Ry·Rx to meshes but
//! Rx·Ry·Rz to BReps — so meshes looked right and primitives were wrong.

use vcad_eval::{evaluate_document, EvalOptions, EvaluatedMesh};

const SIZE_M: [f64; 3] = [0.01, 0.02, 0.03];
const XYZ_M: [f64; 3] = [0.005, -0.01, 0.02];
const RPY: [f64; 3] = [0.4, -0.7, 1.1];

fn corners() -> Vec<[f64; 3]> {
    (0..8)
        .map(|i| {
            let h = |k: usize, bit: usize| {
                if i & bit != 0 {
                    SIZE_M[k] / 2.0
                } else {
                    -SIZE_M[k] / 2.0
                }
            };
            [h(0, 1), h(1, 2), h(2, 4)]
        })
        .collect()
}

/// URDF rpy → matrix, Rz·Ry·Rx, row-major.
fn rpy_matrix([r, p, y]: [f64; 3]) -> [[f64; 3]; 3] {
    let (sr, cr) = r.sin_cos();
    let (sp, cp) = p.sin_cos();
    let (sy, cy) = y.sin_cos();
    [
        [cy * cp, cy * sp * sr - sy * cr, cy * sp * cr + sy * sr],
        [sy * cp, sy * sp * sr + cy * cr, sy * sp * cr - cy * sr],
        [-sp, cp * sr, cp * cr],
    ]
}

fn write_box_stl(path: &std::path::Path) {
    let c = corners();
    let tris: [[usize; 3]; 12] = [
        [0, 2, 1],
        [1, 2, 3],
        [4, 5, 6],
        [5, 7, 6],
        [0, 1, 4],
        [1, 5, 4],
        [2, 6, 3],
        [3, 6, 7],
        [0, 4, 2],
        [2, 4, 6],
        [1, 3, 5],
        [3, 7, 5],
    ];
    let v = |i: usize| stl_io::Vertex::new([c[i][0] as f32, c[i][1] as f32, c[i][2] as f32]);
    let mesh: Vec<stl_io::Triangle> = tris
        .iter()
        .map(|t| stl_io::Triangle {
            normal: stl_io::Normal::new([0.0, 0.0, 0.0]),
            vertices: [v(t[0]), v(t[1]), v(t[2])],
        })
        .collect();
    let mut f = std::fs::File::create(path).unwrap();
    stl_io::write_stl(&mut f, mesh.iter()).unwrap();
}

fn urdf(geometry: &str) -> String {
    format!(
        r#"<robot name="r"><link name="base"><visual>
             <origin xyz="{} {} {}" rpy="{} {} {}"/>
             <geometry>{geometry}</geometry>
           </visual></link></robot>"#,
        XYZ_M[0], XYZ_M[1], XYZ_M[2], RPY[0], RPY[1], RPY[2]
    )
}

fn link_mesh(xml: &str) -> EvaluatedMesh {
    let doc = vcad_kernel_urdf::read_urdf_from_str(xml).unwrap();
    let opts = EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    };
    let scene = evaluate_document(&doc, &opts).unwrap();
    assert!(scene.failures.is_empty(), "{:?}", scene.failures);
    let defs = scene.part_defs.expect("URDF link becomes a part def");
    assert_eq!(defs.len(), 1);
    defs.into_iter().next().unwrap().mesh
}

fn bbox(m: &EvaluatedMesh) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in m.positions.chunks_exact(3) {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k] as f64);
            hi[k] = hi[k].max(p[k] as f64);
        }
    }
    (lo, hi)
}

#[test]
fn oblique_rpy_box_and_mesh_visuals_coincide() {
    let dir = std::env::temp_dir().join(format!("vcad-urdf-rpy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stl = dir.join("box.stl");
    write_box_stl(&stl);

    let prim = link_mesh(&urdf(&format!(
        r#"<box size="{} {} {}"/>"#,
        SIZE_M[0], SIZE_M[1], SIZE_M[2]
    )));
    let mesh = link_mesh(&urdf(&format!(r#"<mesh filename="{}"/>"#, stl.display())));
    let _ = std::fs::remove_dir_all(&dir);

    // Expected: the URDF placement of the centred box, in millimetres.
    let r = rpy_matrix(RPY);
    let mut want_lo = [f64::INFINITY; 3];
    let mut want_hi = [f64::NEG_INFINITY; 3];
    for c in corners() {
        for i in 0..3 {
            let v = 1000.0 * ((0..3).map(|j| r[i][j] * c[j]).sum::<f64>() + XYZ_M[i]);
            want_lo[i] = want_lo[i].min(v);
            want_hi[i] = want_hi[i].max(v);
        }
    }

    for (label, m) in [("box", &prim), ("mesh", &mesh)] {
        let (lo, hi) = bbox(m);
        for k in 0..3 {
            assert!(
                (lo[k] - want_lo[k]).abs() < 1e-2 && (hi[k] - want_hi[k]).abs() < 1e-2,
                "{label} visual bbox {lo:?}..{hi:?} != URDF placement {want_lo:?}..{want_hi:?}"
            );
        }
    }
}
