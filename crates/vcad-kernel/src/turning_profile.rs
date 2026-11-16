//! Extract the exact half-section of a solid of revolution, for turning.
//!
//! A B-rep is accepted only when **every** face is a surface of revolution
//! about one common axis: planes perpendicular to it, and coaxial cylinders,
//! cones, spheres and tori. Anything else — a flat, a cross-drilled hole, a
//! B-spline face, a mesh-only solid — is refused with the offending face, so
//! "turnable" is proven by the face types, not guessed from a mesh.
//!
//! The section itself is exact: each face's meridian is a known line or
//! circle, and the tessellation is used only to find where on that meridian
//! the face starts and ends. Section points are projected onto the exact
//! meridian, and neighbouring pieces meet at the exact intersection of their
//! meridians, so circumferential faceting error never reaches the profile.

use crate::Solid;
use vcad_kernel_cam::turning::{ProfileSeg, TurnProfile, P2};
use vcad_kernel_geom::{ConeSurface, CylinderSurface, Plane, SphereSurface, TorusSurface};
use vcad_kernel_tessellate::{tessellate_brep_by_face, TessellationParams};

/// The turning axis found in a solid (world frame).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurningAxis {
    /// A point on the axis; the profile's `z = 0` (finished face) lies at
    /// `origin + face_z · dir`.
    pub origin: [f64; 3],
    /// Unit direction, pointing from the chuck end toward the finished face.
    pub dir: [f64; 3],
    /// Axial position of the finished face along `dir` from `origin`.
    pub face_z: f64,
}

/// Which end of the part is the finished face (toward the tailstock).
///
/// "Larger" is measured along the axis oriented so its dominant world
/// component is positive: for a part along world Z, `Max` is the +Z end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceEnd {
    /// The end at the larger axial coordinate.
    Max,
    /// The end at the smaller axial coordinate.
    Min,
}

/// Why a solid could not be turned into a profile.
#[derive(Debug, Clone, PartialEq)]
pub enum TurningProfileError {
    /// Only exact B-reps carry the face types the proof needs.
    NotBRep,
    /// No cylinder, cone, sphere or torus to define an axis.
    NoAxis,
    /// A face is not a surface of revolution about the axis.
    NotRevolution {
        /// Face index in the outer shell.
        face: usize,
        /// Why.
        reason: String,
    },
    /// The section did not close into one loop.
    Section(String),
    /// The loop was rejected by [`TurnProfile`] (e.g. an internal cavity).
    Profile(String),
}

impl std::fmt::Display for TurningProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBRep => write!(f, "not an exact B-rep solid"),
            Self::NoAxis => write!(f, "no curved face of revolution to define an axis"),
            Self::NotRevolution { face, reason } => {
                write!(f, "face {face} is not a surface of revolution: {reason}")
            }
            Self::Section(s) => write!(f, "section: {s}"),
            Self::Profile(s) => write!(f, "profile: {s}"),
        }
    }
}

impl std::error::Error for TurningProfileError {}

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: V3) -> V3 {
    let n = norm(a);
    [a[0] / n, a[1] / n, a[2] / n]
}
fn pt(p: vcad_kernel_math::Point3) -> V3 {
    [p.x, p.y, p.z]
}
fn dv(d: vcad_kernel_math::Dir3) -> V3 {
    let v = d.into_inner();
    [v.x, v.y, v.z]
}

/// Meridian of a face in `(t, r)` (axial, radial).
#[derive(Debug, Clone, Copy)]
enum Meridian {
    /// Line through `p` with direction `d` (unit).
    Line { p: P2, d: P2 },
    /// Circle.
    Circle { c: P2, r: f64 },
}

impl Meridian {
    fn project(&self, q: P2) -> P2 {
        match *self {
            Meridian::Line { p, d } => {
                let s = (q[0] - p[0]) * d[0] + (q[1] - p[1]) * d[1];
                [p[0] + s * d[0], p[1] + s * d[1]]
            }
            Meridian::Circle { c, r } => {
                let v = [q[0] - c[0], q[1] - c[1]];
                let n = (v[0] * v[0] + v[1] * v[1]).sqrt().max(1e-15);
                [c[0] + r * v[0] / n, c[1] + r * v[1] / n]
            }
        }
    }

    fn dist(&self, q: P2) -> f64 {
        let p = self.project(q);
        ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()
    }
}

/// Intersections of two meridians.
fn meet(a: &Meridian, b: &Meridian) -> Vec<P2> {
    match (*a, *b) {
        (Meridian::Line { p, d }, Meridian::Line { p: q, d: e }) => {
            let den = d[0] * e[1] - d[1] * e[0];
            if den.abs() < 1e-12 {
                return vec![];
            }
            let s = ((q[0] - p[0]) * e[1] - (q[1] - p[1]) * e[0]) / den;
            vec![[p[0] + s * d[0], p[1] + s * d[1]]]
        }
        (Meridian::Line { p, d }, Meridian::Circle { c, r })
        | (Meridian::Circle { c, r }, Meridian::Line { p, d }) => {
            let f = [p[0] - c[0], p[1] - c[1]];
            let b = f[0] * d[0] + f[1] * d[1];
            let cc = f[0] * f[0] + f[1] * f[1] - r * r;
            let disc = b * b - cc;
            if disc < 0.0 {
                return vec![];
            }
            let s = disc.sqrt();
            [-b - s, -b + s]
                .iter()
                .map(|t| [p[0] + t * d[0], p[1] + t * d[1]])
                .collect()
        }
        (Meridian::Circle { c: c1, r: r1 }, Meridian::Circle { c: c2, r: r2 }) => {
            let dz = c2[0] - c1[0];
            let dx = c2[1] - c1[1];
            let dd = (dz * dz + dx * dx).sqrt();
            if dd < 1e-12 || dd > r1 + r2 + 1e-9 || dd < (r1 - r2).abs() - 1e-9 {
                return vec![];
            }
            let l = (r1 * r1 - r2 * r2 + dd * dd) / (2.0 * dd);
            let h = (r1 * r1 - l * l).max(0.0).sqrt();
            let m = [c1[0] + l * dz / dd, c1[1] + l * dx / dd];
            vec![
                [m[0] + h * dx / dd, m[1] - h * dz / dd],
                [m[0] - h * dx / dd, m[1] + h * dz / dd],
            ]
        }
    }
}

struct Piece {
    mer: Meridian,
    a: P2,
    b: P2,
    mid: P2,
}

/// Extract the half-section of `solid` about its axis of revolution.
///
/// Returns the profile (finished face at `z = 0`, `z` toward the tailstock)
/// and the axis it was measured about.
pub fn turning_profile(
    solid: &Solid,
    face_end: FaceEnd,
) -> Result<(TurnProfile, TurningAxis), TurningProfileError> {
    let brep = solid.as_brep().ok_or(TurningProfileError::NotBRep)?;
    let topo = &brep.topology;
    let geo = &brep.geometry;
    let shell = &topo.shells[topo.solids[brep.solid_id].outer_shell];
    if !topo.solids[brep.solid_id].void_shells.is_empty() {
        return Err(TurningProfileError::Section(
            "solid has an internal void shell".into(),
        ));
    }
    let faces: Vec<_> = shell.faces.clone();

    // 1. Axis from the first curved face.
    let mut axis: Option<(V3, V3)> = None;
    for &f in &faces {
        let s = geo.surfaces[topo.faces[f].surface_index].as_any();
        if let Some(c) = s.downcast_ref::<CylinderSurface>() {
            axis = Some((pt(c.center), unit(dv(c.axis))));
        } else if let Some(c) = s.downcast_ref::<ConeSurface>() {
            axis = Some((pt(c.apex), unit(dv(c.axis))));
        } else if let Some(c) = s.downcast_ref::<TorusSurface>() {
            axis = Some((pt(c.center), unit(dv(c.axis))));
        }
        if axis.is_some() {
            break;
        }
    }
    if axis.is_none() {
        // Spheres + perpendicular planes: axis = plane normal through centre.
        let mut center = None;
        let mut normal = None;
        for &f in &faces {
            let s = geo.surfaces[topo.faces[f].surface_index].as_any();
            if let Some(sp) = s.downcast_ref::<SphereSurface>() {
                center.get_or_insert(pt(sp.center));
            } else if let Some(p) = s.downcast_ref::<Plane>() {
                normal.get_or_insert(unit(dv(p.normal_dir)));
            }
        }
        if let (Some(c), Some(n)) = (center, normal) {
            axis = Some((c, n));
        }
    }
    let (o, a) = axis.ok_or(TurningProfileError::NoAxis)?;
    // Canonical sense: the dominant component positive, so `FaceEnd::Max`
    // means "toward +X / +Y / +Z" whichever face happened to define the axis.
    let dom = (0..3)
        .max_by(|&i, &j| a[i].abs().total_cmp(&a[j].abs()))
        .unwrap_or(2);
    let a = if a[dom] < 0.0 {
        [-a[0], -a[1], -a[2]]
    } else {
        a
    };
    let on_axis = |p: V3| norm(cross(sub(p, o), a));
    let parallel = |d: V3| norm(cross(unit(d), a)) < 1e-7;
    let tcoord = |p: V3| dot(sub(p, o), a);

    // 2. Every face must be a surface of revolution about (o, a). Planes
    //    are read straight from their boundary vertices (exact, and the
    //    per-face tessellator can return no triangles for an annulus); the
    //    curved faces get a meridian and are sectioned below.
    let scale0 = {
        let mut m = 1.0f64;
        for (_, v) in topo.vertices.iter() {
            m = m.max(norm(sub(pt(v.point), o)));
        }
        m
    };
    let eps = 1e-6 * scale0;
    let tr = |p: V3| -> P2 { [tcoord(p), on_axis(p)] };
    let mut meridians: Vec<Option<Meridian>> = Vec::with_capacity(faces.len());
    let mut direct: Vec<Piece> = Vec::new();
    for (i, &f) in faces.iter().enumerate() {
        let face = &topo.faces[f];
        let s = geo.surfaces[face.surface_index].as_any();
        let bad = |reason: String| TurningProfileError::NotRevolution { face: i, reason };
        let loop_pts = |l| -> Vec<P2> {
            topo.loop_vertices(l)
                .into_iter()
                .map(|v| tr(pt(topo.vertices[v].point)))
                .collect()
        };
        let m = if let Some(p) = s.downcast_ref::<Plane>() {
            let mut all: Vec<P2> = loop_pts(face.outer_loop);
            for &l in &face.inner_loops {
                all.extend(loop_pts(l));
            }
            if parallel(dv(p.normal_dir)) {
                // Annulus or disk: every loop must be one circle.
                let t0 = tcoord(pt(p.origin));
                let mut lo = f64::INFINITY;
                let mut hi = 0.0f64;
                for l in std::iter::once(face.outer_loop).chain(face.inner_loops.iter().copied()) {
                    let rs: Vec<f64> = loop_pts(l).iter().map(|q| q[1]).collect();
                    let (mn, mx) = rs
                        .iter()
                        .fold((f64::INFINITY, 0.0f64), |a, &r| (a.0.min(r), a.1.max(r)));
                    if mx - mn > 1e-5 * scale0 {
                        return Err(bad("planar face bounded by a non-circular loop".into()));
                    }
                    lo = lo.min(mn);
                    hi = hi.max(mx);
                }
                if face.inner_loops.len() > 1 {
                    return Err(bad("planar face with several holes".into()));
                }
                if face.inner_loops.is_empty() {
                    lo = 0.0;
                }
                if hi - lo > eps {
                    direct.push(Piece {
                        mer: Meridian::Line {
                            p: [t0, 0.0],
                            d: [0.0, 1.0],
                        },
                        a: [t0, lo],
                        b: [t0, hi],
                        mid: [t0, 0.5 * (lo + hi)],
                    });
                }
                None
            } else {
                // A facet of a faceted cone ring (Solid::revolve approximates
                // cones this way): its vertices sit on two rims of the cone
                // it approximates. Read back that cone's meridian exactly.
                let mut rims: Vec<P2> = Vec::new();
                for q in all {
                    if !rims.iter().any(|r| {
                        (r[0] - q[0]).abs() < 1e-5 * scale0 && (r[1] - q[1]).abs() < 1e-5 * scale0
                    }) {
                        rims.push(q);
                    }
                }
                if rims.len() != 2 || (rims[0][0] - rims[1][0]).abs() < eps {
                    return Err(bad("plane not perpendicular to the axis".into()));
                }
                let (a0, b0) = if rims[0][0] < rims[1][0] {
                    (rims[0], rims[1])
                } else {
                    (rims[1], rims[0])
                };
                let l = ((b0[0] - a0[0]).powi(2) + (b0[1] - a0[1]).powi(2)).sqrt();
                let piece = Piece {
                    mer: Meridian::Line {
                        p: a0,
                        d: [(b0[0] - a0[0]) / l, (b0[1] - a0[1]) / l],
                    },
                    a: a0,
                    b: b0,
                    mid: [0.5 * (a0[0] + b0[0]), 0.5 * (a0[1] + b0[1])],
                };
                let dup = direct.iter().any(|d| {
                    let same =
                        |x: P2, y: P2| (x[0] - y[0]).abs() < eps && (x[1] - y[1]).abs() < eps;
                    same(d.a, piece.a) && same(d.b, piece.b)
                });
                if !dup {
                    direct.push(piece);
                }
                None
            }
        } else if let Some(c) = s.downcast_ref::<CylinderSurface>() {
            if !parallel(dv(c.axis)) || on_axis(pt(c.center)) > 1e-6 {
                return Err(bad("cylinder not coaxial".into()));
            }
            Some(Meridian::Line {
                p: [0.0, c.radius],
                d: [1.0, 0.0],
            })
        } else if let Some(c) = s.downcast_ref::<ConeSurface>() {
            if !parallel(dv(c.axis)) || on_axis(pt(c.apex)) > 1e-6 {
                return Err(bad("cone not coaxial".into()));
            }
            let sg = dot(dv(c.axis), a).signum();
            Some(Meridian::Line {
                p: [tcoord(pt(c.apex)), 0.0],
                d: [sg * c.half_angle.cos(), c.half_angle.sin()],
            })
        } else if let Some(c) = s.downcast_ref::<SphereSurface>() {
            if on_axis(pt(c.center)) > 1e-6 {
                return Err(bad("sphere centre off the axis".into()));
            }
            Some(Meridian::Circle {
                c: [tcoord(pt(c.center)), 0.0],
                r: c.radius,
            })
        } else if let Some(c) = s.downcast_ref::<TorusSurface>() {
            if !parallel(dv(c.axis)) || on_axis(pt(c.center)) > 1e-6 {
                return Err(bad("torus not coaxial".into()));
            }
            Some(Meridian::Circle {
                c: [tcoord(pt(c.center)), c.major_radius],
                r: c.minor_radius,
            })
        } else {
            return Err(bad(format!(
                "{:?} surface",
                geo.surfaces[face.surface_index].surface_type()
            )));
        };
        meridians.push(m);
    }

    // 3. Section the tessellation by a half-plane through the axis.
    let params = TessellationParams {
        chord_tolerance: Some(0.002),
        ..TessellationParams::from_segments(96)
    };
    let meshes = tessellate_brep_by_face(brep, &params);
    let refd = {
        let t = if a[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        unit(cross(a, t))
    };
    // Section angle: just past the vertex farthest from the axis, so the
    // half-plane runs along a column of exact tessellation vertices.
    let mut best = (0.0f64, 0.0f64);
    for (_, _, m) in &meshes {
        for v in m.vertices.chunks(3) {
            let p = [v[0] as f64, v[1] as f64, v[2] as f64];
            let d = sub(p, o);
            let r = norm(cross(d, a));
            if r > best.0 {
                let ang = dot(d, cross(a, refd)).atan2(dot(d, refd));
                best = (r, ang);
            }
        }
    }
    let th = best.1 + 1e-7;
    let w3 = cross(a, refd);
    let u = unit([
        refd[0] * th.cos() + w3[0] * th.sin(),
        refd[1] * th.cos() + w3[1] * th.sin(),
        refd[2] * th.cos() + w3[2] * th.sin(),
    ]);
    let v = cross(a, u);
    let scale = best.0.max(1.0);
    let mut pieces: Vec<Piece> = direct;
    for (k, (_, _, mesh)) in meshes.iter().enumerate() {
        let Some(mer) = meridians[k] else { continue };
        let vx: Vec<V3> = mesh
            .vertices
            .chunks(3)
            .map(|c| [c[0] as f64, c[1] as f64, c[2] as f64])
            .collect();
        let mut segs: Vec<(P2, P2)> = Vec::new();
        for tri in mesh.indices.chunks(3) {
            let p = [
                vx[tri[0] as usize],
                vx[tri[1] as usize],
                vx[tri[2] as usize],
            ];
            let w: Vec<f64> = p.iter().map(|q| dot(sub(*q, o), v)).collect();
            let mut cut: Vec<P2> = Vec::new();
            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                if (w[i] >= 0.0) != (w[j] >= 0.0) {
                    // Interpolate in a canonical vertex order so neighbouring
                    // triangles produce bit-identical points.
                    let (pi, pj, wi, wj) = if p[i] < p[j] {
                        (p[i], p[j], w[i], w[j])
                    } else {
                        (p[j], p[i], w[j], w[i])
                    };
                    let l = wi / (wi - wj);
                    let q = [
                        pi[0] + l * (pj[0] - pi[0]),
                        pi[1] + l * (pj[1] - pi[1]),
                        pi[2] + l * (pj[2] - pi[2]),
                    ];
                    let d = sub(q, o);
                    cut.push([dot(d, a), dot(d, u)]);
                }
            }
            if cut.len() == 2 {
                let (mut s0, mut s1) = (cut[0], cut[1]);
                if s0[1] < 0.0 && s1[1] < 0.0 {
                    continue;
                }
                if s0[1] < 0.0 || s1[1] < 0.0 {
                    let l = s0[1] / (s0[1] - s1[1]);
                    let z = [s0[0] + l * (s1[0] - s0[0]), 0.0];
                    if s0[1] < 0.0 {
                        s0 = z;
                    } else {
                        s1 = z;
                    }
                }
                segs.push((s0, s1));
            }
        }
        for chain in chain_segments(&segs, 1e-6 * scale) {
            if chain.len() < 2 {
                continue;
            }
            let worst = chain.iter().map(|q| mer.dist(*q)).fold(0.0, f64::max);
            if worst > 0.05 {
                return Err(TurningProfileError::Section(format!(
                    "face {k}: section strays {worst:.4} mm from its meridian"
                )));
            }
            let pa = mer.project(chain[0]);
            let pb = mer.project(chain[chain.len() - 1]);
            let mid = mer.project(chain[chain.len() / 2]);
            if ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2)).sqrt() < 1e-9 {
                continue;
            }
            pieces.push(Piece {
                mer,
                a: pa,
                b: pb,
                mid,
            });
        }
    }
    if pieces.is_empty() {
        return Err(TurningProfileError::Section("empty section".into()));
    }

    // 4. Snap shared endpoints to the exact meridian intersection.
    let tol = 0.05f64.max(1e-4 * scale);
    let n = pieces.len();
    let ends: Vec<(usize, bool)> = (0..n).flat_map(|i| [(i, false), (i, true)]).collect();
    let get = |pcs: &[Piece], (i, e): (usize, bool)| if e { pcs[i].b } else { pcs[i].a };
    let mut partner: Vec<Option<(usize, bool)>> = vec![None; 2 * n];
    for (ix, &e1) in ends.iter().enumerate() {
        let p1 = get(&pieces, e1);
        let mut bestp: Option<(f64, usize)> = None;
        for (jx, &e2) in ends.iter().enumerate() {
            if e2.0 == e1.0 {
                continue;
            }
            let p2 = get(&pieces, e2);
            let d = ((p1[0] - p2[0]).powi(2) + (p1[1] - p2[1]).powi(2)).sqrt();
            if d < tol && bestp.is_none_or(|b| d < b.0) {
                bestp = Some((d, jx));
            }
        }
        partner[ix] = bestp.map(|b| ends[b.1]);
    }
    for (ix, &e1) in ends.iter().enumerate() {
        let Some(e2) = partner[ix] else { continue };
        if (e2.0, e2.1) < (e1.0, e1.1) {
            continue;
        }
        let (p1, p2) = (get(&pieces, e1), get(&pieces, e2));
        let avg = [(p1[0] + p2[0]) / 2.0, (p1[1] + p2[1]) / 2.0];
        let exact = meet(&pieces[e1.0].mer, &pieces[e2.0].mer)
            .into_iter()
            .filter(|q| ((q[0] - avg[0]).powi(2) + (q[1] - avg[1]).powi(2)).sqrt() < 2.0 * tol)
            .min_by(|x, y| {
                let dx = (x[0] - avg[0]).powi(2) + (x[1] - avg[1]).powi(2);
                let dy = (y[0] - avg[0]).powi(2) + (y[1] - avg[1]).powi(2);
                dx.total_cmp(&dy)
            })
            .unwrap_or(avg);
        for e in [e1, e2] {
            if e.1 {
                pieces[e.0].b = exact;
            } else {
                pieces[e.0].a = exact;
            }
        }
    }
    // Ends on the axis snap to r = 0 exactly.
    for p in &mut pieces {
        for q in [&mut p.a, &mut p.b] {
            if q[1].abs() < 1e-6 * scale {
                q[1] = 0.0;
            }
        }
    }

    // 5. Chain pieces into one loop, closing along the axis.
    let mut segs: Vec<ProfileSeg> = Vec::new();
    let mut used = vec![false; n];
    used[0] = true;
    let mut cur = pieces[0].b;
    segs.push(to_seg(&pieces[0], false));
    let close =
        |p: P2, q: P2| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt() < 1e-7 * scale;
    loop {
        let next = (0..n).filter(|&i| !used[i]).find_map(|i| {
            if close(pieces[i].a, cur) {
                Some((i, false))
            } else if close(pieces[i].b, cur) {
                Some((i, true))
            } else {
                None
            }
        });
        match next {
            Some((i, rev)) => {
                used[i] = true;
                segs.push(to_seg(&pieces[i], rev));
                cur = if rev { pieces[i].a } else { pieces[i].b };
            }
            None if cur[1] == 0.0 => {
                // Walk along the axis to the nearest unused piece end on it,
                // or back to the start.
                let start = segs[0].start();
                let cand = (0..n)
                    .filter(|&i| !used[i])
                    .flat_map(|i| [(i, false, pieces[i].a), (i, true, pieces[i].b)])
                    .filter(|(_, _, q)| q[1] == 0.0)
                    .min_by(|x, y| (x.2[0] - cur[0]).abs().total_cmp(&(y.2[0] - cur[0]).abs()));
                match cand {
                    Some((i, rev, q))
                        if start[1] != 0.0 || (q[0] - cur[0]).abs() < (start[0] - cur[0]).abs() =>
                    {
                        segs.push(ProfileSeg::Line { a: cur, b: q });
                        used[i] = true;
                        segs.push(to_seg(&pieces[i], rev));
                        cur = if rev { pieces[i].a } else { pieces[i].b };
                    }
                    _ => {
                        if !close(cur, start) {
                            segs.push(ProfileSeg::Line { a: cur, b: start });
                        }
                        break;
                    }
                }
            }
            None => {
                if close(cur, segs[0].start()) {
                    break;
                }
                return Err(TurningProfileError::Section(format!(
                    "section open at (z {:.4}, r {:.4})",
                    cur[0], cur[1]
                )));
            }
        }
        if close(cur, segs[0].start()) && used.iter().all(|u| *u) {
            break;
        }
    }
    if used.iter().any(|u| !*u) {
        return Err(TurningProfileError::Section(
            "section has more than one loop (several bodies?)".into(),
        ));
    }

    // 6. Orient: finished face at z = 0, z toward the tailstock.
    let flip = face_end == FaceEnd::Min;
    let segs: Vec<ProfileSeg> = if flip {
        let m = |p: P2| [-p[0], p[1]];
        segs.iter()
            .map(|s| match *s {
                ProfileSeg::Line { a, b } => ProfileSeg::Line { a: m(a), b: m(b) },
                ProfileSeg::Arc { a, b, center, ccw } => ProfileSeg::Arc {
                    a: m(a),
                    b: m(b),
                    center: m(center),
                    ccw: !ccw,
                },
            })
            .collect()
    } else {
        segs
    };
    let raw = TurnProfile::new(segs).map_err(|e| TurningProfileError::Profile(e.to_string()))?;
    let zmax = raw.z_max();
    let prof = raw.face_at_zero();
    let dir = if flip { [-a[0], -a[1], -a[2]] } else { a };
    Ok((
        prof,
        TurningAxis {
            origin: o,
            dir,
            face_z: zmax,
        },
    ))
}

fn to_seg(p: &Piece, rev: bool) -> ProfileSeg {
    let (a, b) = if rev { (p.b, p.a) } else { (p.a, p.b) };
    match p.mer {
        Meridian::Line { .. } => ProfileSeg::Line { a, b },
        Meridian::Circle { c, .. } => {
            // Pick the sense whose arc passes through the section midpoint.
            let ang = |q: P2| (q[1] - c[1]).atan2(q[0] - c[0]);
            let (a0, a1, am) = (ang(a), ang(b), ang(p.mid));
            let tau = std::f64::consts::TAU;
            let ccw_sweep = (a1 - a0).rem_euclid(tau);
            let mid_sweep = (am - a0).rem_euclid(tau);
            ProfileSeg::Arc {
                a,
                b,
                center: c,
                ccw: mid_sweep < ccw_sweep,
            }
        }
    }
}

/// Chain unordered segments into polylines by shared endpoints.
fn chain_segments(segs: &[(P2, P2)], tol: f64) -> Vec<Vec<P2>> {
    let n = segs.len();
    let mut used = vec![false; n];
    let near = |p: P2, q: P2| (p[0] - q[0]).abs() <= tol && (p[1] - q[1]).abs() <= tol;
    let mut out = Vec::new();
    for s in 0..n {
        if used[s] {
            continue;
        }
        used[s] = true;
        let mut chain = vec![segs[s].0, segs[s].1];
        for forward in [true, false] {
            loop {
                let end = if forward {
                    chain[chain.len() - 1]
                } else {
                    chain[0]
                };
                let found =
                    (0..n).find(|&i| !used[i] && (near(segs[i].0, end) || near(segs[i].1, end)));
                let Some(i) = found else { break };
                used[i] = true;
                let other = if near(segs[i].0, end) {
                    segs[i].1
                } else {
                    segs[i].0
                };
                if forward {
                    chain.push(other);
                } else {
                    chain.insert(0, other);
                }
            }
        }
        out.push(chain);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_kernel_math::{Point3, Vec3};
    use vcad_kernel_sketch::{SketchProfile, SketchSegment};

    fn p2(x: f64, y: f64) -> vcad_kernel_math::Point2 {
        vcad_kernel_math::Point2::new(x, y)
    }

    /// Revolve an (r, z) outline about world Z.
    fn revolve(segs: Vec<SketchSegment>) -> Solid {
        let prof = SketchProfile::new(Point3::origin(), Vec3::x(), Vec3::z(), segs).unwrap();
        Solid::revolve(prof, Point3::origin(), Vec3::z(), 360.0).unwrap()
    }

    fn line(a: (f64, f64), b: (f64, f64)) -> SketchSegment {
        SketchSegment::Line {
            start: p2(a.0, a.1),
            end: p2(b.0, b.1),
        }
    }

    #[test]
    fn stepped_bushing_round_trips_exactly() {
        // (r, z): bore r 3.175, body r 6 for z 3..18, flange r 8 for z 0..3.
        let s = revolve(vec![
            line((3.175, 0.0), (8.0, 0.0)),
            line((8.0, 0.0), (8.0, 3.0)),
            line((8.0, 3.0), (6.0, 3.0)),
            line((6.0, 3.0), (6.0, 18.0)),
            line((6.0, 18.0), (3.175, 18.0)),
            line((3.175, 18.0), (3.175, 0.0)),
        ]);
        let (prof, axis) = turning_profile(&s, FaceEnd::Max).unwrap();
        assert!((axis.dir[2] - 1.0).abs() < 1e-9);
        assert!((axis.face_z - 18.0).abs() < 1e-6);
        assert!((prof.z_min() + 18.0).abs() < 1e-6);
        assert!((prof.max_radius() - 8.0).abs() < 1e-9);
        let (ri, ro) = prof.radii_at(-5.0).unwrap();
        assert!(
            (ri - 3.175).abs() < 1e-9 && (ro - 6.0).abs() < 1e-9,
            "{ri} {ro}"
        );
        let (_, ro) = prof.radii_at(-16.5).unwrap();
        assert!((ro - 8.0).abs() < 1e-9);
        assert!(prof.has_bore());
    }

    #[test]
    fn solid_with_cone_and_flip() {
        // Solid pin: r 5 for z 0..10, cone to r 3 at z 14.
        let s = revolve(vec![
            line((0.0, 0.0), (5.0, 0.0)),
            line((5.0, 0.0), (5.0, 10.0)),
            line((5.0, 10.0), (3.0, 14.0)),
            line((3.0, 14.0), (0.0, 14.0)),
            line((0.0, 14.0), (0.0, 0.0)),
        ]);
        let (prof, _) = turning_profile(&s, FaceEnd::Min).unwrap();
        // Flipped: the r=5 end is now the finished face.
        assert!((prof.radii_at(-5.0).unwrap().1 - 5.0).abs() < 1e-9);
        let r12 = prof.radii_at(-12.0).unwrap().1;
        assert!((r12 - 4.0).abs() < 1e-9, "{r12}");
        assert!(!prof.has_bore());
    }

    #[test]
    fn refuses_a_box() {
        let e = turning_profile(&Solid::cube(10.0, 10.0, 10.0), FaceEnd::Max).unwrap_err();
        assert!(matches!(e, TurningProfileError::NoAxis), "{e}");
    }

    #[test]
    fn cross_drilled_pin_is_refused() {
        let pin = Solid::cylinder(5.0, 20.0, 32);
        let hole = Solid::cylinder(1.0, 30.0, 32)
            .rotate(90.0, 0.0, 0.0)
            .translate(0.0, 15.0, 10.0);
        let cut = pin.difference(&hole);
        assert!(turning_profile(&cut, FaceEnd::Max).is_err());
    }

    #[test]
    fn primitive_cone_is_read_exactly() {
        let (prof, _) = turning_profile(&Solid::cone(6.0, 3.0, 9.0, 32), FaceEnd::Max).unwrap();
        // r goes 6 → 3 over z −9 → 0.
        let r = prof.radii_at(-3.0).unwrap().1;
        assert!((r - 4.0).abs() < 1e-6, "{r}");
    }

    /// Revolve → exact profile → plan → post → replay and verify: the
    /// Delrin stalk bushing for the hobby fusor, built the way a user
    /// would build it (a line-only revolve, so its inside corner is sharp
    /// and the nose-radius fillet there is accepted explicitly).
    #[test]
    fn revolved_bushing_plans_posts_and_verifies() {
        use vcad_kernel_cam::turning::*;
        let s = revolve(vec![
            line((3.175, 0.0), (8.0, 0.0)),
            line((8.0, 0.0), (8.0, 3.0)),
            line((8.0, 3.0), (6.0, 3.0)),
            line((6.0, 3.0), (6.0, 17.6)),
            line((6.0, 17.6), (5.6, 18.0)),
            line((5.6, 18.0), (3.175, 18.0)),
            line((3.175, 18.0), (3.175, 0.0)),
        ]);
        let (profile, _) = turning_profile(&s, FaceEnd::Max).unwrap();
        let stock = TurnStock::bar(19.05, 50.0, 0.5);
        let css = SpindleSpeed::Css {
            m_per_min: 120.0,
            max_rpm: 2500.0,
        };
        let p = |fpr: f64, doc: f64| CutParams {
            speed: css,
            feed_per_rev: fpr,
            depth_of_cut: doc,
            clearance: 1.0,
        };
        let drill_p = CutParams {
            speed: SpindleSpeed::Rpm { rpm: 1200.0 },
            ..p(0.08, 1.0)
        };
        let rough = LatheTool::cnmg_95(1, 0.4);
        let finish = LatheTool::dnmg_93(2, 0.2);
        let mut plan = TurnPlanner::new(profile.clone(), stock).unwrap();
        plan.face(&rough, &p(0.15, 0.5))
            .unwrap()
            .drill(&LatheTool::drill(3, 6.0, 118.0, 45.0), &drill_p, 21.0, 4.0)
            .unwrap()
            .rough_od(&rough, &p(0.2, 1.0), 0.2)
            .unwrap()
            .finish_od(&finish, &p(0.08, 1.0))
            .unwrap()
            .rough_bore(
                &LatheTool::boring_rh(4, 0.2, 95.0, 80.0, 5.5, 45.0),
                &p(0.1, 0.3),
                0.05,
            )
            .unwrap()
            .finish_bore(
                &LatheTool::boring_rh(4, 0.2, 95.0, 80.0, 5.5, 45.0),
                &p(0.06, 0.3),
            )
            .unwrap()
            .part_off(&LatheTool::parting(6, 2.0, 0.1, 12.0), &p(0.04, 1.0), 0.2)
            .unwrap();
        let prog = plan.program("delrin-bushing");
        let strict = VerifyOptions::default();
        let err = prog
            .post_verified(&LathePost::linuxcnc(), &stock, &profile, &strict)
            .unwrap_err();
        assert!(
            err.findings
                .iter()
                .any(|f| matches!(f, Finding::Unfinished { .. })),
            "sharp corner must be flagged"
        );
        let lenient = VerifyOptions {
            inside_corner_allowance: 0.25,
            ..strict
        };
        let r = prog.post_verified(&LathePost::linuxcnc(), &stock, &profile, &lenient);
        assert!(r.is_ok(), "{:#?}", r.err().map(|v| v.findings));
    }
}
