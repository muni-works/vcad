//! Regression: the open-fusor HV conductor (docs/videos/open-fusor) — a thin
//! rod whose top cap sits at the centre of a ball.
//!
//! A sphere centred on a cylinder's axis meets it in exact circles at axial
//! offsets `±√(R² − r²)`. That pair had no analytic SSI arm, so the marching
//! sampler's point dust left both faces unsplit: the B-rep result read
//! 12 950 mm³ against a true 3 197, the volume oracle condemned it, and the
//! mesh fallback's result carried 39 folded edges.

use std::collections::HashMap;

use vcad_kernel_booleans::{
    boolean_op_reported, BooleanOp, BooleanReport, BooleanResult, Fidelity,
};
use vcad_kernel_math::Transform;
use vcad_kernel_primitives::{make_cylinder, make_sphere, BRepSolid};
use vcad_kernel_tessellate::TriangleMesh;

fn transformed(mut brep: BRepSolid, t: &Transform) -> BRepSolid {
    for (_, v) in &mut brep.topology.vertices {
        v.point = t.apply_point(&v.point);
    }
    brep.geometry.surfaces = brep
        .geometry
        .surfaces
        .drain(..)
        .map(|s| s.transform(t))
        .collect();
    brep
}

fn zcyl(r: f64, z0: f64, z1: f64, segments: u32) -> BRepSolid {
    transformed(
        make_cylinder(r, z1 - z0, segments),
        &Transform::translation(0.0, 0.0, z0),
    )
}

fn ball(r: f64, z: f64, segments: u32) -> BRepSolid {
    transformed(
        make_sphere(r, segments),
        &Transform::translation(0.0, 0.0, z),
    )
}

fn signed_volume(mesh: &TriangleMesh) -> f64 {
    let p = |i: u32| {
        let i = i as usize * 3;
        [
            mesh.vertices[i] as f64,
            mesh.vertices[i + 1] as f64,
            mesh.vertices[i + 2] as f64,
        ]
    };
    mesh.indices
        .chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// (non-manifold, folded, boundary) edges of a mesh welded at 1e-4 — the
/// `root_census` metric. "Folded" = the two triangles on an edge traverse it
/// the same way (inconsistent winding) or face within ~25° of opposite.
fn edge_census(mesh: &TriangleMesh) -> (usize, usize, usize) {
    let p = |vi: u32| {
        let i = vi as usize * 3;
        [
            mesh.vertices[i] as f64,
            mesh.vertices[i + 1] as f64,
            mesh.vertices[i + 2] as f64,
        ]
    };
    let key = |vi: u32| p(vi).map(|c| (c * 1e4).round() as i64);
    let normal = |t: &[u32]| {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-300);
        [n[0] / l, n[1] / l, n[2] / l]
    };
    let mut adj: HashMap<([i64; 3], [i64; 3]), Vec<(usize, bool)>> = HashMap::new();
    for (ti, t) in mesh.indices.chunks(3).enumerate() {
        let k = [key(t[0]), key(t[1]), key(t[2])];
        for i in 0..3 {
            let (a, b) = (k[i], k[(i + 1) % 3]);
            if a == b {
                continue;
            }
            let (e, fwd) = if a < b {
                ((a, b), true)
            } else {
                ((b, a), false)
            };
            adj.entry(e).or_default().push((ti, fwd));
        }
    }
    let (mut nonmanifold, mut folded, mut boundary) = (0, 0, 0);
    for ts in adj.values() {
        match ts.len() {
            1 => boundary += 1,
            2 => {
                let n0 = normal(&mesh.indices[ts[0].0 * 3..ts[0].0 * 3 + 3]);
                let n1 = normal(&mesh.indices[ts[1].0 * 3..ts[1].0 * 3 + 3]);
                let dot = n0[0] * n1[0] + n0[1] * n1[1] + n0[2] * n1[2];
                if ts[0].1 == ts[1].1 || dot < -0.9 {
                    folded += 1;
                }
            }
            _ => nonmanifold += 1,
        }
    }
    (nonmanifold, folded, boundary)
}

/// Volume of the cap of a radius-`r` ball beyond a plane at distance `d`
/// from its centre.
fn cap(r: f64, d: f64) -> f64 {
    let h = r - d;
    std::f64::consts::PI * h * h * (3.0 * r - h) / 3.0
}

fn check(
    label: &str,
    result: &BooleanResult,
    report: &BooleanReport,
    segments: u32,
    truth: f64,
    tol_frac: f64,
) -> Vec<String> {
    let mut fails = Vec::new();
    let mesh = result.to_mesh(segments);
    if std::env::var_os("CENSUS_FACES").is_some() {
        let b = result.as_brep().expect("brep");
        let t = &b.topology;
        for (fid, f) in &t.faces {
            let pts = |l| -> Vec<vcad_kernel_math::Point3> {
                t.loop_half_edges(l)
                    .map(|he| t.vertices[t.half_edges[he].origin].point)
                    .collect()
            };
            let outer = pts(f.outer_loop);
            let zs: Vec<f64> = outer.iter().map(|p| p.z).collect();
            let unpaired = t
                .loop_half_edges(f.outer_loop)
                .filter(|he| t.half_edges[*he].twin.is_none())
                .count();
            eprintln!(
                "  {fid:?} {:?} {:?} outer {} (unpaired {unpaired}) inner {:?} z {:.3}..{:.3}",
                b.geometry.surfaces[f.surface_index].surface_type(),
                f.orientation,
                outer.len(),
                f.inner_loops
                    .iter()
                    .map(|&l| pts(l).len())
                    .collect::<Vec<_>>(),
                zs.iter().cloned().fold(f64::INFINITY, f64::min),
                zs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            );
        }
    }
    let vol = signed_volume(&mesh);
    let (nonmanifold, folded, boundary) = edge_census(&mesh);
    eprintln!(
        "{label}: volume {vol:.1} (truth {truth:.1}) tris {} faces {} nonmanifold {nonmanifold} \
         folded {folded} boundary {boundary} {:?} {:?}",
        mesh.indices.len() / 3,
        report.faces,
        report.fidelity,
        report.reason
    );
    if report.fidelity != Fidelity::Analytic {
        fails.push(format!("{label}: not analytic: {:?}", report.reason));
    }
    if (nonmanifold, folded, boundary) != (0, 0, 0) {
        fails.push(format!(
            "{label}: {nonmanifold} non-manifold, {folded} folded, {boundary} boundary edges"
        ));
    }
    if (vol - truth).abs() > tol_frac * truth {
        fails.push(format!(
            "{label}: volume {vol:.1} off truth {truth:.1} by {:.2}%",
            (vol - truth).abs() / truth * 100.0
        ));
    }
    fails
}

/// Inscribed-polygon deficit of a fully round body at `segments`, doubled:
/// a lat-long ball under-reads in both directions.
fn tol(segments: u32) -> f64 {
    let step = 2.0 * std::f64::consts::PI / f64::from(segments);
    3.0 * (1.0 - step.sin() / step) + 1e-3
}

const PI: f64 = std::f64::consts::PI;

/// The root itself: `[union [zcyl 1.6 24 168] [translate 0 0 168 [sphere 8]]]`.
#[test]
fn rod_ending_at_ball_centre_unions_analytically() {
    let mut fails = Vec::new();
    for segments in [32, 64] {
        let rod = zcyl(1.6, 24.0, 168.0, segments);
        let ball = ball(8.0, 168.0, segments);
        // Rod below the lower intersection circle, plus the whole ball.
        let h = (64.0f64 - 2.56).sqrt();
        let truth = PI * 2.56 * (144.0 - h) - cap(8.0, h) + 4.0 / 3.0 * PI * 512.0;
        for (label, a, b) in [("rod ∪ ball", &rod, &ball), ("ball ∪ rod", &ball, &rod)] {
            let (result, report) =
                boolean_op_reported(a, b, BooleanOp::Union, segments).expect("union");
            fails.extend(check(
                &format!("{label} @{segments}"),
                &result,
                &report,
                segments,
                truth,
                tol(segments),
            ));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// A rod straight through the ball: both circles lie on both faces.
#[test]
fn rod_through_ball() {
    let mut fails = Vec::new();
    for segments in [32, 64] {
        let rod = zcyl(1.6, -20.0, 20.0, segments);
        let ball = ball(8.0, 0.0, segments);
        let h = (64.0f64 - 2.56).sqrt();
        let core = PI * 2.56 * 2.0 * h + 2.0 * cap(8.0, h);
        let (rod_v, ball_v) = (PI * 2.56 * 40.0, 4.0 / 3.0 * PI * 512.0);
        for (label, op, a, b, truth) in [
            (
                "rod ∪ ball",
                BooleanOp::Union,
                &rod,
                &ball,
                rod_v + ball_v - core,
            ),
            (
                "ball − rod",
                BooleanOp::Difference,
                &ball,
                &rod,
                ball_v - core,
            ),
            (
                "rod − ball",
                BooleanOp::Difference,
                &rod,
                &ball,
                rod_v - core,
            ),
            ("rod ∩ ball", BooleanOp::Intersection, &rod, &ball, core),
        ] {
            let (result, report) = boolean_op_reported(a, b, op, segments).expect("boolean");
            fails.extend(check(
                &format!("{label} @{segments}"),
                &result,
                &report,
                segments,
                truth,
                tol(segments),
            ));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// The hand-built capsule idiom: a ball of the cylinder's own radius centred
/// on its cap. The surfaces are tangent along the rim and never cross.
#[test]
fn equal_radius_ball_on_cylinder_cap() {
    let mut fails = Vec::new();
    for segments in [32, 64] {
        let rod = zcyl(4.0, 0.0, 20.0, segments);
        let ball = ball(4.0, 20.0, segments);
        let truth = PI * 16.0 * 20.0 + 2.0 / 3.0 * PI * 64.0;
        let (result, report) =
            boolean_op_reported(&rod, &ball, BooleanOp::Union, segments).expect("union");
        fails.extend(check(
            &format!("capsule end @{segments}"),
            &result,
            &report,
            segments,
            truth,
            tol(segments),
        ));
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// The same union at the mesh level. Two closed manifold operands must give
/// a closed manifold result whatever the B-rep pipeline does: this is what
/// every soup operand in a document falls back on.
#[test]
fn rod_into_ball_mesh_union_is_manifold() {
    let segments = 32;
    let rod = vcad_kernel_tessellate::tessellate_brep(&zcyl(1.6, 24.0, 168.0, segments), segments);
    let ball = vcad_kernel_tessellate::tessellate_brep(&ball(8.0, 168.0, segments), segments);
    assert_eq!(edge_census(&rod), (0, 0, 0), "rod operand");
    assert_eq!(edge_census(&ball), (0, 0, 0), "ball operand");
    let truth = signed_volume(&rod) + signed_volume(&ball) - PI * 2.56 * (64.0f64 - 2.56).sqrt();
    let mut fails = Vec::new();
    for (label, mesh) in [
        (
            "manifold_csg",
            vcad_kernel_booleans::mesh::csg::manifold_csg(&rod, &ball, BooleanOp::Union),
        ),
        (
            "mesh_union_all",
            vcad_kernel_booleans::mesh::csg::mesh_union_all(&[rod.clone(), ball.clone()]),
        ),
    ] {
        let census = edge_census(&mesh);
        let vol = signed_volume(&mesh);
        eprintln!(
            "{label}: tris {} volume {vol:.1} (truth ~{truth:.1}) census {census:?}",
            mesh.indices.len() / 3
        );
        if census != (0, 0, 0) {
            fails.push(format!("{label}: census {census:?}"));
        }
        if (vol - truth).abs() > 0.01 * truth {
            fails.push(format!("{label}: volume {vol:.1} vs {truth:.1}"));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}
