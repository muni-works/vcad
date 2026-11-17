//! Crossing tubes — tees, crosses and tube-built chambers — stay true B-reps.
//!
//! Two perpendicular cylinders split each other's walls into strips around
//! the saddle; a third tube then crosses those strips. Each of these used to
//! knock the boolean off the analytic path onto the mesh-soup fallback
//! (seconds to a minute, tens of thousands of triangles):
//!
//! - a plane cutting a saddle strip (a tube end or flange face across a
//!   bore) was refused: the strip's chain steps vertically at the saddle's
//!   u-extreme and the crossing search read the wrong side of the step;
//! - the strip below a saddle rendered inside out once a later union added
//!   loop vertices to it;
//! - a third tube's saddle loop crosses strips the first crossing made, so it
//!   is contained in no single face, and the splitter refused it.
//!
//! Every volume is checked against an independent quadrature of the exact
//! solid: for fixed (x, y) the solid's z-extent is a union of intervals in
//! closed form, integrated by Gauss–Legendre on panels split at every kink.

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::{CsgOp, Document, Node, NodeId, SceneEntry, Vec3};
use vcad_kernel::SolidFidelity;

/// A solid rod: axis 0 = x, 1 = y, 2 = z, radius `r`, spanning `[a, b]`.
#[derive(Clone, Copy)]
struct Rod {
    axis: usize,
    r: f64,
    a: f64,
    b: f64,
}

const fn rod(axis: usize, r: f64, a: f64, b: f64) -> Rod {
    Rod { axis, r, a, b }
}

// ---------------------------------------------------------------------------
// Reference volume: union(body) − union(cut)
// ---------------------------------------------------------------------------

/// The z-interval a rod covers above (x, y), if any.
fn z_interval(rod: &Rod, x: f64, y: f64) -> Option<(f64, f64)> {
    let r2 = rod.r * rod.r;
    match rod.axis {
        2 => (x * x + y * y <= r2).then_some((rod.a, rod.b)),
        0 => (x >= rod.a && x <= rod.b && y * y <= r2).then(|| {
            let h = (r2 - y * y).sqrt();
            (-h, h)
        }),
        _ => (y >= rod.a && y <= rod.b && x * x <= r2).then(|| {
            let h = (r2 - x * x).sqrt();
            (-h, h)
        }),
    }
}

/// Length of `union(body) − union(cut)` along z above (x, y).
fn z_length(body: &[Rod], cut: &[Rod], x: f64, y: f64) -> f64 {
    let bi: Vec<_> = body.iter().filter_map(|s| z_interval(s, x, y)).collect();
    if bi.is_empty() {
        return 0.0;
    }
    let ci: Vec<_> = cut.iter().filter_map(|s| z_interval(s, x, y)).collect();
    let mut ends: Vec<f64> = bi.iter().chain(&ci).flat_map(|&(a, b)| [a, b]).collect();
    ends.sort_by(f64::total_cmp);
    let inside = |set: &[(f64, f64)], z: f64| set.iter().any(|&(a, b)| z > a && z < b);
    ends.windows(2)
        .filter(|w| w[1] > w[0])
        .map(|w| {
            let z = 0.5 * (w[0] + w[1]);
            if inside(&bi, z) && !inside(&ci, z) {
                w[1] - w[0]
            } else {
                0.0
            }
        })
        .sum()
}

/// 16-point Gauss–Legendre nodes and weights on [−1, 1].
const GL: [(f64, f64); 8] = [
    (0.095_012_509_837_637_44, 0.189_450_610_455_068_5),
    (0.281_603_550_779_258_9, 0.182_603_415_044_923_6),
    (0.458_016_777_657_227_4, 0.169_156_519_395_002_5),
    (0.617_876_244_402_643_7, 0.149_595_988_816_576_7),
    (0.755_404_408_355_003, 0.124_628_971_255_533_9),
    (0.865_631_202_387_831_8, 0.095_158_511_682_492_78),
    (0.944_575_023_073_232_6, 0.062_253_523_938_647_89),
    (0.989_400_934_991_649_9, 0.027_152_459_411_754_09),
];

/// Integrate `f` over `[a, b]` split at `breaks`, each piece cut into
/// `sub` panels of 16-point Gauss–Legendre.
fn integrate(f: impl Fn(f64) -> f64, a: f64, b: f64, breaks: &mut Vec<f64>, sub: usize) -> f64 {
    breaks.retain(|&t| t > a && t < b);
    breaks.push(a);
    breaks.push(b);
    breaks.sort_by(f64::total_cmp);
    breaks.dedup_by(|p, q| (*p - *q).abs() < 1e-12);
    let mut total = 0.0;
    for w in breaks.windows(2) {
        let h = (w[1] - w[0]) / sub as f64;
        for k in 0..sub {
            let (lo, hi) = (w[0] + k as f64 * h, w[0] + (k + 1) as f64 * h);
            let (m, r) = (0.5 * (lo + hi), 0.5 * (hi - lo));
            for &(x, wt) in &GL {
                total += wt * r * (f(m - r * x) + f(m + r * x));
            }
        }
    }
    total
}

/// Exact volume of `union(body) − union(cut)`, to ~1e-5 relative.
fn reference_volume(body: &[Rod], cut: &[Rod]) -> f64 {
    let all: Vec<Rod> = body.iter().chain(cut).copied().collect();
    let extent = all
        .iter()
        .map(|s| s.r.max(s.a.abs()).max(s.b.abs()))
        .fold(0.0, f64::max);
    let sqrt_pos = |v: f64| (v > 0.0).then(|| v.sqrt());
    let inner = |y: f64| {
        // Kinks in x at this y: rod silhouettes, rod ends, and where a
        // y-rod's circular z-extent meets another rod's constant one.
        let mut breaks = Vec::new();
        for s in &all {
            match s.axis {
                2 => {
                    if let Some(w) = sqrt_pos(s.r * s.r - y * y) {
                        breaks.extend([-w, w]);
                    }
                }
                0 => breaks.extend([s.a, s.b]),
                _ => breaks.extend([-s.r, s.r]),
            }
        }
        for s in all.iter().filter(|s| s.axis == 1) {
            for t in &all {
                let consts: Vec<f64> = match t.axis {
                    2 => vec![t.a, t.b],
                    0 => sqrt_pos(t.r * t.r - y * y).map_or(vec![], |h| vec![h]),
                    _ => continue,
                };
                for c in consts {
                    if let Some(x) = sqrt_pos(s.r * s.r - c * c) {
                        breaks.extend([-x, x]);
                    }
                }
            }
        }
        integrate(
            |x| z_length(body, cut, x, y),
            -extent,
            extent,
            &mut breaks,
            2,
        )
    };
    let mut breaks: Vec<f64> = all
        .iter()
        .flat_map(|s| {
            let mut v = vec![-s.r, s.r];
            if s.axis == 1 {
                v.extend([s.a, s.b]);
            }
            v
        })
        .collect();
    integrate(inner, -extent, extent, &mut breaks, 24)
}

// ---------------------------------------------------------------------------
// Kernel side
// ---------------------------------------------------------------------------

struct DocBuilder {
    doc: Document,
    next: NodeId,
}

impl DocBuilder {
    fn new() -> Self {
        Self {
            doc: Document::new(),
            next: 1,
        }
    }

    fn add(&mut self, op: CsgOp) -> NodeId {
        let id = self.next;
        self.next += 1;
        self.doc.nodes.insert(id, Node { id, name: None, op });
        id
    }

    /// `CsgOp::Rotate` applies Z first (R = Rx·Ry·Rz); single-axis turns
    /// only here: +90° about y lays the z-cylinder along +x, −90° about x
    /// along +y.
    fn rod(&mut self, s: Rod) -> NodeId {
        let c = self.add(CsgOp::Cylinder {
            radius: s.r,
            height: s.b - s.a,
            segments: 0,
        });
        let c = self.add(CsgOp::Translate {
            child: c,
            offset: Vec3::new(0.0, 0.0, s.a),
        });
        let angles = match s.axis {
            0 => Vec3::new(0.0, 90.0, 0.0),
            1 => Vec3::new(-90.0, 0.0, 0.0),
            _ => return c,
        };
        self.add(CsgOp::Rotate { child: c, angles })
    }

    fn union_all(&mut self, rods: &[Rod]) -> NodeId {
        let mut acc = self.rod(rods[0]);
        for &s in &rods[1..] {
            let r = self.rod(s);
            acc = self.add(CsgOp::Union {
                left: acc,
                right: r,
            });
        }
        acc
    }

    fn finish(mut self, root: NodeId) -> Document {
        self.doc.roots.push(SceneEntry {
            root,
            material: "default".into(),
            visible: None,
        });
        self.doc
    }
}

struct Evaluated {
    volume: f64,
    open_edges: usize,
    fidelity: SolidFidelity,
    why: String,
}

fn evaluate(doc: &Document) -> Evaluated {
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
    let solid = part.solid.as_ref().expect("root solid");
    let (pos, idx) = (&part.mesh.positions, &part.mesh.indices);
    let p = |i: u32| {
        let k = 3 * i as usize;
        [pos[k] as f64, pos[k + 1] as f64, pos[k + 2] as f64]
    };
    let volume = idx
        .chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum();
    let key = |i: u32| {
        let k = 3 * i as usize;
        [pos[k].to_bits(), pos[k + 1].to_bits(), pos[k + 2].to_bits()]
    };
    let mut net: std::collections::HashMap<_, i64> = std::collections::HashMap::new();
    for t in idx.chunks(3) {
        for e in 0..3 {
            let (a, b) = (key(t[e]), key(t[(e + 1) % 3]));
            if a < b {
                *net.entry((a, b)).or_default() += 1;
            } else if b < a {
                *net.entry((b, a)).or_default() -= 1;
            }
        }
    }
    Evaluated {
        volume,
        open_edges: net.values().filter(|v| **v != 0).count(),
        fidelity: solid.fidelity(),
        why: solid.why_not_brep().unwrap_or_default(),
    }
}

/// Mesh volume vs the exact solid: the tessellation inscribes every curved
/// face, which reads a few hundredths of a percent light at these radii.
const TOL: f64 = 0.003;

fn check(name: &str, body: &[Rod], cut: &[Rod]) {
    let mut b = DocBuilder::new();
    let root = if cut.is_empty() {
        b.union_all(body)
    } else {
        let l = b.union_all(body);
        let r = b.union_all(cut);
        b.add(CsgOp::Difference { left: l, right: r })
    };
    let got = evaluate(&b.finish(root));
    let expect = reference_volume(body, cut);
    assert_eq!(
        got.fidelity,
        SolidFidelity::Analytic,
        "{name}: fell off the B-rep path: {}",
        got.why
    );
    assert_eq!(got.open_edges, 0, "{name}: {} open edges", got.open_edges);
    let err = (got.volume - expect) / expect;
    assert!(
        err.abs() < TOL,
        "{name}: volume {:.1} vs exact {expect:.1} ({:+.3}%)",
        got.volume,
        100.0 * err
    );
}

// DN40 CF tee/cross geometry (vacuum library): tube OD 40 / bore 37, flange
// OD 69.4 × 13, center-to-face 63; each further axis shrinks 0.05 mm so no
// two walls are coincident.
const TUBE_Z: Rod = rod(2, 20.0, -56.5, 56.5);
const BORE_Z: Rod = rod(2, 18.5, -64.0, 64.0);

#[test]
fn reference_quadrature_matches_closed_forms() {
    // A lone tube, and two equal perpendicular rods (Steinmetz: the union
    // is 2·πr²L − 16r³/3 when both span the full length).
    let (r, l) = (5.0, 30.0);
    let tube = reference_volume(&[rod(2, r, -l / 2.0, l / 2.0)], &[]);
    let pi = std::f64::consts::PI;
    assert!((tube - pi * r * r * l).abs() < 1e-5 * tube, "{tube}");
    let cross = reference_volume(&[rod(2, r, -15.0, 15.0), rod(0, r, -15.0, 15.0)], &[]);
    let exact = 2.0 * pi * r * r * l - 16.0 * r * r * r / 3.0;
    assert!((cross - exact).abs() < 1e-5 * exact, "{cross} vs {exact}");
    // Tricylinder (three equal rods): 3·πr²L − 3·(16/3)r³ + 8(2−√2)r³.
    let tri = reference_volume(
        &[
            rod(2, r, -15.0, 15.0),
            rod(0, r, -15.0, 15.0),
            rod(1, r, -15.0, 15.0),
        ],
        &[],
    );
    let exact = 3.0 * pi * r * r * l - 16.0 * r.powi(3) + 8.0 * (2.0 - 2f64.sqrt()) * r.powi(3);
    assert!((tri - exact).abs() < 1e-5 * exact, "{tri} vs {exact}");
}

#[test]
fn tee_tubes_minus_bores() {
    check(
        "tee tubes − bores",
        &[TUBE_Z, rod(0, 19.95, 0.0, 56.5)],
        &[BORE_Z, rod(0, 18.45, -0.5, 64.0)],
    );
}

#[test]
fn flanged_tee() {
    // The full DN40 tee body: tubes plus three plain flange discs, bored.
    // (≈128.4 cm³.)
    check(
        "flanged tee",
        &[
            TUBE_Z,
            rod(0, 19.95, 0.0, 56.5),
            rod(2, 34.7, 50.0, 63.0),
            rod(2, 34.7, -63.0, -50.0),
            rod(0, 34.7, 50.0, 63.0),
        ],
        &[BORE_Z, rod(0, 18.45, -0.5, 64.0)],
    );
}

#[test]
fn two_rod_cross_with_bores() {
    check(
        "2-rod cross",
        &[TUBE_Z, rod(0, 19.95, -56.5, 56.5)],
        &[BORE_Z, rod(0, 18.45, -64.0, 64.0)],
    );
}

#[test]
fn three_rod_union() {
    check(
        "3 rods",
        &[
            TUBE_Z,
            rod(0, 19.95, -56.5, 56.5),
            rod(1, 19.9, -56.5, 56.5),
        ],
        &[],
    );
}

#[test]
fn three_rod_cross_with_bores() {
    check(
        "3-rod cross",
        &[
            TUBE_Z,
            rod(0, 19.95, -56.5, 56.5),
            rod(1, 19.9, -56.5, 56.5),
        ],
        &[
            BORE_Z,
            rod(0, 18.45, -64.0, 64.0),
            rod(1, 18.4, -64.0, 64.0),
        ],
    );
}
