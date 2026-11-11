//! Build-orientation search: which way up should a part print?
//!
//! Candidate poses rest the part on each of its largest flat facets plus the
//! six axis directions. Each pose is scored by the support material it would
//! need (overhanging area × height above the plate), then by print height
//! (a proxy for time), and the list comes back best first.
//!
//! Mesh-based, so it works on anything the slicer can slice (BRep
//! tessellations, imported STL).

use serde::{Deserialize, Serialize};
use vcad_kernel_tessellate::TriangleMesh;

/// One candidate resting pose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Orientation {
    /// Row-major 3×3 rotation that takes the part from its modeled pose to
    /// this one (Z up, then dropped onto the plate).
    pub rotation: [[f64; 3]; 3],
    /// Unit normal, in the modeled frame, of the facet that rests on the plate.
    pub down: [f64; 3],
    /// Estimated support volume (mm³): projected overhang area × height.
    pub support_volume_mm3: f64,
    /// Total area of faces that need support (mm²).
    pub overhang_area_mm2: f64,
    /// Triangles that need support, by index into the mesh.
    pub overhang_triangles: Vec<usize>,
    /// Part height in this pose (mm).
    pub height_mm: f64,
    /// Area resting directly on the plate (mm²): bed adhesion.
    pub contact_area_mm2: f64,
}

impl Orientation {
    /// True when this pose needs no support at all.
    pub fn support_free(&self) -> bool {
        self.overhang_triangles.is_empty()
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
fn apply(r: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [dot(r[0], v), dot(r[1], v), dot(r[2], v)]
}

/// Rotation taking unit vector `n` onto −Z (Rodrigues).
pub fn rotation_to_down(n: [f64; 3]) -> [[f64; 3]; 3] {
    let target = [0.0, 0.0, -1.0];
    let c = dot(n, target);
    if c > 1.0 - 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    if c < -1.0 + 1e-12 {
        // n points straight up: flip about X.
        return [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]];
    }
    let v = cross(n, target);
    let k = 1.0 / (1.0 + c);
    let vx = [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]];
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let vx2: f64 = (0..3).map(|m| vx[i][m] * vx[m][j]).sum();
            r[i][j] = if i == j { 1.0 } else { 0.0 } + vx[i][j] + vx2 * k;
        }
    }
    r
}

struct Tri {
    p: [[f64; 3]; 3],
    n: [f64; 3],
    area: f64,
}

fn triangles(mesh: &TriangleMesh) -> Vec<Tri> {
    let v = |i: u32| {
        let i = i as usize * 3;
        [
            mesh.vertices[i] as f64,
            mesh.vertices[i + 1] as f64,
            mesh.vertices[i + 2] as f64,
        ]
    };
    mesh.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let p = [v(t[0]), v(t[1]), v(t[2])];
            let c = cross(sub(p[1], p[0]), sub(p[2], p[0]));
            let len = norm(c);
            let n = if len > 0.0 {
                [c[0] / len, c[1] / len, c[2] / len]
            } else {
                [0.0, 0.0, 0.0]
            };
            Tri {
                p,
                n,
                area: len / 2.0,
            }
        })
        .collect()
}

/// Score one pose. `support_angle_deg` is the steepest overhang (from
/// vertical) that prints without support — 45° for typical FDM.
fn score(tris: &[Tri], down: [f64; 3], support_angle_deg: f64) -> Orientation {
    let r = rotation_to_down(down);
    let rotated: Vec<([[f64; 3]; 3], [f64; 3], f64)> = tris
        .iter()
        .map(|t| {
            (
                [apply(&r, t.p[0]), apply(&r, t.p[1]), apply(&r, t.p[2])],
                apply(&r, t.n),
                t.area,
            )
        })
        .collect();
    let mut zmin = f64::INFINITY;
    let mut zmax = f64::NEG_INFINITY;
    for (p, _, _) in &rotated {
        for q in p {
            zmin = zmin.min(q[2]);
            zmax = zmax.max(q[2]);
        }
    }
    // A face "faces down" past the threshold when its normal is within
    // (90° − support_angle) of −Z.
    let limit = -(support_angle_deg.to_radians().sin());
    let on_plate = 0.05; // mm
    let mut out = Orientation {
        rotation: r,
        down,
        support_volume_mm3: 0.0,
        overhang_area_mm2: 0.0,
        overhang_triangles: Vec::new(),
        height_mm: (zmax - zmin).max(0.0),
        contact_area_mm2: 0.0,
    };
    for (i, (p, n, area)) in rotated.iter().enumerate() {
        if *area <= 0.0 {
            continue;
        }
        let zc = (p[0][2] + p[1][2] + p[2][2]) / 3.0 - zmin;
        let hi = p.iter().map(|q| q[2] - zmin).fold(0.0, f64::max);
        if n[2] < -0.999 && hi < on_plate {
            out.contact_area_mm2 += area;
            continue;
        }
        if n[2] < limit && zc > on_plate {
            out.overhang_triangles.push(i);
            out.overhang_area_mm2 += area;
            out.support_volume_mm3 += area * (-n[2]) * zc;
        }
    }
    out
}

/// Rank candidate resting poses, best first. At most `max` are returned.
pub fn rank_orientations(
    mesh: &TriangleMesh,
    support_angle_deg: f64,
    max: usize,
) -> Vec<Orientation> {
    let tris = triangles(mesh);
    if tris.is_empty() {
        return Vec::new();
    }
    // Candidate down-directions: the six axes, then facet normals by
    // total area (coplanar triangles merged by quantized normal).
    let mut buckets: Vec<([f64; 3], f64)> = Vec::new();
    for t in &tris {
        if t.area <= 0.0 {
            continue;
        }
        match buckets.iter_mut().find(|(n, _)| dot(*n, t.n) > 0.9995) {
            Some(b) => b.1 += t.area,
            None => buckets.push((t.n, t.area)),
        }
    }
    buckets.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut candidates: Vec<[f64; 3]> = vec![
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
    ];
    for (n, _) in buckets.iter().take(12) {
        if !candidates.iter().any(|c| dot(*c, *n) > 0.9995) {
            candidates.push(*n);
        }
    }
    let mut poses: Vec<Orientation> = candidates
        .into_iter()
        .map(|d| score(&tris, d, support_angle_deg))
        .collect();
    // Support volume first (coarse, so near-ties fall through), then bed
    // contact (a pose that barely touches the plate won't stick), then height.
    poses.sort_by(|a, b| {
        let key = |o: &Orientation| (o.support_volume_mm3 / 10.0).round();
        key(a)
            .total_cmp(&key(b))
            .then(b.contact_area_mm2.total_cmp(&a.contact_area_mm2))
            .then(a.height_mm.total_cmp(&b.height_mm))
    });
    poses.truncate(max.max(1));
    poses
}

/// Apply a rotation to a mesh in place (vertices and normals).
pub fn rotate_mesh(mesh: &mut TriangleMesh, r: &[[f64; 3]; 3]) {
    for chunk in [&mut mesh.vertices, &mut mesh.normals] {
        for v in chunk.as_chunks_mut::<3>().0 {
            let p = apply(r, [v[0] as f64, v[1] as f64, v[2] as f64]);
            v[0] = p[0] as f32;
            v[1] = p[1] as f32;
            v[2] = p[2] as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(vertices: Vec<f32>, indices: Vec<u32>) -> TriangleMesh {
        TriangleMesh {
            vertices,
            indices,
            normals: Vec::new(),
            face_kinds: Vec::new(),
            face_ids: Vec::new(),
        }
    }

    /// Axis-aligned box from `lo` to `hi`, outward-facing triangles.
    fn boxm(lo: [f32; 3], hi: [f32; 3]) -> (Vec<f32>, Vec<u32>) {
        let mut v = Vec::new();
        for i in 0..8u32 {
            v.push(if i & 1 == 0 { lo[0] } else { hi[0] });
            v.push(if i & 2 == 0 { lo[1] } else { hi[1] });
            v.push(if i & 4 == 0 { lo[2] } else { hi[2] });
        }
        let f = [
            [0, 2, 3, 1], // -z
            [4, 5, 7, 6], // +z
            [0, 1, 5, 4], // -y
            [2, 6, 7, 3], // +y
            [0, 4, 6, 2], // -x
            [1, 3, 7, 5], // +x
        ];
        let mut idx = Vec::new();
        for q in f {
            idx.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
        }
        (v, idx)
    }

    /// A "T": a 60×60×5 plate on top of a 10×10×40 post. Modeled post-down,
    /// the plate overhangs; flipped plate-down it prints with no supports.
    fn tee() -> TriangleMesh {
        let (mut v, mut i) = boxm([25.0, 25.0, 0.0], [35.0, 35.0, 40.0]);
        // The post's top face is buried in the plate: drop it.
        i.drain(6..12);
        let (v2, i2) = boxm([0.0, 0.0, 40.0], [60.0, 60.0, 45.0]);
        let base = (v.len() / 3) as u32;
        v.extend(v2);
        i.extend(i2.into_iter().map(|x| x + base));
        mesh(v, i)
    }

    #[test]
    fn rotation_maps_normal_down() {
        for n in [
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
            [0.6, 0.0, 0.8],
        ] {
            let r = rotation_to_down(n);
            let d = apply(&r, n);
            assert!((d[2] + 1.0).abs() < 1e-9, "{n:?} -> {d:?}");
            // Orthonormal: columns keep length.
            let x = apply(&r, [1.0, 0.0, 0.0]);
            assert!((norm(x) - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn cube_needs_no_support_resting_on_any_face() {
        let (v, i) = boxm([0.0; 3], [20.0; 3]);
        let poses = rank_orientations(&mesh(v, i), 45.0, 6);
        assert!(poses.iter().all(Orientation::support_free));
        assert!((poses[0].contact_area_mm2 - 400.0).abs() < 1e-6);
    }

    #[test]
    fn tee_prints_plate_down() {
        let poses = rank_orientations(&tee(), 45.0, 3);
        let best = &poses[0];
        assert!(best.support_free(), "{best:?}");
        // Plate-down = the +Z face of the plate rests on the bed.
        assert!(best.down[2] > 0.99, "down {:?}", best.down);
        // The modeled pose (post down) is worse and is flagged.
        let modeled = score(&triangles(&tee()), [0.0, 0.0, -1.0], 45.0);
        assert!(!modeled.support_free());
        assert!(modeled.overhang_area_mm2 > 3000.0);
        assert!(modeled.support_volume_mm3 > best.support_volume_mm3);
    }

    #[test]
    fn rotate_mesh_round_trips() {
        let (v, i) = boxm([0.0; 3], [10.0, 20.0, 30.0]);
        let mut m = mesh(v.clone(), i);
        let r = rotation_to_down([1.0, 0.0, 0.0]);
        rotate_mesh(&mut m, &r);
        let rt = [
            [r[0][0], r[1][0], r[2][0]],
            [r[0][1], r[1][1], r[2][1]],
            [r[0][2], r[1][2], r[2][2]],
        ];
        rotate_mesh(&mut m, &rt);
        for (a, b) in m.vertices.iter().zip(&v) {
            assert!((a - b).abs() < 1e-4);
        }
    }
}
