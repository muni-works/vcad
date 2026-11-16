//! Per-root mesh census of a `.vcad` document: triangle count, signed
//! volume, open / non-manifold / folded edges, and whether each root stayed
//! analytic or degraded (with the kernel's own account of why).
//!
//! ```text
//! cargo run --release -p vcad-eval --example root_census -- part.loon [segments]
//! ```

use std::collections::HashMap;

use vcad_eval::{evaluate_document, EvalOptions};

fn census(verts: &[f32], idx: &[u32]) -> (usize, usize, usize, f64, usize) {
    let p = |i: u32| {
        let i = i as usize * 3;
        [verts[i] as f64, verts[i + 1] as f64, verts[i + 2] as f64]
    };
    let key = |i: u32| {
        let q = p(i);
        [
            (q[0] * 1e4).round() as i64,
            (q[1] * 1e4).round() as i64,
            (q[2] * 1e4).round() as i64,
        ]
    };
    let normal = |t: &[u32]| {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        (n, l)
    };
    let mut adj: HashMap<([i64; 3], [i64; 3]), Vec<(usize, bool)>> = HashMap::new();
    let mut vol = 0.0;
    let mut slivers = 0;
    for (ti, t) in idx.chunks(3).enumerate() {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))
            / 6.0;
        // Sliver: aspect (longest edge² / 2·area) above 50.
        let d = |x: [f64; 3], y: [f64; 3]| {
            (x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)
        };
        let longest = d(a, b).max(d(b, c)).max(d(c, a));
        let (_, twice_area) = normal(t);
        if longest > 50.0 * twice_area {
            slivers += 1;
        }
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
                let (n0, l0) = normal(&idx[ts[0].0 * 3..ts[0].0 * 3 + 3]);
                let (n1, l1) = normal(&idx[ts[1].0 * 3..ts[1].0 * 3 + 3]);
                let dot = (n0[0] * n1[0] + n0[1] * n1[1] + n0[2] * n1[2]) / (l0 * l1).max(1e-300);
                // Same direction along the shared edge = inconsistent winding.
                if ts[0].1 == ts[1].1 || dot < -0.9 {
                    folded += 1;
                }
            }
            _ => nonmanifold += 1,
        }
    }
    (nonmanifold, folded, boundary, vol, slivers)
}

/// Triangles whose winding opposes their own vertex normals — what the
/// viewport shades with, so a non-zero count is a black or inside-out facet.
fn opposed_normals(verts: &[f32], normals: Option<&[f32]>, idx: &[u32]) -> usize {
    let Some(normals) = normals.filter(|n| n.len() == verts.len()) else {
        return 0;
    };
    let v = |a: &[f32], i: u32| {
        let i = i as usize * 3;
        [a[i] as f64, a[i + 1] as f64, a[i + 2] as f64]
    };
    idx.chunks(3)
        .filter(|t| {
            let (a, b, c) = (v(verts, t[0]), v(verts, t[1]), v(verts, t[2]));
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            let (na, nb, nc) = (v(normals, t[0]), v(normals, t[1]), v(normals, t[2]));
            let dot: f64 = (0..3).map(|k| n[k] * (na[k] + nb[k] + nc[k])).sum();
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            // Zero-area triangles have no winding to oppose.
            len > 1e-9 && dot < 0.0
        })
        .count()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: root_census <file.vcad|file.loon> [segments]");
    let segments: Option<u32> = args.next().map(|s| s.parse().expect("segments"));
    let doc: vcad_ir::Document = if path.ends_with(".loon") {
        vcad_loon::eval_vcad_file(std::path::Path::new(&path)).expect("evaluate loon")
    } else {
        let text = std::fs::read_to_string(&path).expect("read");
        serde_json::from_str(&text).expect("parse .vcad")
    };
    let mut options = EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    };
    if let Some(s) = segments {
        options.mesh_segments = s;
    }
    let t0 = std::time::Instant::now();
    let scene = evaluate_document(&doc, &options).expect("evaluate");
    eprintln!("evaluated in {:.2}s", t0.elapsed().as_secs_f64());
    for f in &scene.failures {
        println!("FAILURE: {f:?}");
    }
    for (i, part) in scene.parts.iter().enumerate() {
        let name = doc
            .roots
            .get(i)
            .and_then(|r| doc.nodes.get(&r.root))
            .and_then(|n| n.name.clone())
            .unwrap_or_else(|| format!("part_{i}"));
        let m = &part.mesh;
        let (nm, folded, boundary, vol, slivers) = census(&m.positions, &m.indices);
        let opposed = opposed_normals(&m.positions, m.normals.as_deref(), &m.indices);
        let (fidelity, faces) = match &part.solid {
            Some(s) => (
                s.fidelity().as_str().to_string(),
                s.brep().map(|b| b.topology.faces.len()).unwrap_or(0),
            ),
            None => ("no-solid".into(), 0),
        };
        println!(
            "{name:18} tris {:7} slivers {slivers:6} vol {vol:12.1} nonmanifold {nm:4} folded {folded:4} \
             boundary {boundary:4} opposed-normals {opposed:4} brep-faces {faces:5} {fidelity}",
            m.indices.len() / 3
        );
        if let Some(s) = &part.solid {
            for e in s.degradations() {
                println!("    - {e}");
            }
        }
    }
}
