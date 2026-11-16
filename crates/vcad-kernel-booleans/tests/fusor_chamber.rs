//! Regression: the open-fusor vacuum chamber (docs/videos/open-fusor).
//!
//! A r=79.5 can with radial port nozzles, bored by a r=76 cavity and small
//! (r=19/11/20/10) port bores, then sectioned by a quadrant cube whose
//! corner edge is the can's axis. Unequal-radius crossing cylinders plus
//! planar cuts through the axis.

use std::collections::HashMap;
use std::f64::consts::PI;

use vcad_kernel_booleans::{
    boolean_op_reported, BooleanOp, BooleanReport, BooleanResult, Fidelity,
};
use vcad_kernel_math::Transform;
use vcad_kernel_primitives::{make_cube, make_cylinder, BRepSolid};
use vcad_kernel_tessellate::TriangleMesh;

const SEGMENTS: u32 = 32;

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

fn brep(r: &BooleanResult) -> BRepSolid {
    r.as_brep().expect("brep result").clone()
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

/// (non-manifold edges, folded edges, boundary edges) of a mesh welded at
/// 1e-5 — the same census the Mac app's feature-edge overlay is fed.
/// "Folded" = two triangles sharing an edge whose normals are within ~25°
/// of opposite (a sheet doubled back on itself).
fn edge_census(mesh: &TriangleMesh) -> (usize, usize, usize) {
    let key = |vi: u32| -> [i64; 3] {
        let i = vi as usize * 3;
        [
            (mesh.vertices[i] as f64 * 1e5).round() as i64,
            (mesh.vertices[i + 1] as f64 * 1e5).round() as i64,
            (mesh.vertices[i + 2] as f64 * 1e5).round() as i64,
        ]
    };
    let normal = |t: &[u32]| {
        let p = |i: u32| {
            let i = i as usize * 3;
            [
                mesh.vertices[i] as f64,
                mesh.vertices[i + 1] as f64,
                mesh.vertices[i + 2] as f64,
            ]
        };
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if l > 0.0 {
            [n[0] / l, n[1] / l, n[2] / l]
        } else {
            [0.0; 3]
        }
    };
    let mut adj: HashMap<([i64; 3], [i64; 3]), Vec<usize>> = HashMap::new();
    for (ti, t) in mesh.indices.chunks(3).enumerate() {
        let k = [key(t[0]), key(t[1]), key(t[2])];
        for i in 0..3 {
            let (a, b) = (k[i], k[(i + 1) % 3]);
            if a == b {
                continue;
            }
            adj.entry(if a < b { (a, b) } else { (b, a) })
                .or_default()
                .push(ti);
        }
    }
    let (mut nonmanifold, mut folded, mut boundary) = (0, 0, 0);
    for ts in adj.values() {
        match ts.len() {
            1 => {
                boundary += 1;
                if std::env::var_os("CENSUS_DUMP").is_some() {
                    let t = &mesh.indices[ts[0] * 3..ts[0] * 3 + 3];
                    eprintln!("  boundary tri: {:?} {:?} {:?}", key(t[0]), key(t[1]), key(t[2]));
                }
            }
            2 => {
                let n0 = normal(&mesh.indices[ts[0] * 3..ts[0] * 3 + 3]);
                let n1 = normal(&mesh.indices[ts[1] * 3..ts[1] * 3 + 3]);
                if n0[0] * n1[0] + n0[1] * n1[1] + n0[2] * n1[2] < -0.9 {
                    folded += 1;
                    if std::env::var_os("CENSUS_DUMP").is_some() {
                        let t = &mesh.indices[ts[0] * 3..ts[0] * 3 + 3];
                        let u = &mesh.indices[ts[1] * 3..ts[1] * 3 + 3];
                        eprintln!("  folded: {:?} {:?} {:?} | {:?} {:?} {:?}", key(t[0]), key(t[1]), key(t[2]), key(u[0]), key(u[1]), key(u[2]));
                    }
                }
            }
            _ => nonmanifold += 1,
        }
    }
    (nonmanifold, folded, boundary)
}


fn zcyl(r: f64, z0: f64, z1: f64) -> BRepSolid {
    transformed(
        make_cylinder(r, z1 - z0, SEGMENTS),
        &Transform::translation(0.0, 0.0, z0),
    )
}

fn ycyl(r: f64, y0: f64, y1: f64) -> BRepSolid {
    // `a.then(b)` is a·b: `b` acts first.
    let t = Transform::translation(0.0, y0, 0.0).then(&Transform::rotation_x(-PI / 2.0));
    transformed(make_cylinder(r, y1 - y0, SEGMENTS), &t)
}

fn xcyl(r: f64, x0: f64, x1: f64) -> BRepSolid {
    let t = Transform::translation(x0, 0.0, 0.0).then(&Transform::rotation_y(PI / 2.0));
    transformed(make_cylinder(r, x1 - x0, SEGMENTS), &t)
}

/// Every twin / loop / face key reachable from a live half-edge must be
/// live, and twins must be mutual.
fn dangling_keys(b: &BRepSolid) -> Vec<String> {
    let t = &b.topology;
    let mut bad = Vec::new();
    for (he, h) in &t.half_edges {
        match h.twin {
            Some(tw) => match t.half_edges.get(tw) {
                None => bad.push(format!("{he:?}: dead twin {tw:?}")),
                Some(x) if x.twin != Some(he) => bad.push(format!("{he:?}: twin not mutual")),
                _ => {}
            },
            // An unpaired half-edge is not dangling: the kernel leaves
            // some seam chains unpaired and welds them at tessellation.
            None => {}
        }
        match h.loop_id {
            Some(l) => match t.loops.get(l) {
                None => bad.push(format!("{he:?}: dead loop {l:?}")),
                Some(lp) => match lp.face {
                    Some(f) if t.faces.get(f).is_none() => {
                        bad.push(format!("{he:?}: loop {l:?} on dead face {f:?}"))
                    }
                    None => bad.push(format!("{he:?}: loop {l:?} has no face")),
                    _ => {}
                },
            },
            None => bad.push(format!("{he:?}: no loop")),
        }
    }
    bad
}

struct Bounds {
    volume: f64,
    tol_frac: f64,
    max_tris: usize,
}

/// Check one stage; returns its failures instead of asserting so a test
/// reports every stage of the chain, not just the first bad one.
fn check(label: &str, result: &BooleanResult, report: &BooleanReport, bounds: Bounds) -> Vec<String> {
    let fidelity = report.fidelity;
    let mut fails = Vec::new();
    let dangling = result.as_brep().map(dangling_keys).unwrap_or_default();
    if !dangling.is_empty() {
        fails.push(format!(
            "{label}: {} dangling topology keys, e.g. {:?}",
            dangling.len(),
            &dangling[..dangling.len().min(4)]
        ));
    }
    let mesh = result.to_mesh(SEGMENTS);
    let vol = signed_volume(&mesh);
    let (nonmanifold, folded, boundary) = edge_census(&mesh);
    let tris = mesh.indices.len() / 3;
    eprintln!(
        "{label}: volume {vol:.1} (truth {:.1}) tris {tris} nonmanifold {nonmanifold} \
         folded {folded} boundary {boundary} {fidelity:?}",
        bounds.volume
    );
    if fidelity != Fidelity::Analytic {
        fails.push(format!("{label}: not analytic: {:?}", report.reason));
    }
    if (nonmanifold, folded, boundary) != (0, 0, 0) {
        fails.push(format!(
            "{label}: {nonmanifold} non-manifold, {folded} folded, {boundary} boundary edges"
        ));
    }
    let rel = (vol - bounds.volume).abs() / bounds.volume;
    if rel > bounds.tol_frac {
        fails.push(format!(
            "{label}: volume {vol:.1} off truth {:.1} by {:.3}%",
            bounds.volume,
            rel * 100.0
        ));
    }
    if tris > bounds.max_tris {
        fails.push(format!("{label}: {tris} triangles (ceiling {})", bounds.max_tris));
    }
    fails
}

fn op(label: &str, a: &BRepSolid, b: &BRepSolid, op: BooleanOp) -> (BooleanResult, BooleanReport) {
    boolean_op_reported(a, b, op, SEGMENTS).unwrap_or_else(|e| panic!("{label}: {e:?}"))
}

const CAN: f64 = PI * 79.5 * 79.5 * 159.0;
const CAVITY: f64 = PI * 76.0 * 76.0 * 152.0;

/// Volume of a radius-`r` cylinder along a radial axis from the surface
/// of a coaxial-perpendicular radius-`big` cylinder out to distance `end`.
fn radial_stub(r: f64, big: f64, end: f64) -> f64 {
    // ∫∫ over the disc of (end − sqrt(big² − x²)), x across the big axis.
    let n = 20000;
    let mut v = 0.0;
    for i in 0..n {
        let x = -r + (i as f64 + 0.5) * 2.0 * r / n as f64;
        let chord = 2.0 * (r * r - x * x).sqrt();
        v += chord * (end - (big * big - x * x).sqrt()) * 2.0 * r / n as f64;
    }
    v
}

#[test]
fn can_with_radial_nozzle_then_cavity() {
    let mut fails = Vec::new();
    let can = zcyl(79.5, -79.5, 79.5);
    let (shell, f) = op("nozzle", &can, &ycyl(22.5, 70.0, 112.0), BooleanOp::Union);
    let v_shell = CAN + radial_stub(22.5, 79.5, 112.0);
    fails.extend(check(
        "can ∪ nozzle",
        &shell,
        &f,
        Bounds { volume: v_shell, tol_frac: 1e-3, max_tris: 3000 },
    ));

    let (cav, f) = op("cavity", &brep(&shell), &zcyl(76.0, -76.0, 76.0), BooleanOp::Difference);
    fails.extend(check(
        "− cavity",
        &cav,
        &f,
        Bounds { volume: v_shell - CAVITY, tol_frac: 1e-3, max_tris: 6000 },
    ));

    let (bored, f) = op("y bore", &brep(&cav), &ycyl(19.0, 60.0, 120.0), BooleanOp::Difference);
    // The bore removes the r=19 column from the cavity wall (r 76) outward
    // to the nozzle end (y 112).
    let v_bored = v_shell - CAVITY - radial_stub(19.0, 76.0, 112.0);
    fails.extend(check(
        "− y bore",
        &bored,
        &f,
        Bounds { volume: v_bored, tol_frac: 1e-3, max_tris: 8000 },
    ));
    assert!(fails.is_empty(), "{fails:#?}");
}

/// The section tool: the x > 0, y < 0 quadrant. Its corner edge is the Z
/// axis — the axis of every coaxial cylinder it cuts.
fn quadrant() -> BRepSolid {
    transformed(
        make_cube(400.0, 400.0, 800.0),
        &Transform::translation(0.0, -400.0, -400.0),
    )
}

/// `lid-flange`: a ring with a quadrant cut away.
#[test]
fn sectioned_ring() {
    let mut fails = Vec::new();
    let (ring, f) = op(
        "ring",
        &zcyl(57.0, 79.5, 97.0),
        &zcyl(10.0, 70.0, 110.0),
        BooleanOp::Difference,
    );
    let v_ring = PI * (57.0 * 57.0 - 10.0 * 10.0) * 17.5;
    fails.extend(check(
        "ring",
        &ring,
        &f,
        Bounds { volume: v_ring, tol_frac: 1e-3, max_tris: 1500 },
    ));
    let (cut, f) = op("section", &brep(&ring), &quadrant(), BooleanOp::Difference);
    fails.extend(check(
        "ring − quadrant",
        &cut,
        &f,
        Bounds { volume: 0.75 * v_ring, tol_frac: 1e-3, max_tris: 1500 },
    ));
    assert!(fails.is_empty(), "{fails:#?}");
}
