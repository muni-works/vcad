//! Regression: the open-fusor HV conductor and cathode grid
//! (docs/videos/open-fusor), evaluated the way the Mac app evaluates them.
//!
//! The kernel-level cases live in
//! `vcad-kernel-booleans/tests/fusor_hv_conductor.rs` and
//! `vcad-kernel/tests/fusor_cathode_grid.rs`; this one pins the document
//! path — loon source, right-nested unions, hulls evaluated to soup.
//!
//! * `hv-conductor` (a rod ending at the centre of a ball) came out as
//!   1 430 triangles of soup with 39 folded edges;
//! * `cathode-grid` (four rings, eight hull rods) came out as ~150 000
//!   triangles with ~37 000 non-manifold and ~21 000 folded edges, and a
//!   different mesh on every run.

use std::collections::HashMap;

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_kernel::SolidFidelity;

const SOURCE: &str = r#"
[let wire-r 1.0]
[let r-out 11.168] [let z-out 15.371]
[let r-in 18.070]  [let z-in 5.871]
[material "grid" 0.95 0.55 0.20 0.8 0.4]
[material "copper" 0.85 0.50 0.30 0.9 0.3]

[let zcyl [fn [r z0 z1] [translate 0.0 0.0 z0 [cylinder r [- z1 z0]]]]]
[let rod [fn [r x0 z0 x1 z1]
  [hull [translate x0 0.0 z0 [sphere r]] [translate x1 0.0 z1 [sphere r]]]]]

[root-named [union [zcyl 1.6 24.0 168.0] [translate 0.0 0.0 168.0 [sphere 8.0]]] "copper" "hv-conductor"]

[let ring [fn [r z] [translate 0.0 0.0 z [torus r wire-r]]]]
[let rings
  [union [ring r-out z-out]
    [union [ring r-in z-in]
      [union [ring r-in [- 0.0 z-in]] [ring r-out [- 0.0 z-out]]]]]]
[let spine [fn [sx]
  [union [rod 0.8 0.0 24.0 [* sx r-out] z-out]
    [union [rod 0.8 [* sx r-out] z-out [* sx r-in] z-in]
      [union [rod 0.8 [* sx r-in] z-in [* sx r-in] [- 0.0 z-in]]
             [rod 0.8 [* sx r-in] [- 0.0 z-in] [* sx r-out] [- 0.0 z-out]]]]]]]
[root-named [union rings [union [spine 1.0] [spine -1.0]]] "grid" "cathode-grid"]
"#;

/// (non-manifold, folded-or-misoriented, boundary) edges, signed volume and
/// sliver count of a mesh welded at 1e-4.
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

#[test]
fn fusor_conductor_and_cathode_grid_are_clean() {
    let doc = vcad_loon::eval_vcad(SOURCE, None).expect("loon");
    let options = EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    };
    let scene = evaluate_document(&doc, &options).expect("evaluate");
    assert!(scene.failures.is_empty(), "{:?}", scene.failures);
    assert_eq!(scene.parts.len(), 2);

    // (name, volume window, triangle ceiling, must stay analytic)
    let expect = [
        ("hv-conductor", (3_180.0, 3_240.0), 2_500usize, true),
        ("cathode-grid", (1_290.0, 1_330.0), 25_000, false),
    ];
    let mut fails = Vec::new();
    for (part, (name, volume, max_tris, analytic)) in scene.parts.iter().zip(expect) {
        let m = &part.mesh;
        let tris = m.indices.len() / 3;
        let (nonmanifold, folded, boundary, vol, _) = census(&m.positions, &m.indices);
        let solid = part.solid.as_ref().expect("solid");
        eprintln!(
            "{name}: tris {tris} vol {vol:.1} nonmanifold {nonmanifold} folded {folded} \
             boundary {boundary} {}",
            solid.fidelity().as_str()
        );
        if analytic && solid.fidelity() != SolidFidelity::Analytic {
            fails.push(format!("{name}: {:?}", solid.degradations()));
        }
        if (nonmanifold, folded, boundary) != (0, 0, 0) {
            fails.push(format!(
                "{name}: {nonmanifold} non-manifold, {folded} folded, {boundary} boundary edges"
            ));
        }
        if !(volume.0..=volume.1).contains(&vol) {
            fails.push(format!("{name}: volume {vol:.1} outside {volume:?}"));
        }
        if tris > max_tris {
            fails.push(format!("{name}: {tris} triangles (ceiling {max_tris})"));
        }
    }

    // Same document, same mesh: bit for bit.
    let again = evaluate_document(&doc, &options).expect("evaluate");
    for (a, b) in scene.parts.iter().zip(&again.parts) {
        if a.mesh.positions != b.mesh.positions || a.mesh.indices != b.mesh.indices {
            fails.push("evaluation is not deterministic".to_string());
        }
    }
    assert!(fails.is_empty(), "{fails:#?}");
}
