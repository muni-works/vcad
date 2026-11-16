//! Regression: the open-fusor vacuum chamber and its sectioned flanges
//! (docs/videos/open-fusor), evaluated the way the Mac app evaluates them.
//!
//! The kernel-level cases live in
//! `vcad-kernel-booleans/tests/fusor_chamber.rs`; this one pins the document
//! path — `cut_chain` batching every tool of a difference chain into one cut
//! — which is what the app actually runs and which behaved differently from
//! the authored-order cuts at every stage of the original report:
//!
//! * `chamber` panicked in tessellation (and, before that, came out as
//!   43k–207k triangles of mesh-boolean soup);
//! * `lid-flange` / `pump-flange` reported `analytic` with ~350 open edges
//!   and 11% excess volume — the annular caps were never cut by the section.

use std::collections::HashMap;
use std::f64::consts::PI;

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_kernel::SolidFidelity;

const SOURCE: &str = r#"
[let chamber-r 76.0]
[let chamber-h 76.0]
[let wall 3.5]
[material "steel" 0.72 0.73 0.75 0.9 0.35]

[let zcyl [fn [r z0 z1] [translate 0.0 0.0 z0 [cylinder r [- z1 z0]]]]]
[let ycyl [fn [r y0 y1] [translate 0.0 y0 0.0 [rotate -90.0 0.0 0.0 [cylinder r [- y1 y0]]]]]]
[let xcyl [fn [r x0 x1] [translate x0 0.0 0.0 [rotate 0.0 90.0 0.0 [cylinder r [- x1 x0]]]]]]
; cut the front quadrant away (subject last)
[let section [fn [s] [difference [translate 0.0 -400.0 -400.0 [cube 400.0 400.0 800.0]] s]]]

[let outer-r [+ chamber-r wall]]
[let shell-solid
  [union
    [zcyl outer-r [- 0.0 [+ chamber-h wall]] [+ chamber-h wall]]
    [union [ycyl 22.5 70.0 112.0]
      [union [xcyl 14.0 -112.0 -70.0]
             [zcyl 23.0 -140.0 -70.0]]]]]
[let bores
  [union
    [zcyl chamber-r [- 0.0 chamber-h] chamber-h]
    [union [ycyl 19.0 60.0 120.0]
      [union [xcyl 11.0 -120.0 -60.0]
        [union [zcyl 20.0 -150.0 -60.0]
               [zcyl 10.0 60.0 90.0]]]]]]
[root-named [section [difference bores shell-solid]] "steel" "chamber"]
[root-named [section [difference [zcyl 10.0 70.0 110.0] [zcyl 57.0 [+ chamber-h wall] [+ chamber-h 21.0]]]] "steel" "lid-flange"]
[root-named [section [difference [zcyl 20.0 -150.0 -130.0] [zcyl 27.5 -146.0 -140.0]]] "steel" "pump-flange"]
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

/// Volume of a radius-`r` cylinder on a radial axis, from the surface of a
/// perpendicular radius-`big` cylinder (axes meeting) out to distance `end`.
fn radial_stub(r: f64, big: f64, end: f64) -> f64 {
    let n = 20000;
    (0..n)
        .map(|i| {
            let x = -r + (i as f64 + 0.5) * 2.0 * r / n as f64;
            2.0 * (r * r - x * x).sqrt() * (end - (big * big - x * x).sqrt()) * 2.0 * r / n as f64
        })
        .sum()
}

#[test]
fn fusor_chamber_and_flanges_are_clean_analytic() {
    let doc = vcad_loon::eval_vcad(SOURCE, None).expect("loon");
    let options = EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    };
    let scene = evaluate_document(&doc, &options).expect("evaluate");
    assert!(scene.failures.is_empty(), "{:?}", scene.failures);
    assert_eq!(scene.parts.len(), 3);

    // Chamber, closed form. The section quadrant (x > 0, y < 0) takes a
    // quarter of everything coaxial — can, cavity, pump nozzle and bore,
    // feedthrough bore — and misses the viewport (+y) and gauge (−x) ports.
    let cyl = |r: f64, h: f64| PI * r * r * h;
    let chamber = 0.75 * (cyl(79.5, 159.0) - cyl(76.0, 152.0)) + radial_stub(22.5, 79.5, 112.0)
        - radial_stub(19.0, 76.0, 112.0)
        + radial_stub(14.0, 79.5, 112.0)
        - radial_stub(11.0, 76.0, 112.0)
        + 0.75 * (cyl(23.0, 60.5) - cyl(20.0, 64.0))
        - 0.75 * cyl(10.0, 3.5);
    // Flanges: three quarters of an annulus.
    let ring = |r_out: f64, r_in: f64, h: f64| 0.75 * PI * (r_out * r_out - r_in * r_in) * h;
    let expect = [
        ("chamber", chamber, 25_000usize),
        ("lid-flange", ring(57.0, 10.0, 17.5), 4_000),
        ("pump-flange", ring(27.5, 20.0, 6.0), 4_000),
    ];
    let mut fails = Vec::new();
    for (part, (name, volume, max_tris)) in scene.parts.iter().zip(expect) {
        let m = &part.mesh;
        let tris = m.indices.len() / 3;
        let (nonmanifold, folded, boundary, vol, _) = census(&m.positions, &m.indices);
        let solid = part.solid.as_ref().expect("solid");
        eprintln!(
            "{name}: tris {tris} vol {vol:.1} nonmanifold {nonmanifold} folded {folded} \
             boundary {boundary} {}",
            solid.fidelity().as_str()
        );
        if solid.fidelity() != SolidFidelity::Analytic {
            fails.push(format!("{name}: {:?}", solid.degradations()));
        }
        if (nonmanifold, folded, boundary) != (0, 0, 0) {
            fails.push(format!(
                "{name}: {nonmanifold} non-manifold, {folded} folded, {boundary} boundary edges"
            ));
        }
        if (vol - volume).abs() > 1e-3 * volume {
            fails.push(format!("{name}: volume {vol:.1}, expected {volume:.1}"));
        }
        if tris > max_tris {
            fails.push(format!("{name}: {tris} triangles (ceiling {max_tris})"));
        }
        let opposed = opposed_normals(&m.positions, m.normals.as_deref(), &m.indices);
        if m.normals.is_none() || opposed != 0 {
            fails.push(format!(
                "{name}: {opposed} triangles wound against their vertex normals (normals present: {})",
                m.normals.is_some()
            ));
        }
    }
    assert!(fails.is_empty(), "{fails:#?}");
}
