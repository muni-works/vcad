//! All-edges blend for planar bodies that are *not* convex.
//!
//! The convex rebuild in [`crate::fillet_planar`] / [`crate::chamfer`]
//! orients every strip and corner patch by comparing against the solid's
//! centroid — which is only right when the body is convex. A block with a
//! boss on top, or an L-shaped extrude, has concave (inside-corner) edges
//! where that test points the wrong way, and booleans hand such bodies
//! over with coplanar faces split along seams.
//!
//! This path handles them:
//!
//! 1. [`merge_coplanar_faces`] fuses edge-adjacent coplanar faces into one
//!    polygon and drops the straight-through vertices the seams leave
//!    behind, so only real edges remain.
//! 2. Faces are trimmed exactly as in the convex path — the tangent line
//!    of a blend lies *inside* both adjacent faces at the same setback
//!    whether the edge is convex or concave.
//! 3. Strips and corner patches take their winding from the topology
//!    (each shares its boundary with the trimmed faces, traversed the
//!    other way), never from a centroid.
//! 4. A concave fillet is a cylinder whose axis sits *outside* the solid,
//!    so its face is `Reversed`: the solid gains material there while
//!    convex edges lose it.
//!
//! Corner patches where convex and concave edges meet are planar
//! polygons (fanned into triangles when non-planar): watertight, but an
//! approximation of the true vertex blend.

use std::collections::{HashMap, HashSet};
use vcad_kernel_geom::{CylinderSurface, GeometryStore, Plane, SurfaceKind};
use vcad_kernel_math::{Dir3, Point3, Vec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_topo::{FaceId, HalfEdgeId, Orientation, ShellType, Topology, VertexId};

use crate::topology::{
    compute_face_normal, extract_edges, extract_faces, pair_twin_half_edges, quantize, EdgeInfo,
    FaceInfo,
};
use crate::trim::{compute_trim_vertices_with_setbacks, fillet_edge_setbacks, TrimKey};
use crate::BlendRefusal;

/// What the polyhedral rebuild puts on each edge.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PolyBlend {
    /// Cylindrical fillet of this radius.
    Fillet(f64),
    /// Planar bevel, this distance along each face.
    Chamfer(f64),
}

/// Relative tolerance for "these two faces lie in one plane".
const COPLANAR_EPS: f64 = 1e-9;

/// True when the body has an edge the convex rebuild can't handle: a
/// concave edge, or a seam between exactly coplanar faces.
pub(crate) fn needs_polyhedral(brep: &BRepSolid) -> bool {
    let faces = extract_faces(brep);
    let edges = extract_edges(brep);
    let face_map: HashMap<FaceId, &FaceInfo> = faces.iter().map(|f| (f.face_id, f)).collect();
    edges.iter().any(|e| {
        let (Some(fa), Some(fb)) = (face_map.get(&e.face_a), face_map.get(&e.face_b)) else {
            return false;
        };
        is_concave(brep, fa, fb, e) || same_plane(fa, fb)
    })
}

/// Face A's half-edge runs start→end counter-clockwise about its outward
/// normal, so a convex edge has `(n_a × n_b)·dir > 0`.
fn is_concave(brep: &BRepSolid, fa: &FaceInfo, fb: &FaceInfo, e: &EdgeInfo) -> bool {
    let dir = brep.topology.vertices[e.v_end].point - brep.topology.vertices[e.v_start].point;
    let len = dir.norm();
    len > 1e-12 && fa.normal.cross(fb.normal).dot(dir) < -1e-9 * len
}

fn same_plane(fa: &FaceInfo, fb: &FaceInfo) -> bool {
    if fa.normal.dot(fb.normal) < 1.0 - COPLANAR_EPS {
        return false;
    }
    let scale = fa
        .positions
        .iter()
        .chain(&fb.positions)
        .map(|p| p.x.abs().max(p.y.abs()).max(p.z.abs()))
        .fold(1.0, f64::max);
    fb.positions
        .iter()
        .all(|p| (*p - fa.positions[0]).dot(fa.normal).abs() <= COPLANAR_EPS * scale)
}

/// Fuse edge-adjacent coplanar faces and drop the collinear vertices the
/// seams leave behind. Returns `Ok(None)` when there was nothing to fuse.
///
/// Refuses (rather than guessing) when a fused face would need an inner
/// loop or pinches at a vertex, or when a seam leaves a T-junction.
pub(crate) fn merge_coplanar_faces(brep: &BRepSolid) -> Result<Option<BRepSolid>, BlendRefusal> {
    let faces = extract_faces(brep);
    let edges = extract_edges(brep);
    let idx: HashMap<FaceId, usize> = faces
        .iter()
        .enumerate()
        .map(|(i, f)| (f.face_id, i))
        .collect();

    // Union-find over coplanar edge-adjacent faces.
    let mut parent: Vec<usize> = (0..faces.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut merged_any = false;
    for e in &edges {
        let (Some(&a), Some(&b)) = (idx.get(&e.face_a), idx.get(&e.face_b)) else {
            continue;
        };
        if a != b && same_plane(&faces[a], &faces[b]) {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                parent[ra] = rb;
                merged_any = true;
            }
        }
    }

    // Collinear seam vertices can exist even without anything to merge
    // (booleans leave them on otherwise intact faces).
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<usize, usize> = HashMap::new();
    for i in 0..faces.len() {
        let r = find(&mut parent, i);
        let g = *group_of.entry(r).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }

    // Each group → one loop of points: drop directed edges whose reverse
    // is in the same group, chain the rest.
    type Key = [i64; 3];
    let mut point_of: HashMap<Key, Point3> = HashMap::new();
    let mut loops: Vec<(Vec<Key>, Vec3)> = Vec::new();
    for g in &groups {
        let mut directed: Vec<(Key, Key)> = Vec::new();
        for &fi in g {
            let f = &faces[fi];
            let n = f.positions.len();
            for k in 0..n {
                let (a, b) = (f.positions[k], f.positions[(k + 1) % n]);
                let (ka, kb) = (quantize(a), quantize(b));
                point_of.insert(ka, a);
                point_of.insert(kb, b);
                if ka != kb {
                    directed.push((ka, kb));
                }
            }
        }
        let set: HashSet<(Key, Key)> = directed.iter().copied().collect();
        let boundary: Vec<(Key, Key)> = directed
            .into_iter()
            .filter(|(a, b)| !set.contains(&(*b, *a)))
            .collect();
        let mut next: HashMap<Key, Key> = HashMap::new();
        for &(a, b) in &boundary {
            if next.insert(a, b).is_some() {
                // A vertex the fused face passes through twice.
                return Err(pinch());
            }
        }
        let Some(&(start, _)) = boundary.first() else {
            return Err(pinch());
        };
        let mut lp = vec![start];
        let mut cur = next[&start];
        while cur != start {
            lp.push(cur);
            cur = *next.get(&cur).ok_or_else(pinch)?;
            if lp.len() > boundary.len() {
                return Err(pinch());
            }
        }
        if lp.len() != boundary.len() {
            // More than one loop: the fused face needs a hole.
            return Err(pinch());
        }
        loops.push((lp, faces[g[0]].normal));
    }

    // Neighbours of each vertex over all loops; a vertex with exactly two
    // neighbours that it sits between in a straight line is a seam relic.
    let mut nbrs: HashMap<Key, HashSet<Key>> = HashMap::new();
    for (lp, _) in &loops {
        let n = lp.len();
        for k in 0..n {
            let e = nbrs.entry(lp[k]).or_default();
            e.insert(lp[(k + n - 1) % n]);
            e.insert(lp[(k + 1) % n]);
        }
    }
    let collinear = |k: &Key| -> bool {
        let Some(ns) = nbrs.get(k) else { return false };
        if ns.len() != 2 {
            return false;
        }
        let mut it = ns.iter();
        let (a, b) = (point_of[it.next().unwrap()], point_of[it.next().unwrap()]);
        let p = point_of[k];
        let (u, v) = (p - a, b - p);
        let (lu, lv) = (u.norm(), v.norm());
        lu > 1e-12 && lv > 1e-12 && u.cross(v).norm() <= 1e-9 * lu * lv && u.dot(v) > 0.0
    };
    let mut dropped_any = false;
    let mut polys: Vec<(Vec<Point3>, Vec3)> = Vec::new();
    for (lp, normal) in &loops {
        let kept: Vec<Point3> = lp
            .iter()
            .filter(|k| {
                let c = collinear(k);
                dropped_any |= c;
                !c
            })
            .map(|k| point_of[k])
            .collect();
        if kept.len() < 3 {
            return Err(pinch());
        }
        // A collinear vertex that survived (T-junction) would give the
        // trim a parallel-lines intersection.
        let n = kept.len();
        for k in 0..n {
            let (a, p, b) = (kept[(k + n - 1) % n], kept[k], kept[(k + 1) % n]);
            let (u, v) = (p - a, b - p);
            if u.cross(v).norm() <= 1e-9 * u.norm() * v.norm() {
                return Err(BlendRefusal::CoplanarAdjacentFaces);
            }
        }
        polys.push((kept, *normal));
    }

    if !merged_any && !dropped_any {
        return Ok(None);
    }
    Ok(Some(solid_from_polygons(&polys)))
}

fn pinch() -> BlendRefusal {
    BlendRefusal::CoplanarAdjacentFaces
}

/// Build a planar B-rep from outward-wound polygons sharing vertices by
/// position.
fn solid_from_polygons(polys: &[(Vec<Point3>, Vec3)]) -> BRepSolid {
    let mut topo = Topology::new();
    let mut geom = GeometryStore::new();
    let mut cache: HashMap<[i64; 3], VertexId> = HashMap::new();
    let mut all = Vec::new();
    for (pts, normal) in polys {
        let verts: Vec<VertexId> = pts
            .iter()
            .map(|p| {
                *cache
                    .entry(quantize(*p))
                    .or_insert_with(|| topo.add_vertex(*p))
            })
            .collect();
        let surf = geom.add_surface(Box::new(Plane::from_normal(pts[0], *normal)));
        let hes: Vec<HalfEdgeId> = verts.iter().map(|&v| topo.add_half_edge(v)).collect();
        let lp = topo.add_loop(&hes);
        all.push(topo.add_face(lp, surf, Orientation::Forward));
    }
    pair_twin_half_edges(&mut topo);
    let shell = topo.add_shell(all, ShellType::Outer);
    let solid_id = topo.add_solid(shell);
    BRepSolid {
        topology: topo,
        geometry: geom,
        solid_id,
    }
}

/// Blend every edge of a planar body, convex or concave.
pub(crate) fn blend_polyhedral(
    input: &BRepSolid,
    kind: PolyBlend,
) -> Result<BRepSolid, BlendRefusal> {
    if input
        .geometry
        .surfaces
        .iter()
        .any(|s| s.surface_type() != SurfaceKind::Plane)
    {
        return Err(BlendRefusal::NonPlanarFace);
    }
    let merged = merge_coplanar_faces(input)?;
    let brep = merged.as_ref().unwrap_or(input);

    let faces = extract_faces(brep);
    let edges = extract_edges(brep);
    if edges.is_empty() {
        return Err(BlendRefusal::NoEdges);
    }
    let face_map: HashMap<FaceId, &FaceInfo> = faces.iter().map(|f| (f.face_id, f)).collect();

    // Every half-edge must have found its twin, or the rebuild can't close.
    let paired = brep
        .topology
        .half_edges
        .iter()
        .all(|(_, h)| h.twin.is_some());
    if !paired {
        return Err(BlendRefusal::CoplanarAdjacentFaces);
    }

    for e in &edges {
        let (fa, fb) = (face_map[&e.face_a], face_map[&e.face_b]);
        // The same near-coplanar guard as the convex path (tessellated
        // arc walls): exact seams were merged away above.
        let interior = std::f64::consts::PI - fa.normal.dot(fb.normal).clamp(-1.0, 1.0).acos();
        if interior > 170f64.to_radians() && interior < 190f64.to_radians() {
            return Err(BlendRefusal::CoplanarAdjacentFaces);
        }
    }

    let size = match kind {
        PolyBlend::Fillet(r) | PolyBlend::Chamfer(r) => r,
    };
    let setbacks = match kind {
        PolyBlend::Fillet(r) => {
            let s = fillet_edge_setbacks(&faces, &edges, r, 8.0 * r);
            if s.values().any(|v| !v.is_finite()) {
                return Err(BlendRefusal::KnifeEdge);
            }
            s
        }
        PolyBlend::Chamfer(_) => HashMap::new(),
    };
    let trims = compute_trim_vertices_with_setbacks(&faces, size, &setbacks);
    if !trimmed_faces_ok(&faces, &trims) {
        return Err(BlendRefusal::RadiusTooLargeForFeature);
    }

    let mut topo = Topology::new();
    let mut geom = GeometryStore::new();
    let mut cache: HashMap<[i64; 3], VertexId> = HashMap::new();
    let mut all_faces = Vec::new();
    let mut add_face = |pts: &[Point3],
                        surf: Box<dyn vcad_kernel_geom::Surface>,
                        orient: Orientation,
                        topo: &mut Topology,
                        geom: &mut GeometryStore| {
        let verts: Vec<VertexId> = pts
            .iter()
            .map(|p| {
                *cache
                    .entry(quantize(*p))
                    .or_insert_with(|| topo.add_vertex(*p))
            })
            .collect();
        let s = geom.add_surface(surf);
        let hes: Vec<HalfEdgeId> = verts.iter().map(|&v| topo.add_half_edge(v)).collect();
        let lp = topo.add_loop(&hes);
        all_faces.push(topo.add_face(lp, s, orient));
    };

    // 1. Trimmed original faces. The plane comes from the face normal,
    // not from three loop points — a reflex first vertex would flip it.
    for f in &faces {
        let pts: Vec<Point3> = f
            .vertex_ids
            .iter()
            .map(|&v| trims[&(v, f.face_id)])
            .collect();
        add_face(
            &pts,
            Box::new(Plane::from_normal(pts[0], f.normal)),
            Orientation::Forward,
            &mut topo,
            &mut geom,
        );
    }

    // 2. One strip per edge, wound against both trimmed faces:
    // face A carries pa_s→pa_e, face B carries pb_e→pb_s. A fillet strip
    // also carries its two end curves, sampled at the same angles on both
    // ends: the tessellator renders such a ruled strip verbatim, and the
    // corner patches reuse the very same points, so every seam welds even
    // where an end is slanted (a convex edge running into a concave one).
    //
    // `end_chain[(v, from, to)]` = interior points of the end curve at
    // vertex v, ordered from trim(v,from) to trim(v,to).
    let mut end_chain: HashMap<(VertexId, FaceId, FaceId), Vec<Point3>> = HashMap::new();
    let mut convex_edge: HashMap<(VertexId, VertexId), bool> = HashMap::new();
    for e in &edges {
        let (fa, fb) = (face_map[&e.face_a], face_map[&e.face_b]);
        let pa_s = trims[&(e.v_start, e.face_a)];
        let pa_e = trims[&(e.v_end, e.face_a)];
        let pb_s = trims[&(e.v_start, e.face_b)];
        let pb_e = trims[&(e.v_end, e.face_b)];
        let concave = is_concave(brep, fa, fb, e);
        convex_edge.insert((e.v_start, e.v_end), !concave);
        match kind {
            PolyBlend::Chamfer(_) => {
                let pts = [pa_e, pa_s, pb_s, pb_e];
                let n = compute_face_normal(&pts);
                add_face(
                    &pts,
                    Box::new(Plane::from_normal(pa_s, n)),
                    Orientation::Forward,
                    &mut topo,
                    &mut geom,
                );
            }
            PolyBlend::Fillet(r) => {
                let v0 = brep.topology.vertices[e.v_start].point;
                let v1 = brep.topology.vertices[e.v_end].point;
                let axis = (v1 - v0).normalize();
                let sin2_half = (1.0 + fa.normal.dot(fb.normal)) * 0.5;
                // Axis inside the solid for a convex edge, outside for a
                // concave one; distance r/sin(θ/2) along the bisector.
                let sign = if concave { 1.0 } else { -1.0 };
                let center = v0 + sign * r * (fa.normal + fb.normal) / (2.0 * sin2_half);
                let to_a = pa_s - center;
                let ref_dir = to_a - to_a.dot(axis) * axis;
                if ref_dir.norm() < 1e-12 {
                    return Err(BlendRefusal::RadiusTooLargeForFeature);
                }
                let ref_dir = ref_dir.normalize();
                let y_dir = axis.cross(ref_dir);
                let uv = |p: Point3| {
                    let d = p - center;
                    (d.dot(y_dir).atan2(d.dot(ref_dir)), d.dot(axis))
                };
                let (ua, _) = uv(pa_s);
                let (mut ub, _) = uv(pb_s);
                // Short way round: the blend spans π − θ < π.
                while ub - ua > std::f64::consts::PI {
                    ub -= 2.0 * std::f64::consts::PI;
                }
                while ub - ua < -std::f64::consts::PI {
                    ub += 2.0 * std::f64::consts::PI;
                }
                let segs = ((ub - ua).abs() / (2.0 * std::f64::consts::PI / 32.0))
                    .ceil()
                    .max(2.0) as usize;
                let at =
                    |u: f64, v: f64| center + axis * v + r * (u.cos() * ref_dir + u.sin() * y_dir);
                // Interior samples of an end curve from p (on A) to q (on B):
                // a helix arc, so it lies on the cylinder even when slanted.
                let chain = |p: Point3, q: Point3| -> Vec<Point3> {
                    let (va, vb) = (uv(p).1, uv(q).1);
                    (1..segs)
                        .map(|k| {
                            let t = k as f64 / segs as f64;
                            at(ua + t * (ub - ua), va + t * (vb - va))
                        })
                        .collect()
                };
                let c_start = chain(pa_s, pb_s);
                let c_end = chain(pa_e, pb_e);
                let mut pts = vec![pa_e, pa_s];
                pts.extend(c_start.iter().copied());
                pts.push(pb_s);
                pts.push(pb_e);
                pts.extend(c_end.iter().rev().copied());
                add_face(
                    &pts,
                    Box::new(CylinderSurface {
                        center,
                        axis: Dir3::new_normalize(axis),
                        ref_dir: Dir3::new_normalize(ref_dir),
                        radius: r,
                    }),
                    if concave {
                        Orientation::Reversed
                    } else {
                        Orientation::Forward
                    },
                    &mut topo,
                    &mut geom,
                );
                // Corners walk the end curves the other way round.
                let mut rev_start = c_start;
                rev_start.reverse();
                end_chain.insert((e.v_start, e.face_b, e.face_a), rev_start);
                end_chain.insert((e.v_end, e.face_a, e.face_b), c_end);
            }
        }
    }

    // 3. Corner patches. At vertex v a strip contributes its end curve,
    // reversed: start end trim(v,B)→trim(v,A), finish end trim(v,A)→trim(v,B).
    let mut corner_links: HashMap<VertexId, Vec<(FaceId, FaceId, bool)>> = HashMap::new();
    for e in &edges {
        let convex = convex_edge[&(e.v_start, e.v_end)];
        corner_links
            .entry(e.v_start)
            .or_default()
            .push((e.face_b, e.face_a, convex));
        corner_links
            .entry(e.v_end)
            .or_default()
            .push((e.face_a, e.face_b, convex));
    }
    let mut corners: Vec<_> = corner_links.into_iter().collect();
    corners.sort_by_key(|(v, _)| *v);
    for (v, links) in corners {
        let next: HashMap<FaceId, FaceId> = links.iter().map(|&(a, b, _)| (a, b)).collect();
        if next.len() != links.len() {
            return Err(BlendRefusal::RadiusTooLargeForFeature);
        }
        let start = links[0].0;
        let mut ring = vec![start];
        let mut cur = next[&start];
        while cur != start {
            ring.push(cur);
            cur = *next
                .get(&cur)
                .ok_or(BlendRefusal::RadiusTooLargeForFeature)?;
            if ring.len() > links.len() {
                return Err(BlendRefusal::RadiusTooLargeForFeature);
            }
        }
        if ring.len() != links.len() {
            return Err(BlendRefusal::RadiusTooLargeForFeature);
        }
        let mut pts: Vec<Point3> = Vec::new();
        for (k, &f) in ring.iter().enumerate() {
            let g = ring[(k + 1) % ring.len()];
            pts.push(trims[&(v, f)]);
            if let Some(c) = end_chain.get(&(v, f, g)) {
                pts.extend(c.iter().copied());
            }
        }
        pts.dedup_by(|a, b| quantize(*a) == quantize(*b));
        if pts.len() > 1 && quantize(pts[0]) == quantize(pts[pts.len() - 1]) {
            pts.pop();
        }
        if pts.len() < 3 {
            continue;
        }
        let n = compute_face_normal(&pts);
        let all_convex = links.iter().all(|l| l.2);
        let v_pos = brep.topology.vertices[v].point;
        let normals: Vec<Vec3> = ring.iter().map(|f| face_map[f].normal).collect();
        let sphere = match kind {
            PolyBlend::Fillet(r) if all_convex && ring.len() == 3 => {
                crate::trim::try_sphere_blend(&normals, v_pos, r)
            }
            _ => None,
        };
        if let Some(s) = sphere {
            add_face(
                &pts,
                Box::new(s),
                Orientation::Forward,
                &mut topo,
                &mut geom,
            );
            continue;
        }
        let planar = pts
            .iter()
            .all(|p| (*p - pts[0]).dot(n).abs() <= 1e-9 * size.max(1.0));
        if planar {
            add_face(
                &pts,
                Box::new(Plane::from_normal(pts[0], n)),
                Orientation::Forward,
                &mut topo,
                &mut geom,
            );
        } else {
            // Fan into triangles around the centroid.
            let c = Point3::from(
                pts.iter().fold(Vec3::zeros(), |a, p| a + p.to_vec()) / pts.len() as f64,
            );
            for k in 0..pts.len() {
                let tri = [c, pts[k], pts[(k + 1) % pts.len()]];
                let tn = compute_face_normal(&tri);
                add_face(
                    &tri,
                    Box::new(Plane::from_normal(c, tn)),
                    Orientation::Forward,
                    &mut topo,
                    &mut geom,
                );
            }
        }
    }
    pair_twin_half_edges(&mut topo);
    if topo.half_edges.iter().any(|(_, h)| h.twin.is_none()) {
        return Err(BlendRefusal::RadiusTooLargeForFeature);
    }
    let shell = topo.add_shell(all_faces, ShellType::Outer);
    let solid_id = topo.add_solid(shell);
    Ok(BRepSolid {
        topology: topo,
        geometry: geom,
        solid_id,
    })
}

/// Each trimmed face must keep its orientation and area, and stay a
/// simple polygon (no edge crossing) — a non-convex face can fold over
/// without inverting.
fn trimmed_faces_ok(faces: &[FaceInfo], trims: &HashMap<TrimKey, Point3>) -> bool {
    for f in faces {
        let Some(pts) = f
            .vertex_ids
            .iter()
            .map(|&v| trims.get(&(v, f.face_id)).copied())
            .collect::<Option<Vec<Point3>>>()
        else {
            return false;
        };
        if pts.iter().any(|p| !p.to_vec().norm().is_finite()) {
            return false;
        }
        let mut n = Vec3::zeros();
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            n.x += (a.y - b.y) * (a.z + b.z);
            n.y += (a.z - b.z) * (a.x + b.x);
            n.z += (a.x - b.x) * (a.y + b.y);
        }
        if n.norm() < 1e-9 || n.dot(f.normal) <= 0.0 {
            return false;
        }
        // Project to 2D and check every pair of non-adjacent edges.
        let u = (pts[1] - pts[0]).normalize();
        let w = f.normal.cross(u);
        let q: Vec<(f64, f64)> = pts
            .iter()
            .map(|p| ((*p - pts[0]).dot(u), (*p - pts[0]).dot(w)))
            .collect();
        let m = q.len();
        let orient = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
            (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
        };
        for i in 0..m {
            for j in (i + 2)..m {
                if i == 0 && j == m - 1 {
                    continue;
                }
                let (a, b, c, d) = (q[i], q[(i + 1) % m], q[j], q[(j + 1) % m]);
                let (d1, d2) = (orient(a, b, c), orient(a, b, d));
                let (d3, d4) = (orient(c, d, a), orient(c, d, b));
                if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
                    return false;
                }
            }
        }
    }
    true
}
