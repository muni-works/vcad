//! Topology picking and selection-driven modifiers (`vcad_topology_*`,
//! `vcad_solid_pick`, `vcad_solid_*_edges`, `vcad_solid_shell_open`).
//!
//! # Ids
//!
//! * **Face id** = ordinal of the face in the solid's outer shell — the same
//!   numbering `vcad_scene_part_face_ids` tags triangles with, so a triangle
//!   hit in the Swift viewport and a [`vcad_solid_pick`] agree.
//! * **Edge id** = ordinal of the edge in a deterministic walk: shell faces in
//!   order, each face's outer loop then inner loops, first visit wins. Seam
//!   edges (both sides on the same face, e.g. a cylinder's) are not listed —
//!   they are parametrization artifacts, not selectable features.
//!
//! Both are stable across re-tessellation of the same solid and renumbered by
//! any edit that changes the B-rep. Persist a selection through the `selector`
//! each edge carries (an IR `EdgeQuery`, `Named` when the solid has
//! persistent face names), never through the ordinal.

use std::collections::HashMap;
use std::ffi::c_char;
use std::ptr;

use serde::Serialize;
use vcad_kernel::vcad_kernel_fillet::{BlendKey, BlendSection, EdgeQuery};
use vcad_kernel::vcad_kernel_primitives::BRepSolid;
use vcad_kernel::vcad_kernel_sketch::{SketchProfile, SketchSegment};
use vcad_kernel::vcad_kernel_tessellate::{tessellate_brep_by_face, TessellationParams};
use vcad_kernel::vcad_kernel_topo::{EdgeId, FaceId};
use vcad_kernel::Solid;
use vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel_raytrace::{BrepBvh, Bvh, Ray};

use crate::json::{guard_ptr, json_out};
use crate::{err, VcadScene, VcadSolid};

/// "No face / no edge" marker in [`VcadPick`].
pub const VCAD_TOPO_NONE: u32 = u32::MAX;

/// One topological edge, with what Swift needs to draw and persist it.
#[derive(Debug, Clone, Serialize)]
struct EdgeEntry {
    id: u32,
    /// The two adjacent face ids.
    faces: [u32; 2],
    /// Endpoints (equal for a closed edge such as a full circle).
    a: [f64; 3],
    b: [f64; 3],
    /// Highlight polyline as flat segment pairs: 6 floats per segment.
    segments: Vec<f32>,
    /// True when both adjacent faces are planar — the edges the per-edge
    /// fillet/chamfer path can blend today.
    planar: bool,
    /// IR `EdgeQuery` that re-finds this edge after regeneration, when one
    /// exists (`Named`). Null otherwise.
    selector: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
struct FaceEntry {
    id: u32,
    /// Surface kind ("Plane", "Cylinder", …).
    surface: String,
    /// Area-weighted centroid of the face's tessellation.
    center: [f64; 3],
    /// Mean outward normal (exact for planes).
    normal: [f64; 3],
    area: f64,
    /// IR `FaceRef` that re-finds this face after regeneration: the
    /// persistent name when the solid carries one, plus a point on the
    /// face and its outward normal as the geometric fallback.
    selector: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
struct TopologyReport {
    faces: Vec<FaceEntry>,
    edges: Vec<EdgeEntry>,
}

/// Internal per-edge record (kernel ids kept for the modifiers).
struct EdgeRec {
    kernel: EdgeId,
    entry: EdgeEntry,
}

struct Topo {
    faces: Vec<FaceEntry>,
    edges: Vec<EdgeRec>,
    face_ordinal: HashMap<FaceId, u32>,
}

type QKey = [i64; 3];

fn q(p: [f32; 3]) -> QKey {
    // 1e-4 mm grid: far finer than any feature, coarse enough to absorb f32
    // round-off between two faces' copies of a shared rim vertex.
    p.map(|c| (c as f64 * 1e4).round() as i64)
}

fn seg_key(a: QKey, b: QKey) -> (QKey, QKey) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn analyze(solid: &Solid) -> Result<(Topo, &BRepSolid), String> {
    let brep = solid.as_brep().ok_or_else(|| {
        "solid has no B-rep (mesh-only parts have no faces/edges to pick)".to_string()
    })?;
    let topo = &brep.topology;
    let shell_faces = &topo.shells[topo.solids[brep.solid_id].outer_shell].faces;
    let face_ordinal: HashMap<FaceId, u32> = shell_faces
        .iter()
        .enumerate()
        .map(|(i, &f)| (f, i as u32))
        .collect();

    // Per-face tessellation → face stats + boundary segment sets.
    let meshes = tessellate_brep_by_face(brep, &TessellationParams::from_segments(32));
    let mut faces = Vec::with_capacity(meshes.len());
    let mut boundaries: HashMap<u32, HashMap<(QKey, QKey), [f32; 6]>> = HashMap::new();
    for (fid, kind, m) in &meshes {
        let Some(&ord) = face_ordinal.get(fid) else {
            continue;
        };
        let vert = |i: u32| {
            let i = i as usize * 3;
            [m.vertices[i], m.vertices[i + 1], m.vertices[i + 2]]
        };
        let mut area = 0.0;
        let mut c = Vec3::zeros();
        let mut n = Vec3::zeros();
        // A point guaranteed ON the face (the centroid of an annulus is
        // in its hole): the centre of its largest triangle.
        let (mut big, mut on_face) = (-1.0, Vec3::zeros());
        let mut counts: HashMap<(QKey, QKey), (u32, [f32; 6])> = HashMap::new();
        for tri in m.indices.as_chunks::<3>().0 {
            let p = [tri[0], tri[1], tri[2]].map(vert);
            let v = p.map(|x| Vec3::new(x[0] as f64, x[1] as f64, x[2] as f64));
            let cross = (v[1] - v[0]).cross(v[2] - v[0]);
            let a = 0.5 * cross.norm();
            area += a;
            c += (v[0] + v[1] + v[2]) * (a / 3.0);
            if a > big {
                big = a;
                on_face = (v[0] + v[1] + v[2]) / 3.0;
            }
            n += cross * 0.5;
            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                let k = seg_key(q(p[i]), q(p[j]));
                let e = counts.entry(k).or_insert((0, [0.0; 6]));
                e.0 += 1;
                e.1 = [p[i][0], p[i][1], p[i][2], p[j][0], p[j][1], p[j][2]];
            }
        }
        // Per-face meshes are not welded across faces but are internally
        // indexed, so an interior edge is used twice and a rim edge once.
        let rim = counts
            .into_iter()
            .filter(|(_, (n, _))| *n == 1)
            .map(|(k, (_, s))| (k, s))
            .collect();
        boundaries.insert(ord, rim);
        let c = if area > 0.0 { c / area } else { c };
        let n = if n.norm() > 0.0 { n.normalize() } else { n };
        let mut selector = serde_json::json!({
            "point": {"x": on_face.x, "y": on_face.y, "z": on_face.z},
            "normal": {"x": n.x, "y": n.y, "z": n.z},
        });
        if let Some(name) = solid.names().and_then(|m| m.faces.get(fid)) {
            selector["name"] = serde_json::Value::String(name.to_string());
        }
        faces.push(FaceEntry {
            id: ord,
            surface: format!("{kind:?}"),
            center: [c.x, c.y, c.z],
            normal: [n.x, n.y, n.z],
            area,
            selector,
        });
    }
    faces.sort_by_key(|f| f.id);

    let surface_is_plane = |f: FaceId| {
        format!(
            "{:?}",
            brep.geometry.surfaces[topo.faces[f].surface_index].surface_type()
        ) == "Plane"
    };

    let mut seen = std::collections::HashSet::new();
    let mut edges = Vec::new();
    for &fid in shell_faces {
        let face = &topo.faces[fid];
        for lp in std::iter::once(face.outer_loop).chain(face.inner_loops.iter().copied()) {
            for he in topo.loop_half_edges(lp) {
                let Some(eid) = topo.half_edges[he].edge else {
                    continue;
                };
                if !seen.insert(eid) {
                    continue;
                }
                let (Some(fa), Some(fb)) = topo.edge_faces(eid) else {
                    continue;
                };
                if fa == fb {
                    continue; // seam
                }
                let (Some(&oa), Some(&ob)) = (face_ordinal.get(&fa), face_ordinal.get(&fb)) else {
                    continue;
                };
                let h = topo.edges[eid].half_edge;
                let pa = topo.vertices[topo.half_edges[h].origin].point;
                let pb = topo
                    .half_edge_dest_opt(h)
                    .map(|v| topo.vertices[v].point)
                    .unwrap_or(pa);
                let mut segments = Vec::new();
                if let (Some(ba), Some(bb)) = (boundaries.get(&oa), boundaries.get(&ob)) {
                    for (k, s) in ba {
                        if bb.contains_key(k) {
                            segments.extend_from_slice(s);
                        }
                    }
                }
                if segments.is_empty() && (pa - pb).norm() > 1e-9 {
                    segments = [pa.x, pa.y, pa.z, pb.x, pb.y, pb.z]
                        .map(|c| c as f32)
                        .to_vec();
                }
                let selector = solid.names().and_then(|names| {
                    let r = vcad_kernel_naming::EdgeRef::capture(brep, names, eid)?;
                    Some(serde_json::json!({
                        "type": "Named",
                        "face_a": r.face_a.to_string(),
                        "face_b": r.face_b.to_string(),
                    }))
                });
                let id = edges.len() as u32;
                edges.push(EdgeRec {
                    kernel: eid,
                    entry: EdgeEntry {
                        id,
                        faces: [oa, ob],
                        a: [pa.x, pa.y, pa.z],
                        b: [pb.x, pb.y, pb.z],
                        segments,
                        planar: surface_is_plane(fa) && surface_is_plane(fb),
                        selector,
                    },
                });
            }
        }
    }
    Ok((
        Topo {
            faces,
            edges,
            face_ordinal,
        },
        brep,
    ))
}

fn solid_ref<'a>(s: *const VcadSolid) -> Option<&'a VcadSolid> {
    if s.is_null() {
        err::set_error("null solid");
        return None;
    }
    Some(unsafe { &*s })
}

/// Clone the `index`-th evaluated root part's solid out of a scene so the
/// topology / modifier entry points can work on a document part. Null when the
/// part has no solid (cache hit or mesh import) or the index is out of range.
/// Free with `vcad_solid_free`.
#[no_mangle]
pub extern "C" fn vcad_scene_part_solid(scene: *const VcadScene, index: usize) -> *mut VcadSolid {
    err::clear_error();
    if scene.is_null() {
        err::set_error("null scene");
        return ptr::null_mut();
    }
    let s: &VcadScene = unsafe { &*scene };
    match s.inner.parts.get(index).and_then(|p| p.solid.clone()) {
        Some(inner) => Box::into_raw(Box::new(VcadSolid { inner })),
        None => {
            err::set_error(format!("part {index} has no solid"));
            ptr::null_mut()
        }
    }
}

/// Faces and edges of a solid as JSON:
/// `{"faces":[{id,surface,center,normal,area,selector}],
///   "edges":[{id,faces:[a,b],a,b,segments:[f32×6…],planar,selector}]}`.
/// Null for a mesh-only solid. Free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_topology_json(solid: *const VcadSolid) -> *mut c_char {
    err::clear_error();
    let Some(s) = solid_ref(solid) else {
        return ptr::null_mut();
    };
    guard_ptr("topology", || match analyze(&s.inner) {
        Ok((t, _)) => json_out(&TopologyReport {
            faces: t.faces,
            edges: t.edges.into_iter().map(|e| e.entry).collect(),
        }),
        Err(e) => {
            err::set_error(format!("topology: {e}"));
            ptr::null_mut()
        }
    })
}

/// Result of [`vcad_solid_pick`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VcadPick {
    /// 1 on a hit.
    pub hit: u8,
    /// Face id, or `VCAD_TOPO_NONE`.
    pub face: u32,
    /// Nearest edge of that face within `edge_tol`, or `VCAD_TOPO_NONE`.
    pub edge: u32,
    /// Hit point (kernel coords).
    pub point: [f64; 3],
    /// Outward normal at the hit.
    pub normal: [f64; 3],
    /// Ray parameter.
    pub t: f64,
    /// Distance from the hit point to `edge` (∞ when none).
    pub edge_distance: f64,
}

const MISS: VcadPick = VcadPick {
    hit: 0,
    face: VCAD_TOPO_NONE,
    edge: VCAD_TOPO_NONE,
    point: [0.0; 3],
    normal: [0.0; 3],
    t: 0.0,
    edge_distance: f64::INFINITY,
};

fn seg_dist(p: Vec3, a: Vec3, b: Vec3) -> f64 {
    let ab = b - a;
    let l2 = ab.norm_squared();
    let t = if l2 > 0.0 {
        ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + ab * t - p).norm()
}

/// Ray-pick a face — and an edge, when the hit lands within `edge_tol` mm of
/// one of that face's edges. `origin`/`dir` are 3 doubles in kernel coords.
/// Builds a BVH per call, fine for click-rate picking (hover should use the
/// tessellation's face ids instead).
#[no_mangle]
pub extern "C" fn vcad_solid_pick(
    solid: *const VcadSolid,
    origin: *const f64,
    dir: *const f64,
    edge_tol: f64,
) -> VcadPick {
    err::clear_error();
    let Some(s) = solid_ref(solid) else {
        return MISS;
    };
    if origin.is_null() || dir.is_null() {
        return MISS;
    }
    let o = unsafe { std::slice::from_raw_parts(origin, 3) };
    let d = unsafe { std::slice::from_raw_parts(dir, 3) };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Ok((topo, brep)) = analyze(&s.inner) else {
            return MISS;
        };
        let bvh = Bvh::build_brep(brep);
        let ray = Ray::new(Point3::new(o[0], o[1], o[2]), Vec3::new(d[0], d[1], d[2]));
        let Some(h) = bvh.trace_closest(&ray) else {
            return MISS;
        };
        let face = bvh
            .geometry()
            .face_id(h.prim)
            .and_then(|f| topo.face_ordinal.get(&f).copied())
            .unwrap_or(VCAD_TOPO_NONE);
        let p = Vec3::new(h.point.x, h.point.y, h.point.z);
        let mut best = (VCAD_TOPO_NONE, f64::INFINITY);
        for e in topo.edges.iter().filter(|e| e.entry.faces.contains(&face)) {
            for s in e.entry.segments.as_chunks::<6>().0 {
                let a = Vec3::new(s[0] as f64, s[1] as f64, s[2] as f64);
                let b = Vec3::new(s[3] as f64, s[4] as f64, s[5] as f64);
                let dd = seg_dist(p, a, b);
                if dd < best.1 {
                    best = (e.entry.id, dd);
                }
            }
        }
        let (edge, edge_distance) = if best.1 <= edge_tol {
            best
        } else {
            (VCAD_TOPO_NONE, best.1)
        };
        VcadPick {
            hit: 1,
            face,
            edge,
            point: [h.point.x, h.point.y, h.point.z],
            normal: [h.normal.x, h.normal.y, h.normal.z],
            t: h.t,
            edge_distance,
        }
    }))
    .unwrap_or(MISS)
}

fn p3(a: [f64; 3]) -> Point3 {
    Point3::new(a[0], a[1], a[2])
}

fn ids<'a>(p: *const u32, n: usize) -> &'a [u32] {
    if p.is_null() || n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(p, n) }
    }
}

/// Blend the chosen edges one at a time. `shape` 1 = fillet, 0 = chamfer.
fn blend_edges(solid: &Solid, edge_ids: &[u32], size: f64, shape: f64) -> Result<Solid, String> {
    if edge_ids.is_empty() {
        return Err("no edges selected".into());
    }
    if size.is_nan() || size <= 0.0 {
        return Err(format!("size must be > 0 (got {size})"));
    }
    let (topo, _) = analyze(solid)?;
    // Capture every endpoint pair against the ORIGINAL topology first: ids
    // are renumbered by the first blend, endpoints of untouched edges are not.
    let mut targets = Vec::with_capacity(edge_ids.len());
    for &id in edge_ids {
        let e = topo
            .edges
            .get(id as usize)
            .ok_or_else(|| format!("no edge {id} (solid has {})", topo.edges.len()))?;
        if !e.entry.planar {
            return Err(format!(
                "edge {id} borders a curved face; per-edge blends support plane-plane edges only"
            ));
        }
        let _ = e.kernel;
        targets.push((p3(e.entry.a), p3(e.entry.b)));
    }
    let keys = [BlendKey {
        t: 0.0,
        section: BlendSection { size, shape },
    }];
    let mut cur = solid.clone();
    for (i, (a, b)) in targets.into_iter().enumerate() {
        cur = cur
            .edge_blend(&EdgeQuery::Endpoints { a, b }, &keys)
            .map_err(|e| format!("edge {}: {e}", edge_ids[i]))?;
    }
    Ok(cur)
}

/// Fillet the chosen edges (ids from [`vcad_topology_json`]). Fail-closed:
/// null + `vcad_last_error` if any edge can't be blended — never a partially
/// filleted solid. Edges sharing a vertex with an earlier pick are skipped by
/// the kernel (miter corners pending), which reports as an error.
#[no_mangle]
pub extern "C" fn vcad_solid_fillet_edges(
    solid: *const VcadSolid,
    edge_ids: *const u32,
    count: usize,
    radius: f64,
) -> *mut VcadSolid {
    err::clear_error();
    let Some(s) = solid_ref(solid) else {
        return ptr::null_mut();
    };
    let picked = ids(edge_ids, count);
    guard_ptr("fillet edges", || {
        match blend_edges(&s.inner, picked, radius, 1.0) {
            Ok(inner) => Box::into_raw(Box::new(VcadSolid { inner })),
            Err(e) => {
                err::set_error(format!("fillet edges: {e}"));
                ptr::null_mut()
            }
        }
    })
}

/// Chamfer the chosen edges by `distance`; same contract as
/// [`vcad_solid_fillet_edges`].
#[no_mangle]
pub extern "C" fn vcad_solid_chamfer_edges(
    solid: *const VcadSolid,
    edge_ids: *const u32,
    count: usize,
    distance: f64,
) -> *mut VcadSolid {
    err::clear_error();
    let Some(s) = solid_ref(solid) else {
        return ptr::null_mut();
    };
    let picked = ids(edge_ids, count);
    guard_ptr("chamfer edges", || {
        match blend_edges(&s.inner, picked, distance, 0.0) {
            Ok(inner) => Box::into_raw(Box::new(VcadSolid { inner })),
            Err(e) => {
                err::set_error(format!("chamfer edges: {e}"));
                ptr::null_mut()
            }
        }
    })
}

/// Planar face → prism that removes it from an inward shell of `thickness`.
fn face_cutter(
    brep: &BRepSolid,
    fid: FaceId,
    normal: Vec3,
    thickness: f64,
) -> Result<Solid, String> {
    let topo = &brep.topology;
    let verts: Vec<Point3> = topo
        .loop_vertices(topo.faces[fid].outer_loop)
        .into_iter()
        .map(|v| topo.vertices[v].point)
        .collect();
    if verts.len() < 3 {
        return Err("face outer loop has < 3 vertices (curved boundary)".into());
    }
    let n = normal.normalize();
    let x = (verts[1] - verts[0]).normalize();
    let y = n.cross(x);
    let eps = (thickness * 0.01).max(1e-3);
    let origin = verts[0] + n * eps;
    let to2 = |p: &Point3| {
        let d = p - verts[0];
        Point2::new(d.dot(x), d.dot(y))
    };
    let raw: Vec<Point2> = verts.iter().map(to2).collect();
    // Inset the footprint by the wall thickness so the cutter removes only
    // the face's interior, not the tops of the side walls. Miter offset:
    // exact for the straight-edged faces this path accepts (very acute
    // corners would over-inset; the caller's boolean then fails closed).
    let k = raw.len();
    let area2: f64 = (0..k)
        .map(|i| {
            let (a, b) = (raw[i], raw[(i + 1) % k]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let sign = if area2 >= 0.0 { 1.0 } else { -1.0 };
    let inward = |a: Point2, b: Point2| {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let l = (dx * dx + dy * dy).sqrt().max(1e-12);
        (-dy / l * sign, dx / l * sign)
    };
    let pts: Vec<Point2> = (0..k)
        .map(|i| {
            let (prev, cur, next) = (raw[(i + k - 1) % k], raw[i], raw[(i + 1) % k]);
            let (n1, n2) = (inward(prev, cur), inward(cur, next));
            let (bx, by) = (n1.0 + n2.0, n1.1 + n2.1);
            let dotn = (bx * n1.0 + by * n1.1).max(1e-6);
            let s = thickness / dotn;
            Point2::new(cur.x + bx * s, cur.y + by * s)
        })
        .collect();
    let segs: Vec<SketchSegment> = (0..pts.len())
        .map(|i| SketchSegment::Line {
            start: pts[i],
            end: pts[(i + 1) % pts.len()],
        })
        .collect();
    let profile =
        SketchProfile::new(origin, x, y, segs).map_err(|e| format!("cutter profile: {e}"))?;
    Solid::extrude(profile, -n * (thickness + 2.0 * eps))
        .map_err(|e| format!("cutter extrude: {e}"))
}

/// Shell (hollow inward by `thickness`) with the chosen planar faces removed —
/// the "open the top of the box" shell. With no faces it is a closed hollow.
/// Faces must be planar with straight edges (an outer loop of ≥ 3 vertices);
/// anything else fails closed with an error.
#[no_mangle]
pub extern "C" fn vcad_solid_shell_open(
    solid: *const VcadSolid,
    thickness: f64,
    face_ids: *const u32,
    count: usize,
) -> *mut VcadSolid {
    err::clear_error();
    let Some(s) = solid_ref(solid) else {
        return ptr::null_mut();
    };
    let removed = ids(face_ids, count);
    guard_ptr("shell", || {
        let run = || -> Result<Solid, String> {
            if thickness.is_nan() || thickness <= 0.0 {
                return Err(format!("thickness must be > 0 (got {thickness})"));
            }
            let (topo, brep) = analyze(&s.inner)?;
            let inv: HashMap<u32, FaceId> =
                topo.face_ordinal.iter().map(|(k, v)| (*v, *k)).collect();
            let mut cutters = Vec::new();
            for &f in removed {
                let fid = *inv.get(&f).ok_or_else(|| format!("no face {f}"))?;
                let entry = &topo.faces[f as usize];
                if entry.surface != "Plane" {
                    return Err(format!(
                        "face {f} is {} — only planar faces can be opened",
                        entry.surface
                    ));
                }
                cutters.push(face_cutter(
                    brep,
                    fid,
                    Vec3::new(entry.normal[0], entry.normal[1], entry.normal[2]),
                    thickness,
                )?);
            }
            let mut out = s
                .inner
                .shell(thickness)
                .map_err(|e| format!("shell: {e}"))?;
            for c in cutters {
                out = out
                    .try_difference(&c)
                    .map_err(|e| format!("open face: {e}"))?;
            }
            Ok(out)
        };
        match run() {
            Ok(inner) => Box::into_raw(Box::new(VcadSolid { inner })),
            Err(e) => {
                err::set_error(format!("shell open: {e}"));
                ptr::null_mut()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;
    use crate::{vcad_solid_cube, vcad_solid_free};

    fn report(s: *const VcadSolid) -> serde_json::Value {
        serde_json::from_str(&take(vcad_topology_json(s))).unwrap()
    }

    #[test]
    fn cube_topology_has_6_faces_12_edges() {
        let s = vcad_solid_cube(10.0, 10.0, 10.0);
        let r = report(s);
        assert_eq!(r["faces"].as_array().unwrap().len(), 6);
        let edges = r["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 12);
        for e in edges {
            assert!(e["planar"].as_bool().unwrap());
            assert!(!e["segments"].as_array().unwrap().is_empty(), "{e}");
        }
        vcad_solid_free(s);
    }

    #[test]
    fn pick_top_face_and_its_edge() {
        let s = vcad_solid_cube(10.0, 10.0, 10.0);
        // Straight down onto the middle of the top face.
        let p = vcad_solid_pick(s, [5.0, 5.0, 50.0].as_ptr(), [0.0, 0.0, -1.0].as_ptr(), 0.5);
        assert_eq!(p.hit, 1);
        assert_ne!(p.face, VCAD_TOPO_NONE);
        assert!(
            (p.point[2] - 10.0).abs() < 1e-6 && p.normal[2] > 0.99,
            "{p:?}"
        );
        assert_eq!(p.edge, VCAD_TOPO_NONE);
        let r = report(s);
        let face = &r["faces"][p.face as usize];
        assert!(face["normal"][2].as_f64().unwrap() > 0.99, "{face}");

        // Near the y=0 rim of the top face → an edge of that face.
        let p = vcad_solid_pick(s, [5.0, 0.2, 50.0].as_ptr(), [0.0, 0.0, -1.0].as_ptr(), 0.5);
        assert_ne!(p.edge, VCAD_TOPO_NONE, "{p:?}");
        let e = &r["edges"][p.edge as usize];
        assert!(e["faces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_u64() == Some(p.face as u64)));
        vcad_solid_free(s);
    }

    #[test]
    fn fillet_a_picked_edge() {
        let s = vcad_solid_cube(10.0, 10.0, 10.0);
        let p = vcad_solid_pick(s, [5.0, 0.2, 50.0].as_ptr(), [0.0, 0.0, -1.0].as_ptr(), 0.5);
        assert_ne!(p.edge, VCAD_TOPO_NONE);
        let f = vcad_solid_fillet_edges(s, &p.edge, 1, 2.0);
        assert!(!f.is_null(), "{:?}", crate::err::last_error_string());
        let v = unsafe { &*f }.inner.volume();
        // One r=2 fillet along a 10 mm edge removes (4 − π)·10 ≈ 8.58 mm³.
        let expect = 1000.0 - (4.0 - std::f64::consts::PI) * 10.0;
        assert!((v - expect).abs() < 1.0, "volume {v} vs {expect}");
        let c = vcad_solid_chamfer_edges(s, &p.edge, 1, 2.0);
        assert!(!c.is_null(), "{:?}", crate::err::last_error_string());
        let vc = unsafe { &*c }.inner.volume();
        assert!((vc - (1000.0 - 20.0)).abs() < 1.0, "chamfer volume {vc}");
        // Bad id fails closed.
        assert!(vcad_solid_fillet_edges(s, &999, 1, 2.0).is_null());
        vcad_solid_free(f);
        vcad_solid_free(c);
        vcad_solid_free(s);
    }

    #[test]
    fn shell_with_top_face_removed() {
        let s = vcad_solid_cube(20.0, 20.0, 20.0);
        let p = vcad_solid_pick(
            s,
            [10.0, 10.0, 50.0].as_ptr(),
            [0.0, 0.0, -1.0].as_ptr(),
            0.0,
        );
        let open = vcad_solid_shell_open(s, 2.0, &p.face, 1);
        assert!(!open.is_null(), "{:?}", crate::err::last_error_string());
        let v = unsafe { &*open }.inner.volume();
        // Open-top box: 20³ − 16·16·18 = 3392.
        assert!((v - 3392.0).abs() < 5.0, "volume {v}");
        // A ray down the middle now passes the missing top and lands on the floor.
        let hit = vcad_solid_pick(
            open,
            [10.0, 10.0, 50.0].as_ptr(),
            [0.0, 0.0, -1.0].as_ptr(),
            0.0,
        );
        assert!((hit.point[2] - 2.0).abs() < 1e-3, "{hit:?}");
        vcad_solid_free(open);
        vcad_solid_free(s);
    }
}
