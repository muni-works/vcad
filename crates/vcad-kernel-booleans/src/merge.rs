//! Merge edge-adjacent faces that lie on one plane.
//!
//! A boolean leaves a surface in pieces: a cap is split along the line where
//! another tool's plane would run on through it, a section face is cut where
//! a wall crossed it, a lid is scored by a cavity's extended cylinder. The
//! pieces are separate topological faces with nothing changing across the
//! edges between them. This pass removes those edges, so the B-rep has one
//! face per connected region of a plane.
//!
//! # What is merged
//!
//! Faces of one shell that
//!
//! * lie on the same plane with the same outward normal,
//! * wind the same way about that normal (so the survivor's surface record
//!   and orientation flag describe every piece), and
//! * share at least one edge, as a pair of opposite half-edges between the
//!   same two vertices.
//!
//! The merged face keeps the surface, orientation and shell position of its
//! first member; its loops are re-traced from the half-edges that did not
//! cancel, so a region that closes around a hole gets an inner loop and one
//! that swallows a bridge edge loses it. Vertices are left where they are,
//! including ones now collinear on the merged boundary: the faces across
//! that boundary still end edges on them.
//!
//! # What is refused
//!
//! The pass is structural and fails closed, group by group. A group is left
//! split when
//!
//! * a directed edge occurs twice among its faces (non-manifold),
//! * a cancelling half-edge is the true `twin` of a half-edge that stays
//!   (removing it would unpair a neighbour; a stale twin link between two
//!   unrelated half-edges is dropped instead),
//! * the merged boundary would pass through a vertex more than once (a
//!   pinch, or a T-junction seam whose two sides were split differently and
//!   so did not cancel),
//! * the re-traced loops are not exactly one outer boundary plus holes.
//!
//! Whether the merged solid still *tessellates* as well as the split one is
//! a separate question, answered by [`merged_if_no_worse`].
//!
//! # Where it runs
//!
//! On the solid that leaves document evaluation for a root (`vcad-eval`,
//! `finish_root`) — the one that is meshed, picked and exported — and on
//! nothing that a later boolean consumes. Both earlier placements were
//! measured. Merging after every kernel boolean (in `Solid::try_boolean`)
//! passes the torture track but leaves the open-fusor chamber with 78 faces
//! and 8 non-manifold edges: the later cuts of the chain meet merged caps
//! where the splitters were tuned on split ones. Merging each boolean
//! node's result keeps the chamber clean (92 faces → 59) but turns the
//! shell-ring document's later mesh-fallback union non-manifold. Merged at
//! the root only, the chamber is the same 59 faces and no boolean ever sees
//! a merged face unless a caller asks for it.
//!
//! # Why only planes
//!
//! Cylinder walls were tried with the same pass (same axis, radius and
//! orientation, edges cancelled the same way) and are left split. A wall
//! merged back around a cross bore becomes a band with holes, which the
//! band-strip tessellator does not take; on the chamber the trimmed path
//! then lost 14% of the volume with 550 open edges, and the best case
//! doubled the triangle count (7 544 → 16 066) with the volume off. Walls
//! merged only along rulings (a sectioned can, 40 faces → 8) do mesh
//! cleanly, but the next boolean on them degrades and they no longer
//! round-trip through STEP. Until the wall tessellator and the band parser
//! take a multi-loop wall, the outline grouping
//! (`Solid::smooth_face_groups`) is what hides those seams.

use std::collections::{BTreeMap, HashMap, HashSet};

use vcad_kernel_geom::Plane;
use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_tessellate::{tessellate_brep, TriangleMesh};
use vcad_kernel_topo::{FaceId, HalfEdgeId, LoopId, ShellId, Topology, VertexId};

/// Two planes are the same when their unit normals differ by less than this
/// (as `1 − cos θ`; 1e-12 is ~1.4 µrad, 1.4 µm over a metre) …
const NORMAL_TOL: f64 = 1e-12;
/// … and their signed offsets from the origin by less than this (mm).
const OFFSET_TOL: f64 = 1e-6;

/// What [`merge_coplanar_faces`] did.
#[derive(Debug, Clone, Default)]
pub struct MergeReport {
    /// For every face that was absorbed, the face it became part of.
    pub absorbed: Vec<(FaceId, FaceId)>,
    /// Number of merged faces produced (each from two or more inputs).
    pub merged_faces: usize,
    /// Edge-connected groups that were left split by a structural guard.
    pub refused_groups: usize,
}

impl MergeReport {
    /// Whether the topology was changed at all.
    pub fn changed(&self) -> bool {
        !self.absorbed.is_empty()
    }
}

/// A face that qualifies for merging, with what the grouping needs.
struct Candidate {
    face: FaceId,
    /// Outward unit normal.
    normal: Vec3,
    /// Signed offset of the plane along `normal`.
    offset: f64,
    /// Whether the outer loop runs counter-clockwise about `normal`.
    ccw: bool,
}

fn loop_points(topo: &Topology, lp: LoopId) -> Option<Vec<Point3>> {
    let mut out = Vec::new();
    for he in topo.loop_half_edges(lp) {
        out.push(topo.vertices.get(topo.half_edges.get(he)?.origin)?.point);
    }
    Some(out)
}

/// Twice the signed area of a closed polyline about `normal`.
fn signed_area2(points: &[Point3], normal: Vec3) -> f64 {
    let Some(&o) = points.first() else {
        return 0.0;
    };
    let mut acc = Vec3::zeros();
    for w in points.windows(2) {
        acc += (w[0] - o).cross(w[1] - o);
    }
    acc.dot(normal)
}

/// Every half-edge of every loop of `face`, with its destination vertex.
/// `None` when a loop is not a closed ring of live half-edges.
fn face_half_edges(topo: &Topology, face: FaceId) -> Option<Vec<(HalfEdgeId, VertexId, VertexId)>> {
    let f = topo.faces.get(face)?;
    let mut out = Vec::new();
    for lp in std::iter::once(f.outer_loop).chain(f.inner_loops.iter().copied()) {
        let start = topo.loops.get(lp)?.half_edge;
        let mut he = start;
        let mut steps = 0usize;
        loop {
            let h = topo.half_edges.get(he)?;
            if h.loop_id != Some(lp) {
                return None;
            }
            let next = h.next?;
            let dest = topo.half_edges.get(next)?.origin;
            out.push((he, h.origin, dest));
            he = next;
            steps += 1;
            if he == start {
                break;
            }
            if steps > topo.half_edges.len() {
                return None;
            }
        }
    }
    Some(out)
}

fn candidate(brep: &BRepSolid, face: FaceId) -> Option<Candidate> {
    let topo = &brep.topology;
    let f = topo.faces.get(face)?;
    let plane = brep
        .geometry
        .surfaces
        .get(f.surface_index)?
        .as_any()
        .downcast_ref::<Plane>()?;
    let stored: Vec3 = plane.normal_dir.into_inner();
    let normal = match f.orientation {
        vcad_kernel_topo::Orientation::Forward => stored,
        vcad_kernel_topo::Orientation::Reversed => -stored,
    };
    let offset = normal.dot(plane.origin.to_vec());

    let outer = loop_points(topo, f.outer_loop)?;
    if outer.len() < 3 {
        return None;
    }
    let a_outer = signed_area2(&outer, normal);
    if a_outer.abs() < 1e-12 {
        return None;
    }
    let ccw = a_outer > 0.0;
    // Holes must run the other way: only then does every half-edge of the
    // face keep the material on the same side, which is what lets a shared
    // edge be recognised as a pair of opposite half-edges.
    for &inner in &f.inner_loops {
        let pts = loop_points(topo, inner)?;
        if pts.len() < 3 {
            return None;
        }
        let a = signed_area2(&pts, normal);
        if a.abs() < 1e-12 || (a > 0.0) == ccw {
            return None;
        }
    }
    // The loops must actually lie on the plane the surface record names.
    for p in &outer {
        if (normal.dot(p.to_vec()) - offset).abs() > 1e-5 {
            return None;
        }
    }
    Some(Candidate {
        face,
        normal,
        offset,
        ccw,
    })
}

/// The plan for one group: which half-edges go, and the loops that remain.
struct GroupPlan {
    members: Vec<FaceId>,
    cancelled: Vec<HalfEdgeId>,
    outer: Vec<HalfEdgeId>,
    inners: Vec<Vec<HalfEdgeId>>,
}

fn plan_group(
    topo: &Topology,
    members: &[FaceId],
    normal: Vec3,
    ccw: bool,
    hes: &HashMap<FaceId, Vec<(HalfEdgeId, VertexId, VertexId)>>,
) -> Option<GroupPlan> {
    let refuse = |why: u32| -> Option<GroupPlan> {
        if std::env::var_os("VCAD_MERGE_DEBUG").is_some() {
            let p = topo.vertices[hes[&members[0]][0].1].point;
            eprintln!(
                "vcad merge: refused group of {} faces (guard {why}) normal ({:.3},{:.3},{:.3}) near ({:.2},{:.2},{:.2})",
                members.len(), normal.x, normal.y, normal.z, p.x, p.y, p.z
            );
        }
        None
    };
    let mut by_dir: HashMap<(VertexId, VertexId), HalfEdgeId> = HashMap::new();
    let mut all: Vec<(HalfEdgeId, VertexId, VertexId)> = Vec::new();
    for f in members {
        for &(he, u, v) in &hes[f] {
            if u == v || by_dir.insert((u, v), he).is_some() {
                return refuse(1);
            }
            all.push((he, u, v));
        }
    }
    let cancelled: HashSet<HalfEdgeId> = all
        .iter()
        .filter(|(_, u, v)| by_dir.contains_key(&(*v, *u)))
        .map(|(he, _, _)| *he)
        .collect();
    // Removing a half-edge whose twin stays would unpair a neighbour — when
    // the twin really is the other side of that edge. Twin pointers are not
    // all that good: an edge split on one side only leaves a half-edge
    // paired with one somewhere else entirely (the chamber floor has a
    // radial edge "twinned" with a rim chord 30 mm away). Such a link pairs
    // nothing, and is dropped along with the half-edge.
    let ends: HashMap<HalfEdgeId, (VertexId, VertexId)> =
        all.iter().map(|&(he, u, v)| (he, (u, v))).collect();
    for &(he, u, v) in &all {
        let Some(twin) = topo.half_edges[he].twin else {
            continue;
        };
        if cancelled.contains(&he) == cancelled.contains(&twin) {
            continue;
        }
        let twin_ends = match ends.get(&twin) {
            Some(&e) => Some(e),
            None => topo
                .half_edges
                .get(twin)
                .and_then(|t| Some((t.origin, topo.half_edges.get(t.next?)?.origin))),
        };
        if twin_ends.is_some_and(|(a, b)| (a, b) == (v, u) || (a, b) == (u, v)) {
            return refuse(2);
        }
    }
    // What is left is the merged boundary. It must leave every vertex it
    // touches exactly once: a second exit is a pinch, or a seam whose two
    // sides were subdivided differently and therefore did not cancel.
    let mut exit: HashMap<VertexId, HalfEdgeId> = HashMap::new();
    let mut dest: HashMap<HalfEdgeId, VertexId> = HashMap::new();
    let mut kept: Vec<HalfEdgeId> = Vec::new();
    for &(he, u, v) in &all {
        if cancelled.contains(&he) {
            continue;
        }
        if exit.insert(u, he).is_some() {
            return refuse(3);
        }
        dest.insert(he, v);
        kept.push(he);
    }
    if kept.is_empty() {
        return refuse(4);
    }
    kept.sort_unstable();
    let mut seen: HashSet<HalfEdgeId> = HashSet::new();
    let mut outer: Option<Vec<HalfEdgeId>> = None;
    let mut inners: Vec<Vec<HalfEdgeId>> = Vec::new();
    for &start in &kept {
        if seen.contains(&start) {
            continue;
        }
        let mut ring = Vec::new();
        let mut he = start;
        loop {
            if !seen.insert(he) {
                return refuse(5);
            }
            ring.push(he);
            he = *exit.get(&dest[&he])?;
            if he == start {
                break;
            }
        }
        if ring.len() < 3 {
            return refuse(6);
        }
        let pts: Vec<Point3> = ring
            .iter()
            .map(|h| topo.vertices[topo.half_edges[*h].origin].point)
            .collect();
        let a = signed_area2(&pts, normal);
        if a.abs() < 1e-12 {
            return refuse(7);
        }
        if (a > 0.0) == ccw {
            if outer.replace(ring).is_some() {
                return refuse(8);
            }
        } else {
            inners.push(ring);
        }
    }
    let mut cancelled: Vec<HalfEdgeId> = cancelled.into_iter().collect();
    cancelled.sort_unstable();
    Some(GroupPlan {
        members: members.to_vec(),
        cancelled,
        outer: outer?,
        inners,
    })
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Merge edge-adjacent coplanar faces of every shell of `brep`, in place.
///
/// Purely topological; see the module docs for what is merged and what is
/// refused. Deterministic: groups are processed in shell order and every
/// removal happens in sorted key order.
pub fn merge_coplanar_faces(brep: &mut BRepSolid) -> MergeReport {
    let mut report = MergeReport::default();
    let shells: Vec<ShellId> = brep.topology.shells.keys().collect();
    let mut plans: Vec<GroupPlan> = Vec::new();

    for shell in shells {
        let faces = brep.topology.shells[shell].faces.clone();
        let mut cands: Vec<Candidate> = Vec::new();
        let mut hes: HashMap<FaceId, Vec<(HalfEdgeId, VertexId, VertexId)>> = HashMap::new();
        for &face in &faces {
            let Some(c) = candidate(brep, face) else {
                continue;
            };
            let Some(h) = face_half_edges(&brep.topology, face) else {
                continue;
            };
            hes.insert(face, h);
            cands.push(c);
        }

        // Plane classes: (outward plane, winding). Linear scan against
        // class representatives — a solid has few distinct planes.
        let mut class_of: Vec<usize> = Vec::with_capacity(cands.len());
        let mut reps: Vec<usize> = Vec::new();
        for (i, c) in cands.iter().enumerate() {
            let hit = reps.iter().position(|&r| {
                let o = &cands[r];
                o.ccw == c.ccw
                    && 1.0 - o.normal.dot(c.normal) < NORMAL_TOL
                    && (o.offset - c.offset).abs() < OFFSET_TOL
            });
            class_of.push(match hit {
                Some(k) => k,
                None => {
                    reps.push(i);
                    reps.len() - 1
                }
            });
        }

        // Edge adjacency inside a class: opposite half-edges between the
        // same two vertices.
        let mut parent: Vec<usize> = (0..cands.len()).collect();
        let mut owner: HashMap<(usize, VertexId, VertexId), usize> = HashMap::new();
        for (i, c) in cands.iter().enumerate() {
            for &(_, u, v) in &hes[&c.face] {
                owner.insert((class_of[i], u, v), i);
            }
        }
        for (i, c) in cands.iter().enumerate() {
            for &(_, u, v) in &hes[&c.face] {
                if let Some(&j) = owner.get(&(class_of[i], v, u)) {
                    if j != i {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                        if a != b {
                            parent[a.max(b)] = a.min(b);
                        }
                    }
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..cands.len() {
            let root = find(&mut parent, i);
            groups.entry(root).or_default().push(i);
        }
        for (_, idx) in groups {
            if idx.len() < 2 {
                continue;
            }
            // `cands` is in shell order, so `idx[0]` is the first member.
            let members: Vec<FaceId> = idx.iter().map(|&i| cands[i].face).collect();
            let lead = &cands[idx[0]];
            match plan_group(&brep.topology, &members, lead.normal, lead.ccw, &hes) {
                Some(plan) => plans.push(plan),
                None => report.refused_groups += 1,
            }
        }
    }

    if plans.is_empty() {
        return report;
    }

    let topo = &mut brep.topology;
    let mut dead_faces: Vec<FaceId> = Vec::new();
    let mut dead_loops: Vec<LoopId> = Vec::new();
    let mut dead_hes: Vec<HalfEdgeId> = Vec::new();
    for plan in &plans {
        let survivor = plan.members[0];
        for &f in &plan.members {
            let face = &topo.faces[f];
            dead_loops.push(face.outer_loop);
            dead_loops.extend(face.inner_loops.iter().copied());
        }
        for &f in &plan.members[1..] {
            dead_faces.push(f);
            report.absorbed.push((f, survivor));
        }
        dead_hes.extend(plan.cancelled.iter().copied());
        report.merged_faces += 1;
    }
    dead_loops.sort_unstable();
    for lp in dead_loops {
        topo.loops.remove(lp);
    }
    for plan in &plans {
        let survivor = plan.members[0];
        let outer = topo.add_loop(&plan.outer);
        topo.loops[outer].face = Some(survivor);
        topo.faces[survivor].outer_loop = outer;
        topo.faces[survivor].inner_loops.clear();
        for ring in &plan.inners {
            let inner = topo.add_loop(ring);
            topo.add_inner_loop(survivor, inner);
        }
    }

    // Drop the cancelled half-edges and the edge records that paired them.
    dead_hes.sort_unstable();
    let dead_he_set: HashSet<HalfEdgeId> = dead_hes.iter().copied().collect();
    let mut dead_edges: Vec<_> = dead_hes
        .iter()
        .filter_map(|he| topo.half_edges[*he].edge)
        .collect();
    dead_edges.sort_unstable();
    dead_edges.dedup();
    let mut touched: Vec<VertexId> = dead_hes
        .iter()
        .map(|he| topo.half_edges[*he].origin)
        .collect();
    touched.sort_unstable();
    touched.dedup();
    for he in &dead_hes {
        // A surviving half-edge that named a removed one as its twin (a
        // stale pairing, see `plan_group`) is left unpaired.
        let (twin, edge) = (topo.half_edges[*he].twin, topo.half_edges[*he].edge);
        if let Some(t) = twin.filter(|t| !dead_he_set.contains(t)) {
            if let Some(th) = topo.half_edges.get_mut(t) {
                if th.twin == Some(*he) {
                    th.twin = None;
                }
                if th.edge == edge {
                    th.edge = None;
                }
            }
        }
    }
    // An edge record whose two half-edges both stay but which named a
    // removed half-edge through a stale link is covered above; any other
    // survivor still pointing at a removed edge is cleared here.
    let dead_edge_set: HashSet<_> = dead_edges.iter().copied().collect();
    for e in dead_edges {
        topo.edges.remove(e);
    }
    for he in &dead_hes {
        topo.half_edges.remove(*he);
    }
    for (_, h) in &mut topo.half_edges {
        if h.edge.is_some_and(|e| dead_edge_set.contains(&e)) {
            h.edge = None;
        }
        if h.twin.is_some_and(|t| dead_he_set.contains(&t)) {
            h.twin = None;
        }
    }

    // Vertices that lost half-edges: re-point the ones still in use, drop
    // the ones now interior to a merged face.
    let needs_fix: HashSet<VertexId> = touched
        .iter()
        .copied()
        .filter(|v| {
            topo.vertices[*v]
                .half_edge
                .is_none_or(|he| dead_he_set.contains(&he))
        })
        .collect();
    if !needs_fix.is_empty() {
        let mut first_out: HashMap<VertexId, HalfEdgeId> = HashMap::new();
        for (he, h) in &topo.half_edges {
            if needs_fix.contains(&h.origin) {
                let slot = first_out.entry(h.origin).or_insert(he);
                if he < *slot {
                    *slot = he;
                }
            }
        }
        let mut fix: Vec<VertexId> = needs_fix.into_iter().collect();
        fix.sort_unstable();
        for v in fix {
            match first_out.get(&v) {
                Some(&he) => topo.vertices[v].half_edge = Some(he),
                None => {
                    topo.vertices.remove(v);
                }
            }
        }
    }

    // Faces, and their slots in the shells (survivors keep their order, so
    // shell ordinals of untouched faces only shift down).
    dead_faces.sort_unstable();
    let dead_face_set: HashSet<FaceId> = dead_faces.iter().copied().collect();
    for (_, shell) in &mut topo.shells {
        shell.faces.retain(|f| !dead_face_set.contains(f));
    }
    for f in dead_faces {
        topo.faces.remove(f);
    }
    report
}

/// Structural quality of a tessellation, as compared by
/// [`merged_if_no_worse`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshQuality {
    /// Net unpaired directed edges (welded at 1e-5).
    pub open_edges: usize,
    /// Edges used by more than two triangles.
    pub overused_edges: usize,
    /// Edges whose two triangles face nearly opposite ways.
    pub folded_edges: usize,
    /// Signed volume.
    pub volume: f64,
    /// Triangle count.
    pub triangles: usize,
}

/// Measure `mesh` for [`merged_if_no_worse`].
pub fn mesh_quality(mesh: &TriangleMesh) -> MeshQuality {
    let r = crate::mesh_report(mesh);
    MeshQuality {
        open_edges: r.open_edges,
        overused_edges: r.overused_edges,
        folded_edges: folded_edges(mesh),
        volume: r.signed_volume,
        triangles: r.triangles,
    }
}

/// Edges shared by exactly two triangles whose normals are within ~25° of
/// opposite: a sheet doubled back on itself.
fn folded_edges(mesh: &TriangleMesh) -> usize {
    let p = |i: u32| {
        let i = i as usize * 3;
        Vec3::new(
            mesh.vertices[i] as f64,
            mesh.vertices[i + 1] as f64,
            mesh.vertices[i + 2] as f64,
        )
    };
    let key = |i: u32| {
        let q = p(i);
        [
            (q.x * 1e5).round() as i64,
            (q.y * 1e5).round() as i64,
            (q.z * 1e5).round() as i64,
        ]
    };
    type EdgeKey = ([i64; 3], [i64; 3]);
    let mut adj: HashMap<EdgeKey, (usize, [Vec3; 2])> = HashMap::new();
    for t in mesh.indices.chunks(3) {
        let n = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
        let l = n.norm();
        let n = if l > 0.0 { n / l } else { Vec3::zeros() };
        let k = [key(t[0]), key(t[1]), key(t[2])];
        for i in 0..3 {
            let (a, b) = (k[i], k[(i + 1) % 3]);
            if a == b {
                continue;
            }
            let e = adj
                .entry(if a < b { (a, b) } else { (b, a) })
                .or_insert((0, [Vec3::zeros(); 2]));
            if e.0 < 2 {
                e.1[e.0] = n;
            }
            e.0 += 1;
        }
    }
    adj.values()
        .filter(|(n, ns)| *n == 2 && ns[0].dot(ns[1]) < -0.9)
        .count()
}

/// [`merge_coplanar_faces`] on a copy of `brep`, kept only if the merged
/// solid tessellates at least as well as the split one.
///
/// "At least as well" is measured on the two full tessellations at
/// `segments`: no more open, over-used or folded edges, and the same volume
/// to one part in 10⁶. Merging changes which planar triangulator a face
/// goes through (a cap that gains a hole) and what the curved faces next to
/// it see across their rims, so the structural guards in the merge are not
/// enough on their own.
///
/// Returns `None` when nothing merged, when `brep` is triangle soup (whose
/// thousands of coplanar triangles must stay recognisable as soup), or when
/// the merged solid came out worse.
pub fn merged_if_no_worse(brep: &BRepSolid, segments: u32) -> Option<(BRepSolid, MergeReport)> {
    if crate::is_triangle_soup(brep) {
        return None;
    }
    let mut merged = brep.clone();
    let report = merge_coplanar_faces(&mut merged);
    if !report.changed() {
        return None;
    }
    let before = mesh_quality(&tessellate_brep(brep, segments));
    let after = mesh_quality(&tessellate_brep(&merged, segments));
    let same_volume = (after.volume - before.volume).abs() <= 1e-6 * before.volume.abs().max(1e-9);
    let ok = after.triangles > 0
        && after.open_edges <= before.open_edges
        && after.overused_edges <= before.overused_edges
        && after.folded_edges <= before.folded_edges
        && same_volume;
    if std::env::var_os("VCAD_MERGE_DEBUG").is_some() {
        eprintln!(
            "vcad merge: {} faces absorbed into {}, {} groups refused; before {before:?} after {after:?} -> {}",
            report.absorbed.len(),
            report.merged_faces,
            report.refused_groups,
            if ok { "kept" } else { "DROPPED" }
        );
    }
    if !ok && std::env::var_os("VCAD_MERGE_DEBUG").is_some() {
        let params = vcad_kernel_tessellate::TessellationParams::from_segments(segments);
        let survivors: HashSet<FaceId> = report.absorbed.iter().map(|(_, s)| *s).collect();
        for (fid, _, m) in vcad_kernel_tessellate::tessellate_brep_by_face(&merged, &params) {
            if !survivors.contains(&fid) {
                continue;
            }
            let face = &merged.topology.faces[fid];
            let lens: Vec<usize> = std::iter::once(face.outer_loop)
                .chain(face.inner_loops.iter().copied())
                .map(|l| merged.topology.loop_len(l))
                .collect();
            let p = |i: u32| {
                let i = i as usize * 3;
                Vec3::new(
                    m.vertices[i] as f64,
                    m.vertices[i + 1] as f64,
                    m.vertices[i + 2] as f64,
                )
            };
            let (mut area, mut degenerate) = (0.0, 0);
            for t in m.indices.chunks(3) {
                let a = 0.5 * (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).norm();
                if a < 1e-9 {
                    degenerate += 1;
                }
                area += a;
            }
            let q = mesh_quality(&m);
            eprintln!(
                "  merged {fid:?}: loops {lens:?} tris {} degenerate {degenerate} area {area:.4} open {} over {} folded {}",
                m.indices.len() / 3,
                q.open_edges,
                q.overused_edges,
                q.folded_edges
            );
        }
    }
    ok.then_some((merged, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{boolean_op, BooleanOp};
    use vcad_kernel_math::Transform;
    use vcad_kernel_primitives::{make_cube, make_cylinder};

    fn moved(mut brep: BRepSolid, t: &Transform) -> BRepSolid {
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

    /// Every key a live entity names must be live; loops must be closed
    /// rings owned by a face that lists them.
    fn assert_consistent(b: &BRepSolid) {
        let t = &b.topology;
        for (he, h) in &t.half_edges {
            assert!(t.vertices.contains_key(h.origin), "{he:?}: dead origin");
            if let Some(tw) = h.twin {
                assert_eq!(t.half_edges[tw].twin, Some(he), "{he:?}: twin not mutual");
            }
            if let Some(e) = h.edge {
                assert!(t.edges.contains_key(e), "{he:?}: dead edge");
            }
            let lp = h.loop_id.expect("half-edge outside any loop");
            let face = t.loops[lp].face.expect("loop without face");
            let f = &t.faces[face];
            assert!(f.outer_loop == lp || f.inner_loops.contains(&lp));
            assert_eq!(t.half_edges[h.next.unwrap()].prev, Some(he));
        }
        for (_, e) in &t.edges {
            assert!(
                t.half_edges.contains_key(e.half_edge),
                "edge on dead half-edge"
            );
        }
        for (v, vert) in &t.vertices {
            let he = vert.half_edge.expect("vertex without half-edge");
            assert_eq!(t.half_edges[he].origin, v);
        }
        for (_, s) in &t.shells {
            for f in &s.faces {
                assert!(t.faces.contains_key(*f), "shell lists dead face");
            }
        }
        for (f, face) in &t.faces {
            for lp in std::iter::once(face.outer_loop).chain(face.inner_loops.iter().copied()) {
                assert_eq!(t.loops[lp].face, Some(f));
                assert!(t.loop_len(lp) >= 3);
            }
        }
    }

    #[test]
    fn untouched_cube_is_left_alone() {
        let mut cube = make_cube(10.0, 10.0, 10.0);
        let report = merge_coplanar_faces(&mut cube);
        assert!(!report.changed());
        assert_eq!(cube.topology.faces.len(), 6);
    }

    /// Two boxes side by side, flush on four sides: the union's top, bottom,
    /// front and back are each one rectangle again.
    #[test]
    fn flush_boxes_union_to_six_faces() {
        let a = make_cube(10.0, 10.0, 10.0);
        let b = moved(
            make_cube(10.0, 10.0, 10.0),
            &Transform::translation(10.0, 0.0, 0.0),
        );
        let u = boolean_op(&a, &b, BooleanOp::Union, 32).unwrap();
        let u = u.as_brep().unwrap();
        let (merged, _) = merged_if_no_worse(u, 32).expect("merges");
        assert_consistent(&merged);
        assert_eq!(
            merged.topology.faces.len(),
            6,
            "was {}",
            u.topology.faces.len()
        );
    }

    /// A plate with a through bore, sectioned through the bore's axis and
    /// merged, then cut again: the merged faces are valid boolean input.
    #[test]
    fn merged_solid_survives_another_boolean() {
        let plate = make_cube(40.0, 40.0, 10.0);
        let bore = moved(
            make_cylinder(8.0, 30.0, 32),
            &Transform::translation(20.0, 20.0, -10.0),
        );
        let cut = boolean_op(&plate, &bore, BooleanOp::Difference, 32).unwrap();
        let half = moved(
            make_cube(60.0, 60.0, 30.0),
            &Transform::translation(-10.0, 20.0, -10.0),
        );
        let sect = boolean_op(cut.as_brep().unwrap(), &half, BooleanOp::Difference, 32).unwrap();
        let sect = sect.as_brep().unwrap();
        let base = match merged_if_no_worse(sect, 32) {
            Some((m, _)) => {
                assert_consistent(&m);
                m
            }
            None => sect.clone(),
        };
        let slot = moved(
            make_cube(10.0, 50.0, 4.0),
            &Transform::translation(2.0, -5.0, 3.0),
        );
        let split = boolean_op(sect, &slot, BooleanOp::Difference, 32).unwrap();
        let after = boolean_op(&base, &slot, BooleanOp::Difference, 32).unwrap();
        let (q0, q1) = (
            mesh_quality(&split.to_mesh(32)),
            mesh_quality(&after.to_mesh(32)),
        );
        assert!(
            (q0.volume - q1.volume).abs() < 1e-6 * q0.volume.abs(),
            "{q0:?} vs {q1:?}"
        );
        assert_eq!(
            (q1.open_edges, q1.overused_edges, q1.folded_edges),
            (0, 0, 0)
        );
    }
}
