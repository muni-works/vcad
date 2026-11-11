//! Snapping of near-coincident axis-aligned planes between operands.
//!
//! Two operands often meet on a plane both were built to share, but reach it
//! through different float arithmetic: a flange `translate -8.3` of a height-8
//! cylinder has its top at `-0.3000000000000007`, while a body translated to
//! `-0.3` has its base at `-0.3`. The pipeline's coincidence handling is
//! tuned for exactly-equal planes, and a 7e-16 offset sends the shared seam
//! down a different split/classify path that left the K1 flange ∪
//! half-cylinder union with 132 open edges. Offsets this small are float
//! noise, not geometry, so B's face is moved onto A's plane before anything
//! downstream sees it.

use vcad_kernel_geom::Plane;
use vcad_kernel_math::Point3;
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_topo::FaceId;

fn get(p: &Point3, k: usize) -> f64 {
    [p.x, p.y, p.z][k]
}

fn set(p: &mut Point3, k: usize, v: f64) {
    match k {
        0 => p.x = v,
        1 => p.y = v,
        _ => p.z = v,
    }
}

/// Relative offset below which two parallel planes are the same plane.
const SNAP_REL: f64 = 1e-9;

/// The axis `k` a plane's normal is aligned with, if any.
fn axis_of(p: &Plane) -> Option<usize> {
    let n = p.normal_dir.as_ref();
    let n = [n.x, n.y, n.z];
    (0..3).find(|&k| (n[k].abs() - 1.0).abs() < 1e-12)
}

fn planes(s: &BRepSolid) -> impl Iterator<Item = (FaceId, &Plane)> {
    s.topology.faces.iter().filter_map(|(fid, f)| {
        s.geometry.surfaces[f.surface_index]
            .as_any()
            .downcast_ref::<Plane>()
            .map(|p| (fid, p))
    })
}

/// Move every axis-aligned planar face of `b` that lies within float noise of
/// a parallel face of `a` exactly onto `a`'s plane, vertices included.
pub(crate) fn snap_coplanar_offsets(a: &BRepSolid, b: &mut BRepSolid) {
    let mut targets: [Vec<f64>; 3] = Default::default();
    for (_, p) in planes(a) {
        if let Some(k) = axis_of(p) {
            targets[k].push(get(&p.origin, k));
        }
    }
    let snaps: Vec<(FaceId, usize, f64, f64)> = planes(b)
        .filter_map(|(fid, p)| {
            let k = axis_of(p)?;
            let from = get(&p.origin, k);
            let to = *targets[k].iter().find(|&&t| {
                let d = (t - from).abs();
                d > 0.0 && d <= SNAP_REL * t.abs().max(from.abs()).max(1.0)
            })?;
            Some((fid, k, from, to))
        })
        .collect();

    for (fid, k, from, to) in snaps {
        let tol = SNAP_REL * from.abs().max(1.0);
        let face = &b.topology.faces[fid];
        let surface_index = face.surface_index;
        let loops: Vec<_> = std::iter::once(face.outer_loop)
            .chain(face.inner_loops.iter().copied())
            .collect();
        let verts: Vec<_> = loops
            .into_iter()
            .flat_map(|lp| b.topology.loop_half_edges(lp).collect::<Vec<_>>())
            .map(|he| b.topology.half_edges[he].origin)
            .collect();
        for v in verts {
            let pt = &mut b.topology.vertices[v].point;
            if (get(pt, k) - from).abs() <= tol {
                set(pt, k, to);
            }
        }
        if let Some(p) = b.geometry.surfaces[surface_index]
            .as_any()
            .downcast_ref::<Plane>()
        {
            let mut p = p.clone();
            set(&mut p.origin, k, to);
            b.geometry.surfaces[surface_index] = Box::new(p);
        }
    }
}
