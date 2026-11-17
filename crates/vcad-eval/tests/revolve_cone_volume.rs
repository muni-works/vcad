//! Full revolutions of slanted profile edges are analytic cones.
//!
//! A slanted edge used to revolve into 32 planar facets: the volume read as
//! the inscribed 32-gon's, and every boolean against the part split 32 faces
//! instead of one (a KF flange unioned with its coaxial tube came back with
//! 1058 faces). Now each profile edge is one face, and the volume matches
//! Pappus to tessellation accuracy.

use std::f64::consts::PI;

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::{CsgOp, Document, Node, SceneEntry, SketchSegment2D, Vec2, Vec3};

/// Revolve the closed polygon `pts` (x = radius, y = height, in the XZ
/// plane) a full turn about Z, optionally unioned with a coaxial cylinder.
fn revolve_doc(pts: &[(f64, f64)], tube: Option<(f64, f64, f64)>) -> Document {
    let mut doc = Document::new();
    let v = |(x, y): (f64, f64)| Vec2 { x, y };
    let segments = (0..pts.len())
        .map(|i| SketchSegment2D::Line {
            start: v(pts[i]),
            end: v(pts[(i + 1) % pts.len()]),
        })
        .collect();
    let node = |id, op| Node { id, name: None, op };
    doc.nodes.insert(
        1,
        node(
            1,
            CsgOp::Sketch2D {
                origin: Vec3::new(0.0, 0.0, 0.0),
                x_dir: Vec3::new(1.0, 0.0, 0.0),
                y_dir: Vec3::new(0.0, 0.0, 1.0),
                segments,
                holes: None,
            },
        ),
    );
    doc.nodes.insert(
        2,
        node(
            2,
            CsgOp::Revolve {
                sketch: 1,
                axis_origin: Vec3::new(0.0, 0.0, 0.0),
                axis_dir: Vec3::new(0.0, 0.0, 1.0),
                angle_deg: 360.0,
            },
        ),
    );
    let mut root = 2;
    if let Some((r, z0, h)) = tube {
        doc.nodes.insert(
            3,
            node(
                3,
                CsgOp::Cylinder {
                    radius: r,
                    height: h,
                    segments: 0,
                },
            ),
        );
        doc.nodes.insert(
            4,
            node(
                4,
                CsgOp::Translate {
                    child: 3,
                    offset: Vec3::new(0.0, 0.0, z0),
                },
            ),
        );
        doc.nodes
            .insert(5, node(5, CsgOp::Union { left: 2, right: 4 }));
        root = 5;
    }
    doc.roots.push(SceneEntry {
        root,
        material: "default".into(),
        visible: None,
    });
    doc
}

/// (volume, face count of the root B-rep).
fn eval(doc: &Document) -> (f64, usize) {
    let scene = evaluate_document(
        doc,
        &EvalOptions {
            skip_clash_detection: true,
            ..Default::default()
        },
    )
    .expect("evaluate");
    assert!(scene.failures.is_empty(), "failures: {:?}", scene.failures);
    let part = &scene.parts[0];
    let (pos, idx) = (&part.mesh.positions, &part.mesh.indices);
    let p = |i: u32| {
        let k = 3 * i as usize;
        [pos[k] as f64, pos[k + 1] as f64, pos[k + 2] as f64]
    };
    let vol = idx
        .chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum();
    let faces = part
        .solid
        .as_ref()
        .and_then(|s| s.as_brep())
        .map_or(0, |b| b.topology.faces.len());
    (vol, faces)
}

/// Pappus: area × 2π × centroid radius.
fn pappus(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    let (mut a2, mut cx6) = (0.0, 0.0);
    for i in 0..n {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % n];
        let c = x0 * y1 - x1 * y0;
        a2 += c;
        cx6 += (x0 + x1) * c;
    }
    let area = a2 / 2.0;
    let cx = cx6 / (3.0 * a2);
    (area * 2.0 * PI * cx).abs()
}

fn check(pts: &[(f64, f64)], faces: usize) {
    let (vol, n) = eval(&revolve_doc(pts, None));
    let expect = pappus(pts);
    let err = (vol - expect) / expect;
    // 32-segment rims keep 99.36% of a circle's area.
    assert!(
        err.abs() < 0.01,
        "{pts:?}: volume {vol:.1} vs Pappus {expect:.1} ({:+.2}%)",
        100.0 * err
    );
    assert_eq!(n, faces, "{pts:?}: one face per profile edge");
}

#[test]
fn revolved_trapezoid_ring_is_one_cone_face() {
    // Ring with a conical outer wall: bottom r 5..10, top r 5..7, height 6.
    check(&[(5.0, 0.0), (10.0, 0.0), (7.0, 6.0), (5.0, 6.0)], 4);
}

#[test]
fn revolved_triangle_on_axis_is_a_pointed_cone() {
    // Right triangle with a leg on the axis: a plain cone, r 8, h 12. The
    // on-axis edge sweeps nothing, so two faces: base disc + cone.
    check(&[(0.0, 0.0), (8.0, 0.0), (0.0, 12.0)], 2);
}

#[test]
fn revolved_triangle_ring_volume() {
    // A triangle off the axis with two slanted edges (both cones).
    check(&[(10.0, 0.0), (14.0, 0.0), (12.0, 3.0)], 3);
}

#[test]
fn kf_flange_profile_union_with_coaxial_tube_stays_small() {
    // The KF40 flange profile (cone back face) unioned with its tube: every
    // face is a surface of revolution about the same axis, so the result
    // must stay a handful of faces — it was 1058 with a faceted cone.
    let flange = [
        (0.0, -6.264),
        (19.55, -6.264),
        (27.5, -3.0),
        (27.5, 0.0),
        (0.0, 0.0),
    ];
    let (vol, faces) = eval(&revolve_doc(&flange, Some((19.05, -40.0, 38.0))));
    assert!(faces < 40, "coaxial union produced {faces} faces");
    // Tube spans z ∈ [-40, -2]; the flange covers [-6.264, 0].
    let tube_outside = PI * 19.05 * 19.05 * (40.0 - 6.264);
    let expect = pappus(&flange) + tube_outside;
    let err = (vol - expect) / expect;
    assert!(
        err.abs() < 0.01,
        "volume {vol:.0} vs {expect:.0} ({:+.2}%)",
        100.0 * err
    );
}
