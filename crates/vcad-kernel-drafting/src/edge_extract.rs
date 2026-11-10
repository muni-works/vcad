//! Sharp edge and silhouette edge detection from triangle meshes.
//!
//! Extracts edges that should be shown in technical drawings:
//! - Sharp edges: where adjacent faces have significantly different normals
//! - Silhouette edges: boundary between front-facing and back-facing faces
//! - Boundary edges: mesh boundaries (edges with only one adjacent face)

use std::collections::HashMap;

use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_tessellate::TriangleMesh;

use crate::types::{EdgeType, MeshEdge, Triangle3D, ViewDirection};

/// Default threshold angle (in radians) for sharp edge detection.
/// Edges where adjacent face normals differ by more than this are considered sharp.
pub const DEFAULT_SHARP_ANGLE: f64 = std::f64::consts::FRAC_PI_6; // 30 degrees

/// Edge key for hash map lookup (canonical vertex pair ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EdgeKey {
    v0: u32,
    v1: u32,
}

impl EdgeKey {
    fn new(a: u32, b: u32) -> Self {
        if a < b {
            Self { v0: a, v1: b }
        } else {
            Self { v0: b, v1: a }
        }
    }
}

/// Temporary edge data during extraction.
struct EdgeData {
    tri0: u32,
    tri1: Option<u32>,
}

/// Extract all mesh edges with adjacency information.
///
/// Returns edges with their adjacent triangles, which can be used to
/// classify edges as sharp, silhouette, or boundary.
pub fn extract_edges(mesh: &TriangleMesh) -> Vec<MeshEdge> {
    extract_edges_with_threshold(mesh, DEFAULT_SHARP_ANGLE)
}

/// Extract mesh edges with a custom sharp angle threshold.
///
/// # Arguments
///
/// * `mesh` - Triangle mesh to extract edges from
/// * `sharp_threshold` - Angle threshold in radians for sharp edge detection
pub fn extract_edges_with_threshold(mesh: &TriangleMesh, sharp_threshold: f64) -> Vec<MeshEdge> {
    let triangles = build_triangles(mesh);
    let edge_map = build_adjacency(mesh, &triangles);

    // Classify edges
    let mut result = Vec::with_capacity(edge_map.len());

    for (key, data) in edge_map {
        let edge_type = classify_edge(&triangles, data.tri0, data.tri1, sharp_threshold);
        result.push(MeshEdge::new(
            key.v0, key.v1, data.tri0, data.tri1, edge_type,
        ));
    }

    result
}

/// Classify an edge based on adjacent face normals.
fn classify_edge(
    triangles: &[Triangle3D],
    tri0: u32,
    tri1: Option<u32>,
    sharp_threshold: f64,
) -> EdgeType {
    match tri1 {
        None => EdgeType::Boundary,
        Some(tri1_idx) => {
            let n0 = triangles[tri0 as usize].normal;
            let n1 = triangles[tri1_idx as usize].normal;

            // Compute angle between normals
            let dot = n0.dot(n1).clamp(-1.0, 1.0);
            let angle = dot.acos();

            if angle > sharp_threshold {
                EdgeType::Sharp
            } else {
                // Will be marked as silhouette later if applicable
                EdgeType::Sharp // Default to sharp, will be filtered
            }
        }
    }
}

/// Extract only sharp edges (angle > threshold) from the mesh.
pub fn extract_sharp_edges(mesh: &TriangleMesh, sharp_threshold: f64) -> Vec<MeshEdge> {
    let triangles = build_triangles(mesh);
    let edge_map = build_adjacency(mesh, &triangles);
    let coverage = Coverage::new(&triangles);

    // Filter to sharp and boundary edges only
    let mut result = Vec::new();

    for (key, data) in edge_map {
        let is_sharp = match data.tri1 {
            None => coverage.boundary_is_feature(mesh, key, data.tri0, sharp_threshold),
            Some(tri1_idx) => {
                let n0 = triangles[data.tri0 as usize].normal;
                let n1 = triangles[tri1_idx as usize].normal;
                let dot = n0.dot(n1).clamp(-1.0, 1.0);
                let angle = dot.acos();
                angle > sharp_threshold
            }
        };

        if is_sharp {
            let edge_type = if data.tri1.is_none() {
                EdgeType::Boundary
            } else {
                EdgeType::Sharp
            };
            result.push(MeshEdge::new(
                key.v0, key.v1, data.tri0, data.tri1, edge_type,
            ));
        }
    }

    result
}

/// Extract silhouette edges for a given view direction.
///
/// A silhouette edge is one where one adjacent face is front-facing
/// and the other is back-facing relative to the view direction.
pub fn extract_silhouette_edges(mesh: &TriangleMesh, view_dir: ViewDirection) -> Vec<MeshEdge> {
    let view_vec = view_dir.view_vector();
    let triangles = build_triangles(mesh);
    let edge_map = build_adjacency(mesh, &triangles);
    let coverage = Coverage::new(&triangles);

    // Find silhouette edges
    let mut result = Vec::new();

    for (key, data) in edge_map {
        let is_silhouette = match data.tri1 {
            // Boundary edges are silhouettes from any view — unless the
            // "boundary" is a T-junction inside a flat face.
            None => coverage.boundary_is_feature(mesh, key, data.tri0, DEFAULT_SHARP_ANGLE),
            Some(tri1_idx) => is_silhouette(
                &triangles[data.tri0 as usize],
                &triangles[tri1_idx as usize],
                &view_vec,
            ),
        };

        if is_silhouette {
            result.push(MeshEdge::new(
                key.v0,
                key.v1,
                data.tri0,
                data.tri1,
                EdgeType::Silhouette,
            ));
        }
    }

    result
}

/// Extract both sharp and silhouette edges for technical drawing.
///
/// This is the primary function for generating edges for a drafting view.
/// It returns all edges that should appear in the drawing:
/// - Sharp edges (geometric features)
/// - Silhouette edges (outline of the shape from this view)
/// - Boundary edges (mesh boundaries)
pub fn extract_drawing_edges(
    mesh: &TriangleMesh,
    view_dir: ViewDirection,
    sharp_threshold: f64,
) -> Vec<MeshEdge> {
    let view_vec = view_dir.view_vector();
    let triangles = build_triangles(mesh);
    let edge_map = build_adjacency(mesh, &triangles);
    let coverage = Coverage::new(&triangles);

    // Extract relevant edges
    let mut result = Vec::new();

    for (key, data) in edge_map {
        let (include, edge_type) = match data.tri1 {
            None => (
                coverage.boundary_is_feature(mesh, key, data.tri0, sharp_threshold),
                EdgeType::Boundary,
            ),
            Some(tri1_idx) => {
                let t0 = &triangles[data.tri0 as usize];
                let t1 = &triangles[tri1_idx as usize];

                let is_silhouette = is_silhouette(t0, t1, &view_vec);

                // Check if sharp
                let dot = t0.normal.dot(t1.normal).clamp(-1.0, 1.0);
                let angle = dot.acos();
                let is_sharp = angle > sharp_threshold;

                if is_silhouette {
                    (true, EdgeType::Silhouette)
                } else if is_sharp {
                    (true, EdgeType::Sharp)
                } else {
                    (false, EdgeType::Sharp)
                }
            }
        };

        if include {
            result.push(MeshEdge::new(
                key.v0, key.v1, data.tri0, data.tri1, edge_type,
            ));
        }
    }

    result
}

/// Positions closer than this (mm) are the same vertex when building
/// adjacency. Evaluated meshes duplicate vertices per face (flat-shading
/// normals), so index-based adjacency saw every triangle edge as a mesh
/// boundary and drew every tessellation diagonal.
const WELD_TOLERANCE: f64 = 1e-5;

/// Facing below this |n·v| counts as edge-on.
const FACING_EPS: f64 = 1e-6;

fn facing(t: &Triangle3D, view_vec: &Vec3) -> i8 {
    let d = t.normal.dot(*view_vec);
    if d > FACING_EPS {
        1
    } else if d < -FACING_EPS {
        -1
    } else {
        0
    }
}

/// Silhouette between two triangles: their facings differ and at least one
/// faces the viewer. An edge-on triangle next to a front-facing one is the
/// outline; next to a back-facing one it is not. Float noise on edge-on
/// coplanar pairs no longer flips them into "silhouettes".
fn is_silhouette(t0: &Triangle3D, t1: &Triangle3D, view_vec: &Vec3) -> bool {
    let (f0, f1) = (facing(t0, view_vec), facing(t1, view_vec));
    f0 != f1 && (f0 > 0 || f1 > 0)
}

/// Edge adjacency keyed by *welded* vertex position. Each vertex maps to the
/// first vertex index sharing its (quantized) position, so the returned keys
/// still index `mesh.vertices`. Degenerate (zero-area) triangles are skipped:
/// they create spurious unpaired edges.
fn build_adjacency(mesh: &TriangleMesh, triangles: &[Triangle3D]) -> HashMap<EdgeKey, EdgeData> {
    let n_verts = mesh.vertices.len() / 3;
    let mut canon: Vec<u32> = Vec::with_capacity(n_verts);
    let mut by_pos: HashMap<(i64, i64, i64), u32> = HashMap::with_capacity(n_verts);
    let q = |c: f32| (c as f64 / WELD_TOLERANCE).round() as i64;
    for i in 0..n_verts {
        let key = (
            q(mesh.vertices[3 * i]),
            q(mesh.vertices[3 * i + 1]),
            q(mesh.vertices[3 * i + 2]),
        );
        canon.push(*by_pos.entry(key).or_insert(i as u32));
    }

    let mut edge_map: HashMap<EdgeKey, EdgeData> = HashMap::new();
    for (tri_idx, tri_indices) in mesh.indices.chunks(3).enumerate() {
        if tri_indices.len() < 3 || !triangles[tri_idx].normal.x.is_finite() {
            continue;
        }
        let [a, b, c] = [
            canon[tri_indices[0] as usize],
            canon[tri_indices[1] as usize],
            canon[tri_indices[2] as usize],
        ];
        if a == b || b == c || c == a {
            continue;
        }
        let tri_idx = tri_idx as u32;
        for key in [EdgeKey::new(a, b), EdgeKey::new(b, c), EdgeKey::new(c, a)] {
            edge_map
                .entry(key)
                .and_modify(|e| {
                    if e.tri1.is_none() {
                        e.tri1 = Some(tri_idx);
                    }
                })
                .or_insert(EdgeData {
                    tri0: tri_idx,
                    tri1: None,
                });
        }
    }
    edge_map
}

type Cell = (i64, i64, i64);

/// Spatial index over triangles that decides whether an unpaired edge is a
/// real boundary or a T-junction inside a flat face (boolean results split
/// one side of a face but not the other, leaving edges with no matching
/// partner even after welding).
struct Coverage<'a> {
    triangles: &'a [Triangle3D],
    origin: Point3,
    cell: f64,
    grid: std::cell::OnceCell<HashMap<Cell, Vec<u32>>>,
}

impl<'a> Coverage<'a> {
    fn new(triangles: &'a [Triangle3D]) -> Self {
        let mut lo = Point3::new(f64::MAX, f64::MAX, f64::MAX);
        let mut hi = Point3::new(f64::MIN, f64::MIN, f64::MIN);
        for t in triangles {
            for v in [t.v0, t.v1, t.v2] {
                lo = Point3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
                hi = Point3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
            }
        }
        let diag = if triangles.is_empty() {
            1.0
        } else {
            (hi - lo).norm().max(1e-9)
        };
        Self {
            triangles,
            origin: lo,
            cell: diag / 64.0,
            grid: std::cell::OnceCell::new(),
        }
    }

    fn cell_of(&self, p: Point3) -> Cell {
        let d = p - self.origin;
        (
            (d.x / self.cell).floor() as i64,
            (d.y / self.cell).floor() as i64,
            (d.z / self.cell).floor() as i64,
        )
    }

    fn grid(&self) -> &HashMap<Cell, Vec<u32>> {
        self.grid.get_or_init(|| {
            let mut g: HashMap<Cell, Vec<u32>> = HashMap::new();
            for (i, t) in self.triangles.iter().enumerate() {
                if !t.normal.x.is_finite() {
                    continue;
                }
                let lo = Point3::new(
                    t.v0.x.min(t.v1.x).min(t.v2.x),
                    t.v0.y.min(t.v1.y).min(t.v2.y),
                    t.v0.z.min(t.v1.z).min(t.v2.z),
                );
                let hi = Point3::new(
                    t.v0.x.max(t.v1.x).max(t.v2.x),
                    t.v0.y.max(t.v1.y).max(t.v2.y),
                    t.v0.z.max(t.v1.z).max(t.v2.z),
                );
                let (a, b) = (self.cell_of(lo), self.cell_of(hi));
                for x in a.0..=b.0 {
                    for y in a.1..=b.1 {
                        for z in a.2..=b.2 {
                            g.entry((x, y, z)).or_default().push(i as u32);
                        }
                    }
                }
            }
            g
        })
    }

    /// True if `p` lies on a triangle (other than `skip`) within `max_angle`
    /// of `normal` and within `tol` of that triangle's plane.
    fn covered(&self, p: Point3, normal: Vec3, skip: u32, max_angle: f64, tol: f64) -> bool {
        let cos_max = max_angle.cos();
        let Some(cands) = self.grid().get(&self.cell_of(p)) else {
            return false;
        };
        cands.iter().any(|&i| {
            if i == skip {
                return false;
            }
            let t = &self.triangles[i as usize];
            t.normal.dot(normal) >= cos_max
                && (p - t.v0).dot(t.normal).abs() <= tol
                && point_in_triangle_3d(p, t, tol)
        })
    }

    /// An unpaired edge is a drawing feature unless the far side of it is
    /// covered by a (nearly) coplanar triangle along its length.
    fn boundary_is_feature(
        &self,
        mesh: &TriangleMesh,
        key: EdgeKey,
        tri: u32,
        sharp_threshold: f64,
    ) -> bool {
        let t = &self.triangles[tri as usize];
        let a = get_vertex(mesh, key.v0);
        let b = get_vertex(mesh, key.v1);
        let e = b - a;
        let len = e.norm();
        if len < 1e-12 {
            return false;
        }
        // In-plane direction pointing away from the owning triangle.
        let mut out = t.normal.cross(e / len);
        if (t.centroid() - a).dot(out) > 0.0 {
            out = -out;
        }
        let step = (len * 1e-3).clamp(1e-5, 1e-2);
        let tol = step * 0.5;
        [0.2, 0.5, 0.8].iter().any(|&s| {
            let p = a + e * s + out * step;
            !self.covered(p, t.normal, tri, sharp_threshold, tol)
        })
    }
}

/// Barycentric containment in the triangle's plane, with absolute slack `tol`.
fn point_in_triangle_3d(p: Point3, t: &Triangle3D, tol: f64) -> bool {
    let v0 = t.v1 - t.v0;
    let v1 = t.v2 - t.v0;
    let v2 = p - t.v0;
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-20 {
        return false;
    }
    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    let u = 1.0 - v - w;
    let slack = tol / d00.max(d11).sqrt().max(1e-12);
    u >= -slack && v >= -slack && w >= -slack
}

/// Build Triangle3D structures from mesh data.
pub fn build_triangles(mesh: &TriangleMesh) -> Vec<Triangle3D> {
    let verts = &mesh.vertices;
    mesh.indices
        .chunks(3)
        .map(|tri| {
            let i0 = tri[0] as usize * 3;
            let i1 = tri[1] as usize * 3;
            let i2 = tri[2] as usize * 3;

            Triangle3D::new(
                Point3::new(verts[i0] as f64, verts[i0 + 1] as f64, verts[i0 + 2] as f64),
                Point3::new(verts[i1] as f64, verts[i1 + 1] as f64, verts[i1 + 2] as f64),
                Point3::new(verts[i2] as f64, verts[i2 + 1] as f64, verts[i2 + 2] as f64),
            )
        })
        .collect()
}

/// Get vertex position from mesh.
pub fn get_vertex(mesh: &TriangleMesh, idx: u32) -> Point3 {
    let i = idx as usize * 3;
    Point3::new(
        mesh.vertices[i] as f64,
        mesh.vertices[i + 1] as f64,
        mesh.vertices[i + 2] as f64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a simple cube mesh for testing.
    fn make_cube_mesh() -> TriangleMesh {
        // 8 vertices of a unit cube
        #[rustfmt::skip]
        let vertices: Vec<f32> = vec![
            0.0, 0.0, 0.0,  // 0
            1.0, 0.0, 0.0,  // 1
            1.0, 1.0, 0.0,  // 2
            0.0, 1.0, 0.0,  // 3
            0.0, 0.0, 1.0,  // 4
            1.0, 0.0, 1.0,  // 5
            1.0, 1.0, 1.0,  // 6
            0.0, 1.0, 1.0,  // 7
        ];

        // 12 triangles (2 per face)
        #[rustfmt::skip]
        let indices: Vec<u32> = vec![
            // Bottom (-Z)
            0, 2, 1, 0, 3, 2,
            // Top (+Z)
            4, 5, 6, 4, 6, 7,
            // Front (-Y)
            0, 1, 5, 0, 5, 4,
            // Back (+Y)
            2, 3, 7, 2, 7, 6,
            // Left (-X)
            0, 4, 7, 0, 7, 3,
            // Right (+X)
            1, 2, 6, 1, 6, 5,
        ];

        TriangleMesh {
            vertices,
            indices,
            normals: Vec::new(),
            face_kinds: Vec::new(),
            face_ids: Vec::new(),
        }
    }

    #[test]
    fn test_extract_edges_cube() {
        let mesh = make_cube_mesh();
        let edges = extract_edges(&mesh);

        // A cube has 12 edges, but with 2 triangles per face (12 total triangles),
        // each edge is shared by 2 triangles. The mesh we create has 18 unique edges
        // because the diagonal edges within each face are also counted.
        // 6 faces × 3 edges per triangle × 2 triangles = 36 edges
        // After deduplication by EdgeKey: 18 unique edges (12 cube edges + 6 diagonals)
        assert!(
            edges.len() >= 12,
            "Should have at least 12 cube edges, got {}",
            edges.len()
        );

        // Most edges should have 2 adjacent triangles
        let manifold_edges = edges.iter().filter(|e| e.tri1.is_some()).count();
        assert!(
            manifold_edges >= 12,
            "Should have at least 12 manifold edges"
        );
    }

    #[test]
    fn test_extract_sharp_edges_cube() {
        let mesh = make_cube_mesh();
        let edges = extract_sharp_edges(&mesh, DEFAULT_SHARP_ANGLE);

        // All 12 cube edges are sharp (90 degree angles)
        assert_eq!(edges.len(), 12, "Cube should have 12 sharp edges");
    }

    #[test]
    fn test_extract_silhouette_edges_cube() {
        let mesh = make_cube_mesh();

        // Front view: should see 4 silhouette edges (the outline)
        let edges = extract_silhouette_edges(&mesh, ViewDirection::Front);
        assert!(
            edges.len() >= 4,
            "Front view should have at least 4 silhouette edges"
        );
    }

    #[test]
    fn test_extract_drawing_edges_cube() {
        let mesh = make_cube_mesh();
        let edges = extract_drawing_edges(&mesh, ViewDirection::Front, DEFAULT_SHARP_ANGLE);

        // Should get all 12 edges (all are sharp in a cube)
        assert_eq!(edges.len(), 12);
    }

    /// The cube with every face carrying its own vertices (how evaluated
    /// meshes arrive, for flat-shading normals).
    fn unwelded(mesh: &TriangleMesh) -> TriangleMesh {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (i, &v) in mesh.indices.iter().enumerate() {
            let k = v as usize * 3;
            vertices.extend_from_slice(&mesh.vertices[k..k + 3]);
            indices.push(i as u32);
        }
        TriangleMesh {
            vertices,
            indices,
            normals: Vec::new(),
            face_kinds: Vec::new(),
            face_ids: Vec::new(),
        }
    }

    #[test]
    fn unwelded_mesh_has_no_diagonals() {
        let mesh = unwelded(&make_cube_mesh());
        for view in [
            ViewDirection::Front,
            ViewDirection::Top,
            ViewDirection::Right,
        ] {
            let edges = extract_drawing_edges(&mesh, view, DEFAULT_SHARP_ANGLE);
            assert_eq!(edges.len(), 12, "{view:?}: diagonals leaked");
        }
        assert_eq!(extract_sharp_edges(&mesh, DEFAULT_SHARP_ANGLE).len(), 12);
    }

    #[test]
    fn t_junction_inside_flat_face_is_not_an_edge() {
        // A unit square in z=0: triangle (0,0)-(1,0)-(0,1) on one side of the
        // diagonal, two triangles on the other side meeting it at its
        // midpoint — a T-junction, so no diagonal piece has an index match.
        #[rustfmt::skip]
        let vertices: Vec<f32> = vec![
            0.0, 0.0, 0.0, // 0
            1.0, 0.0, 0.0, // 1
            0.0, 1.0, 0.0, // 2
            1.0, 1.0, 0.0, // 3
            0.5, 0.5, 0.0, // 4 (midpoint of 1-2)
        ];
        let indices = vec![0, 1, 2, 1, 3, 4, 4, 3, 2];
        let mesh = TriangleMesh {
            vertices,
            indices,
            normals: Vec::new(),
            face_kinds: Vec::new(),
            face_ids: Vec::new(),
        };
        let edges = extract_sharp_edges(&mesh, DEFAULT_SHARP_ANGLE);
        // Only the 4 outer sides survive (side 1-3 and 3-2 are single edges,
        // 0-1 and 2-0 too); the diagonal pieces are interior.
        assert_eq!(edges.len(), 4, "{edges:?}");
    }

    #[test]
    fn test_triangle_normal() {
        let mesh = make_cube_mesh();
        let triangles = build_triangles(&mesh);

        // First triangle is on bottom face, normal should point -Z
        let bottom_tri = &triangles[0];
        assert!(
            bottom_tri.normal.z < -0.9,
            "Bottom face normal should point -Z"
        );
    }
}
