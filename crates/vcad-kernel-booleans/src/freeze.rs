//! Freezing of analytic circle loops into canonical polylines.
//!
//! Primitive B-reps carry *analytic* full-circle edges (a single half-edge
//! whose origin equals its destination, e.g. a cylinder cap boundary). The
//! boolean pipeline, in contrast, emits *frozen* polyline boundaries
//! (`split::canonical_arc_points`). A result that mixes the two is
//! watertight only by resolution coincidence — the analytic side is
//! re-sampled at display time with its own count and phase — and its
//! topology can never conform, which is exactly what poisons any further
//! boolean taken on the result (the "boolean-result operand" failure family
//! in the torr catalogue).
//!
//! `freeze_circle_loops` rewrites every full-circle edge of both operands
//! into the canonical polyline before the pipeline runs, so everything
//! downstream — SSI splitting, classification, sewing, tessellation — sees
//! one concrete, conforming boundary representation.

use vcad_kernel_geom::CylinderSurface;
use vcad_kernel_math::Vec3;
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_topo::HalfEdgeId;

use crate::split::canonical_arc_points;

/// Give each unpaired full-circle wall rim its planar neighbour as a twin.
///
/// A full-turn revolve leaves its wall rims unpaired, and the flat rings
/// between walls carry a one-vertex outer loop and a sampled inner hole
/// through the same seam vertex. Pair them the way `make_cylinder` pairs a
/// wall with its cap, so the freeze below writes one chain to both sides.
fn pair_cylinder_rims_with_planes(brep: &mut BRepSolid) {
    let shell = brep.topology.solids[brep.solid_id].outer_shell;
    let faces = brep.topology.shells[shell].faces.clone();
    let is_kind = |b: &BRepSolid, f: vcad_kernel_topo::FaceId, cyl: bool| {
        let any = b.geometry.surfaces[b.topology.faces[f].surface_index].as_any();
        if cyl {
            any.is::<CylinderSurface>()
        } else {
            any.is::<vcad_kernel_geom::Plane>()
        }
    };
    let mut rims = Vec::new();
    for &f in faces.iter().filter(|&&f| is_kind(brep, f, true)) {
        let lp = brep.topology.faces[f].outer_loop;
        if brep.topology.loop_len(lp) > 4 {
            continue;
        }
        for h in brep.topology.loop_half_edges(lp).collect::<Vec<_>>() {
            let he = &brep.topology.half_edges[h];
            let closed = he.next.map(|n| brep.topology.half_edges[n].origin) == Some(he.origin);
            if closed && he.twin.is_none() {
                rims.push(h);
            }
        }
    }
    for rim in rims {
        let v = brep.topology.half_edges[rim].origin;
        for &f in faces.iter().filter(|&&f| is_kind(brep, f, false)) {
            let outer = brep.topology.faces[f].outer_loop;
            let first = brep.topology.loop_half_edges(outer).next();
            let single = brep.topology.loop_len(outer) == 1;
            if let Some(h) = first.filter(|&h| {
                single
                    && brep.topology.half_edges[h].origin == v
                    && brep.topology.half_edges[h].twin.is_none()
            }) {
                brep.topology.add_edge(rim, h);
                break;
            }
            let holes = brep.topology.faces[f].inner_loops.clone();
            let hole = holes.into_iter().find(|&l| {
                brep.topology.loop_len(l) > 4
                    && brep
                        .topology
                        .loop_half_edges(l)
                        .all(|h| brep.topology.half_edges[h].twin.is_none())
                    && brep
                        .topology
                        .loop_half_edges(l)
                        .any(|h| brep.topology.half_edges[h].origin == v)
            });
            if let Some(old) = hole {
                let stale: Vec<_> = brep.topology.loop_half_edges(old).collect();
                brep.topology.faces[f].inner_loops.retain(|&l| l != old);
                for h in stale {
                    brep.topology.half_edges.remove(h);
                }
                brep.topology.loops.remove(old);
                let h = brep.topology.add_half_edge(v);
                let lp = brep.topology.add_loop(&[h]);
                brep.topology.add_inner_loop(f, lp);
                brep.topology.add_edge(rim, h);
                break;
            }
        }
    }
}

/// Replace analytic full-circle edges with canonical polyline chains.
///
/// A full-circle edge is a half-edge whose origin and destination are the
/// same vertex and whose own face or twin face is a cylinder. Both the
/// half-edge and its twin (when present) are rewritten against the same
/// vertex chain, so the rebuilt edges pair exactly.
pub(crate) fn freeze_circle_loops(brep: &mut BRepSolid, segments: u32) {
    pair_cylinder_rims_with_planes(brep);
    let he_ids: Vec<HalfEdgeId> = brep.topology.half_edges.keys().collect();
    let mut done: std::collections::HashSet<HalfEdgeId> = std::collections::HashSet::new();

    for he_id in he_ids {
        if done.contains(&he_id) || !brep.topology.half_edges.contains_key(he_id) {
            continue;
        }
        let he = &brep.topology.half_edges[he_id];
        let loop_id = match he.loop_id {
            Some(l) => l,
            None => continue,
        };
        let next = match he.next {
            Some(n) => n,
            None => continue,
        };
        let origin = he.origin;
        let dest = brep.topology.half_edges[next].origin;
        if origin != dest {
            continue; // not a closed-curve edge
        }
        // origin == dest alone is ambiguous: a PINCH DUPLICATE in a dense
        // band loop (two consecutive half-edges sharing an origin, kept
        // deliberately by realize_bands) looks identical from endpoints.
        // A genuine analytic full-circle edge only occurs in the tiny
        // legacy loops (1-he cap seam loops, ≤4-he wall rectangles);
        // splicing a ring into a dense loop would thread a phantom
        // full-circle polyline through the middle of the face.
        if brep.topology.loop_len(loop_id) > 4 {
            continue;
        }
        let twin = he.twin;

        // Find the cylinder or cone that carries this circle (own face or
        // twin's). Both carry rings perpendicular to their axis; all the
        // machinery below needs is the axis direction and a point on it.
        let axis_of = |hid: HalfEdgeId| -> Option<(Vec3, vcad_kernel_math::Point3)> {
            let lp = brep.topology.half_edges[hid].loop_id?;
            let face_id = brep.topology.loops[lp].face?;
            let face = &brep.topology.faces[face_id];
            let surface = &brep.geometry.surfaces[face.surface_index];
            if let Some(cyl) = surface.as_any().downcast_ref::<CylinderSurface>() {
                return Some((*cyl.axis.as_ref(), cyl.center));
            }
            if let Some(cone) = surface
                .as_any()
                .downcast_ref::<vcad_kernel_geom::ConeSurface>()
            {
                return Some((*cone.axis.as_ref(), cone.apex));
            }
            None
        };
        let (axis, axis_pt) = match axis_of(he_id).or_else(|| twin.and_then(axis_of)) {
            Some(c) => c,
            None => continue, // sphere poles etc. — skip
        };

        let v_pt = brep.topology.vertices[origin].point;
        let d = v_pt - axis_pt;
        let center = axis_pt + axis * d.dot(axis);
        let radius = (v_pt - center).norm();
        if radius < 1e-9 {
            continue;
        }

        // Which travel direction does `he_id` take? Prefer the planar-face
        // side to decide: producers wind loops CCW about the STORED surface
        // normal (Orientation only flips the effective normal). Fall back
        // to the cylinder-wall convention: the lower circle travels +u
        // (CCW about the axis), the upper one −u.
        let ccw_about = |hid: HalfEdgeId| -> Option<Vec3> {
            let lp = brep.topology.half_edges[hid].loop_id?;
            let face_id = brep.topology.loops[lp].face?;
            let face = &brep.topology.faces[face_id];
            let surface = &brep.geometry.surfaces[face.surface_index];
            if surface
                .as_any()
                .downcast_ref::<vcad_kernel_geom::Plane>()
                .is_some()
            {
                // A hole winds against the face normal.
                let sign = if face.inner_loops.contains(&lp) {
                    -1.0
                } else {
                    1.0
                };
                Some(
                    *surface
                        .normal(vcad_kernel_math::Point2::new(0.0, 0.0))
                        .as_ref()
                        * sign,
                )
            } else {
                None
            }
        };
        // A ring on a cylinder is ALWAYS perpendicular to the axis; the
        // bordering planar face's stored normal may only choose the SIGN.
        // Using the plane normal directly tilts the ring wherever the twin
        // is an oblique face (a blade plane grazing the wall), sweeping
        // phantom vertices far outside the solid.
        let sign_of = |n: Vec3| -> Option<f64> {
            let d = n.dot(axis);
            if d.abs() > 0.1 {
                Some(d.signum())
            } else {
                None
            }
        };
        let he_normal: Vec3 = if let Some(s) = ccw_about(he_id).and_then(sign_of) {
            axis * s
        } else if let Some(s) = twin.and_then(ccw_about).and_then(sign_of) {
            axis * -s
        } else {
            // Wall-only circle (e.g. an inner bore after a difference):
            // lower ring +u, upper ring −u, matching `make_cylinder`.
            let lp = brep.topology.half_edges[he_id].loop_id;
            let is_lower = lp
                .map(|l| {
                    let mut vmin = f64::MAX;
                    for h in brep.topology.loop_half_edges(l) {
                        let p = brep.topology.vertices[brep.topology.half_edges[h].origin].point;
                        vmin = vmin.min((p - axis_pt).dot(axis));
                    }
                    (d.dot(axis) - vmin).abs() < 1e-6
                })
                .unwrap_or(true);
            if is_lower {
                axis
            } else {
                -axis
            }
        };

        // Canonical ring, CCW about `he_normal`; interior points only.
        let ring = canonical_arc_points(center, radius, he_normal, v_pt, v_pt, segments);
        let interior = &ring[1..ring.len() - 1];
        if interior.is_empty() {
            continue;
        }
        let interior_vids: Vec<_> = interior
            .iter()
            .map(|p| brep.topology.add_vertex(*p))
            .collect();

        // Rechain `he_id` through the interior vertices (forward order) and
        // the twin through them in reverse, then pair the sub-edges.
        let chain = |topo: &mut vcad_kernel_topo::Topology,
                     start_he: HalfEdgeId,
                     vids: &[vcad_kernel_topo::VertexId]|
         -> Vec<HalfEdgeId> {
            let lp = topo.half_edges[start_he].loop_id;
            let next = topo.half_edges[start_he].next.unwrap();
            let mut hes = vec![start_he];
            let mut prev = start_he;
            for &v in vids {
                let h = topo.add_half_edge(v);
                topo.half_edges[h].loop_id = lp;
                topo.half_edges[h].prev = Some(prev);
                topo.half_edges[h].next = Some(next);
                topo.half_edges[prev].next = Some(h);
                topo.half_edges[next].prev = Some(h);
                prev = h;
                hes.push(h);
            }
            hes
        };
        let hes_a = chain(&mut brep.topology, he_id, &interior_vids);
        done.insert(he_id);

        if let Some(twin_id) = twin {
            // Twin travels the ring in the opposite direction.
            let rev: Vec<_> = interior_vids.iter().rev().copied().collect();
            let hes_b = chain(&mut brep.topology, twin_id, &rev);
            done.insert(twin_id);
            // Unlink the old whole-circle edge and re-pair segmentwise:
            // hes_a[i] runs v_i → v_{i+1}; its twin is hes_b[n-1-i].
            brep.topology.half_edges[he_id].twin = None;
            brep.topology.half_edges[twin_id].twin = None;
            let n = hes_a.len();
            for i in 0..n {
                let a = hes_a[i];
                let b = hes_b[n - 1 - i];
                brep.topology.add_edge(a, b);
            }
        }
    }
}

/// Resample sampled partial-arc rims onto the canonical grid.
///
/// An arc-bearing sketch extrude stores each arc as a coarse polyline
/// (`ExtrudeOptions::arc_segments` points) shared by the cylindrical wall's
/// rim and the planar cap. The boolean's cylinder splitters rebuild a split
/// wall's rims from `canonical_arc_points`, which samples the same circle
/// far more densely — so the rebuilt rim no longer matches the cap chain it
/// was twinned with, sewing leaves both sides unpaired, and the result is
/// open (the K1 `wrist_interface.loon` tearing: a box pocket cut into an
/// extruded D's curved wall tore the wall away from both caps).
///
/// This is the partial-arc counterpart of [`freeze_circle_loops`]: a wall
/// face shaped as two constant-height rim chains joined by two rulings has
/// each rim chain (when sampled, ≥2 edges, and not already canonical)
/// rewritten — on the wall AND on its twin chain — as the canonical arc
/// between the same endpoints, so every split downstream lands on vertices
/// both sides already share.
///
/// Only walls whose bounds strictly overlap `other` (the opposite operand)
/// are touched: a wall the other solid merely touches cannot be split, and
/// a dense cap that is only coplanar-imprinted (a D standing on a flange)
/// trips the imprint path, which still expects the extrude's sparse chain.
pub(crate) fn freeze_arc_chains(brep: &mut BRepSolid, other: &BRepSolid, segments: u32) {
    let bounds = |pts: &mut dyn Iterator<Item = vcad_kernel_math::Point3>| {
        pts.fold(([f64::MAX; 3], [f64::MIN; 3]), |(lo, hi), p| {
            (
                [lo[0].min(p.x), lo[1].min(p.y), lo[2].min(p.z)],
                [hi[0].max(p.x), hi[1].max(p.y), hi[2].max(p.z)],
            )
        })
    };
    let (olo, ohi) = bounds(&mut other.topology.vertices.values().map(|v| v.point));
    let face_ids: Vec<_> = brep.topology.faces.keys().collect();
    for face_id in face_ids {
        let cyl = {
            let face = &brep.topology.faces[face_id];
            match brep.geometry.surfaces[face.surface_index]
                .as_any()
                .downcast_ref::<CylinderSurface>()
            {
                Some(c) => c.clone(),
                None => continue,
            }
        };
        let axis = *cyl.axis.as_ref();
        let lp = brep.topology.faces[face_id].outer_loop;
        let hes: Vec<HalfEdgeId> = brep.topology.loop_half_edges(lp).collect();
        if hes.len() < 6 {
            continue;
        }
        let height = |b: &BRepSolid, h: HalfEdgeId| {
            (b.topology.vertices[b.topology.half_edges[h].origin].point - cyl.center).dot(axis)
        };
        let (lo, hi) = bounds(
            &mut hes
                .iter()
                .map(|&h| brep.topology.vertices[brep.topology.half_edges[h].origin].point),
        );
        if (0..3).any(|k| hi[k].min(ohi[k]) - lo[k].max(olo[k]) <= 1e-6) {
            continue;
        }
        let hs: Vec<f64> = hes.iter().map(|&h| height(brep, h)).collect();
        let (hmin, hmax) = hs
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), &h| (a.min(h), b.max(h)));
        if hmax - hmin < 1e-6
            || hs
                .iter()
                .any(|&h| (h - hmin).abs() > 1e-6 && (h - hmax).abs() > 1e-6)
        {
            continue;
        }
        // Edge i runs hes[i] → hes[i+1]; a rim edge keeps its height.
        let n = hes.len();
        let rim = |i: usize| (hs[i] - hs[(i + 1) % n]).abs() < 1e-6;
        let rulings: Vec<usize> = (0..n).filter(|&i| !rim(i)).collect();
        if rulings.len() != 2 {
            continue;
        }
        for &r in &rulings {
            // The rim run starts right after a ruling and ends at the next.
            let start = (r + 1) % n;
            let run: Vec<HalfEdgeId> = (0..n)
                .map(|k| (start + k) % n)
                .take_while(|&i| rim(i))
                .map(|i| hes[i])
                .collect();
            if run.len() >= 2 {
                rechain_rim(brep, &cyl, &run, segments);
            }
        }
    }
}

/// Rewrite one sampled rim run (and its twin run) as the canonical arc.
fn rechain_rim(brep: &mut BRepSolid, cyl: &CylinderSurface, run: &[HalfEdgeId], segments: u32) {
    let topo = &brep.topology;
    let axis = *cyl.axis.as_ref();
    let pt = |h: HalfEdgeId| topo.vertices[topo.half_edges[h].origin].point;
    let last = *run.last().unwrap();
    let Some(after) = topo.half_edges[last].next else {
        return;
    };
    let mut pts: Vec<_> = run.iter().map(|&h| pt(h)).collect();
    pts.push(pt(after));
    // Twins must exist and form one contiguous reversed run in a single loop.
    let twins: Option<Vec<HalfEdgeId>> =
        run.iter().rev().map(|&h| topo.half_edges[h].twin).collect();
    let Some(twins) = twins else {
        return;
    };
    let twin_loop = topo.half_edges[twins[0]].loop_id;
    if twin_loop.is_none()
        || twins
            .iter()
            .any(|&t| topo.half_edges[t].loop_id != twin_loop)
        || twins
            .windows(2)
            .any(|w| topo.half_edges[w[0]].next != Some(w[1]))
    {
        return;
    }
    let Some(twin_after) = topo.half_edges[*twins.last().unwrap()].next else {
        return;
    };
    let p0 = pts[0];
    let center = cyl.center + axis * (p0 - cyl.center).dot(axis);
    let radius = (p0 - center).norm();
    // Travel sense about the axis, from the run's own first step.
    let turn = (pts[0] - center).cross(pts[1] - center).dot(axis);
    if radius < 1e-9 || turn.abs() < 1e-12 {
        return;
    }
    let normal = if turn > 0.0 { axis } else { -axis };
    let (a, b) = (pts[0], *pts.last().unwrap());
    let canon = canonical_arc_points(center, radius, normal, a, b, segments);
    // Every sampled point must lie on the circle, and the canonical arc must
    // travel the same way (its midpoint near the run's own path).
    if pts
        .iter()
        .any(|p| ((*p - center).norm() - radius).abs() > 1e-6)
    {
        return;
    }
    let already =
        canon.len() == pts.len() && canon.iter().zip(&pts).all(|(c, p)| (*c - *p).norm() < 1e-9);
    if already || canon.len() < 2 {
        return;
    }

    let topo = &mut brep.topology;
    let old_interior: Vec<_> = run[1..]
        .iter()
        .map(|&h| topo.half_edges[h].origin)
        .collect();
    // Detach the old interior half-edges on both sides.
    let removed: Vec<HalfEdgeId> = run[1..].iter().chain(&twins[1..]).copied().collect();
    let (h0, t0) = (run[0], twins[0]);
    for (head, tail) in [(h0, after), (t0, twin_after)] {
        topo.half_edges[head].next = Some(tail);
        topo.half_edges[tail].prev = Some(head);
    }
    for (_, l) in topo.loops.iter_mut() {
        if removed.contains(&l.half_edge) {
            l.half_edge = h0;
        }
    }
    if let Some(l) = topo.half_edges[t0].loop_id {
        if topo.loops[l].half_edge == h0 {
            topo.loops[l].half_edge = t0;
        }
    }
    if let Some(l) = topo.half_edges[h0].loop_id {
        topo.loops[l].half_edge = h0;
    }
    for &h in &[h0, t0] {
        if let Some(e) = topo.half_edges[h].edge.take() {
            topo.edges.remove(e);
        }
        topo.half_edges[h].twin = None;
    }
    for h in removed {
        if let Some(e) = topo.half_edges[h].edge {
            topo.edges.remove(e);
        }
        topo.half_edges.remove(h);
    }
    for v in old_interior {
        topo.vertices.remove(v);
    }

    let vids: Vec<_> = canon[1..canon.len() - 1]
        .iter()
        .map(|p| topo.add_vertex(*p))
        .collect();
    let chain = |topo: &mut vcad_kernel_topo::Topology,
                 start_he: HalfEdgeId,
                 vids: &[vcad_kernel_topo::VertexId]|
     -> Vec<HalfEdgeId> {
        let lp = topo.half_edges[start_he].loop_id;
        let next = topo.half_edges[start_he].next.unwrap();
        let mut hes = vec![start_he];
        let mut prev = start_he;
        for &v in vids {
            let h = topo.add_half_edge(v);
            topo.half_edges[h].loop_id = lp;
            topo.half_edges[h].prev = Some(prev);
            topo.half_edges[h].next = Some(next);
            topo.half_edges[prev].next = Some(h);
            topo.half_edges[next].prev = Some(h);
            prev = h;
            hes.push(h);
        }
        hes
    };
    let hes_a = chain(topo, h0, &vids);
    let rev: Vec<_> = vids.iter().rev().copied().collect();
    let hes_b = chain(topo, t0, &rev);
    let m = hes_a.len();
    for i in 0..m {
        topo.add_edge(hes_a[i], hes_b[m - 1 - i]);
    }
}
