//! One tessellation + triangle BVH per solid, shared by the CNC checks.
//!
//! The older samplers probe each face at the midpoint of its surface's
//! parameter *domain*. For a trimmed face that point need not lie on the
//! face at all (every face on one plane got the same "midpoint"), and they
//! traced against trimmed analytic faces, which costs a point-in-face test
//! per candidate and dominated the runtime on boolean-heavy parts.
//!
//! [`Probe`] tessellates the solid once, keeps every triangle tagged with
//! the kernel face it came from, and answers ray queries against a flat
//! triangle BVH. Samples are triangle centroids — points that really are on
//! the face — weighted by triangle area, so a face is judged by how much of
//! it passes a test rather than by one arbitrary point.

use std::collections::{HashMap, HashSet};

use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_tessellate::{tessellate_brep, FACE_ID_UNKNOWN};

/// One sample point on a face.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Kernel face index (`topology.faces` iteration order).
    pub face: usize,
    /// Point on the face (triangle centroid).
    pub point: Point3,
    /// Outward unit normal (triangle geometric normal).
    pub normal: Vec3,
    /// Area this sample stands for, mm².
    pub area: f64,
    /// Triangle index the sample came from.
    pub tri: u32,
}

/// A ray hit against the probe mesh.
#[derive(Debug, Clone, Copy)]
pub struct Hit {
    /// Distance along the (unit) ray direction.
    pub t: f64,
    /// Triangle index.
    pub tri: u32,
}

#[derive(Debug, Clone, Copy)]
struct Node {
    lo: [f64; 3],
    hi: [f64; 3],
    /// Leaf: first triangle slot in `order`; inner: left child (right = left + 1).
    start: u32,
    /// Leaf: triangle count; inner: 0.
    count: u32,
}

/// Tessellated solid with face provenance and a triangle BVH.
#[derive(Debug, Clone)]
pub struct Probe {
    /// Triangle corners.
    pub tris: Vec<[Point3; 3]>,
    /// Outward unit normal per triangle.
    pub normals: Vec<Vec3>,
    /// Area per triangle, mm².
    pub areas: Vec<f64>,
    /// Kernel face index per triangle (`usize::MAX` when unknown).
    pub tri_face: Vec<usize>,
    /// Triangle indices per kernel face index.
    pub face_tris: Vec<Vec<u32>>,
    /// Kernel face index → kernel face indices sharing an edge with it.
    pub adjacency: Vec<HashSet<usize>>,
    /// Largest bbox extent, mm (scale for epsilons).
    pub scale: f64,
    nodes: Vec<Node>,
    order: Vec<u32>,
}

/// Circle segment count for the probe tessellation. Matches the viewport's
/// default so a probe sample and a picked triangle agree.
const SEGMENTS: u32 = 32;

impl Probe {
    /// Tessellate `brep` and build the BVH.
    pub fn new(brep: &BRepSolid) -> Self {
        let mesh = tessellate_brep(brep, SEGMENTS);
        // Shell ordinal → kernel face index.
        let topo = &brep.topology;
        let index_of: HashMap<_, usize> = topo
            .faces
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id, i))
            .collect();
        let shell_faces = &topo.shells[topo.solids[brep.solid_id].outer_shell].faces;
        let ordinal_to_index: Vec<usize> = shell_faces
            .iter()
            .map(|f| index_of.get(f).copied().unwrap_or(usize::MAX))
            .collect();

        let n_faces = topo.faces.len();
        let v = |i: u32| {
            let i = i as usize * 3;
            Point3::new(
                mesh.vertices[i] as f64,
                mesh.vertices[i + 1] as f64,
                mesh.vertices[i + 2] as f64,
            )
        };
        let n_tris = mesh.indices.len() / 3;
        let mut tris = Vec::with_capacity(n_tris);
        let mut normals = Vec::with_capacity(n_tris);
        let mut areas = Vec::with_capacity(n_tris);
        let mut tri_face = Vec::with_capacity(n_tris);
        let mut face_tris = vec![Vec::new(); n_faces];
        for t in 0..n_tris {
            let (a, b, c) = (
                v(mesh.indices[t * 3]),
                v(mesh.indices[t * 3 + 1]),
                v(mesh.indices[t * 3 + 2]),
            );
            let cr = (b - a).cross(c - a);
            let len = cr.norm();
            if len.is_nan() || len <= 1e-12 {
                continue; // degenerate: no normal, no area
            }
            let face = match mesh.face_ids.get(t) {
                Some(&o) if o != FACE_ID_UNKNOWN => ordinal_to_index
                    .get(o as usize)
                    .copied()
                    .unwrap_or(usize::MAX),
                _ => usize::MAX,
            };
            let idx = tris.len() as u32;
            tris.push([a, b, c]);
            normals.push(cr / len);
            areas.push(0.5 * len);
            tri_face.push(face);
            if face < n_faces {
                face_tris[face].push(idx);
            }
        }

        // Outward winding is what every downstream test assumes. The
        // tessellator emits it, but a reversed shell would silently invert
        // reachability and thickness — flip once if the signed volume says so.
        let signed_volume: f64 = tris
            .iter()
            .map(|[a, b, c]: &[Point3; 3]| {
                Vec3::new(a.x, a.y, a.z)
                    .dot(Vec3::new(b.x, b.y, b.z).cross(Vec3::new(c.x, c.y, c.z)))
                    / 6.0
            })
            .sum();
        if signed_volume < 0.0 {
            for n in &mut normals {
                *n = -*n;
            }
        }

        // Face adjacency from the mesh itself: two faces are neighbours when
        // their triangles share an edge (welded at 1 µm). Works the same for
        // analytic B-reps and for triangulated ones whose topology carries
        // no useful twins.
        let q = |p: &Point3| {
            (
                (p.x * 1e3).round() as i64,
                (p.y * 1e3).round() as i64,
                (p.z * 1e3).round() as i64,
            )
        };
        let mut edge_faces: HashMap<_, usize> = HashMap::with_capacity(tris.len() * 2);
        let mut adjacency = vec![HashSet::new(); n_faces];
        for (t, tri) in tris.iter().enumerate() {
            let f = tri_face[t];
            if f >= n_faces {
                continue;
            }
            for k in 0..3 {
                let (u, w) = (q(&tri[k]), q(&tri[(k + 1) % 3]));
                if u == w {
                    continue;
                }
                let key = if u < w { (u, w) } else { (w, u) };
                match edge_faces.get(&key) {
                    Some(&g) if g != f => {
                        adjacency[f].insert(g);
                        adjacency[g].insert(f);
                    }
                    Some(_) => {}
                    None => {
                        edge_faces.insert(key, f);
                    }
                }
            }
        }

        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for t in &tris {
            for p in t {
                for (k, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                    lo[k] = lo[k].min(c);
                    hi[k] = hi[k].max(c);
                }
            }
        }
        let scale = (0..3)
            .map(|k| hi[k] - lo[k])
            .fold(0.0_f64, f64::max)
            .max(1e-6);

        let mut probe = Self {
            tris,
            normals,
            areas,
            tri_face,
            face_tris,
            adjacency,
            scale,
            nodes: Vec::new(),
            order: Vec::new(),
        };
        probe.build_bvh();
        probe
    }

    /// Whether the probe has any triangles at all.
    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// Area of a face's triangles, mm².
    pub fn face_area(&self, face: usize) -> f64 {
        self.face_tris
            .get(face)
            .map(|ts| ts.iter().map(|&t| self.areas[t as usize]).sum())
            .unwrap_or(0.0)
    }

    /// Area-weighted centroid of a face's triangles — a point near (usually
    /// on) the face, unlike the surface-domain midpoint.
    pub fn face_centroid(&self, face: usize) -> Option<Point3> {
        let ts = self.face_tris.get(face)?;
        let (mut acc, mut w) = (Vec3::new(0.0, 0.0, 0.0), 0.0);
        for &t in ts {
            let c = self.centroid(t);
            let a = self.areas[t as usize];
            acc += Vec3::new(c.x, c.y, c.z) * a;
            w += a;
        }
        (w > 0.0).then(|| Point3::new(acc.x / w, acc.y / w, acc.z / w))
    }

    fn centroid(&self, t: u32) -> Point3 {
        let [a, b, c] = self.tris[t as usize];
        Point3::new(
            (a.x + b.x + c.x) / 3.0,
            (a.y + b.y + c.y) / 3.0,
            (a.z + b.z + c.z) / 3.0,
        )
    }

    /// Up to `max` samples on `face`, area-weighted. When the face has more
    /// triangles than `max`, the largest triangles are kept and their areas
    /// rescaled so the samples still sum to the face area.
    pub fn samples(&self, face: usize, max: usize) -> Vec<Sample> {
        let Some(ts) = self.face_tris.get(face) else {
            return Vec::new();
        };
        let mut picked: Vec<u32> = ts.clone();
        let total: f64 = picked.iter().map(|&t| self.areas[t as usize]).sum();
        if picked.len() > max {
            picked.sort_by(|&a, &b| self.areas[b as usize].total_cmp(&self.areas[a as usize]));
            picked.truncate(max);
        }
        let kept: f64 = picked.iter().map(|&t| self.areas[t as usize]).sum();
        let k = if kept > 0.0 { total / kept } else { 1.0 };
        picked
            .into_iter()
            .map(|t| Sample {
                face,
                point: self.centroid(t),
                normal: self.normals[t as usize],
                area: self.areas[t as usize] * k,
                tri: t,
            })
            .collect()
    }

    /// Closest hit with `t` in `(t_min, t_max)` along unit `dir`.
    pub fn first_hit(&self, origin: Point3, dir: Vec3, t_min: f64, t_max: f64) -> Option<Hit> {
        if self.nodes.is_empty() {
            return None;
        }
        let o = [origin.x, origin.y, origin.z];
        let d = [dir.x, dir.y, dir.z];
        let inv = d.map(|c| 1.0 / c);
        let mut best: Option<Hit> = None;
        let mut limit = t_max;
        let mut stack: Vec<u32> = Vec::with_capacity(64);
        stack.push(0);
        while let Some(ni) = stack.pop() {
            let n = self.nodes[ni as usize];
            if !slab(&n.lo, &n.hi, &o, &inv, t_min, limit) {
                continue;
            }
            if n.count > 0 {
                for &t in &self.order[n.start as usize..(n.start + n.count) as usize] {
                    if let Some(tt) = self.intersect(t, origin, dir) {
                        if tt > t_min && tt < limit {
                            limit = tt;
                            best = Some(Hit { t: tt, tri: t });
                        }
                    }
                }
            } else {
                stack.push(n.start);
                stack.push(n.start + 1);
            }
        }
        best
    }

    /// Whether a sample faces into the material — an inside-out (folded)
    /// triangle left by a boolean. Casting outward from a correct face goes
    /// into air, so the first surface met is one the ray *enters*; from an
    /// inverted face it is already inside, so the first surface met is one
    /// it *exits* (its outward normal points along the ray).
    pub fn inverted(&self, s: &Sample, eps: f64) -> bool {
        let o = s.point + s.normal * eps;
        match self.first_hit(o, s.normal, 0.0, f64::INFINITY) {
            Some(h) => self.normals[h.tri as usize].dot(s.normal) > 0.0,
            None => false,
        }
    }

    /// Whether anything is hit along `dir` beyond `t_min`.
    pub fn occluded(&self, origin: Point3, dir: Vec3, t_min: f64) -> bool {
        self.first_hit(origin, dir, t_min, f64::INFINITY).is_some()
    }

    /// Möller–Trumbore, two-sided.
    fn intersect(&self, t: u32, o: Point3, d: Vec3) -> Option<f64> {
        let [a, b, c] = self.tris[t as usize];
        let e1 = b - a;
        let e2 = c - a;
        let p = d.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-14 {
            return None;
        }
        let inv = 1.0 / det;
        let s = o - a;
        let u = s.dot(p) * inv;
        if !(-1e-9..=1.0 + 1e-9).contains(&u) {
            return None;
        }
        let q = s.cross(e1);
        let v = d.dot(q) * inv;
        if v < -1e-9 || u + v > 1.0 + 1e-9 {
            return None;
        }
        Some(e2.dot(q) * inv)
    }

    fn build_bvh(&mut self) {
        let n = self.tris.len();
        if n == 0 {
            return;
        }
        let cent: Vec<[f64; 3]> = (0..n as u32)
            .map(|t| {
                let c = self.centroid(t);
                [c.x, c.y, c.z]
            })
            .collect();
        let bounds: Vec<([f64; 3], [f64; 3])> = self
            .tris
            .iter()
            .map(|t| {
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for p in t {
                    for (k, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                        lo[k] = lo[k].min(c);
                        hi[k] = hi[k].max(c);
                    }
                }
                (lo, hi)
            })
            .collect();
        self.order = (0..n as u32).collect();
        self.nodes = Vec::with_capacity(2 * n / 4 + 1);
        self.nodes.push(Node {
            lo: [0.0; 3],
            hi: [0.0; 3],
            start: 0,
            count: n as u32,
        });
        let mut work = vec![(0usize, 0usize, n)];
        while let Some((ni, start, end)) = work.pop() {
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            let (mut clo, mut chi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for &t in &self.order[start..end] {
                let (blo, bhi) = bounds[t as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(blo[k]);
                    hi[k] = hi[k].max(bhi[k]);
                    clo[k] = clo[k].min(cent[t as usize][k]);
                    chi[k] = chi[k].max(cent[t as usize][k]);
                }
            }
            self.nodes[ni].lo = lo;
            self.nodes[ni].hi = hi;
            let count = end - start;
            let axis = (0..3)
                .max_by(|&a, &b| (chi[a] - clo[a]).total_cmp(&(chi[b] - clo[b])))
                .unwrap_or(0);
            if count <= 4 || chi[axis] - clo[axis] <= 1e-12 {
                self.nodes[ni].start = start as u32;
                self.nodes[ni].count = count as u32;
                continue;
            }
            let mid = start + count / 2;
            self.order[start..end].select_nth_unstable_by(mid - start, |&a, &b| {
                cent[a as usize][axis].total_cmp(&cent[b as usize][axis])
            });
            let left = self.nodes.len();
            let blank = Node {
                lo: [0.0; 3],
                hi: [0.0; 3],
                start: 0,
                count: 0,
            };
            self.nodes.push(blank);
            self.nodes.push(blank);
            self.nodes[ni].start = left as u32;
            self.nodes[ni].count = 0;
            work.push((left, start, mid));
            work.push((left + 1, mid, end));
        }
    }
}

/// Faces grouped into the features a person would point at: one region per
/// analytic surface, and runs of near-coplanar planar faces (a triangulated
/// bore's facets, a top face a boolean split in two) merged across shallow
/// edges. Issues are reported per region, not per face.
#[derive(Debug, Clone)]
pub struct Regions {
    /// Kernel face index → region id (`usize::MAX` for faces with no
    /// triangles).
    pub of_face: Vec<usize>,
    /// Region id → member kernel face indices, ascending.
    pub faces: Vec<Vec<usize>>,
    /// Region id → total area, mm².
    pub area: Vec<f64>,
    /// Region id → neighbouring region ids.
    pub adjacency: Vec<HashSet<usize>>,
    /// Faces too small to judge on their own (sliver / folded boolean
    /// debris). They join a neighbour's region but never seed samples.
    pub sliver: Vec<bool>,
}

impl Regions {
    /// Group `probe`'s faces. `smooth_deg` is the largest dihedral angle
    /// merged between planar faces; `sliver_area` (mm²) marks faces too
    /// small to stand alone.
    pub fn new(probe: &Probe, brep: &BRepSolid, smooth_deg: f64, sliver_area: f64) -> Self {
        use vcad_kernel_geom::SurfaceKind;
        let n = probe.face_tris.len();
        let surf: Vec<(usize, bool)> = brep
            .topology
            .faces
            .iter()
            .map(|(_, f)| {
                let plane = brep
                    .geometry
                    .surfaces
                    .get(f.surface_index)
                    .is_some_and(|s| s.surface_type() == SurfaceKind::Plane);
                (f.surface_index, plane)
            })
            .collect();
        let area: Vec<f64> = (0..n).map(|f| probe.face_area(f)).collect();
        let normal: Vec<Vec3> = (0..n)
            .map(|f| {
                let mut acc = Vec3::new(0.0, 0.0, 0.0);
                for &t in &probe.face_tris[f] {
                    acc += probe.normals[t as usize] * probe.areas[t as usize];
                }
                let l = acc.norm();
                if l > 0.0 {
                    acc / l
                } else {
                    acc
                }
            })
            .collect();
        let sliver: Vec<bool> = (0..n).map(|f| area[f] < sliver_area).collect();
        let cos_smooth = smooth_deg.to_radians().cos();

        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut a: usize) -> usize {
            while p[a] != a {
                p[a] = p[p[a]];
                a = p[a];
            }
            a
        }
        for f in 0..n {
            for &g in &probe.adjacency[f] {
                if g <= f || sliver[f] || sliver[g] {
                    continue;
                }
                let same_surface = surf.get(f).map(|s| s.0) == surf.get(g).map(|s| s.0);
                let smooth_planes = surf.get(f).is_some_and(|s| s.1)
                    && surf.get(g).is_some_and(|s| s.1)
                    && normal[f].dot(normal[g]) >= cos_smooth;
                if same_surface || smooth_planes {
                    let (a, b) = (find(&mut parent, f), find(&mut parent, g));
                    if a != b {
                        parent[a.max(b)] = a.min(b);
                    }
                }
            }
        }
        // Slivers join their largest non-sliver neighbour (by region area
        // is overkill — face area is a fine proxy and order-independent).
        for f in 0..n {
            if !sliver[f] {
                continue;
            }
            if let Some(&g) = probe.adjacency[f]
                .iter()
                .filter(|&&g| !sliver[g])
                .max_by(|&&a, &&b| area[a].total_cmp(&area[b]).then(b.cmp(&a)))
            {
                let (a, b) = (find(&mut parent, f), find(&mut parent, g));
                if a != b {
                    parent[a.max(b)] = a.min(b);
                }
            }
        }

        let mut of_face = vec![usize::MAX; n];
        let mut id_of_root: HashMap<usize, usize> = HashMap::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        let mut rarea: Vec<f64> = Vec::new();
        for f in 0..n {
            if probe.face_tris[f].is_empty() {
                continue;
            }
            let r = find(&mut parent, f);
            let id = *id_of_root.entry(r).or_insert_with(|| {
                faces.push(Vec::new());
                rarea.push(0.0);
                faces.len() - 1
            });
            of_face[f] = id;
            faces[id].push(f);
            rarea[id] += area[f];
        }
        let mut adjacency = vec![HashSet::new(); faces.len()];
        for f in 0..n {
            let rf = of_face[f];
            if rf == usize::MAX {
                continue;
            }
            for &g in &probe.adjacency[f] {
                let rg = of_face[g];
                if rg != usize::MAX && rg != rf {
                    adjacency[rf].insert(rg);
                }
            }
        }
        Self {
            of_face,
            faces,
            area: rarea,
            adjacency,
            sliver,
        }
    }

    /// Number of regions.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// Whether there are no regions.
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Region of a kernel face, if it has one.
    pub fn region(&self, face: usize) -> Option<usize> {
        self.of_face.get(face).copied().filter(|&r| r != usize::MAX)
    }
}

#[inline]
fn slab(lo: &[f64; 3], hi: &[f64; 3], o: &[f64; 3], inv: &[f64; 3], t0: f64, t1: f64) -> bool {
    let (mut tmin, mut tmax) = (t0, t1);
    for k in 0..3 {
        let (mut a, mut b) = ((lo[k] - o[k]) * inv[k], (hi[k] - o[k]) * inv[k]);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        // NaN (0 · ∞, ray parallel and on the slab plane) → treat as inside.
        if a.is_nan() || b.is_nan() {
            if o[k] < lo[k] - 1e-9 || o[k] > hi[k] + 1e-9 {
                return false;
            }
            continue;
        }
        tmin = tmin.max(a);
        tmax = tmax.min(b);
        if tmin > tmax {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_kernel_primitives::make_cube;

    #[test]
    fn cube_probe_hits_the_far_wall() {
        let p = Probe::new(&make_cube(10.0, 20.0, 30.0));
        assert!(!p.is_empty());
        let h = p
            .first_hit(
                Point3::new(5.0, 10.0, 15.0),
                Vec3::new(0.0, 0.0, 1.0),
                0.0,
                1e9,
            )
            .expect("hit");
        assert!((h.t - 15.0).abs() < 1e-4, "{}", h.t);
        // Every face has samples whose normals point outward.
        for f in 0..p.face_tris.len() {
            let c = p.face_centroid(f).unwrap();
            for s in p.samples(f, 8) {
                let out = Vec3::new(c.x - 5.0, c.y - 10.0, c.z - 15.0);
                assert!(s.normal.dot(out) > 0.0);
            }
            assert_eq!(p.adjacency[f].len(), 4);
        }
        assert!(
            (p.face_area(0)
                + p.face_area(1)
                + p.face_area(2)
                + p.face_area(3)
                + p.face_area(4)
                + p.face_area(5)
                - 2.0 * (200.0 + 300.0 + 600.0))
                .abs()
                < 1e-3
        );
    }
}
