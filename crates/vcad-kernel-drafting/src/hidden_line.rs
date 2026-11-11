//! Hidden line removal via occlusion testing.
//!
//! Determines which edges are visible and which are hidden by checking
//! if sample points along each edge are occluded by mesh triangles.

use vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel_tessellate::TriangleMesh;

use crate::edge_extract::{
    build_triangles, extract_drawing_edges, get_vertex, DEFAULT_SHARP_ANGLE,
};
use crate::projection::ViewMatrix;
use crate::types::{MeshEdge, ProjectedEdge, ProjectedView, Triangle3D, ViewDirection, Visibility};

/// Number of sample points along each edge for occlusion testing.
const EDGE_SAMPLES: usize = 5;

/// Small offset to avoid self-intersection in occlusion tests.
const EPSILON: f64 = 1e-6;

/// Project a mesh to a 2D view with hidden line removal.
///
/// This is the main entry point for generating a technical drawing view.
/// It extracts sharp and silhouette edges, projects them to 2D, and
/// classifies each edge as visible or hidden.
pub fn project_mesh(mesh: &TriangleMesh, view_dir: ViewDirection) -> ProjectedView {
    project_mesh_with_options(mesh, view_dir, DEFAULT_SHARP_ANGLE)
}

/// Project a mesh with custom sharp angle threshold.
pub fn project_mesh_with_options(
    mesh: &TriangleMesh,
    view_dir: ViewDirection,
    sharp_threshold: f64,
) -> ProjectedView {
    let view_matrix = ViewMatrix::from_view_direction(view_dir);
    let occluders = OcclusionGrid::new(
        &build_triangles(mesh),
        &view_matrix,
        &view_dir.view_vector(),
    );
    let edges = extract_drawing_edges(mesh, view_dir, sharp_threshold);

    let mut segments = Vec::with_capacity(edges.len());

    for edge in edges {
        let v0 = get_vertex(mesh, edge.v0);
        let v1 = get_vertex(mesh, edge.v1);

        // Project endpoints
        let (p0, depth0) = view_matrix.project(v0);
        let (p1, depth1) = view_matrix.project(v1);
        let avg_depth = (depth0 + depth1) / 2.0;

        // Check visibility by sampling points along the edge
        let visibility = check_edge_visibility(v0, v1, &occluders, &view_matrix);

        let projected =
            ProjectedEdge::new(p0.into(), p1.into(), visibility, edge.edge_type, avg_depth);

        // Skip degenerate edges (e.g. an edge parallel to the view ray)
        if !projected.is_degenerate(1e-6) {
            segments.push(projected);
        }
    }

    let mut result = ProjectedView::new(view_dir);
    for edge in merge_collinear(segments) {
        result.add_edge(edge);
    }
    result
}

/// Angular tolerance (radians) for two projected segments to share a line.
const MERGE_ANGLE_TOL: f64 = 1e-4;
/// Distance tolerance (view units, mm) for line offset and interval gaps.
const MERGE_DIST_TOL: f64 = 1e-3;

/// Merge collinear, touching or overlapping projected segments into single
/// entities, so a straight model edge tessellated into many mesh edges — or
/// several model edges stacked along one sight line, like a cylinder's front
/// and back rim — becomes one line. Hidden runs covered by a visible run on
/// the same line are dropped: a solid line already shows that edge.
pub(crate) fn merge_collinear(segments: Vec<ProjectedEdge>) -> Vec<ProjectedEdge> {
    use std::collections::HashMap;

    struct Run {
        t0: f64,
        t1: f64,
        edge_type: crate::types::EdgeType,
        depth: f64,
    }
    struct Line {
        d: (f64, f64),
        c: f64,
        visible: Vec<Run>,
        hidden: Vec<Run>,
    }

    let mut lines: HashMap<(i64, i64), Line> = HashMap::new();
    let mut order: Vec<(i64, i64)> = Vec::new();
    for e in segments {
        let (dx, dy) = (e.end.x - e.start.x, e.end.y - e.start.y);
        let len = dx.hypot(dy);
        // Canonical direction: angle in [0, π).
        let (mut ux, mut uy) = (dx / len, dy / len);
        if uy < -1e-12 || (uy.abs() <= 1e-12 && ux < 0.0) {
            ux = -ux;
            uy = -uy;
        }
        let angle = uy.atan2(ux).rem_euclid(std::f64::consts::PI);
        // Signed offset of the line from the origin along its normal.
        let c = ux * e.start.y - uy * e.start.x;
        let key = (
            (angle / MERGE_ANGLE_TOL).round() as i64,
            (c / MERGE_DIST_TOL).round() as i64,
        );
        let line = lines.entry(key).or_insert_with(|| {
            order.push(key);
            Line {
                d: (ux, uy),
                c,
                visible: Vec::new(),
                hidden: Vec::new(),
            }
        });
        let (a, b) = (
            line.d.0 * e.start.x + line.d.1 * e.start.y,
            line.d.0 * e.end.x + line.d.1 * e.end.y,
        );
        let run = Run {
            t0: a.min(b),
            t1: a.max(b),
            edge_type: e.edge_type,
            depth: e.depth,
        };
        match e.visibility {
            Visibility::Visible => line.visible.push(run),
            Visibility::Hidden => line.hidden.push(run),
        }
    }

    fn union(mut runs: Vec<Run>) -> Vec<Run> {
        runs.sort_by(|a, b| a.t0.total_cmp(&b.t0));
        let mut out: Vec<Run> = Vec::new();
        for r in runs {
            match out.last_mut() {
                Some(last) if r.t0 <= last.t1 + MERGE_DIST_TOL => {
                    last.t1 = last.t1.max(r.t1);
                    last.depth = last.depth.max(r.depth);
                }
                _ => out.push(r),
            }
        }
        out
    }

    /// `runs` minus the (sorted, disjoint) `cover` intervals.
    fn subtract(runs: Vec<Run>, cover: &[Run]) -> Vec<Run> {
        let mut out = Vec::new();
        for r in runs {
            let mut pieces = vec![(r.t0, r.t1)];
            for c in cover {
                pieces = pieces
                    .into_iter()
                    .flat_map(|(a, b)| {
                        let mut v = Vec::new();
                        if c.t1 <= a || c.t0 >= b {
                            v.push((a, b));
                        } else {
                            if c.t0 > a {
                                v.push((a, c.t0));
                            }
                            if c.t1 < b {
                                v.push((c.t1, b));
                            }
                        }
                        v
                    })
                    .collect();
            }
            for (a, b) in pieces {
                if b - a > MERGE_DIST_TOL {
                    out.push(Run {
                        t0: a,
                        t1: b,
                        edge_type: r.edge_type,
                        depth: r.depth,
                    });
                }
            }
        }
        out
    }

    let mut out = Vec::new();
    for key in order {
        let line = lines.remove(&key).expect("key recorded on insert");
        let (ux, uy) = line.d;
        let (nx, ny) = (-uy, ux);
        let at = |t: f64| Point2::new(ux * t + nx * line.c, uy * t + ny * line.c);
        let visible = union(line.visible);
        let hidden = subtract(union(line.hidden), &visible);
        for (runs, vis) in [(visible, Visibility::Visible), (hidden, Visibility::Hidden)] {
            for r in runs {
                out.push(ProjectedEdge::new(
                    at(r.t0).into(),
                    at(r.t1).into(),
                    vis,
                    r.edge_type,
                    r.depth,
                ));
            }
        }
    }
    out
}

/// Check if an edge is visible or hidden.
///
/// Samples multiple points along the edge and checks each for occlusion.
/// If any sample point is occluded, the entire edge is marked as hidden.
fn check_edge_visibility(
    v0: Point3,
    v1: Point3,
    occluders: &OcclusionGrid,
    view_matrix: &ViewMatrix,
) -> Visibility {
    // Sample points along the edge
    for i in 0..EDGE_SAMPLES {
        let t = (i as f64 + 0.5) / EDGE_SAMPLES as f64;
        let sample = Point3::new(
            v0.x + t * (v1.x - v0.x),
            v0.y + t * (v1.y - v0.y),
            v0.z + t * (v1.z - v0.z),
        );

        // Project the sample point
        let (sample_2d, sample_depth) = view_matrix.project(sample);

        // Check if this point is occluded by any triangle
        if occluders.is_point_occluded(sample_2d, sample_depth) {
            return Visibility::Hidden;
        }
    }

    Visibility::Visible
}

/// A front-facing triangle projected into the view plane.
struct Occluder {
    p: [Point2; 3],
    depth: [f64; 3],
    max_depth: f64,
}

/// Uniform 2D grid over the projected front-facing triangles.
///
/// Occlusion used to test every sample point against every triangle —
/// O(edges × triangles), which on the 127k-triangle `fpga-board` sample ran
/// for tens of minutes. The grid hands back only triangles whose (padded)
/// projected bounding box covers the query cell and applies exactly the same
/// per-triangle predicate, so the answer is identical to the full scan.
struct OcclusionGrid {
    occluders: Vec<Occluder>,
    min: (f64, f64),
    cell: (f64, f64),
    side: usize,
    /// Per cell, indices into `occluders`.
    cells: Vec<Vec<u32>>,
}

impl OcclusionGrid {
    fn new(triangles: &[Triangle3D], view_matrix: &ViewMatrix, view_vec: &Vec3) -> Self {
        // Only front-facing triangles can occlude.
        let occluders: Vec<Occluder> = triangles
            .iter()
            .filter(|t| t.is_front_facing(view_vec))
            .map(|t| {
                let (p0, d0) = view_matrix.project(t.v0);
                let (p1, d1) = view_matrix.project(t.v1);
                let (p2, d2) = view_matrix.project(t.v2);
                Occluder {
                    p: [p0, p1, p2],
                    depth: [d0, d1, d2],
                    max_depth: d0.max(d1).max(d2),
                }
            })
            .collect();

        // Padded bbox: covers the barycentric epsilon of
        // `point_in_triangle_2d` with a wide margin. `None` if non-finite.
        let bbox = |o: &Occluder| {
            let xs = o.p.map(|p| p.x);
            let ys = o.p.map(|p| p.y);
            let (x0, x1) = (xs[0].min(xs[1]).min(xs[2]), xs[0].max(xs[1]).max(xs[2]));
            let (y0, y1) = (ys[0].min(ys[1]).min(ys[2]), ys[0].max(ys[1]).max(ys[2]));
            let pad = 1e-6 * (x1 - x0).max(y1 - y0) + 1e-9;
            let b = (x0 - pad, y0 - pad, x1 + pad, y1 + pad);
            [b.0, b.1, b.2, b.3]
                .iter()
                .all(|v| v.is_finite())
                .then_some(b)
        };

        let (mut lo, mut hi) = (
            (f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NEG_INFINITY),
        );
        for b in occluders.iter().filter_map(bbox) {
            lo = (lo.0.min(b.0), lo.1.min(b.1));
            hi = (hi.0.max(b.2), hi.1.max(b.3));
        }
        if !(lo.0 <= hi.0 && lo.1 <= hi.1) {
            lo = (0.0, 0.0);
            hi = (0.0, 0.0);
        }

        // ~1 triangle per cell on average, capped to bound memory.
        let side = ((occluders.len() as f64).sqrt().ceil() as usize).clamp(1, 1024);
        let cell = (
            ((hi.0 - lo.0) / side as f64).max(1e-12),
            ((hi.1 - lo.1) / side as f64).max(1e-12),
        );
        let mut grid = Self {
            occluders: Vec::new(),
            min: lo,
            cell,
            side,
            cells: vec![Vec::new(); side * side],
        };
        for (i, o) in occluders.iter().enumerate() {
            // A non-finite triangle goes in every cell, to stay exactly
            // equivalent to the full scan.
            let (cx0, cy0, cx1, cy1) = match bbox(o) {
                Some(b) => {
                    let (cx0, cy0) = grid.cell_of(b.0, b.1);
                    let (cx1, cy1) = grid.cell_of(b.2, b.3);
                    (cx0, cy0, cx1, cy1)
                }
                None => (0, 0, side - 1, side - 1),
            };
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    grid.cells[cy * side + cx].push(i as u32);
                }
            }
        }
        grid.occluders = occluders;
        grid
    }

    /// Grid cell containing `(x, y)`, clamped to the grid.
    fn cell_of(&self, x: f64, y: f64) -> (usize, usize) {
        let f = |v: f64, lo: f64, c: f64| {
            let i = ((v - lo) / c).floor();
            if i.is_nan() || i < 0.0 {
                0
            } else {
                (i as usize).min(self.side - 1)
            }
        };
        (f(x, self.min.0, self.cell.0), f(y, self.min.1, self.cell.1))
    }

    /// Check if a projected point is occluded by any triangle.
    ///
    /// A point is occluded if there's a front-facing triangle that:
    /// 1. Contains the point's 2D projection
    /// 2. Is in front of the point (larger depth value — depth increases
    ///    toward the viewer)
    fn is_point_occluded(&self, point_2d: Point2, point_depth: f64) -> bool {
        // A point outside the grid lands in a clamped border cell; the exact
        // per-triangle test below still decides.
        let (cx, cy) = self.cell_of(point_2d.x, point_2d.y);
        self.cells[cy * self.side + cx].iter().any(|&i| {
            let o = &self.occluders[i as usize];
            // Quick depth check: triangle entirely behind the point.
            if o.max_depth <= point_depth + EPSILON {
                return false;
            }
            let [a, b, c] = o.p;
            if !point_in_triangle_2d(point_2d, a, b, c) {
                return false;
            }
            let [da, db, dc] = o.depth;
            // In front of the point (closer to the viewer)?
            interpolate_depth_at_point(point_2d, a, b, c, da, db, dc)
                .is_some_and(|d| d > point_depth + EPSILON)
        })
    }
}

/// Check if a 2D point is inside a 2D triangle using barycentric coordinates.
fn point_in_triangle_2d(p: Point2, a: Point2, b: Point2, c: Point2) -> bool {
    let v0 = Vec3::new(c.x - a.x, c.y - a.y, 0.0);
    let v1 = Vec3::new(b.x - a.x, b.y - a.y, 0.0);
    let v2 = Vec3::new(p.x - a.x, p.y - a.y, 0.0);

    let dot00 = v0.dot(v0);
    let dot01 = v0.dot(v1);
    let dot02 = v0.dot(v2);
    let dot11 = v1.dot(v1);
    let dot12 = v1.dot(v2);

    let inv_denom = 1.0 / (dot00 * dot11 - dot01 * dot01);
    let u = (dot11 * dot02 - dot01 * dot12) * inv_denom;
    let v = (dot00 * dot12 - dot01 * dot02) * inv_denom;

    // Check if point is in triangle (with small epsilon for numerical stability)
    let eps = 1e-8;
    u >= -eps && v >= -eps && (u + v) <= 1.0 + eps
}

/// Interpolate depth at a 2D point using barycentric coordinates.
fn interpolate_depth_at_point(
    p: Point2,
    a: Point2,
    b: Point2,
    c: Point2,
    da: f64,
    db: f64,
    dc: f64,
) -> Option<f64> {
    let v0 = Vec3::new(c.x - a.x, c.y - a.y, 0.0);
    let v1 = Vec3::new(b.x - a.x, b.y - a.y, 0.0);
    let v2 = Vec3::new(p.x - a.x, p.y - a.y, 0.0);

    let dot00 = v0.dot(v0);
    let dot01 = v0.dot(v1);
    let dot02 = v0.dot(v2);
    let dot11 = v1.dot(v1);
    let dot12 = v1.dot(v2);

    let denom = dot00 * dot11 - dot01 * dot01;
    if denom.abs() < 1e-12 {
        return None; // Degenerate triangle
    }

    let inv_denom = 1.0 / denom;
    let u = (dot11 * dot02 - dot01 * dot12) * inv_denom;
    let v = (dot00 * dot12 - dot01 * dot02) * inv_denom;
    let w = 1.0 - u - v;

    Some(w * da + v * db + u * dc)
}

/// Classify visibility for a pre-extracted set of edges.
///
/// Useful when you've already extracted edges and want to check visibility
/// for multiple view directions.
pub fn classify_edge_visibility(
    edges: &[MeshEdge],
    mesh: &TriangleMesh,
    view_dir: ViewDirection,
) -> Vec<(MeshEdge, Visibility)> {
    let view_matrix = ViewMatrix::from_view_direction(view_dir);
    let view_vec = view_dir.view_vector();
    let occluders = OcclusionGrid::new(&build_triangles(mesh), &view_matrix, &view_vec);

    edges
        .iter()
        .map(|edge| {
            let v0 = get_vertex(mesh, edge.v0);
            let v1 = get_vertex(mesh, edge.v1);

            let visibility = check_edge_visibility(v0, v1, &occluders, &view_matrix);

            (edge.clone(), visibility)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a simple cube mesh for testing.
    fn make_cube_mesh() -> TriangleMesh {
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
    fn test_project_mesh_cube() {
        let mesh = make_cube_mesh();
        let view = project_mesh(&mesh, ViewDirection::Front);

        // Should have some visible and some hidden edges
        assert!(!view.edges.is_empty());
        assert!(view.bounds.is_valid());

        // Front view of a cube should have 4 visible edges (the front face outline)
        // and 8 hidden edges (the back face and connecting edges)
        let visible = view.num_visible();
        let _hidden = view.num_hidden();

        // At minimum, we should see some edges
        assert!(visible > 0, "Should have visible edges");
    }

    #[test]
    fn test_project_mesh_top_view() {
        let mesh = make_cube_mesh();
        let view = project_mesh(&mesh, ViewDirection::Top);

        assert!(!view.edges.is_empty());
        assert!(view.bounds.is_valid());
    }

    #[test]
    fn test_point_in_triangle() {
        let a = Point2::new(0.0, 0.0);
        let b = Point2::new(1.0, 0.0);
        let c = Point2::new(0.0, 1.0);

        // Inside
        assert!(point_in_triangle_2d(Point2::new(0.2, 0.2), a, b, c));

        // Outside
        assert!(!point_in_triangle_2d(Point2::new(1.0, 1.0), a, b, c));

        // On edge
        assert!(point_in_triangle_2d(Point2::new(0.5, 0.0), a, b, c));
    }

    #[test]
    fn test_isometric_projection() {
        let mesh = make_cube_mesh();
        let view = project_mesh(&mesh, ViewDirection::ISOMETRIC_STANDARD);

        assert!(!view.edges.is_empty());
        assert!(view.bounds.is_valid());

        // Isometric view should show all 12 edges (some visible, some hidden)
        assert_eq!(view.edges.len(), 12);
    }

    #[test]
    fn test_bounding_box_computed() {
        let mesh = make_cube_mesh();
        let view = project_mesh(&mesh, ViewDirection::Front);

        // Bounding box should be approximately 1x1 (unit cube)
        assert!(view.bounds.width() > 0.9 && view.bounds.width() < 1.1);
        assert!(view.bounds.height() > 0.9 && view.bounds.height() < 1.1);
    }

    /// Un-weld a mesh: every triangle gets its own three vertices, the way
    /// kernel tessellations hand faces over (per-face vertex copies).
    fn soup(mesh: &TriangleMesh) -> TriangleMesh {
        let mut vertices = Vec::new();
        for &i in &mesh.indices {
            let i = i as usize * 3;
            vertices.extend_from_slice(&mesh.vertices[i..i + 3]);
        }
        TriangleMesh {
            indices: (0..mesh.indices.len() as u32).collect(),
            vertices,
            normals: Vec::new(),
            face_kinds: Vec::new(),
            face_ids: Vec::new(),
        }
    }

    /// Closed cylinder, axis +Z, `n` side facets, outward winding.
    fn make_cylinder_mesh(r: f32, h: f32, n: u32) -> TriangleMesh {
        let mut vertices = vec![0.0, 0.0, 0.0, 0.0, 0.0, h]; // 0 bottom centre, 1 top centre
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            let (x, y) = (r * a.cos(), r * a.sin());
            vertices.extend_from_slice(&[x, y, 0.0, x, y, h]);
        }
        let b = |i: u32| 2 + 2 * (i % n);
        let t = |i: u32| 3 + 2 * (i % n);
        let mut indices = Vec::new();
        for i in 0..n {
            indices.extend_from_slice(&[0, b(i + 1), b(i)]); // bottom (-Z)
            indices.extend_from_slice(&[1, t(i), t(i + 1)]); // top (+Z)
            indices.extend_from_slice(&[b(i), b(i + 1), t(i + 1)]);
            indices.extend_from_slice(&[b(i), t(i + 1), t(i)]);
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
    fn cube_front_view_is_four_visible_lines() {
        // Both welded and per-face (soup) meshes: the soup used to draw every
        // triangle edge, diagonals included, as a boundary edge.
        for mesh in [make_cube_mesh(), soup(&make_cube_mesh())] {
            let view = project_mesh(&mesh, ViewDirection::Front);
            assert_eq!(view.num_visible(), 4, "{:?}", view.edges);
            assert_eq!(view.num_hidden(), 0, "{:?}", view.edges);
        }
    }

    #[test]
    fn cube_has_twelve_model_edges_even_as_soup() {
        let edges =
            crate::edge_extract::extract_sharp_edges(&soup(&make_cube_mesh()), DEFAULT_SHARP_ANGLE);
        assert_eq!(edges.len(), 12);
    }

    #[test]
    fn cylinder_front_view_is_two_silhouettes_plus_caps() {
        for mesh in [
            make_cylinder_mesh(1.0, 2.0, 32),
            soup(&make_cylinder_mesh(1.0, 2.0, 32)),
        ] {
            let view = project_mesh(&mesh, ViewDirection::Front);
            assert_eq!(view.num_hidden(), 0, "{:?}", view.edges);
            assert_eq!(view.num_visible(), 4, "{:?}", view.edges);
            let vertical = view
                .edges
                .iter()
                .filter(|e| (e.end.x - e.start.x).abs() < 1e-6)
                .count();
            assert_eq!(vertical, 2, "two silhouette generators");
        }
    }
}
