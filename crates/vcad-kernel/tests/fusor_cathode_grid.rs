//! Regression: the open-fusor cathode grid (docs/videos/open-fusor) — four
//! wire rings (tori) and eight support "rods", each the hull of two r = 0.8
//! spheres, joined end to end at shared spheres.
//!
//! A hull is a mesh by construction, so every union here runs on triangle
//! soup. Folded pairwise in authored order the root came out at ~150 000
//! triangles from operands totalling ~17 000, with ~37 000 non-manifold and
//! ~21 000 folded edges, different on every run.

use std::collections::HashMap;

use vcad_kernel::vcad_kernel_tessellate::TriangleMesh;
use vcad_kernel::Solid;

const R_OUT: f64 = 11.168;
const Z_OUT: f64 = 15.371;
const R_IN: f64 = 18.070;
const Z_IN: f64 = 5.871;
const SEGMENTS: u32 = 32;

fn rod(x0: f64, z0: f64, x1: f64, z1: f64) -> Solid {
    let a = Solid::sphere(0.8, SEGMENTS).translate(x0, 0.0, z0);
    let b = Solid::sphere(0.8, SEGMENTS).translate(x1, 0.0, z1);
    Solid::hull(&[&a, &b], &[]).expect("hull")
}

fn ring(r: f64, z: f64) -> Solid {
    Solid::torus(r, 1.0, SEGMENTS).translate(0.0, 0.0, z)
}

/// The four rods of one spine, in authored order.
fn spine_rods(sx: f64) -> Vec<Solid> {
    vec![
        rod(0.0, 24.0, sx * R_OUT, Z_OUT),
        rod(sx * R_OUT, Z_OUT, sx * R_IN, Z_IN),
        rod(sx * R_IN, Z_IN, sx * R_IN, -Z_IN),
        rod(sx * R_IN, -Z_IN, sx * R_OUT, -Z_OUT),
    ]
}

fn rings() -> Vec<Solid> {
    vec![
        ring(R_OUT, Z_OUT),
        ring(R_IN, Z_IN),
        ring(R_IN, -Z_IN),
        ring(R_OUT, -Z_OUT),
    ]
}

/// Right-nested fold, as `[union a [union b [union c d]]]` evaluates.
fn fold_right(mut operands: Vec<Solid>) -> Solid {
    let mut acc = operands.pop().expect("operands");
    while let Some(s) = operands.pop() {
        acc = s.union(&acc);
    }
    acc
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

/// FNV-1a over the exact vertex and index buffers: two runs agree iff the
/// mesh is bit-identical.
fn fingerprint(mesh: &TriangleMesh) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for v in &mesh.vertices {
        eat(&v.to_bits().to_le_bytes());
    }
    for i in &mesh.indices {
        eat(&i.to_le_bytes());
    }
    h
}

struct Stage {
    label: &'static str,
    /// Loose volume window: the grid is ~1300 mm³ of wire, and no closed
    /// form is worth deriving for the overlaps.
    volume: (f64, f64),
    max_tris: usize,
}

fn check(stage: &Stage, build: impl Fn() -> Solid) -> Vec<String> {
    let mut fails = Vec::new();
    let solid = build();
    let mesh = solid.to_mesh(SEGMENTS);
    let (nonmanifold, folded, boundary) = edge_census(&mesh);
    let vol = signed_volume(&mesh);
    let tris = mesh.indices.len() / 3;
    let print = fingerprint(&mesh);
    eprintln!(
        "{}: tris {tris} volume {vol:.1} nonmanifold {nonmanifold} folded {folded} \
         boundary {boundary} {:?} fingerprint {print:016x}",
        stage.label,
        solid.fidelity()
    );
    if (nonmanifold, folded, boundary) != (0, 0, 0) {
        fails.push(format!(
            "{}: {nonmanifold} non-manifold, {folded} folded, {boundary} boundary edges",
            stage.label
        ));
    }
    if !(stage.volume.0..=stage.volume.1).contains(&vol) {
        fails.push(format!(
            "{}: volume {vol:.1} outside {:?}",
            stage.label, stage.volume
        ));
    }
    if tris > stage.max_tris {
        fails.push(format!(
            "{}: {tris} triangles (ceiling {})",
            stage.label, stage.max_tris
        ));
    }
    // Same inputs, same process: the result must be bit-identical.
    let again = fingerprint(&build().to_mesh(SEGMENTS));
    if again != print {
        fails.push(format!(
            "{}: not deterministic ({print:016x} then {again:016x})",
            stage.label
        ));
    }
    fails
}

/// `r-rod`: one hull. Soup by construction, but closed and manifold.
#[test]
fn rod_is_closed() {
    let fails = check(
        &Stage {
            label: "r-rod",
            volume: (28.0, 32.0),
            max_tris: 1200,
        },
        || rod(0.0, 24.0, R_OUT, Z_OUT),
    );
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// `t-rod-rod`: two rods sharing an end sphere (the exposed apex joint).
#[test]
fn rods_sharing_an_end_sphere() {
    let fails = check(
        &Stage {
            label: "t-rod-rod",
            volume: (55.0, 62.0),
            max_tris: 3000,
        },
        || rod(0.0, 24.0, R_OUT, Z_OUT).union(&rod(0.0, 24.0, -R_OUT, Z_OUT)),
    );
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// `s-torus-rod`: a rod ending inside a ring's tube.
#[test]
fn ring_and_rod() {
    let fails = check(
        &Stage {
            label: "s-torus-rod",
            volume: (243.0, 250.0),
            max_tris: 6000,
        },
        || ring(R_OUT, Z_OUT).union(&rod(0.0, 24.0, R_OUT, Z_OUT)),
    );
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// One spine: four rods chained through three shared spheres.
#[test]
fn spine() {
    let fails = check(
        &Stage {
            label: "spine",
            volume: (95.0, 105.0),
            max_tris: 6000,
        },
        || fold_right(spine_rods(1.0)),
    );
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// The whole root, nested as authored:
/// `[union rings [union [spine 1] [spine -1]]]`.
#[test]
fn cathode_grid() {
    let fails = check(
        &Stage {
            label: "cathode-grid",
            volume: (1290.0, 1330.0),
            max_tris: 30_000,
        },
        || {
            let spines = fold_right(spine_rods(1.0)).union(&fold_right(spine_rods(-1.0)));
            fold_right(rings()).union(&spines)
        },
    );
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}
