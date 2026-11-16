//! Display-side thinning of feature chains.
//!
//! A boolean freezes every circular rim it creates onto the kernel's
//! canonical grid — 1 µm of sag, up to 512 points a turn — because the
//! splitters' tolerances depend on that density. The tessellator then
//! renders those rims verbatim, so a 76 mm cylinder wall reaches the
//! viewport as 512 full-height strips under a millimetre wide, whatever
//! chord tolerance the viewport asked for.
//!
//! The kernel grid is not the viewport's business. [`thin_feature_chains`]
//! takes a finished, welded mesh and removes vertices along its *feature
//! chains* — the polylines where two faces meet — until each chain is only
//! as fine as the display tolerance needs. Because a chain vertex is one
//! mesh vertex shared by both faces, removing it thins both at once: the
//! wall and the cap that share a rim lose the same points, and the mesh
//! stays closed by construction.
//!
//! For display only. Volumes, slicing and export keep the full grid.

use std::collections::{HashMap, HashSet};

use crate::TriangleMesh;

type P = [f64; 3];

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: P, b: P) -> P {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: P) -> f64 {
    dot(a, a).sqrt()
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: P, a: P, b: P) -> f64 {
    let ab = sub(b, a);
    let len2 = dot(ab, ab);
    let t = if len2 < 1e-30 {
        0.0
    } else {
        (dot(sub(p, a), ab) / len2).clamp(0.0, 1.0)
    };
    norm(sub(
        p,
        [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t],
    ))
}

struct Work {
    pos: Vec<P>,
    tris: Vec<[u32; 3]>,
    alive: Vec<bool>,
    /// Vertex → triangles that use it (may list dead ones).
    of_vertex: Vec<Vec<u32>>,
    /// Cosine of the largest turn a surviving triangle's normal may take.
    cos_turn: f64,
}

impl Work {
    fn normal(&self, t: [u32; 3]) -> P {
        let (a, b, c) = (
            self.pos[t[0] as usize],
            self.pos[t[1] as usize],
            self.pos[t[2] as usize],
        );
        cross(sub(b, a), sub(c, a))
    }

    fn live(&self, v: u32) -> impl Iterator<Item = u32> + '_ {
        self.of_vertex[v as usize]
            .iter()
            .copied()
            .filter(|&t| self.alive[t as usize])
    }

    fn neighbours(&self, v: u32) -> HashSet<u32> {
        self.live(v)
            .flat_map(|t| self.tris[t as usize])
            .filter(|&w| w != v)
            .collect()
    }

    /// Collapse `v` onto its neighbour `u` if that keeps the mesh a
    /// manifold with every surviving triangle facing the way it did.
    fn collapse(&mut self, v: u32, u: u32) -> bool {
        let around: Vec<u32> = self.live(v).collect();
        let shared: Vec<u32> = around
            .iter()
            .copied()
            .filter(|&t| self.tris[t as usize].contains(&u))
            .collect();
        if shared.len() != 2 {
            return false;
        }
        // Link condition: the only vertices adjacent to both ends are the
        // two opposite the edge. Anything more and the collapse pinches the
        // surface or doubles a triangle.
        let opposite: HashSet<u32> = shared
            .iter()
            .flat_map(|&t| self.tris[t as usize])
            .filter(|&w| w != v && w != u)
            .collect();
        let common: HashSet<u32> = self
            .neighbours(v)
            .intersection(&self.neighbours(u))
            .copied()
            .collect();
        if opposite.len() != 2 || common != opposite {
            return false;
        }
        for &t in &around {
            let tri = self.tris[t as usize];
            if tri.contains(&u) {
                continue;
            }
            let before = self.normal(tri);
            let after = self.normal(tri.map(|w| if w == v { u } else { w }));
            let (lb, la) = (norm(before), norm(after));
            if la <= 1e-12 {
                return false;
            }
            // A triangle that had no area to begin with has no facing to
            // preserve; one that did must keep it.
            if lb > 1e-12 && dot(before, after) < self.cos_turn * lb * la {
                return false;
            }
        }
        for &t in &around {
            if self.tris[t as usize].contains(&u) {
                self.alive[t as usize] = false;
            } else {
                for w in &mut self.tris[t as usize] {
                    if *w == v {
                        *w = u;
                    }
                }
                self.of_vertex[u as usize].push(t);
            }
        }
        true
    }
}

/// Thin the mesh's feature chains to a display tolerance. Returns the
/// number of vertices removed.
///
/// A feature chain is a run of mesh edges separating two faces (by
/// `face_ids`). A vertex in the interior of a chain is removed when
///
/// * every vertex removed since the last kept one stays within `sag_tol`
///   of the chord that replaces them, and
/// * the thinned polyline's exterior angle there stays within `max_turn`
///   radians (so a small rim never drops below the caller's own segment
///   count), and
/// * collapsing it keeps the mesh manifold and every surviving triangle
///   facing the way it did.
///
/// Vertices where three or more faces meet, and vertices on any edge not
/// shared by exactly two triangles, are never touched — so an open or
/// unwelded mesh is left as it is. A mesh without per-triangle `face_ids`
/// is returned unchanged.
pub fn thin_feature_chains(mesh: &mut TriangleMesh, sag_tol: f64, max_turn: f64) -> usize {
    let n_tris = mesh.indices.len() / 3;
    let n_verts = mesh.vertices.len() / 3;
    if sag_tol <= 0.0 || max_turn <= 0.0 || n_tris == 0 || mesh.face_ids.len() != n_tris {
        return 0;
    }
    let pos: Vec<P> = (0..n_verts)
        .map(|i| {
            [
                mesh.vertices[3 * i] as f64,
                mesh.vertices[3 * i + 1] as f64,
                mesh.vertices[3 * i + 2] as f64,
            ]
        })
        .collect();
    let tris: Vec<[u32; 3]> = mesh.indices.chunks(3).map(|t| [t[0], t[1], t[2]]).collect();

    // Edge → triangles, to find feature edges and edges that are not
    // two-manifold.
    let key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };
    let mut edge_tris: HashMap<(u32, u32), Vec<u32>> = HashMap::with_capacity(n_tris * 2);
    let mut of_vertex: Vec<Vec<u32>> = vec![Vec::new(); n_verts];
    for (t, tri) in tris.iter().enumerate() {
        if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
            continue;
        }
        for k in 0..3 {
            edge_tris
                .entry(key(tri[k], tri[(k + 1) % 3]))
                .or_default()
                .push(t as u32);
            of_vertex[tri[k] as usize].push(t as u32);
        }
    }
    let mut locked = vec![false; n_verts];
    let mut feature: Vec<Vec<u32>> = vec![Vec::new(); n_verts];
    for (&(a, b), ts) in &edge_tris {
        if ts.len() != 2 {
            locked[a as usize] = true;
            locked[b as usize] = true;
        } else if mesh.face_ids[ts[0] as usize] != mesh.face_ids[ts[1] as usize] {
            feature[a as usize].push(b);
            feature[b as usize].push(a);
        }
    }
    // Interior of a chain: exactly two feature edges, exactly two faces.
    let removable: Vec<bool> = (0..n_verts)
        .map(|v| {
            if locked[v] || feature[v].len() != 2 {
                return false;
            }
            let mut faces: Vec<u32> = of_vertex[v]
                .iter()
                .map(|&t| mesh.face_ids[t as usize])
                .collect();
            faces.sort_unstable();
            faces.dedup();
            faces.len() == 2
        })
        .collect();

    // Hash order must not reach the result: the same solid has to thin to
    // the same mesh every time (a warm viewport session is compared against
    // a cold evaluation).
    for f in &mut feature {
        f.sort_by(|&a, &b| {
            pos[a as usize]
                .partial_cmp(&pos[b as usize])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
    }

    // Chains, each from one kept vertex to the next (or round a closed
    // loop back to its anchor), in a direction fixed by position so the two
    // rims of one wall are walked — and thinned — alike.
    let lex = |a: u32, b: u32| {
        let (p, q) = (pos[a as usize], pos[b as usize]);
        p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal)
    };
    let mut visited = vec![false; n_verts];
    let mut chains: Vec<Vec<u32>> = Vec::new();
    let walk = |from: u32, first: u32, visited: &mut Vec<bool>| -> Vec<u32> {
        let mut chain = vec![from, first];
        loop {
            let last = chain[chain.len() - 1];
            if !removable[last as usize] || last == from {
                break;
            }
            visited[last as usize] = true;
            let prev = chain[chain.len() - 2];
            let f = &feature[last as usize];
            chain.push(if f[0] == prev { f[1] } else { f[0] });
        }
        chain
    };
    let mut order: Vec<u32> = (0..n_verts as u32).collect();
    order.sort_by(|&a, &b| lex(a, b).then(a.cmp(&b)));
    // Open chains, from each kept end.
    for &s in &order {
        if removable[s as usize] || feature[s as usize].is_empty() {
            continue;
        }
        for &w in &feature[s as usize] {
            if !removable[w as usize] || visited[w as usize] {
                continue;
            }
            let mut chain = walk(s, w, &mut visited);
            let (a, b) = (chain[0], chain[chain.len() - 1]);
            if lex(b, a) == std::cmp::Ordering::Less {
                chain.reverse();
            }
            chains.push(chain);
        }
    }
    // Closed loops with no kept vertex: anchor at the lowest position.
    for &s in &order {
        if !removable[s as usize] || visited[s as usize] {
            continue;
        }
        let f = &feature[s as usize];
        let first = if lex(f[0], f[1]) == std::cmp::Ordering::Greater {
            f[1]
        } else {
            f[0]
        };
        visited[s as usize] = true;
        chains.push(walk(s, first, &mut visited));
    }

    let mut work = Work {
        pos,
        tris,
        alive: vec![true; n_tris],
        of_vertex,
        // A strip merged across a chord turns by up to the chord's own
        // exterior angle; leave headroom over that limit.
        cos_turn: (1.5 * max_turn).min(std::f64::consts::FRAC_PI_2).cos(),
    };
    for t in 0..n_tris {
        let tri = work.tris[t];
        if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
            work.alive[t] = false;
        }
    }

    let mut removed = 0usize;
    for chain in &chains {
        let m = chain.len() - 1;
        if m < 2 {
            continue;
        }
        let pts: Vec<P> = chain.iter().map(|&v| work.pos[v as usize]).collect();
        let mut kept = 0usize; // index of the last kept chain vertex
        for k in 1..m {
            let (a, b) = (pts[kept], pts[k + 1]);
            let within = (kept + 1..=k).all(|i| segment_distance(pts[i], a, b) <= sag_tol);
            // Exterior angle of the polyline the chord would leave: the
            // turn between the first and last edges it replaces, scaled up
            // by the one edge that difference does not count.
            let exterior = {
                let (d0, d1) = (sub(pts[kept + 1], a), sub(b, pts[k]));
                let (l0, l1) = (norm(d0), norm(d1));
                let turn = if l0 < 1e-30 || l1 < 1e-30 {
                    0.0
                } else {
                    (dot(d0, d1) / (l0 * l1)).clamp(-1.0, 1.0).acos()
                };
                let span = (k + 1 - kept) as f64;
                turn * span / (span - 1.0)
            };
            if within && exterior <= max_turn && work.collapse(chain[k], chain[kept]) {
                removed += 1;
            } else {
                kept = k;
            }
        }
    }
    if removed == 0 {
        return 0;
    }

    // Rebuild the mesh from the survivors, per-triangle arrays in lockstep,
    // unused vertices dropped.
    let mut remap = vec![u32::MAX; n_verts];
    let mut vertices = Vec::with_capacity(mesh.vertices.len());
    let mut normals = Vec::with_capacity(mesh.normals.len());
    let has_normals = mesh.normals.len() == mesh.vertices.len();
    let mut indices = Vec::with_capacity(mesh.indices.len());
    let mut face_ids = Vec::with_capacity(n_tris);
    let has_kinds = mesh.face_kinds.len() == n_tris;
    let mut face_kinds = Vec::with_capacity(if has_kinds { n_tris } else { 0 });
    for t in 0..n_tris {
        if !work.alive[t] {
            continue;
        }
        for v in work.tris[t] {
            if remap[v as usize] == u32::MAX {
                remap[v as usize] = (vertices.len() / 3) as u32;
                let k = v as usize * 3;
                vertices.extend_from_slice(&mesh.vertices[k..k + 3]);
                if has_normals {
                    normals.extend_from_slice(&mesh.normals[k..k + 3]);
                }
            }
            indices.push(remap[v as usize]);
        }
        face_ids.push(mesh.face_ids[t]);
        if has_kinds {
            face_kinds.push(mesh.face_kinds[t]);
        }
    }
    mesh.vertices = vertices;
    if has_normals {
        mesh.normals = normals;
    }
    mesh.indices = indices;
    mesh.face_ids = face_ids;
    if has_kinds {
        mesh.face_kinds = face_kinds;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// A closed prism over a regular `n`-gon of radius `r`: bottom cap
    /// (face 0), top cap (face 1), wall (face 2) — the shape a frozen rim
    /// gives a cylinder.
    fn prism(n: u32, r: f64, h: f64) -> TriangleMesh {
        let mut mesh = TriangleMesh::new();
        for z in [0.0, h] {
            for i in 0..n {
                let a = 2.0 * PI * i as f64 / n as f64;
                mesh.vertices.extend_from_slice(&[
                    (r * a.cos()) as f32,
                    (r * a.sin()) as f32,
                    z as f32,
                ]);
                mesh.normals
                    .extend_from_slice(&[a.cos() as f32, a.sin() as f32, 0.0]);
            }
        }
        let mut tri = |a: u32, b: u32, c: u32, face: u32| {
            mesh.indices.extend_from_slice(&[a, b, c]);
            mesh.face_ids.push(face);
            mesh.face_kinds.push(face as u8);
        };
        for i in 1..n - 1 {
            tri(0, i + 1, i, 0);
            tri(n, n + i, n + i + 1, 1);
        }
        for i in 0..n {
            let j = (i + 1) % n;
            tri(i, j, n + j, 2);
            tri(i, n + j, n + i, 2);
        }
        mesh
    }

    fn signed_volume(mesh: &TriangleMesh) -> f64 {
        let p = |i: u32| {
            let k = i as usize * 3;
            [
                mesh.vertices[k] as f64,
                mesh.vertices[k + 1] as f64,
                mesh.vertices[k + 2] as f64,
            ]
        };
        mesh.indices
            .chunks(3)
            .map(|t| dot(p(t[0]), cross(p(t[1]), p(t[2]))) / 6.0)
            .sum()
    }

    /// Every edge used by exactly two triangles, once each way.
    fn closed(mesh: &TriangleMesh) -> bool {
        let mut uses: HashMap<(u32, u32), i32> = HashMap::new();
        for t in mesh.indices.chunks(3) {
            for k in 0..3 {
                *uses.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
            }
        }
        uses.iter()
            .all(|(&(a, b), &n)| n == 1 && uses.get(&(b, a)) == Some(&1))
    }

    #[test]
    fn a_512_gon_wall_thins_to_the_chord_tolerance() {
        let (r, h, tol) = (76.0, 152.0, 0.02);
        let mut mesh = prism(512, r, h);
        let exact = signed_volume(&mesh);
        let before = mesh.indices.len() / 3;
        let removed = thin_feature_chains(&mut mesh, tol, 2.0 * PI / 32.0);
        let after = mesh.indices.len() / 3;

        // 0.02 mm of sag on r 76 is ~137 chords a turn; the greedy walk
        // lands on a divisor-ish spacing at or under that sag.
        let per_rim = (mesh.vertices.len() / 3) / 2;
        assert!(
            (100..=180).contains(&per_rim),
            "{per_rim} points per rim (512 before)"
        );
        assert_eq!(removed, 2 * (512 - per_rim));
        assert!(after * 2 < before, "{before} → {after} triangles");
        assert!(closed(&mesh), "thinned mesh is not closed");
        // The coarser polygon loses at most its sag's worth of area.
        let lost = (exact - signed_volume(&mesh)) / exact;
        assert!((0.0..2.0 * tol / r).contains(&lost), "volume lost {lost}");
        // Per-triangle arrays stay in lockstep, and every face survives.
        assert_eq!(mesh.face_ids.len(), after);
        assert_eq!(mesh.face_kinds.len(), after);
        for face in 0..3 {
            assert!(mesh.face_ids.contains(&face), "face {face} vanished");
        }
        assert_eq!(mesh.normals.len(), mesh.vertices.len());
        // Both rims kept the same points: the wall is still upright strips.
        let n = per_rim;
        let xy = |i: usize| (mesh.vertices[3 * i], mesh.vertices[3 * i + 1]);
        let mut bottom: Vec<_> = (0..2 * n)
            .filter(|&i| mesh.vertices[3 * i + 2] == 0.0)
            .map(|i| xy(i).0.to_bits() as u64 | ((xy(i).1.to_bits() as u64) << 32))
            .collect();
        let mut top: Vec<_> = (0..2 * n)
            .filter(|&i| mesh.vertices[3 * i + 2] != 0.0)
            .map(|i| xy(i).0.to_bits() as u64 | ((xy(i).1.to_bits() as u64) << 32))
            .collect();
        bottom.sort_unstable();
        top.sort_unstable();
        assert_eq!(bottom, top, "the two rims were thinned differently");
    }

    #[test]
    fn a_small_rim_never_drops_below_the_segment_count() {
        // r 2: a 0.02 mm sag alone would allow ~22 chords; the turn limit
        // holds it at the caller's 32.
        let mut mesh = prism(128, 2.0, 10.0);
        thin_feature_chains(&mut mesh, 0.02, 2.0 * PI / 32.0);
        let per_rim = (mesh.vertices.len() / 3) / 2;
        assert!(per_rim >= 32, "{per_rim} points per rim");
        assert!(closed(&mesh));
    }

    #[test]
    fn untagged_and_open_meshes_are_left_alone() {
        let mut untagged = prism(64, 10.0, 10.0);
        untagged.face_ids.clear();
        let before = untagged.indices.clone();
        assert_eq!(thin_feature_chains(&mut untagged, 1.0, 1.0), 0);
        assert_eq!(untagged.indices, before);

        // Drop one wall triangle: every vertex on the hole's edges locks,
        // and the rest still thins without touching them.
        let mut open = prism(64, 10.0, 10.0);
        let n = open.indices.len() / 3;
        open.indices.truncate((n - 1) * 3);
        open.face_ids.truncate(n - 1);
        open.face_kinds.truncate(n - 1);
        let hole: Vec<u32> = prism(64, 10.0, 10.0).indices[(n - 1) * 3..].to_vec();
        let hole_pos: Vec<[f32; 3]> = hole
            .iter()
            .map(|&i| {
                let k = i as usize * 3;
                [open.vertices[k], open.vertices[k + 1], open.vertices[k + 2]]
            })
            .collect();
        thin_feature_chains(&mut open, 0.5, 1.0);
        for p in hole_pos {
            assert!(
                open.vertices.chunks(3).any(|v| v == p),
                "a vertex on the open edge was removed"
            );
        }
    }
}
