//! Exact fillets and chamfers on the circular edges of bodies of revolution.
//!
//! A cylinder rim is a *closed* edge: one seam vertex, one half-edge per
//! side. The per-edge blend builders assume an edge runs between two
//! distinct vertices, so a closed rim gave them nothing to trim against
//! (the curved pipeline dropped every face but the wall), and the planar
//! chamfer pipeline refused the cylinder outright.
//!
//! Every face of a body of revolution is either a plane perpendicular to
//! the axis or a cylinder / cone coaxial with it, so the whole solid is
//! described exactly by its half-profile in the (radius, axial) plane.
//! Rounding a circular edge is rounding a corner of that profile:
//!
//! - **fillet**: the corner becomes a circular arc tangent to both
//!   segments, which revolves into a torus band (convex rim: the material
//!   is inside the tube; concave rim: outside);
//! - **chamfer**: the corner becomes a straight cut, which revolves into a
//!   cone band (or a plane / cylinder in degenerate orientations).
//!
//! The size limit falls out of the profile: the tangent setbacks from both
//! ends of a segment must fit in that segment's length — the cylinder
//! height, the cap radius, the wall thickness of a cup rim — not the
//! circumference of the edge.

use std::f64::consts::PI;

use vcad_kernel_geom::{
    ConeSurface, CylinderSurface, GeometryStore, Plane, SurfaceKind, TorusSurface,
};
use vcad_kernel_math::{Dir3, Point3, Vec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_topo::{HalfEdgeId, Orientation, ShellType, Topology, VertexId};

/// Which circular edges of a body of revolution to blend.
#[derive(Debug, Clone, Copy)]
pub enum RevolvedEdges {
    /// Every circular edge.
    All,
    /// The single circular edge nearest to this point.
    Near(Point3),
    /// The circular edge both of these points lie on (to tolerance). A
    /// closed rim reports its seam vertex twice; a boolean-densified rim
    /// reports two neighboring rim vertices — both land on the same corner.
    Through(Point3, Point3),
}

/// Cross-section of the blend.
#[derive(Debug, Clone, Copy)]
pub enum RevolvedBlendKind {
    /// Circular fillet of this radius (torus band).
    Fillet(f64),
    /// Equal-leg chamfer with this setback (cone band).
    Chamfer(f64),
}

/// A successful revolved blend.
#[derive(Debug, Clone)]
pub struct RevolvedBlend {
    /// The blended solid.
    pub solid: BRepSolid,
    /// How many circular edges were blended.
    pub blended: usize,
    /// Exact volume of the input, from its profile (Pappus).
    pub input_volume: f64,
    /// Exact volume of the output, from its profile (Pappus).
    pub output_volume: f64,
}

/// Why a revolved blend that *was* applicable could not be built.
#[derive(Debug, Clone, PartialEq)]
pub struct RevolvedRefusal(pub String);

impl std::fmt::Display for RevolvedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type P2 = (f64, f64);

/// Half-profile of a body of revolution, CCW in the (r, t) plane.
struct Profile {
    origin: Point3,
    axis: Vec3,
    ref_dir: Vec3,
    /// Polygon corners, CCW (r = x, t = y).
    pts: Vec<P2>,
    /// `on_axis[i]` — the segment `pts[i] → pts[i+1]` lies on the axis
    /// (it is not a face of the solid).
    on_axis: Vec<bool>,
    scale: f64,
}

/// Fillet or chamfer circular edges of a body of revolution.
///
/// Returns `Ok(None)` when `brep` is not a body of revolution or `edges`
/// matches none of its circular edges — the caller should use its
/// general pipeline. Returns `Err` when the blend applies but cannot be
/// built (the size does not fit the profile), fail-closed.
pub fn blend_revolved(
    brep: &BRepSolid,
    edges: RevolvedEdges,
    kind: RevolvedBlendKind,
) -> Result<Option<RevolvedBlend>, RevolvedRefusal> {
    let size = match kind {
        RevolvedBlendKind::Fillet(r) | RevolvedBlendKind::Chamfer(r) => r,
    };
    let Some(profile) = extract_profile(brep) else {
        return Ok(None);
    };
    if !(size.is_finite() && size > 0.0) {
        return Err(RevolvedRefusal(format!(
            "blend size must be a positive length, got {size}"
        )));
    }
    let n = profile.pts.len();
    // A corner is a real 3D circular edge when neither adjacent segment
    // lies on the axis.
    let real = |i: usize| !profile.on_axis[(i + n - 1) % n] && !profile.on_axis[i];
    let corners: Vec<usize> = match edges {
        RevolvedEdges::All => (0..n).filter(|&i| real(i)).collect(),
        RevolvedEdges::Near(p) => {
            let q = profile.rt(p);
            (0..n)
                .filter(|&i| real(i))
                .min_by(|&a, &b| dist(profile.pts[a], q).total_cmp(&dist(profile.pts[b], q)))
                .into_iter()
                .collect()
        }
        RevolvedEdges::Through(a, b) => {
            let (qa, qb) = (profile.rt(a), profile.rt(b));
            let tol = 1e-5 * profile.scale.max(1.0);
            (0..n)
                .filter(|&i| real(i))
                .filter(|&i| dist(profile.pts[i], qa) < tol && dist(profile.pts[i], qb) < tol)
                .take(1)
                .collect()
        }
    };
    if corners.is_empty() {
        return Ok(None);
    }

    let input_volume = profile_volume(&profile.pts);
    let pieces = blend_corners(&profile, &corners, kind)?;
    let output_volume = pieces_volume(&pieces);
    let solid = build_revolved(&profile, &pieces).map_err(RevolvedRefusal)?;
    Ok(Some(RevolvedBlend {
        solid,
        blended: corners.len(),
        input_volume,
        output_volume,
    }))
}

fn dist(a: P2, b: P2) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

impl Profile {
    fn rt(&self, p: Point3) -> P2 {
        rt_of(self.origin, self.axis, p)
    }
}

fn rt_of(origin: Point3, axis: Vec3, p: Point3) -> P2 {
    let d = p - origin;
    let t = d.dot(axis);
    ((d - axis * t).norm(), t)
}

/// Recover the half-profile, or `None` when `brep` is not a full body of
/// revolution about a single axis.
fn extract_profile(brep: &BRepSolid) -> Option<Profile> {
    let topo = &brep.topology;
    let geom = &brep.geometry;
    let solid = topo.solids.get(brep.solid_id)?;
    let shell = topo.shells.get(solid.outer_shell)?;

    // Axis from the first cylinder / cone.
    let mut axis_frame: Option<(Point3, Vec3)> = None;
    for &f in &shell.faces {
        let s = &geom.surfaces[topo.faces[f].surface_index];
        let frame = if let Some(c) = s.as_any().downcast_ref::<CylinderSurface>() {
            Some((c.center, *c.axis.as_ref()))
        } else {
            s.as_any()
                .downcast_ref::<ConeSurface>()
                .map(|c| (c.apex, *c.axis.as_ref()))
        };
        if frame.is_some() {
            axis_frame = frame;
            break;
        }
    }
    let (origin, axis) = axis_frame?;
    let axis = axis.normalize();

    let mut scale: f64 = 0.0;
    for (_, v) in &topo.vertices {
        scale = scale.max((v.point - origin).norm());
    }
    let tol = 1e-6 * scale.max(1.0);

    let on_axis_line = |p: Point3| {
        let d = p - origin;
        (d - axis * d.dot(axis)).norm() < tol
    };

    let mut segs: Vec<(P2, P2)> = Vec::new();
    for &f in &shell.faces {
        let face = &topo.faces[f];
        let s = &geom.surfaces[face.surface_index];
        let rt_loop = |l| -> Vec<P2> {
            topo.loop_half_edges(l)
                .map(|he| {
                    rt_of(
                        origin,
                        axis,
                        topo.vertices[topo.half_edges[he].origin].point,
                    )
                })
                .collect()
        };
        let outer = rt_loop(face.outer_loop);
        let inner: Vec<Vec<P2>> = face.inner_loops.iter().map(|&l| rt_loop(l)).collect();
        if outer.is_empty() {
            return None;
        }
        match s.surface_type() {
            SurfaceKind::Plane => {
                let p = s.as_any().downcast_ref::<Plane>()?;
                if p.normal_dir.as_ref().cross(axis).norm() > 1e-9 {
                    return None;
                }
                let t = outer[0].1;
                if outer
                    .iter()
                    .chain(inner.iter().flatten())
                    .any(|q| (q.1 - t).abs() > tol)
                    || inner.len() > 1
                {
                    return None;
                }
                let r_out = outer[0].0;
                if outer.iter().any(|q| (q.0 - r_out).abs() > tol) {
                    return None;
                }
                let r_in = match inner.first() {
                    None => 0.0,
                    Some(l) => {
                        let r = l.first()?.0;
                        if l.iter().any(|q| (q.0 - r).abs() > tol) {
                            return None;
                        }
                        r
                    }
                };
                if r_in >= r_out - tol {
                    return None;
                }
                segs.push(((r_in, t), (r_out, t)));
            }
            SurfaceKind::Cylinder | SurfaceKind::Cone => {
                if !inner.is_empty() {
                    return None;
                }
                let (c_origin, c_axis) =
                    if let Some(c) = s.as_any().downcast_ref::<CylinderSurface>() {
                        (c.center, *c.axis.as_ref())
                    } else {
                        let c = s.as_any().downcast_ref::<ConeSurface>()?;
                        (c.apex, *c.axis.as_ref())
                    };
                if c_axis.normalize().cross(axis).norm() > 1e-9 || !on_axis_line(c_origin) {
                    return None;
                }
                // Exactly two distinct (r, t) points: the two rims.
                let mut ends: Vec<P2> = Vec::new();
                for q in &outer {
                    if !ends.iter().any(|e| dist(*e, *q) < tol) {
                        ends.push(*q);
                    }
                }
                if ends.len() != 2 {
                    return None;
                }
                if s.surface_type() == SurfaceKind::Cylinder && (ends[0].0 - ends[1].0).abs() > tol
                {
                    return None;
                }
                segs.push((ends[0], ends[1]));
            }
            _ => return None,
        }
    }

    // Chain the segments into one closed polygon. Nodes are quantized
    // (r, t) points; on-axis nodes are joined by the axis itself.
    let key = |p: P2| -> (i64, i64) { ((p.0 / tol).round() as i64, (p.1 / tol).round() as i64) };
    let mut nodes: Vec<P2> = Vec::new();
    let mut node_of = std::collections::HashMap::new();
    let mut node = |p: P2, nodes: &mut Vec<P2>| -> usize {
        let p = if p.0.abs() < tol { (0.0, p.1) } else { p };
        *node_of.entry(key(p)).or_insert_with(|| {
            nodes.push(p);
            nodes.len() - 1
        })
    };
    // (a, b, on_axis)
    let mut links: Vec<(usize, usize, bool)> = Vec::new();
    for (a, b) in &segs {
        let (ia, ib) = (node(*a, &mut nodes), node(*b, &mut nodes));
        if ia == ib {
            return None;
        }
        links.push((ia, ib, false));
    }
    let axis_nodes: Vec<usize> = (0..nodes.len()).filter(|&i| nodes[i].0 == 0.0).collect();
    match axis_nodes.len() {
        0 => {}
        2 => links.push((axis_nodes[0], axis_nodes[1], true)),
        _ => return None,
    }
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (li, &(a, b, _)) in links.iter().enumerate() {
        adj[a].push(li);
        adj[b].push(li);
    }
    if adj.iter().any(|l| l.len() != 2) {
        return None;
    }
    let mut pts = Vec::new();
    let mut on_axis = Vec::new();
    let mut used = vec![false; links.len()];
    let mut cur = 0usize;
    let mut li = adj[0][0];
    for _ in 0..links.len() {
        if used[li] {
            return None;
        }
        used[li] = true;
        let (a, b, ax) = links[li];
        let next = if a == cur { b } else { a };
        pts.push(nodes[cur]);
        on_axis.push(ax);
        cur = next;
        li = if adj[cur][0] == li {
            adj[cur][1]
        } else {
            adj[cur][0]
        };
    }
    if cur != 0 || used.iter().any(|u| !u) {
        return None;
    }

    // Merge collinear runs (a boolean splits a wall where a cut plane
    // crossed it; that split is not an edge of the profile).
    let mut changed = true;
    while changed && pts.len() > 3 {
        changed = false;
        let n = pts.len();
        for i in 0..n {
            let (p0, p1, p2) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            let cross = (p1.0 - p0.0) * (p2.1 - p1.1) - (p1.1 - p0.1) * (p2.0 - p1.0);
            let dot = (p1.0 - p0.0) * (p2.0 - p1.0) + (p1.1 - p0.1) * (p2.1 - p1.1);
            let same_kind = on_axis[(i + n - 1) % n] == on_axis[i];
            if same_kind && cross.abs() < tol * dist(p0, p2).max(tol) && dot > 0.0 {
                pts.remove(i);
                // Segments i-1 and i fuse into segment i-1.
                on_axis.remove(i);
                changed = true;
                break;
            }
        }
    }
    if pts.len() < 3 {
        return None;
    }

    // Orient CCW. Reversing the corners turns segment i (pts[i]→pts[i+1])
    // into old segment n-2-i.
    if shoelace(&pts) < 0.0 {
        pts.reverse();
        let n = on_axis.len();
        let old = on_axis.clone();
        for (i, o) in on_axis.iter_mut().enumerate() {
            *o = old[(2 * n - 2 - i) % n];
        }
    }

    let ref_dir = *CylinderSurface::with_axis(origin, axis, 1.0)
        .ref_dir
        .as_ref();
    Some(Profile {
        origin,
        axis,
        ref_dir,
        pts,
        on_axis,
        scale,
    })
}

fn shoelace(pts: &[P2]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        * 0.5
}

/// Pappus volume of a CCW polygon revolved about r = 0.
fn profile_volume(pts: &[P2]) -> f64 {
    let n = pts.len();
    let pieces: Vec<Piece> = (0..n)
        .map(|i| Piece::Line {
            a: pts[i],
            b: pts[(i + 1) % n],
            on_axis: false,
        })
        .collect();
    pieces_volume(&pieces)
}

/// One piece of the blended profile.
#[derive(Debug, Clone, Copy)]
enum Piece {
    /// Straight segment `a → b`; `on_axis` pieces are not faces.
    Line { a: P2, b: P2, on_axis: bool },
    /// Circular arc `a → b` about `c`, radius `rho`; `ccw` = turning left.
    Arc {
        a: P2,
        b: P2,
        c: P2,
        rho: f64,
        ccw: bool,
    },
}

/// Exact Pappus volume of a closed CCW chain of lines and arcs:
/// V = 2π ∬ r dA = 2π ∮ (r²/2) dt (Green's theorem).
fn pieces_volume(pieces: &[Piece]) -> f64 {
    let mut m = 0.0;
    for p in pieces {
        match *p {
            Piece::Line { a, b, .. } => {
                m += (b.1 - a.1) / 6.0 * (a.0 * a.0 + a.0 * b.0 + b.0 * b.0);
            }
            Piece::Arc { a, b, c, rho, ccw } => {
                let th0 = (a.1 - c.1).atan2(a.0 - c.0);
                let mut th1 = (b.1 - c.1).atan2(b.0 - c.0);
                if ccw {
                    while th1 < th0 {
                        th1 += 2.0 * PI;
                    }
                } else {
                    while th1 > th0 {
                        th1 -= 2.0 * PI;
                    }
                }
                // r = c_r + ρ cos θ, dt = ρ cos θ dθ:
                // ∫ ½ (c_r + ρ cos θ)² ρ cos θ dθ.
                let cr = c.0;
                let f = |th: f64| {
                    let s = th.sin();
                    0.5 * rho
                        * (cr * cr * s
                            + 2.0 * cr * rho * (th / 2.0 + (2.0 * th).sin() / 4.0)
                            + rho * rho * (s - s * s * s / 3.0))
                };
                m += f(th1) - f(th0);
            }
        }
    }
    2.0 * PI * m
}

fn describe_segment(a: P2, b: P2) -> String {
    if (a.1 - b.1).abs() < 1e-12 {
        if a.0.min(b.0) < 1e-12 {
            format!("the {:.4} mm cap radius", a.0.max(b.0))
        } else {
            format!(
                "the {:.4} mm annular face (wall thickness)",
                (a.0 - b.0).abs()
            )
        }
    } else if (a.0 - b.0).abs() < 1e-12 {
        format!("the {:.4} mm wall height", (a.1 - b.1).abs())
    } else {
        format!("the {:.4} mm conical face", dist(a, b))
    }
}

fn norm2(v: P2) -> P2 {
    let l = (v.0 * v.0 + v.1 * v.1).sqrt();
    (v.0 / l, v.1 / l)
}

fn blend_corners(
    profile: &Profile,
    corners: &[usize],
    kind: RevolvedBlendKind,
) -> Result<Vec<Piece>, RevolvedRefusal> {
    let pts = &profile.pts;
    let n = pts.len();
    let eps = 1e-9 * profile.scale.max(1.0);
    let (size, what) = match kind {
        RevolvedBlendKind::Fillet(r) => (r, "fillet radius"),
        RevolvedBlendKind::Chamfer(d) => (d, "chamfer distance"),
    };
    // Unit directions from each corner to its neighbors, and the interior
    // angle between them.
    let frame = |i: usize| -> (P2, P2, f64) {
        let (p0, p, p1) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
        let u1 = norm2((p0.0 - p.0, p0.1 - p.1));
        let u2 = norm2((p1.0 - p.0, p1.1 - p.1));
        let theta = (u1.0 * u2.0 + u1.1 * u2.1).clamp(-1.0, 1.0).acos();
        (u1, u2, theta)
    };
    let mut setback = vec![0.0f64; n];
    let mut blended = vec![false; n];
    for &i in corners {
        let (_, _, theta) = frame(i);
        if theta < 1e-6 {
            return Err(RevolvedRefusal(
                "knife edge: the faces meet at a zero angle".into(),
            ));
        }
        setback[i] = match kind {
            RevolvedBlendKind::Fillet(r) => r / (theta / 2.0).tan(),
            RevolvedBlendKind::Chamfer(d) => d,
        };
        blended[i] = true;
    }
    // Each segment must host the setbacks from both of its ends.
    let mut drop = vec![false; n];
    for i in 0..n {
        let j = (i + 1) % n;
        let need = setback[i] + setback[j];
        if need <= 0.0 {
            continue;
        }
        let len = dist(pts[i], pts[j]);
        if need < len - eps {
            continue;
        }
        // Two blends meeting exactly in the middle of a segment is a full
        // round — the segment vanishes and the arcs join tangentially.
        let touches_axis = pts[i].0 == 0.0 || pts[j].0 == 0.0;
        if blended[i] && blended[j] && (need - len).abs() <= eps && !touches_axis {
            drop[i] = true;
            continue;
        }
        return Err(RevolvedRefusal(format!(
            "{what} {size} mm does not fit: it needs {need:.4} mm of {} \
             (the blend must be smaller than the face it trims)",
            describe_segment(pts[i], pts[j])
        )));
    }

    // Start/end points of each segment after trimming.
    let trimmed = |i: usize| -> (P2, P2) {
        let j = (i + 1) % n;
        let u = norm2((pts[j].0 - pts[i].0, pts[j].1 - pts[i].1));
        (
            (pts[i].0 + u.0 * setback[i], pts[i].1 + u.1 * setback[i]),
            (pts[j].0 - u.0 * setback[j], pts[j].1 - u.1 * setback[j]),
        )
    };
    let mut pieces = Vec::new();
    for i in 0..n {
        // Corner piece at pts[i], between segment i-1 and segment i.
        if blended[i] {
            let a = trimmed((i + n - 1) % n).1;
            let b = trimmed(i).0;
            match kind {
                RevolvedBlendKind::Chamfer(_) => pieces.push(Piece::Line {
                    a,
                    b,
                    on_axis: false,
                }),
                RevolvedBlendKind::Fillet(r) => {
                    let (u1, u2, theta) = frame(i);
                    let p = pts[i];
                    let bis = norm2((u1.0 + u2.0, u1.1 + u2.1));
                    let d = r / (theta / 2.0).sin();
                    let c = (p.0 + bis.0 * d, p.1 + bis.1 * d);
                    // Left turn on a CCW profile = convex corner.
                    let turn = (-u1.0) * u2.1 - (-u1.1) * u2.0;
                    if c.0 - r < eps {
                        return Err(RevolvedRefusal(format!(
                            "{what} {size} mm reaches the axis of revolution"
                        )));
                    }
                    pieces.push(Piece::Arc {
                        a,
                        b,
                        c,
                        rho: r,
                        ccw: turn > 0.0,
                    });
                }
            }
        }
        if !drop[i] {
            let (a, b) = trimmed(i);
            pieces.push(Piece::Line {
                a,
                b,
                on_axis: profile.on_axis[i],
            });
        }
    }
    Ok(pieces)
}

fn build_revolved(profile: &Profile, pieces: &[Piece]) -> Result<BRepSolid, String> {
    let (origin, axis, ref_dir) = (profile.origin, profile.axis, profile.ref_dir);
    let y_dir = axis.cross(ref_dir);
    let eps = 1e-9 * profile.scale.max(1.0);
    let to3 = |p: P2, ang: f64| -> Point3 {
        let radial = ref_dir * ang.cos() + y_dir * ang.sin();
        origin + axis * p.1 + radial * p.0
    };
    let axis_pt = |t: f64| origin + axis * t;

    let mut topo = Topology::new();
    let mut geom = GeometryStore::new();
    let mut faces = Vec::new();
    // One seam vertex per off-axis profile point.
    let mut vkeys: Vec<(P2, VertexId)> = Vec::new();
    // Circle half-edges at each profile point, paired across faces below.
    let mut rims: Vec<(P2, HalfEdgeId)> = Vec::new();

    fn vertex(
        topo: &mut Topology,
        vkeys: &mut Vec<(P2, VertexId)>,
        p: P2,
        pos: Point3,
        eps: f64,
    ) -> VertexId {
        if let Some((_, v)) = vkeys.iter().find(|(q, _)| dist(*q, p) < eps) {
            return *v;
        }
        let v = topo.add_vertex(pos);
        vkeys.push((p, v));
        v
    }

    let mut planar: Vec<(P2, P2)> = Vec::new();
    for piece in pieces {
        // (a, b, surface, forward) for seam-quad faces.
        let quad: Option<(P2, P2, usize, bool)> = match *piece {
            Piece::Line { on_axis: true, .. } => None,
            Piece::Line { a, b, .. } => {
                let (dr, dt) = (b.0 - a.0, b.1 - a.1);
                if dist(a, b) < eps {
                    None
                } else if dt.abs() < eps {
                    planar.push((a, b));
                    None
                } else if dr.abs() < eps {
                    // CCW profile: walking up (+t) means the wall faces out.
                    let s =
                        geom.add_surface(Box::new(CylinderSurface::with_axis(origin, axis, a.0)));
                    Some((a, b, s, dt > 0.0))
                } else {
                    if a.0 < eps || b.0 < eps {
                        return Err("a conical face reaching the axis is not supported".into());
                    }
                    // Apex where the generator line meets r = 0; the cone
                    // axis points from the apex into the band.
                    let t_apex = a.1 - a.0 * dt / dr;
                    let t_mid = 0.5 * (a.1 + b.1);
                    let c_axis = if t_mid > t_apex { axis } else { -axis };
                    let s = geom.add_surface(Box::new(ConeSurface {
                        apex: axis_pt(t_apex),
                        axis: Dir3::new_normalize(c_axis),
                        ref_dir: Dir3::new_normalize(ref_dir),
                        half_angle: (dr / dt).abs().atan(),
                    }));
                    Some((a, b, s, dt > 0.0))
                }
            }
            Piece::Arc { a, b, c, rho, ccw } => {
                let s = geom.add_surface(Box::new(TorusSurface {
                    center: axis_pt(c.1),
                    axis: Dir3::new_normalize(axis),
                    ref_dir: Dir3::new_normalize(ref_dir),
                    major_radius: c.0,
                    minor_radius: rho,
                }));
                // Convex rim: material inside the tube, torus normal is
                // outward. Concave rim: reversed.
                Some((a, b, s, ccw))
            }
        };
        let Some((a, b, s, forward)) = quad else {
            continue;
        };
        let va = vertex(&mut topo, &mut vkeys, a, to3(a, 0.0), eps);
        let vb = vertex(&mut topo, &mut vkeys, b, to3(b, 0.0), eps);
        let he_a = topo.add_half_edge(va);
        let he_up = topo.add_half_edge(va);
        let he_b = topo.add_half_edge(vb);
        let he_down = topo.add_half_edge(vb);
        let l = topo.add_loop(&[he_a, he_up, he_b, he_down]);
        let orient = if forward {
            Orientation::Forward
        } else {
            Orientation::Reversed
        };
        let f = topo.add_face(l, s, orient);
        topo.add_edge(he_up, he_down);
        rims.push((a, he_a));
        rims.push((b, he_b));
        faces.push(f);
    }
    for (a, b) in planar {
        let t = a.1;
        // CCW profile: outward normal (dt, −dr) → axial component −dr.
        let normal = if b.0 - a.0 < 0.0 { axis } else { -axis };
        let s = geom.add_surface(Box::new(Plane::from_normal(axis_pt(t), normal)));
        let (outer, inner) = if a.0 >= b.0 { (a, b) } else { (b, a) };
        let v_out = vertex(&mut topo, &mut vkeys, outer, to3(outer, 0.0), eps);
        let he_out = topo.add_half_edge(v_out);
        let l = topo.add_loop(&[he_out]);
        let f = topo.add_face(l, s, Orientation::Forward);
        rims.push((outer, he_out));
        if inner.0 > eps {
            // The hole is a full circle too: one closed half-edge on the
            // inner seam vertex, paired with the neighboring wall's rim.
            let v_in = vertex(&mut topo, &mut vkeys, inner, to3(inner, 0.0), eps);
            let he_in = topo.add_half_edge(v_in);
            let il = topo.add_loop(&[he_in]);
            topo.add_inner_loop(f, il);
            rims.push((inner, he_in));
        }
        faces.push(f);
    }

    // Exactly two faces meet at each rim: pair their circle half-edges.
    let mut paired = vec![false; rims.len()];
    for i in 0..rims.len() {
        if paired[i] {
            continue;
        }
        if let Some(j) =
            (i + 1..rims.len()).find(|&j| !paired[j] && dist(rims[i].0, rims[j].0) < eps)
        {
            topo.add_edge(rims[i].1, rims[j].1);
            paired[i] = true;
            paired[j] = true;
        }
    }

    if faces.is_empty() {
        return Err("blended profile has no faces".into());
    }
    let shell = topo.add_shell(faces, ShellType::Outer);
    let solid_id = topo.add_solid(shell);
    Ok(BRepSolid {
        topology: topo,
        geometry: geom,
        solid_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_volume_matches_pappus_for_a_quarter_disk() {
        // Quarter disk of radius ρ at (R, 0), closed by two radii: its
        // centroid sits at R + 4ρ/(3π).
        let (rr, rho) = (10.0, 2.0);
        let pieces = [
            Piece::Line {
                a: (rr, 0.0),
                b: (rr + rho, 0.0),
                on_axis: false,
            },
            Piece::Arc {
                a: (rr + rho, 0.0),
                b: (rr, rho),
                c: (rr, 0.0),
                rho,
                ccw: true,
            },
            Piece::Line {
                a: (rr, rho),
                b: (rr, 0.0),
                on_axis: false,
            },
        ];
        let expected = 2.0 * PI * (rr + 4.0 * rho / (3.0 * PI)) * PI * rho * rho / 4.0;
        assert!((pieces_volume(&pieces) - expected).abs() < 1e-9 * expected);
    }

    #[test]
    fn polygon_volume_is_cylinder_volume() {
        let pts = [(0.0, 0.0), (3.0, 0.0), (3.0, 5.0), (0.0, 5.0)];
        assert!((profile_volume(&pts) - PI * 9.0 * 5.0).abs() < 1e-9);
    }
}
