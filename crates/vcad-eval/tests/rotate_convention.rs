//! Code that turns a direction or frame into `CsgOp::Rotate` angles must use
//! the matrix the evaluator actually builds: `R = Rx·Ry·Rz`
//! (`rx.then(&ry).then(&rz)`, a right-multiply — Z acts on the part first,
//! X last). Helpers that decomposed as `Rz·Ry·Rx` passed their own round-trip
//! tests and still mis-placed every part needing two nonzero angles, so these
//! tests go through `evaluate_document` and check where geometry lands.

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::{CsgOp, Document, Node, SceneEntry, Vec3};
use vcad_loon::eval_vcad;

/// Evaluate every root; return all mesh vertices.
fn vertices(doc: &Document) -> Vec<[f64; 3]> {
    let scene = evaluate_document(
        doc,
        &EvalOptions {
            skip_clash_detection: true,
            ..Default::default()
        },
    )
    .expect("evaluate");
    assert!(scene.failures.is_empty(), "failures: {:?}", scene.failures);
    let mut out = Vec::new();
    for part in &scene.parts {
        assert!(!part.mesh.indices.is_empty(), "empty mesh");
        out.extend(
            part.mesh
                .positions
                .chunks(3)
                .map(|v| [v[0] as f64, v[1] as f64, v[2] as f64]),
        );
    }
    assert!(!out.is_empty(), "no geometry");
    out
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / n, v[1] / n, v[2] / n]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// (min, max) of the vertices projected onto `d`.
fn span(vs: &[[f64; 3]], d: [f64; 3]) -> (f64, f64) {
    vs.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| {
        let t = dot(*v, d);
        (lo.min(t), hi.max(t))
    })
}

/// Largest distance of any vertex from the line through `p` along unit `d`.
fn max_radial(vs: &[[f64; 3]], p: [f64; 3], d: [f64; 3]) -> f64 {
    vs.iter()
        .map(|v| {
            let w = [v[0] - p[0], v[1] - p[1], v[2] - p[2]];
            let t = dot(w, d);
            let r = [w[0] - t * d[0], w[1] - t * d[1], w[2] - t * d[2]];
            dot(r, r).sqrt()
        })
        .fold(0.0, f64::max)
}

/// A hole tool along an oblique axis is a cylinder on that axis: every vertex
/// within the hole radius of the axis line, the length spanning `depth`.
#[test]
fn clearance_hole_lies_on_an_oblique_axis() {
    for axis in [[1.0, 2.0, 3.0], [-0.3, 0.7, -0.2], [0.6, -0.8, 0.0]] {
        let src = format!(
            "[root [clearance-hole \"M4\" 40  5 -7 11  {} {} {}] \"steel\"]",
            axis[0], axis[1], axis[2]
        );
        let doc = eval_vcad(&src, None).expect("evaluates");
        let vs = vertices(&doc);
        let d = unit(axis);
        let p = [5.0, -7.0, 11.0];
        let r = max_radial(&vs, p, d);
        // M4 clearance is ⌀4.5; a mis-oriented 40 mm cylinder strays ~tens of mm.
        assert!(
            r < 2.25 + 0.05,
            "axis {axis:?}: vertex {r:.2} mm off the hole axis"
        );
        let (lo, hi) = span(&vs, d);
        let base = dot(p, d);
        assert!(
            (lo - base).abs() < 0.5 && (hi - base - 40.0).abs() < 0.5,
            "axis {axis:?}: hole spans {:.2}..{:.2} along its axis, want 0..40",
            lo - base,
            hi - base
        );
    }
}

/// Bolts on a bolt circle about an oblique axis: the ring lies in the plane
/// normal to the axis and every bolt points along it, so the whole ring
/// projects onto the axis exactly like the same ring about +Z projects onto Z.
/// A ring plane and bolt axis that disagree (one rotated by each convention)
/// smear the projection by the 30 mm ring radius.
#[test]
fn bolt_circle_about_an_oblique_axis_matches_the_z_ring() {
    let ring = |a: [f64; 3]| {
        let src = format!(
            "[root [bolt-circle \"M4x12\" \"shcs\" 60 3  0 0 0  {} {} {}  8] \"steel\"]",
            a[0], a[1], a[2]
        );
        vertices(&eval_vcad(&src, None).expect("evaluates"))
    };
    let (zlo, zhi) = span(&ring([0.0, 0.0, 1.0]), [0.0, 0.0, 1.0]);
    for axis in [[1.0, 2.0, 3.0], [-0.3, 0.7, -0.2]] {
        let d = unit(axis);
        let (lo, hi) = span(&ring(axis), d);
        assert!(
            (lo - zlo).abs() < 0.3 && (hi - zhi).abs() < 0.3,
            "axis {axis:?}: ring spans {lo:.2}..{hi:.2} along its axis, +Z ring spans {zlo:.2}..{zhi:.2}"
        );
        let r = max_radial(&ring(axis), [0.0, 0.0, 0.0], d);
        let rz = max_radial(&ring([0.0, 0.0, 1.0]), [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        assert!(
            (r - rz).abs() < 0.3,
            "axis {axis:?}: ring radius {r:.2} vs +Z ring {rz:.2}"
        );
    }
}

/// Sanity anchor for the convention itself: `Rotate{x:90, y:0, z:90}` on a
/// long X bar. Rx·Ry·Rz turns X→Y by Rz, then Y→Z by Rx: the bar ends up
/// along +Z. (Rz·Ry·Rx would leave it along +Y.)
#[test]
fn rotate_applies_z_first() {
    let mut doc = Document::new();
    doc.nodes.insert(
        1,
        Node {
            id: 1,
            name: None,
            op: CsgOp::Cube {
                size: Vec3::new(40.0, 2.0, 2.0),
            },
        },
    );
    doc.nodes.insert(
        2,
        Node {
            id: 2,
            name: None,
            op: CsgOp::Rotate {
                child: 1,
                angles: Vec3::new(90.0, 0.0, 90.0),
            },
        },
    );
    doc.roots.push(SceneEntry {
        root: 2,
        material: "default".into(),
        visible: None,
    });
    let vs = vertices(&doc);
    let (zlo, zhi) = span(&vs, [0.0, 0.0, 1.0]);
    assert!(
        (zhi - zlo - 40.0).abs() < 1e-3,
        "bar spans {zlo:.2}..{zhi:.2} in z; Rotate is Rx·Ry·Rz"
    );
}
