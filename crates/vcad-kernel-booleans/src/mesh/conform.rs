//! Conforming mesh boolean: closed manifold operands in, closed manifold
//! result out — or `None`.
//!
//! [`super::csg`] cuts every polygon of one operand along the carrier
//! *planes* of the other's triangles and then repairs what that leaves:
//! t-junctions are stitched, hairline holes capped, doubled patches
//! cancelled. On two rods that share an end sphere, or a rod through a wire
//! ring, the repair cannot keep up — the result carries hundreds of
//! non-manifold and folded edges and several times the operands' triangle
//! count, and a chain of such unions compounds (the open-fusor cathode grid:
//! 150 000 triangles from operands totalling 17 000).
//!
//! This module computes the arrangement instead:
//!
//! 1. **General position.** Operand B is translated by a few nanometres in
//!    a fixed generic direction. CAD soup is full of exact
//!    coincidence — a mirror-symmetric part puts both operands' edges in one
//!    plane, two rods share an end sphere facet for facet — and every such
//!    tie would need its own case. After the nudge exact predicates never
//!    tie: each triangle pair either misses or crosses in one segment.
//!    Shared surface becomes two sheets a hair apart, and the predicates
//!    decide, consistently everywhere, which one is outside.
//! 2. **Exact crossings.** Each triangle pair is intersected with exact
//!    predicates on the (nudged) input vertices only. Every new point is one
//!    edge of one operand piercing one triangle of the other, created once
//!    and keyed by that (edge, triangle) — both triangles on the edge and
//!    the triangle pierced all receive the same vertex id, so nothing is
//!    ever welded by distance.
//! 3. **Constrained retriangulation.** A crossed triangle is re-cut along
//!    exactly its intersection segments; untouched triangles pass through
//!    whole. Output size is the kept input plus the intersection curve.
//! 4. **Patch classification.** The intersection curve partitions each
//!    operand's surface into patches wholly inside or wholly outside the
//!    other; one exact ray-parity query classifies each.
//! 5. **Snap back.** Input vertices are written out at their original
//!    positions, so the nudge leaves no trace except hair-thin slivers
//!    where shared surface was resolved; intersection points all but on a
//!    vertex are collapsed into it and zero-height triangles flipped away.
//!
//! The result is checked — every directed edge matched by exactly one
//! opposite — and anything else is `None`, as is non-manifold input or a
//! facet crossed by thousands of others. The caller then falls back to the
//! BSP boolean.

use std::collections::{HashMap, HashSet, VecDeque};

use vcad_kernel_math::predicates::{incircle, orient2d, orient3d, Sign};
use vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel_tessellate::TriangleMesh;

use crate::api::BooleanOp;
use crate::mesh::MeshRayIndex;

type V = u32;
type Tri = [V; 3];

/// Edges of the result shorter than this (mm) that end on an intersection
/// point are collapsed into their other end: the point is snapped onto the
/// vertex it all but coincides with. A quarter of a micron — far below any
/// tessellation's own chord error, and wide enough that no two result
/// vertices land in one 0.1 µm weld cell of a downstream consumer.
const MIN_EDGE: f64 = 2.5e-4;

/// Triangles of the result flatter than this (mm, apex to opposite edge)
/// have no usable normal in f32 coordinates; their long edge is flipped.
const MIN_HEIGHT: f64 = 1e-4;

/// Sweeps of the shape-improving flip pass over one re-cut triangle.
const IMPROVE_SWEEPS: usize = 6;

/// Intersection points one input triangle may carry before the arrangement
/// is declined: the retriangulation's edge lookups are linear scans, fine
/// for the handful of points a crossed facet gets and quadratic beyond.
const MAX_POINTS_PER_TRIANGLE: usize = 2048;

/// The nudge applied to operand B, as a fraction of the operands' joint
/// bounding-box diagonal (capped at a tenth of [`MIN_EDGE`]): 3 nm on a
/// 30 mm part. Large enough that the
/// slivers it opens between shared surfaces are many orders above f64
/// rounding (the retriangulation works on computed coordinates), small
/// enough to vanish in the result — far below any tessellation's chord
/// error and below [`MIN_EDGE`], so the slivers collapse.
const NUDGE: f64 = 1e-7;

/// Its direction: nothing a modelled face normal or edge is aligned with.
const NUDGE_DIR: [f64; 3] = [0.539_104_917, 0.741_682_093, 0.398_927_351];

fn sign(s: Sign) -> i8 {
    match s {
        Sign::Negative => -1,
        Sign::Zero => 0,
        Sign::Positive => 1,
    }
}

fn s3(a: &Point3, b: &Point3, c: &Point3, d: &Point3) -> i8 {
    sign(orient3d(a, b, c, d))
}

/// `VCAD_CONFORM_DEBUG=1`: say on stderr why an arrangement was declined.
fn note<T>(what: &str, v: Option<T>) -> Option<T> {
    if v.is_none() && std::env::var_os("VCAD_CONFORM_DEBUG").is_some() {
        eprintln!("conforming boolean declined: {what}");
    }
    v
}

fn edge_key(a: V, b: V) -> (V, V) {
    (a.min(b), a.max(b))
}

/// Is `tris` a closed, consistently oriented 2-manifold at the index level:
/// every directed edge present once, with its reverse?
fn oriented_closed(tris: &[Tri]) -> bool {
    let mut seen: HashSet<(V, V)> = HashSet::with_capacity(tris.len() * 3);
    for t in tris {
        for k in 0..3 {
            if !seen.insert((t[k], t[(k + 1) % 3])) {
                return false;
            }
        }
    }
    seen.iter().all(|&(a, b)| seen.contains(&(b, a)))
}

struct Arrangement {
    pos: Vec<Point3>,
    /// For an intersection point: its parameter along the edge it sits on,
    /// measured from the edge's lower-id end.
    param: Vec<f64>,
    tris: [Vec<Tri>; 2],
    /// Intersection points on each operand's edges.
    edge_pts: [HashMap<(V, V), Vec<V>>; 2],
    /// Intersection segments inside each operand's triangles.
    cons: [HashMap<u32, Vec<(V, V)>>; 2],
    /// (operand owning the edge, edge, triangle of the other operand).
    pierce: HashMap<(u8, V, V, u32), V>,
}

impl Arrangement {
    /// Does the segment `p q` (already known to straddle the triangle's
    /// plane) pass through the triangle's interior? `None` when its line
    /// meets one of the triangle's edges or vertices.
    fn pierces(&self, p: V, q: V, t: &Tri) -> Option<bool> {
        let (pp, pq) = (&self.pos[p as usize], &self.pos[q as usize]);
        let c = t.map(|v| self.pos[v as usize]);
        let o = [
            s3(pp, pq, &c[0], &c[1]),
            s3(pp, pq, &c[1], &c[2]),
            s3(pp, pq, &c[2], &c[0]),
        ];
        if o.contains(&0) {
            None
        } else {
            Some(o == [1; 3] || o == [-1; 3])
        }
    }

    /// The vertex where edge `p q` of operand `m` pierces triangle `tri` of
    /// the other operand.
    fn pierce_vertex(&mut self, m: usize, p: V, q: V, tri: u32) -> V {
        let (lo, hi) = edge_key(p, q);
        if let Some(&v) = self.pierce.get(&(m as u8, lo, hi, tri)) {
            return v;
        }
        let t = self.tris[1 - m][tri as usize].map(|v| self.pos[v as usize]);
        let n = (t[1] - t[0]).cross(t[2] - t[0]);
        let (a, b) = (self.pos[lo as usize], self.pos[hi as usize]);
        let (da, db) = (n.dot(a - t[0]), n.dot(b - t[0]));
        let s = if (da - db).abs() > 0.0 {
            (da / (da - db)).clamp(1e-12, 1.0 - 1e-12)
        } else {
            0.5
        };
        let v = self.pos.len() as V;
        self.pos.push(a + (b - a) * s);
        self.param.push(s);
        self.pierce.insert((m as u8, lo, hi, tri), v);
        self.edge_pts[m].entry((lo, hi)).or_default().push(v);
        v
    }

    /// Intersect triangle `i` of operand 0 with triangle `j` of operand 1.
    fn intersect_pair(&mut self, i: u32, j: u32) -> Option<()> {
        let (a, b) = (self.tris[0][i as usize], self.tris[1][j as usize]);
        let pa = a.map(|v| self.pos[v as usize]);
        let pb = b.map(|v| self.pos[v as usize]);
        // Side of each vertex relative to the other triangle's plane.
        let sb: [i8; 3] = std::array::from_fn(|k| s3(&pa[0], &pa[1], &pa[2], &pb[k]));
        if sb == [1; 3] || sb == [-1; 3] {
            return Some(());
        }
        let sa: [i8; 3] = std::array::from_fn(|k| s3(&pb[0], &pb[1], &pb[2], &pa[k]));
        if sa == [1; 3] || sa == [-1; 3] {
            return Some(());
        }
        if sa.contains(&0) || sb.contains(&0) {
            return note("a vertex lies exactly in the other triangle's plane", None);
        }
        // Both triangles straddle each other's plane. The intersection
        // segment ends where an edge of one pierces the other.
        let mut ends: Vec<V> = Vec::with_capacity(2);
        for k in 0..3 {
            let (p, q) = (a[k], a[(k + 1) % 3]);
            if sa[k] != sa[(k + 1) % 3]
                && note("edge of A through an edge of B", self.pierces(p, q, &b))?
            {
                ends.push(self.pierce_vertex(0, p, q, j));
            }
        }
        for k in 0..3 {
            let (p, q) = (b[k], b[(k + 1) % 3]);
            if sb[k] != sb[(k + 1) % 3]
                && note("edge of B through an edge of A", self.pierces(p, q, &a))?
            {
                ends.push(self.pierce_vertex(1, p, q, i));
            }
        }
        let seg = match ends.len() {
            0 => return Some(()),
            2 => (ends[0], ends[1]),
            _ => return note("triangle pair with an odd number of crossings", None),
        };
        self.cons[0].entry(i).or_default().push(seg);
        self.cons[1].entry(j).or_default().push(seg);
        Some(())
    }

    /// Intersection points on the edge `from → to` of operand `m`, in order.
    fn points_on_edge(&self, m: usize, from: V, to: V) -> Vec<V> {
        let Some(list) = self.edge_pts[m].get(&edge_key(from, to)) else {
            return Vec::new();
        };
        let mut pts = list.clone();
        pts.sort_by(|&x, &y| {
            self.param[x as usize]
                .total_cmp(&self.param[y as usize])
                .then(x.cmp(&y))
        });
        if from > to {
            pts.reverse();
        }
        pts
    }

    /// Triangle `idx` of operand `m`, re-cut along its intersection
    /// segments.
    fn subdivide(&self, m: usize, idx: u32, out: &mut Vec<Tri>) -> Option<()> {
        let t = self.tris[m][idx as usize];
        let on_edges: [Vec<V>; 3] =
            std::array::from_fn(|k| self.points_on_edge(m, t[k], t[(k + 1) % 3]));
        let cons = self.cons[m].get(&idx);
        if cons.is_none() && on_edges.iter().all(Vec::is_empty) {
            out.push(t);
            return Some(());
        }
        let points = on_edges.iter().map(Vec::len).sum::<usize>() + cons.map_or(0, |c| 2 * c.len());
        if points > MAX_POINTS_PER_TRIANGLE {
            return note("a triangle carries too many intersection points", None);
        }
        let c = t.map(|v| self.pos[v as usize]);
        let ex = (c[1] - c[0]).normalize();
        let ey = (c[1] - c[0]).cross(c[2] - c[0]).cross(ex).normalize();
        if !(ex.norm() > 0.5 && ey.norm() > 0.5) {
            return None;
        }
        let mut cdt = Cdt::default();
        let add = |cdt: &mut Cdt, v: V, on: u8| -> usize {
            let d = self.pos[v as usize] - c[0];
            cdt.pts.push(Point2::new(d.dot(ex), d.dot(ey)));
            cdt.gid.push(v);
            cdt.on.push(on);
            cdt.pts.len() - 1
        };
        // Corner k lies on boundary edges k and k−1.
        for (k, &corner) in t.iter().enumerate() {
            add(&mut cdt, corner, (1 << k) | (1 << ((k + 2) % 3)));
        }
        cdt.tris.push([0, 1, 2]);
        // Boundary points: split the boundary sub-edge they lie on. Purely
        // combinatorial, so a point a hair off the edge in 2-D still lands
        // on it.
        for (k, on_edge) in on_edges.iter().enumerate() {
            let mut prev = k;
            for &v in on_edge {
                let p = add(&mut cdt, v, 1 << k);
                note("boundary split", cdt.split_boundary(prev, (k + 1) % 3, p))?;
                prev = p;
            }
        }
        // Interior points: segment ends that are neither corners nor
        // boundary points.
        if let Some(cons) = cons {
            for &(a, b) in cons {
                for v in [a, b] {
                    if !cdt.gid.contains(&v) {
                        let p = add(&mut cdt, v, 0);
                        note(
                            "interior point on the triangle boundary",
                            cdt.insert_interior(p),
                        )?;
                    }
                }
            }
            for &(a, b) in cons {
                let la = cdt.gid.iter().position(|&g| g == a)?;
                let lb = cdt.gid.iter().position(|&g| g == b)?;
                note("constraint could not be enforced", cdt.enforce(la, lb))?;
            }
        }
        cdt.improve();
        out.extend(cdt.tris.iter().map(|l| l.map(|i| cdt.gid[i])));
        Some(())
    }
}

/// A small constrained triangulation of one input triangle's interior.
#[derive(Default)]
struct Cdt {
    pts: Vec<Point2>,
    gid: Vec<V>,
    /// Bit k set: the vertex lies on boundary edge k of the input triangle.
    on: Vec<u8>,
    /// Counter-clockwise.
    tris: Vec<[usize; 3]>,
    fixed: HashSet<(usize, usize)>,
}

impl Cdt {
    fn o(&self, a: usize, b: usize, c: usize) -> i8 {
        sign(orient2d(&self.pts[a], &self.pts[b], &self.pts[c]))
    }

    /// Index of the triangle carrying the directed edge `u → v`, and the
    /// vertex opposite it.
    fn with_edge(&self, u: usize, v: usize) -> Option<(usize, usize)> {
        self.tris.iter().enumerate().find_map(|(i, t)| {
            (0..3)
                .find(|&k| t[k] == u && t[(k + 1) % 3] == v)
                .map(|k| (i, t[(k + 2) % 3]))
        })
    }

    fn key(a: usize, b: usize) -> (usize, usize) {
        (a.min(b), a.max(b))
    }

    fn split_boundary(&mut self, u: usize, v: usize, p: usize) -> Option<()> {
        let (i, w) = self.with_edge(u, v)?;
        self.tris[i] = [u, p, w];
        self.tris.push([p, v, w]);
        Some(())
    }

    fn insert_interior(&mut self, p: usize) -> Option<()> {
        let mut on_edge: Option<(usize, usize)> = None;
        for i in 0..self.tris.len() {
            let t = self.tris[i];
            let s = [
                self.o(t[0], t[1], p),
                self.o(t[1], t[2], p),
                self.o(t[2], t[0], p),
            ];
            if s.iter().any(|&x| x < 0) {
                continue;
            }
            let zeros: Vec<usize> = (0..3).filter(|&k| s[k] == 0).collect();
            match zeros.len() {
                0 => {
                    self.tris[i] = [t[0], t[1], p];
                    self.tris.push([t[1], t[2], p]);
                    self.tris.push([t[2], t[0], p]);
                    return Some(());
                }
                1 => {
                    on_edge = Some((t[zeros[0]], t[(zeros[0] + 1) % 3]));
                    break;
                }
                _ => return None,
            }
        }
        // On an interior edge: split both triangles across it. On the input
        // triangle's own boundary there is no way to tell the neighbour.
        let (u, v) = on_edge?;
        let (i, w) = self.with_edge(u, v)?;
        let (j, x) = self.with_edge(v, u)?;
        self.tris[i] = [u, p, w];
        self.tris[j] = [v, p, x];
        self.tris.push([p, v, w]);
        self.tris.push([p, u, x]);
        Some(())
    }

    /// Would `a b c` have all three vertices on one boundary edge of the
    /// input triangle (a zero-area triangle whatever the 2-D noise says)?
    fn collinear(&self, a: usize, b: usize, c: usize) -> bool {
        self.on[a] & self.on[b] & self.on[c] != 0
    }

    /// Flip the interior edge `x y`. Refused unless the quad around it is
    /// strictly convex.
    fn flip(&mut self, x: usize, y: usize) -> Option<(usize, usize)> {
        let (i, o1) = self.with_edge(x, y)?;
        let (j, o2) = self.with_edge(y, x)?;
        if self.o(o2, y, o1) <= 0
            || self.o(o1, x, o2) <= 0
            || self.collinear(o2, y, o1)
            || self.collinear(o1, x, o2)
        {
            return None;
        }
        self.tris[i] = [o2, y, o1];
        self.tris[j] = [o1, x, o2];
        Some((o1, o2))
    }

    fn crosses(&self, a: usize, b: usize, x: usize, y: usize) -> bool {
        if x == a || x == b || y == a || y == b {
            return false;
        }
        self.o(a, b, x) * self.o(a, b, y) < 0 && self.o(x, y, a) * self.o(x, y, b) < 0
    }

    /// Make `a b` an edge of the triangulation (Sloan's flip queue).
    fn enforce(&mut self, a: usize, b: usize) -> Option<()> {
        let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
        for t in &self.tris {
            for k in 0..3 {
                let (x, y) = (t[k], t[(k + 1) % 3]);
                if x < y && self.crosses(a, b, x, y) {
                    queue.push_back((x, y));
                }
            }
        }
        let mut budget = 64 + 16 * queue.len() * queue.len();
        while let Some((x, y)) = queue.pop_front() {
            budget = budget.checked_sub(1)?;
            if self.fixed.contains(&Self::key(x, y)) {
                return None;
            }
            if self.with_edge(x, y).is_none() || !self.crosses(a, b, x, y) {
                continue;
            }
            match self.flip(x, y) {
                Some((p, q)) => {
                    if self.crosses(a, b, p, q) {
                        queue.push_back((p, q));
                    }
                }
                None => queue.push_back((x, y)),
            }
        }
        (self.with_edge(a, b).is_some() || self.with_edge(b, a).is_some()).then(|| {
            self.fixed.insert(Self::key(a, b));
        })
    }

    /// Lawson flips toward Delaunay over the unconstrained interior edges:
    /// the cut itself is fixed, this only keeps the pieces well shaped. A
    /// fixed number of sweeps — shape is a nicety, and edge lookups here are
    /// linear scans.
    fn improve(&mut self) {
        for _ in 0..IMPROVE_SWEEPS {
            let mut changed = false;
            for i in 0..self.tris.len() {
                for k in 0..3 {
                    let (x, y) = (self.tris[i][k], self.tris[i][(k + 1) % 3]);
                    if x > y || self.fixed.contains(&Self::key(x, y)) {
                        continue;
                    }
                    let (Some((_, o1)), Some((_, o2))) =
                        (self.with_edge(x, y), self.with_edge(y, x))
                    else {
                        continue;
                    };
                    let inside = sign(incircle(
                        &self.pts[x],
                        &self.pts[y],
                        &self.pts[o1],
                        &self.pts[o2],
                    )) > 0;
                    if inside && self.flip(x, y).is_some() {
                        changed = true;
                        // Triangle `i` is a different triangle now.
                        break;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }
}

/// Would the nudge resolve some flush contact between the operands the wrong
/// way?
///
/// Where a facet of B lies exactly in the plane of a facet of A and overlaps
/// it, the nudge decides which sheet is outside, and for three combinations
/// only one answer is the solid the user meant:
///
/// * `A − B`, facets facing the same way (a tool flush with the wall it
///   cuts): B must poke through, or a zero-thickness skin is left over the
///   opening.
/// * `A ∪ B`, facets facing each other (two bodies in face contact): B must
///   press in, or the bodies stay separated by a hairline void.
/// * `A ∩ B`, facets facing each other: B must back off, or a zero-volume
///   wafer is left.
///
/// No one direction suits a pocket flush with both the top and the bottom
/// of a part, so such arrangements are left to the BSP boolean, whose
/// coplanar handling is exactly this.
fn flush_contact_misresolved(
    pos: &[Point3],
    tris_a: &[Tri],
    tris_b: &[Tri],
    op: BooleanOp,
    nudge: Vec3,
) -> bool {
    for (i, j) in candidate_pairs(pos, tris_a, tris_b) {
        let a = tris_a[i as usize].map(|v| pos[v as usize]);
        let b = tris_b[j as usize].map(|v| pos[v as usize]);
        if b.iter().any(|q| s3(&a[0], &a[1], &a[2], q) != 0) {
            continue;
        }
        let (na, nb) = (
            (a[1] - a[0]).cross(a[2] - a[0]),
            (b[1] - b[0]).cross(b[2] - b[0]),
        );
        // Drop the axis the common plane is most normal to; exact in-plane
        // tests then run on untouched coordinates.
        let drop = if na.x.abs() >= na.y.abs() && na.x.abs() >= na.z.abs() {
            0
        } else if na.y.abs() >= na.z.abs() {
            1
        } else {
            2
        };
        let flat = |q: &Point3| match drop {
            0 => Point2::new(q.y, q.z),
            1 => Point2::new(q.z, q.x),
            _ => Point2::new(q.x, q.y),
        };
        let (fa, fb) = (a.map(|q| flat(&q)), b.map(|q| flat(&q)));
        // Interiors overlap unless some edge of either separates them.
        let separated = |t: &[Point2; 3], u: &[Point2; 3]| {
            let o = sign(orient2d(&t[0], &t[1], &t[2]));
            o == 0
                || (0..3).any(|k| {
                    u.iter()
                        .all(|q| sign(orient2d(&t[k], &t[(k + 1) % 3], q)) * o <= 0)
                })
        };
        if separated(&fa, &fb) || separated(&fb, &fa) {
            continue;
        }
        let aligned = na.dot(nb) > 0.0;
        let pokes_out = nudge.dot(nb) > 0.0;
        let wrong = match op {
            BooleanOp::Difference => aligned && !pokes_out,
            BooleanOp::Union => !aligned && !pokes_out,
            BooleanOp::Intersection => !aligned && pokes_out,
        };
        if wrong {
            return true;
        }
    }
    false
}

/// Candidate triangle pairs by bounding-box overlap (sweep along x).
fn candidate_pairs(pos: &[Point3], a: &[Tri], b: &[Tri]) -> Vec<(u32, u32)> {
    let boxes = |tris: &[Tri]| -> Vec<([f64; 3], [f64; 3])> {
        tris.iter()
            .map(|t| {
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for &v in t {
                    let p = pos[v as usize];
                    for (k, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                        lo[k] = lo[k].min(c);
                        hi[k] = hi[k].max(c);
                    }
                }
                (lo, hi)
            })
            .collect()
    };
    let (ba, bb) = (boxes(a), boxes(b));
    let order = |bx: &[([f64; 3], [f64; 3])]| {
        let mut o: Vec<u32> = (0..bx.len() as u32).collect();
        o.sort_by(|&x, &y| bx[x as usize].0[0].total_cmp(&bx[y as usize].0[0]));
        o
    };
    let (oa, ob) = (order(&ba), order(&bb));
    let overlap = |x: &([f64; 3], [f64; 3]), y: &([f64; 3], [f64; 3])| {
        (0..3).all(|k| x.0[k] <= y.1[k] && y.0[k] <= x.1[k])
    };
    let mut pairs = Vec::new();
    let (mut ia, mut ib) = (0usize, 0usize);
    let (mut live_a, mut live_b): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
    while ia < oa.len() || ib < ob.len() {
        let take_a = ib >= ob.len()
            || (ia < oa.len() && ba[oa[ia] as usize].0[0] <= bb[ob[ib] as usize].0[0]);
        if take_a {
            let i = oa[ia];
            ia += 1;
            let x = ba[i as usize].0[0];
            live_b.retain(|&j| bb[j as usize].1[0] >= x);
            for &j in &live_b {
                if overlap(&ba[i as usize], &bb[j as usize]) {
                    pairs.push((i, j));
                }
            }
            live_a.push(i);
        } else {
            let j = ob[ib];
            ib += 1;
            let x = bb[j as usize].0[0];
            live_a.retain(|&i| ba[i as usize].1[0] >= x);
            for &i in &live_a {
                if overlap(&ba[i as usize], &bb[j as usize]) {
                    pairs.push((i, j));
                }
            }
            live_b.push(j);
        }
    }
    pairs.sort_unstable();
    pairs
}

fn sorted(t: &Tri) -> Tri {
    let mut s = *t;
    s.sort_unstable();
    s
}

/// Even permutation of its sorted self?
fn even(t: &Tri) -> bool {
    (t[0] < t[1]) as u8 + (t[1] < t[2]) as u8 + (t[2] < t[0]) as u8 == 2
}

/// Per triangle: is the patch it belongs to inside the other operand?
///
/// `shift` moves a query point from this operand's (possibly nudged) frame
/// into the frame `other` is indexed in.
fn classify_patches(
    pos: &[Point3],
    tris: &[Tri],
    barrier: &HashSet<(V, V)>,
    other: &MeshRayIndex<'_>,
    shift: Vec3,
) -> Option<Vec<bool>> {
    let mut by_edge: HashMap<(V, V), [u32; 2]> = HashMap::with_capacity(tris.len() * 3 / 2);
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            let slot = by_edge
                .entry(edge_key(t[k], t[(k + 1) % 3]))
                .or_insert([u32::MAX; 2]);
            let free = slot.iter().position(|&s| s == u32::MAX)?;
            slot[free] = i as u32;
        }
    }
    let area2 = |t: &Tri| {
        let p = t.map(|v| pos[v as usize]);
        (p[1] - p[0]).cross(p[2] - p[0]).norm()
    };
    let mut class = vec![false; tris.len()];
    let mut seen = vec![false; tris.len()];
    let mut stack: Vec<u32> = Vec::new();
    let mut patch: Vec<u32> = Vec::new();
    for start in 0..tris.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start as u32);
        patch.clear();
        while let Some(i) = stack.pop() {
            patch.push(i);
            let t = tris[i as usize];
            for k in 0..3 {
                let e = edge_key(t[k], t[(k + 1) % 3]);
                if barrier.contains(&e) {
                    continue;
                }
                for &n in &by_edge[&e] {
                    if n != u32::MAX && !seen[n as usize] {
                        seen[n as usize] = true;
                        stack.push(n);
                    }
                }
            }
        }
        // The patch's largest triangle: its centroid is as far from the
        // intersection curve as the patch allows.
        let rep = *patch.iter().max_by(|&&x, &&y| {
            area2(&tris[x as usize])
                .total_cmp(&area2(&tris[y as usize]))
                .then(y.cmp(&x))
        })?;
        let p = tris[rep as usize].map(|v| pos[v as usize]);
        let centroid = Point3::from_vec((p[0].to_vec() + p[1].to_vec() + p[2].to_vec()) / 3.0);
        let inside = other.contains(&(centroid + shift));
        for &i in &patch {
            class[i as usize] = inside;
        }
    }
    Some(class)
}

/// Snap intersection points onto vertices they all but coincide with, merge
/// the operands' two copies of a shared vertex, and flip away triangles too
/// flat to carry a normal. Works on the f32 positions the result is stored
/// in; every step preserves the manifold, and `None` says one could not.
fn clean(pos: &[[f32; 3]], tris: &[Tri], is_new: &[bool]) -> Option<Vec<Tri>> {
    let p = |v: V| {
        let q = pos[v as usize];
        Point3::new(q[0] as f64, q[1] as f64, q[2] as f64)
    };
    let mut tris: Vec<Tri> = tris.to_vec();
    let mut flips_allowed = true;
    for _ in 0..256 {
        let mut changed = false;

        // 1. Collapse short edges that end on an intersection point.
        let mut remap: HashMap<V, V> = HashMap::new();
        let mut busy: HashSet<V> = HashSet::new();
        let mut ring: HashMap<V, Vec<V>> = HashMap::new();
        for t in &tris {
            for k in 0..3 {
                ring.entry(t[k]).or_default().push(t[(k + 1) % 3]);
            }
        }
        for t in &tris {
            for k in 0..3 {
                let (u, v) = (t[k], t[(k + 1) % 3]);
                let len = (p(u) - p(v)).norm();
                if u > v || len >= MIN_EDGE {
                    continue;
                }
                // Never move an input vertex: an intersection point goes to
                // the vertex it sits on, and two input vertices merge only
                // when they are one position (the operands' two copies).
                let (from, to) = match (is_new[u as usize], is_new[v as usize]) {
                    (true, false) => (u, v),
                    (false, true) => (v, u),
                    (true, true) => (v, u),
                    (false, false) if len == 0.0 => (v, u),
                    (false, false) => continue,
                };
                let (rf, rt) = (&ring[&from], &ring[&to]);
                if busy.contains(&from)
                    || busy.contains(&to)
                    || rf.iter().chain(rt.iter()).any(|x| busy.contains(x))
                {
                    continue;
                }
                // Link condition: the ends share exactly the two vertices
                // opposite the edge.
                if rf.iter().filter(|x| rt.contains(x)).count() != 2 {
                    continue;
                }
                remap.insert(from, to);
                busy.insert(from);
                busy.insert(to);
                busy.extend(rf.iter().copied());
                busy.extend(rt.iter().copied());
            }
        }
        if !remap.is_empty() {
            changed = true;
            tris = tris
                .iter()
                .map(|t| t.map(|v| *remap.get(&v).unwrap_or(&v)))
                .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
                .collect();
            if !oriented_closed(&tris) {
                return note("an edge collapse broke the manifold", None);
            }
        }

        // 2. Flip the long edge of triangles with no usable height.
        let mut by_dir: HashMap<(V, V), usize> = HashMap::with_capacity(tris.len() * 3);
        for (i, t) in tris.iter().enumerate() {
            for k in 0..3 {
                by_dir.insert((t[k], t[(k + 1) % 3]), i);
            }
        }
        let height = |a: Point3, b: Point3, c: Point3| {
            // Smallest altitude: twice the area over the longest edge.
            let l = (b - a).norm().max((c - b).norm()).max((a - c).norm());
            if l > 0.0 {
                (b - a).cross(c - a).norm() / l
            } else {
                0.0
            }
        };
        let mut touched: HashSet<usize> = HashSet::new();
        // Edges this pass has already created: `by_dir` predates them.
        let mut created: HashSet<(V, V)> = HashSet::new();
        let before_flips = tris.clone();
        for i in 0..tris.len() {
            if !flips_allowed {
                break;
            }
            if touched.contains(&i) {
                continue;
            }
            let t = tris[i];
            let q = t.map(p);
            let h = height(q[0], q[1], q[2]);
            if h >= MIN_HEIGHT {
                continue;
            }
            // Longest edge x → y, apex z.
            let k = (0..3)
                .max_by(|&a, &b| {
                    (q[(a + 1) % 3] - q[a])
                        .norm()
                        .total_cmp(&(q[(b + 1) % 3] - q[b]).norm())
                })
                .unwrap_or(0);
            let (x, y, z) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
            let Some(&j) = by_dir.get(&(y, x)) else {
                continue;
            };
            if touched.contains(&j) || j == i {
                continue;
            }
            let u = tris[j];
            let w = *u.iter().find(|&&v| v != x && v != y)?;
            if by_dir.contains_key(&(z, w))
                || by_dir.contains_key(&(w, z))
                || created.contains(&edge_key(z, w))
            {
                continue;
            }
            let (n1, n2) = ([w, y, z], [z, x, w]);
            let h_new = height(p(w), p(y), p(z)).min(height(p(z), p(x), p(w)));
            let facing = |t: &Tri| {
                let c = t.map(p);
                (c[1] - c[0]).cross(c[2] - c[0])
            };
            let reference = facing(&u);
            if h_new < 4.0 * h.max(1e-9)
                || facing(&n1).dot(reference) <= 0.0
                || facing(&n2).dot(reference) <= 0.0
            {
                continue;
            }
            tris[i] = n1;
            tris[j] = n2;
            created.insert(edge_key(z, w));
            touched.insert(i);
            touched.insert(j);
            changed = true;
        }
        if !touched.is_empty() && !oriented_closed(&tris) {
            // Keep the flat triangles rather than a broken surface.
            tris = before_flips;
            flips_allowed = false;
        }
        if !changed {
            break;
        }
    }
    oriented_closed(&tris).then_some(tris)
}

/// Boolean of two closed manifold triangle meshes as a closed manifold
/// triangle mesh, or `None` when either operand is not one or the result
/// does not close (see the module docs). Deterministic: the result is a
/// function of the operands alone.
pub fn conforming_csg(
    mesh_a: &TriangleMesh,
    mesh_b: &TriangleMesh,
    op: BooleanOp,
) -> Option<TriangleMesh> {
    // Each operand's vertices interned by exact position; the two operands
    // never share an id (B is about to move).
    let mut original: Vec<[f32; 3]> = Vec::new();
    let load = |mesh: &TriangleMesh, original: &mut Vec<[f32; 3]>| -> Option<Vec<Tri>> {
        let mut by_bits: HashMap<[u32; 3], V> = HashMap::new();
        let ids: Vec<V> = mesh
            .vertices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| {
                // `+ 0.0` folds −0.0 into 0.0.
                let c = [c[0] + 0.0, c[1] + 0.0, c[2] + 0.0];
                *by_bits.entry(c.map(f32::to_bits)).or_insert_with(|| {
                    original.push(c);
                    (original.len() - 1) as V
                })
            })
            .collect();
        let tris: Vec<Tri> = mesh
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| [ids[t[0] as usize], ids[t[1] as usize], ids[t[2] as usize]])
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .collect();
        (!tris.is_empty() && oriented_closed(&tris)).then_some(tris)
    };
    let tris_a = note(
        "operand A is not a closed manifold",
        load(mesh_a, &mut original),
    )?;
    let first_b = original.len();
    let tris_b = note(
        "operand B is not a closed manifold",
        load(mesh_b, &mut original),
    )?;
    if original.iter().flatten().any(|c| !c.is_finite()) {
        return None;
    }

    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for c in &original {
        for k in 0..3 {
            lo[k] = lo[k].min(c[k] as f64);
            hi[k] = hi[k].max(c[k] as f64);
        }
    }
    let diag = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
    let nudge = Vec3::new(NUDGE_DIR[0], NUDGE_DIR[1], NUDGE_DIR[2])
        * (NUDGE * diag).clamp(1e-10, 0.1 * MIN_EDGE);
    let unmoved: Vec<Point3> = original
        .iter()
        .map(|c| Point3::new(c[0] as f64, c[1] as f64, c[2] as f64))
        .collect();
    if flush_contact_misresolved(&unmoved, &tris_a, &tris_b, op, nudge) {
        return note(
            "a flush contact the nudge would resolve the wrong way",
            None,
        );
    }
    let pos: Vec<Point3> = original
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let p = Point3::new(c[0] as f64, c[1] as f64, c[2] as f64);
            if i >= first_b {
                p + nudge
            } else {
                p
            }
        })
        .collect();

    let first_new = pos.len();
    let mut arr = Arrangement {
        param: vec![f64::NAN; pos.len()],
        pos,
        tris: [tris_a, tris_b],
        edge_pts: [HashMap::new(), HashMap::new()],
        cons: [HashMap::new(), HashMap::new()],
        pierce: HashMap::new(),
    };
    for (i, j) in candidate_pairs(&arr.pos, &arr.tris[0], &arr.tris[1]) {
        arr.intersect_pair(i, j)?;
    }

    let mut sub: [Vec<Tri>; 2] = [Vec::new(), Vec::new()];
    for (m, out) in sub.iter_mut().enumerate() {
        out.reserve(arr.tris[m].len());
        for idx in 0..arr.tris[m].len() as u32 {
            arr.subdivide(m, idx, out)?;
        }
    }

    // The intersection curve: the edges both re-cut surfaces carry.
    let edges = |tris: &[Tri]| -> HashSet<(V, V)> {
        tris.iter()
            .flat_map(|t| (0..3).map(move |k| edge_key(t[k], t[(k + 1) % 3])))
            .collect()
    };
    let (ea, eb) = (edges(&sub[0]), edges(&sub[1]));
    let barrier: HashSet<(V, V)> = ea.intersection(&eb).copied().collect();

    // Ray parity against the operands as given: a point of A is inside the
    // nudged B exactly when, moved back by the nudge, it is inside B.
    let (index_a, index_b) = (MeshRayIndex::new(mesh_a), MeshRayIndex::new(mesh_b));
    let class_a = note(
        "A's re-cut surface is not manifold",
        classify_patches(&arr.pos, &sub[0], &barrier, &index_b, -nudge),
    )?;
    let class_b = note(
        "B's re-cut surface is not manifold",
        classify_patches(&arr.pos, &sub[1], &barrier, &index_a, Vec3::zeros()),
    )?;

    let mut out: Vec<Tri> = Vec::with_capacity(sub[0].len() + sub[1].len());
    for (t, &inside) in sub[0].iter().zip(&class_a) {
        let keep = match op {
            BooleanOp::Union | BooleanOp::Difference => !inside,
            BooleanOp::Intersection => inside,
        };
        if keep {
            out.push(*t);
        }
    }
    for (t, &inside) in sub[1].iter().zip(&class_b) {
        match op {
            BooleanOp::Union if !inside => out.push(*t),
            BooleanOp::Intersection if inside => out.push(*t),
            // Kept B fragments bound the carved cavity: they face inward.
            BooleanOp::Difference if inside => out.push([t[0], t[2], t[1]]),
            _ => {}
        }
    }
    if out.is_empty() {
        return Some(TriangleMesh::new());
    }
    if !oriented_closed(&out) {
        return note("kept patches do not close", None);
    }

    // Snap back: input vertices at their original positions, intersection
    // points rounded to f32. Shared surface now shows as hair-thin pillows
    // between the two operands' copies of one vertex; collapsing every
    // near-zero edge closes them, and each collapse keeps the manifold.
    let stored: Vec<[f32; 3]> = (0..arr.pos.len())
        .map(|v| {
            if v >= first_new {
                let p = arr.pos[v];
                [p.x as f32 + 0.0, p.y as f32 + 0.0, p.z as f32 + 0.0]
            } else {
                original[v]
            }
        })
        .collect();
    let is_new: Vec<bool> = (0..arr.pos.len()).map(|v| v >= first_new).collect();
    let tris = if std::env::var_os("VCAD_CONFORM_NO_CLEAN").is_some() {
        out
    } else {
        note(
            "slivers could not be collapsed",
            clean(&stored, &out, &is_new),
        )?
    };

    // Whatever still shares a position is one vertex. Two sheets left face
    // to face (the whole of `A − A`, a tool flush with the wall it cuts)
    // then carry the same triangles in opposite senses, and cancel.
    let mut bits: HashMap<[u32; 3], V> = HashMap::new();
    let mut tris: Vec<Tri> = tris
        .iter()
        .map(|t| {
            t.map(|v| {
                let next = bits.len() as V;
                *bits
                    .entry(stored[v as usize].map(f32::to_bits))
                    .or_insert(next)
            })
        })
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    let mut net: HashMap<Tri, i32> = HashMap::new();
    for t in &tris {
        *net.entry(sorted(t)).or_insert(0) += if even(t) { 1 } else { -1 };
    }
    if net.values().any(|&n| n != 1 && n != -1) {
        tris.retain(|t| {
            let slot = net.get_mut(&sorted(t)).expect("counted above");
            let s = if even(t) { 1 } else { -1 };
            // Keep while this orientation still has an uncancelled surplus.
            if *slot * s > 0 {
                *slot -= s;
                true
            } else {
                false
            }
        });
    }
    if !oriented_closed(&tris) {
        return note("result does not close once snapped back", None);
    }

    let mut positions: Vec<[f32; 3]> = vec![[0.0; 3]; bits.len()];
    for (key, &id) in &bits {
        positions[id as usize] = key.map(f32::from_bits);
    }
    // Compact to the vertices still referenced.
    let mut mesh = TriangleMesh::new();
    let mut slot: HashMap<V, u32> = HashMap::new();
    for t in &tris {
        for &v in t {
            let next = slot.len() as u32;
            let i = *slot.entry(v).or_insert_with(|| {
                mesh.vertices.extend_from_slice(&positions[v as usize]);
                next
            });
            mesh.indices.push(i);
        }
    }
    Some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_kernel_primitives::{make_cube, make_cylinder, make_sphere};
    use vcad_kernel_tessellate::tessellate_brep;

    fn volume(mesh: &TriangleMesh) -> f64 {
        crate::validate::mesh_signed_volume(mesh)
    }

    fn shifted(mesh: &TriangleMesh, d: [f32; 3]) -> TriangleMesh {
        let mut m = mesh.clone();
        for v in m.vertices.chunks_mut(3) {
            v[0] += d[0];
            v[1] += d[1];
            v[2] += d[2];
        }
        m
    }

    fn closed(mesh: &TriangleMesh) -> bool {
        let tris: Vec<Tri> = mesh
            .indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();
        oriented_closed(&tris)
    }

    #[test]
    fn crossing_spheres_all_three_ops() {
        let a = tessellate_brep(&make_sphere(10.0, 32), 32);
        let b = shifted(&a, [7.3, 1.1, 0.4]);
        let (va, vb) = (volume(&a), volume(&b));
        let uni = conforming_csg(&a, &b, BooleanOp::Union).expect("union");
        let int = conforming_csg(&a, &b, BooleanOp::Intersection).expect("intersection");
        let dif = conforming_csg(&a, &b, BooleanOp::Difference).expect("difference");
        for m in [&uni, &int, &dif] {
            assert!(closed(m));
        }
        // Inclusion–exclusion holds exactly for the polyhedra.
        assert!((volume(&uni) + volume(&int) - va - vb).abs() < 1e-2);
        assert!((volume(&dif) + volume(&int) - va).abs() < 1e-2);
        // Only the crossed triangles are re-cut.
        assert!(uni.indices.len() < a.indices.len() + b.indices.len());
    }

    #[test]
    fn cylinder_through_sphere() {
        let a = tessellate_brep(&make_sphere(10.0, 32), 32);
        let b = shifted(
            &tessellate_brep(&make_cylinder(3.0, 40.0, 32), 32),
            [0.7, 0.3, -20.0],
        );
        let dif = conforming_csg(&a, &b, BooleanOp::Difference).expect("difference");
        assert!(closed(&dif));
        let removed = volume(&a) - volume(&dif);
        // A r=3 bore through a r=10 ball removes a little under π·9·20.
        assert!((530.0..566.0).contains(&removed), "removed {removed}");
    }

    #[test]
    fn disjoint_and_nested() {
        let a = tessellate_brep(&make_sphere(10.0, 32), 32);
        let far = shifted(&a, [40.0, 0.0, 0.0]);
        let uni = conforming_csg(&a, &far, BooleanOp::Union).expect("union");
        assert!((volume(&uni) - volume(&a) - volume(&far)).abs() < 1e-6);
        let small = tessellate_brep(&make_sphere(3.0, 32), 32);
        let uni = conforming_csg(&a, &small, BooleanOp::Union).expect("union");
        assert!((volume(&uni) - volume(&a)).abs() < 1e-6);
        let dif = conforming_csg(&a, &small, BooleanOp::Difference).expect("difference");
        assert!((volume(&dif) - volume(&a) + volume(&small)).abs() < 1e-6);
        let none = conforming_csg(&small, &a, BooleanOp::Difference).expect("difference");
        assert!(none.indices.is_empty());
    }

    #[test]
    fn identical_operands() {
        let a = tessellate_brep(&make_sphere(10.0, 32), 32);
        let uni = conforming_csg(&a, &a.clone(), BooleanOp::Union).expect("union");
        assert!(closed(&uni));
        assert!((volume(&uni) - volume(&a)).abs() < 1e-6);
        // `A − A` is a tool flush with every wall it cuts, half of them
        // facing against the nudge: left to the BSP boolean.
        assert!(conforming_csg(&a, &a.clone(), BooleanOp::Difference).is_none());
    }

    #[test]
    fn coplanar_faces_resolve() {
        // Two boxes sharing four face planes, with overlapping, different
        // facets on each.
        let a = tessellate_brep(&make_cube(10.0, 10.0, 10.0), 16);
        let b = shifted(&a, [5.0, 0.0, 0.0]);
        let uni = conforming_csg(&a, &b, BooleanOp::Union).expect("union");
        assert!(closed(&uni));
        assert!((volume(&uni) - 1500.0).abs() < 1e-3, "{}", volume(&uni));
        let int = conforming_csg(&a, &b, BooleanOp::Intersection).expect("intersection");
        assert!(closed(&int));
        assert!((volume(&int) - 500.0).abs() < 1e-3, "{}", volume(&int));
    }

    #[test]
    fn open_operand_declines() {
        let a = tessellate_brep(&make_sphere(10.0, 32), 32);
        let mut open = shifted(&a, [7.0, 0.0, 0.0]);
        open.indices.truncate(open.indices.len() - 3);
        assert!(conforming_csg(&a, &open, BooleanOp::Union).is_none());
    }
}
