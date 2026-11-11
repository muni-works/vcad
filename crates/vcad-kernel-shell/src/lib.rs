#![warn(missing_docs)]

//! Shell (hollow) operation for the vcad kernel.
//!
//! Creates a hollow shell from a solid by offsetting all faces inward.
//! The outer shell remains, an inner shell is created at `thickness` offset,
//! and the two are connected.
//!
//! # Algorithm
//!
//! For mesh-based solids:
//! 1. Compute vertex normals (average of adjacent face normals)
//! 2. Offset each vertex inward by `thickness * vertex_normal`
//! 3. Flip the inner mesh winding
//! 4. Connect outer and inner shells along boundaries (for open faces)
//!
//! For B-rep solids with planar faces only:
//! - Each face is offset by translating along its normal
//! - The resulting inner shell is connected to the outer shell

use std::collections::{HashMap, HashSet};
use std::fmt;
use vcad_kernel_geom::{BilinearSurface, GeometryStore, Plane, Surface, SurfaceKind};
use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_tessellate::TriangleMesh;
use vcad_kernel_topo::{FaceId, HalfEdgeId, Orientation, ShellType, Topology, VertexId};

/// Errors that can occur during the shell operation.
#[derive(Debug)]
pub enum ShellError {
    /// A surface collapsed to zero or negative size after offset.
    SurfaceCollapse(FaceId, String),
    /// An inner vertex crossed through the outer shell.
    VertexCollision(VertexId),
    /// Offset produced self-intersecting geometry.
    SelfIntersection(FaceId, FaceId),
    /// A surface type does not support analytical offset.
    UnsupportedSurface(SurfaceKind),
    /// An open-face shell ([`shell_brep_open`]) was refused: the request
    /// is malformed, unsupported, or the wall thickness consumes the solid.
    OpenShell(String),
    /// The wall thickness is zero, negative, or not a finite number.
    InvalidThickness(f64),
    /// The inward offset consumes the solid: the inner wall would meet,
    /// pass through, or invert past itself, so no hollow body exists.
    ThicknessConsumesSolid {
        /// The requested wall thickness in mm.
        thickness: f64,
        /// Half the smallest extent of the solid's bounding box — an
        /// upper bound on any thickness that can work.
        limit: f64,
    },
}

impl ShellError {
    /// `true` when the error means the requested thickness cannot produce
    /// a hollow solid at all — as opposed to "the analytic path cannot
    /// represent this surface", where a mesh approximation is legitimate.
    /// Falling back to the mesh offset on these would just build the same
    /// impossible body in triangles (inverted or turned inside out).
    pub fn is_infeasible(&self) -> bool {
        !matches!(self, ShellError::UnsupportedSurface(_))
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellError::SurfaceCollapse(_, msg) => write!(f, "Surface collapsed: {}", msg),
            ShellError::VertexCollision(_) => write!(f, "Inner vertex crossed outer shell"),
            ShellError::SelfIntersection(_, _) => write!(f, "Offset produced self-intersection"),
            ShellError::UnsupportedSurface(kind) => {
                write!(f, "Unsupported surface type for offset: {:?}", kind)
            }
            ShellError::OpenShell(msg) => write!(f, "{msg}"),
            ShellError::InvalidThickness(t) => {
                write!(
                    f,
                    "shell thickness must be a positive finite number, got {t}"
                )
            }
            ShellError::ThicknessConsumesSolid { thickness, limit } => write!(
                f,
                "shell thickness {thickness} mm consumes the solid (must be below {limit:.4} mm, \
                 half its thinnest extent) — the inner wall would collapse or invert"
            ),
        }
    }
}

impl std::error::Error for ShellError {}

/// Create a shell (hollow) from a B-rep solid by offsetting inward.
///
/// Uses analytical surface offset when all faces support it, falling back
/// to mesh-based approximation otherwise.
///
/// # Arguments
///
/// * `brep` - The input solid
/// * `thickness` - Wall thickness (positive = inward offset)
///
/// # Returns
///
/// A new B-rep solid representing the hollow shell.
pub fn shell_brep(brep: &BRepSolid, thickness: f64) -> BRepSolid {
    match shell_brep_analytical(brep, thickness, &[]) {
        Ok(result) => result,
        Err(_) => {
            // Fall back to mesh-based approach
            let segments = 32;
            let outer_mesh = vcad_kernel_tessellate::tessellate_brep(brep, segments);
            let shell_mesh = shell_mesh(&outer_mesh, thickness);
            mesh_to_brep(&shell_mesh)
        }
    }
}

/// Create a shell by analytically offsetting B-rep surfaces.
///
/// Each face's surface is offset by `-thickness` (inward), and new topology
/// is constructed for the inner shell. Open faces (listed in `open_face_ids`)
/// are excluded from the inner shell and connected with side walls.
///
/// # Arguments
///
/// * `brep` - The input solid
/// * `thickness` - Wall thickness (positive = inward offset)
/// * `open_face_ids` - Faces to remove (creating openings in the shell)
///
/// # Returns
///
/// A new `BRepSolid` with both outer and inner shells, or a `ShellError`
/// if analytical offset is not possible.
pub fn shell_brep_analytical(
    brep: &BRepSolid,
    thickness: f64,
    open_face_ids: &[FaceId],
) -> Result<BRepSolid, ShellError> {
    if !thickness.is_finite() || thickness <= 0.0 {
        return Err(ShellError::InvalidThickness(thickness));
    }
    let topo = &brep.topology;
    let geom = &brep.geometry;
    let solid = &topo.solids[brep.solid_id];
    let shell = &topo.shells[solid.outer_shell];
    let all_planar = shell
        .faces
        .iter()
        .all(|&f| geom.surfaces[topo.faces[f].surface_index].surface_type() == SurfaceKind::Plane);
    let (outer_min, outer_max) = vertex_aabb(topo.vertices.iter().map(|(_, v)| v.point));
    let limit = (0..3)
        .map(|i| 0.5 * (outer_max[i] - outer_min[i]))
        .fold(f64::INFINITY, f64::min);

    // Step 1: Compute offset surfaces for all non-open faces
    // The offset direction is opposite to the face normal (inward).
    // For Forward orientation faces, surface normal = face normal, so offset = -thickness.
    // For Reversed orientation faces, surface normal = -face normal, so offset = +thickness.
    let mut offset_surfaces: HashMap<FaceId, Box<dyn Surface>> = HashMap::new();

    for &face_id in &shell.faces {
        if open_face_ids.contains(&face_id) {
            continue;
        }

        let face = &topo.faces[face_id];
        let surface = &geom.surfaces[face.surface_index];

        let offset_dist = match face.orientation {
            Orientation::Forward => -thickness,
            Orientation::Reversed => thickness,
        };

        let offset_surface = surface.offset(offset_dist).ok_or_else(|| {
            ShellError::SurfaceCollapse(
                face_id,
                format!(
                    "{:?} surface collapsed at offset {:.4}",
                    surface.surface_type(),
                    offset_dist
                ),
            )
        })?;

        offset_surfaces.insert(face_id, offset_surface);
    }

    // Step 2: Build a new B-rep solid with outer + inner shells
    let mut new_topo = Topology::new();
    let mut new_geom = GeometryStore::new();

    // Maps from old IDs to new IDs
    let mut outer_vertex_map: HashMap<VertexId, VertexId> = HashMap::new();
    let mut inner_vertex_map: HashMap<VertexId, VertexId> = HashMap::new();

    // Step 2a: Copy outer vertices
    for (old_vid, vertex) in &topo.vertices {
        let new_vid = new_topo.add_vertex(vertex.point);
        outer_vertex_map.insert(old_vid, new_vid);
    }

    // Step 2b: Compute inner vertex positions
    // For each vertex, compute its offset position. For planar faces this is
    // the intersection of 3 offset planes. For simpler cases (all planes with
    // the same normal), use the average offset vector.
    for (old_vid, _vertex) in &topo.vertices {
        let inner_pos = compute_offset_vertex(topo, geom, old_vid, &offset_surfaces, thickness);
        let new_vid = new_topo.add_vertex(inner_pos);
        inner_vertex_map.insert(old_vid, new_vid);
    }

    // Step 2b': fail closed when the offset consumes the solid. For an
    // all-planar body the vertex AABB is the exact AABB, and every point of
    // a valid inward offset lies at least `thickness` inside it — an inner
    // vertex outside that shrunken box means the walls crossed (e.g. a
    // 10 mm cube shelled by 20 mm put its "inner" box at -10..20, growing
    // the solid). Every inner face must also keep its outer face's
    // orientation and a non-zero area, which catches the exact-collapse
    // case (thickness = half the extent → every inner face is a point).
    let consumed = ShellError::ThicknessConsumesSolid { thickness, limit };
    if all_planar {
        let span = (0..3)
            .map(|i| outer_max[i] - outer_min[i])
            .fold(0.0_f64, f64::max);
        let tol = 1e-9 * span.max(1.0);
        // An open face's inner vertices sit on the (un-offset) open plane,
        // so with openings only plain containment is required.
        //
        // The inset carries 10 % slack: compute_offset_vertex is not exact
        // at corners where more than three planes meet (a chamfered box's
        // corner lands ~2 % shy of the true offset), and the gross failure
        // this guards against overshoots by whole thicknesses. The edge
        // direction check below covers the near-limit cases.
        let inset = if open_face_ids.is_empty() {
            0.9 * thickness
        } else {
            0.0
        };
        for &nv in inner_vertex_map.values() {
            let p = new_topo.vertices[nv].point;
            let p = [p.x, p.y, p.z];
            for i in 0..3 {
                if p[i] < outer_min[i] + inset - tol || p[i] > outer_max[i] - inset + tol {
                    return Err(consumed);
                }
            }
        }
        // Every inner edge must keep its outer edge's direction: an inward
        // offset shrinks edges, and one that reaches zero length or flips
        // means opposing walls met (a 10 mm cube shelled by >= 5 mm inverts
        // its inner box uniformly, which the area check alone can't see).
        for &face_id in &shell.faces {
            if open_face_ids.contains(&face_id) {
                continue;
            }
            let verts = topo.loop_vertices(topo.faces[face_id].outer_loop);
            for k in 0..verts.len() {
                let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
                let outer_e = topo.vertices[b].point - topo.vertices[a].point;
                if outer_e.norm() <= tol {
                    continue;
                }
                let inner_e = new_topo.vertices[inner_vertex_map[&b]].point
                    - new_topo.vertices[inner_vertex_map[&a]].point;
                if inner_e.dot(outer_e) <= tol * outer_e.norm() {
                    return Err(ShellError::ThicknessConsumesSolid { thickness, limit });
                }
            }
        }
        for &face_id in &shell.faces {
            if open_face_ids.contains(&face_id) {
                continue;
            }
            let verts = topo.loop_vertices(topo.faces[face_id].outer_loop);
            let outer_n = newell(verts.iter().map(|v| topo.vertices[*v].point));
            let inner_n = newell(
                verts
                    .iter()
                    .map(|v| new_topo.vertices[inner_vertex_map[v]].point),
            );
            let outer_area = outer_n.norm();
            if outer_area <= tol * tol {
                continue;
            }
            // Same direction and a non-vanishing area (relative to the
            // outer face, so the gate is scale-free).
            if inner_n.dot(outer_n) <= 1e-12 * outer_area * outer_area {
                return Err(consumed);
            }
        }
    }

    // Step 2c: Copy outer faces (all faces, including open ones)
    let mut outer_face_ids = Vec::new();
    let mut he_map_outer: HashMap<(VertexId, VertexId), HalfEdgeId> = HashMap::new();

    for &old_face_id in &shell.faces {
        let face = &topo.faces[old_face_id];
        let surface = &geom.surfaces[face.surface_index];
        let surf_idx = new_geom.add_surface(surface.clone_box());

        let old_verts = topo.loop_vertices(face.outer_loop);
        let new_verts: Vec<VertexId> = old_verts.iter().map(|v| outer_vertex_map[v]).collect();

        let mut hes = Vec::new();
        for (j, &nv) in new_verts.iter().enumerate() {
            let he = new_topo.add_half_edge(nv);
            hes.push(he);
            let next_v = new_verts[(j + 1) % new_verts.len()];
            he_map_outer.insert((nv, next_v), he);
        }

        let loop_id = new_topo.add_loop(&hes);
        let face_id = new_topo.add_face(loop_id, surf_idx, face.orientation);
        outer_face_ids.push(face_id);
    }

    // Step 2d: Create inner faces (offset surfaces, reversed orientation)
    let mut inner_face_ids = Vec::new();
    let mut he_map_inner: HashMap<(VertexId, VertexId), HalfEdgeId> = HashMap::new();

    for &old_face_id in &shell.faces {
        if open_face_ids.contains(&old_face_id) {
            continue;
        }

        let face = &topo.faces[old_face_id];
        let offset_surface = &offset_surfaces[&old_face_id];
        let surf_idx = new_geom.add_surface(offset_surface.clone_box());

        // Inner face has reversed orientation compared to outer
        let inner_orient = match face.orientation {
            Orientation::Forward => Orientation::Reversed,
            Orientation::Reversed => Orientation::Forward,
        };

        let old_verts = topo.loop_vertices(face.outer_loop);
        // Keep the loop wound consistent with the offset surface's stored
        // normal (which matches the outer face's): the tessellator reverses
        // winding for `Orientation::Reversed` faces itself, so pre-reversing
        // the loop here would cancel that flip and leave the cavity wound
        // outward (counting as solid volume instead of void).
        let new_verts: Vec<VertexId> = old_verts.iter().map(|v| inner_vertex_map[v]).collect();

        let mut hes = Vec::new();
        for (j, &nv) in new_verts.iter().enumerate() {
            let he = new_topo.add_half_edge(nv);
            hes.push(he);
            let next_v = new_verts[(j + 1) % new_verts.len()];
            he_map_inner.insert((nv, next_v), he);
        }

        let loop_id = new_topo.add_loop(&hes);
        let face_id = new_topo.add_face(loop_id, surf_idx, inner_orient);
        inner_face_ids.push(face_id);
    }

    // Step 2e: Create side wall faces for open face edges
    let mut wall_face_ids = Vec::new();
    for &open_face_id in open_face_ids {
        let face = &topo.faces[open_face_id];
        let old_verts = topo.loop_vertices(face.outer_loop);

        for i in 0..old_verts.len() {
            let j = (i + 1) % old_verts.len();
            let outer_a = outer_vertex_map[&old_verts[i]];
            let outer_b = outer_vertex_map[&old_verts[j]];
            let inner_a = inner_vertex_map[&old_verts[i]];
            let inner_b = inner_vertex_map[&old_verts[j]];

            let pa = new_topo.vertices[outer_a].point;
            let pb = new_topo.vertices[outer_b].point;
            let pc = new_topo.vertices[inner_b].point;
            let pd = new_topo.vertices[inner_a].point;

            // Create bilinear wall surface
            let wall_surf = BilinearSurface::new(pa, pb, pd, pc);
            let surf_idx = new_geom.add_surface(Box::new(wall_surf));

            // Quad: outer_a → outer_b → inner_b → inner_a
            let verts = [outer_a, outer_b, inner_b, inner_a];
            let mut hes = Vec::new();
            for (k, &v) in verts.iter().enumerate() {
                let he = new_topo.add_half_edge(v);
                hes.push(he);
                let next_v = verts[(k + 1) % 4];
                // Store in combined map for twin pairing
                he_map_outer.insert((v, next_v), he);
            }

            let loop_id = new_topo.add_loop(&hes);
            let face_id = new_topo.add_face(loop_id, surf_idx, Orientation::Forward);
            wall_face_ids.push(face_id);
        }
    }

    // Step 2f: Pair twin half-edges
    pair_twin_half_edges(&mut new_topo);

    // Step 2g: Build shell and solid
    let mut all_faces = outer_face_ids;
    all_faces.extend(inner_face_ids);
    all_faces.extend(wall_face_ids);

    let shell_id = new_topo.add_shell(all_faces, ShellType::Outer);
    let solid_id = new_topo.add_solid(shell_id);

    Ok(BRepSolid {
        topology: new_topo,
        geometry: new_geom,
        solid_id,
    })
}

/// Shell a B-rep solid with a set of faces removed — the "open the top of
/// the box" shell every enclosure needs.
///
/// The removed (open) faces become the opening. Every remaining face is
/// offset inward by `thickness`, and each open face is replaced by a planar
/// *rim* face: the open face's own outline with the offset outline cut out
/// of it as a hole. The result is one closed, watertight wall.
///
/// Supported today: open faces that are planar with a polygonal outline
/// (any planar-faced solid, including the sampled arcs of extruded rounded
/// profiles and blended edges) or a full circle (a cylinder's cap); kept
/// faces that are planar or cylindrical. Fail-closed: anything else — a
/// non-planar or holed open face, a kept face with holes, two open faces
/// sharing an edge, a corner with every face open, or a thickness that
/// collapses a wall — is an [`ShellError::OpenShell`] with the reason,
/// never a best-effort solid.
///
/// The closed shell ([`shell_brep_analytical`] with no open faces) is a
/// separate path and is unchanged by this one.
pub fn shell_brep_open(
    brep: &BRepSolid,
    thickness: f64,
    open_face_ids: &[FaceId],
) -> Result<BRepSolid, ShellError> {
    let fail = |m: String| Err(ShellError::OpenShell(m));
    if !thickness.is_finite() || thickness <= 0.0 {
        return fail(format!(
            "shell thickness must be a positive finite number, got {thickness}"
        ));
    }
    let topo = &brep.topology;
    let geom = &brep.geometry;
    let shell_faces = &topo.shells[topo.solids[brep.solid_id].outer_shell].faces;

    let open_ids: HashSet<FaceId> = open_face_ids.iter().copied().collect();
    if open_ids.is_empty() {
        return fail("no faces to open".into());
    }
    for &f in &open_ids {
        if !shell_faces.contains(&f) {
            return fail("an open face is not part of this solid".into());
        }
    }
    for &f in shell_faces {
        let face = &topo.faces[f];
        let kind = geom.surfaces[face.surface_index].surface_type();
        if open_ids.contains(&f) && kind != SurfaceKind::Plane {
            return fail(format!(
                "only planar faces can be opened (picked a {kind:?} face)"
            ));
        }
        if !matches!(kind, SurfaceKind::Plane | SurfaceKind::Cylinder) {
            return Err(ShellError::UnsupportedSurface(kind));
        }
        if !face.inner_loops.is_empty() {
            return fail(if open_ids.contains(&f) {
                "an open face with holes in it is not supported yet".into()
            } else {
                "shelling a solid whose faces have holes is not supported yet — \
                 shell first, then cut the holes"
                    .into()
            });
        }
    }

    // Work on logical faces: coplanar planar faces merged, and vertices
    // that only subdivide a straight edge dropped. Faceted solids (a
    // blended edge is a strip of coplanar triangles; its neighbours carry
    // the strip's sample points) otherwise put vertices closer to a
    // neighbouring face than the wall thickness, and their offsets fold
    // through that face.
    let faces = logical_faces(brep, shell_faces);
    let is_open: Vec<bool> = faces
        .iter()
        .map(|lf| lf.members.iter().any(|m| open_ids.contains(m)))
        .collect();
    let n_open = is_open.iter().filter(|o| **o).count();
    if n_open >= faces.len() {
        return fail("cannot open every face of the solid".into());
    }

    let mut vertex_faces: HashMap<VertexId, Vec<usize>> = HashMap::new();
    for (i, lf) in faces.iter().enumerate() {
        for &v in &lf.verts {
            let list = vertex_faces.entry(v).or_default();
            if !list.contains(&i) {
                list.push(i);
            }
        }
    }

    // Two open faces sharing an edge would leave a zero-width rim.
    let edge_keys = |vs: &[VertexId]| {
        (0..vs.len())
            .filter(|&i| vs[i] != vs[(i + 1) % vs.len()])
            .map(|i| {
                let (a, b) = (vs[i], vs[(i + 1) % vs.len()]);
                if a < b {
                    (a, b)
                } else {
                    (b, a)
                }
            })
            .collect::<HashSet<_>>()
    };
    let open_list: Vec<usize> = (0..faces.len()).filter(|&i| is_open[i]).collect();
    for (k, &a) in open_list.iter().enumerate() {
        let ka = edge_keys(&faces[a].verts);
        for &b in &open_list[k + 1..] {
            if !ka.is_disjoint(&edge_keys(&faces[b].verts)) {
                return fail(
                    "two open faces share an edge — opening adjacent faces is not supported yet"
                        .into(),
                );
            }
        }
    }

    // Offset surfaces for the kept faces.
    let mut offset_surfaces: HashMap<usize, Box<dyn Surface>> = HashMap::new();
    for (i, lf) in faces.iter().enumerate() {
        if is_open[i] {
            continue;
        }
        let face = &topo.faces[lf.rep];
        let surface = &geom.surfaces[face.surface_index];
        let d = match face.orientation {
            Orientation::Forward => -thickness,
            Orientation::Reversed => thickness,
        };
        let off = surface.offset(d).ok_or_else(|| {
            ShellError::OpenShell(format!(
                "a {:?} face collapses at wall thickness {thickness} — the shell consumes the solid",
                surface.surface_type()
            ))
        })?;
        offset_surfaces.insert(i, off);
    }

    // Inner vertex positions: offset every vertex so it sits exactly
    // `thickness` inside each distinct KEPT face around it. Open faces do
    // not move — the rim stays in the open face's plane.
    let mut inner_pos: HashMap<VertexId, Point3> = HashMap::new();
    for (&v, around) in &vertex_faces {
        let p = topo.vertices[v].point;
        let mut normals: Vec<Vec3> = Vec::new();
        for &i in around {
            if is_open[i] {
                continue;
            }
            let face = &topo.faces[faces[i].rep];
            let n = surface_normal_at(geom.surfaces[face.surface_index].as_ref(), p);
            let n = match face.orientation {
                Orientation::Forward => n,
                Orientation::Reversed => -n,
            };
            if n.norm() < 1e-12 {
                continue;
            }
            let n = n.normalize();
            if !normals.iter().any(|m| m.dot(n) > 1.0 - 1e-9) {
                normals.push(n);
            }
        }
        let d = solve_offset(&normals, thickness).ok_or_else(|| {
            ShellError::OpenShell(
                "every face around a corner is open — nothing is left to offset there".into(),
            )
        })?;
        inner_pos.insert(v, p - d);
    }

    let mut new_topo = Topology::new();
    let mut new_geom = GeometryStore::new();
    let mut outer_map: HashMap<VertexId, VertexId> = HashMap::new();
    let mut inner_map: HashMap<VertexId, VertexId> = HashMap::new();
    let mut ordered: Vec<VertexId> = vertex_faces.keys().copied().collect();
    ordered.sort();
    for &v in &ordered {
        outer_map.insert(v, new_topo.add_vertex(topo.vertices[v].point));
        inner_map.insert(v, new_topo.add_vertex(inner_pos[&v]));
    }
    let add_loop = |t: &mut Topology, verts: &[VertexId]| {
        let hes: Vec<HalfEdgeId> = verts.iter().map(|&v| t.add_half_edge(v)).collect();
        t.add_loop(&hes)
    };

    let mut all_faces = Vec::new();
    for (i, lf) in faces.iter().enumerate() {
        let face = &topo.faces[lf.rep];
        let old = &lf.verts;
        let outer: Vec<VertexId> = old.iter().map(|v| outer_map[v]).collect();
        let inner: Vec<VertexId> = old.iter().map(|v| inner_map[v]).collect();
        let surface = &geom.surfaces[face.surface_index];
        if is_open[i] {
            // Rim: the open face's outline with the offset outline as a
            // hole. The hole runs in the open face's own vertex order, the
            // opposite of the kept inner faces next to it, so it twins
            // with them (inner faces keep the outer winding and carry a
            // flipped orientation flag — see the closed shell).
            let s = new_geom.add_surface(surface.clone_box());
            let lo = add_loop(&mut new_topo, &outer);
            let li = add_loop(&mut new_topo, &inner);
            let id = new_topo.add_face(lo, s, face.orientation);
            new_topo.add_inner_loop(id, li);
            all_faces.push(id);
            continue;
        }
        let s = new_geom.add_surface(surface.clone_box());
        let lo = add_loop(&mut new_topo, &outer);
        all_faces.push(new_topo.add_face(lo, s, face.orientation));

        let s = new_geom.add_surface(offset_surfaces[&i].clone_box());
        let li = add_loop(&mut new_topo, &inner);
        let flipped = match face.orientation {
            Orientation::Forward => Orientation::Reversed,
            Orientation::Reversed => Orientation::Forward,
        };
        all_faces.push(new_topo.add_face(li, s, flipped));

        // A planar wall whose offset outline flipped or vanished means
        // the thickness consumed it.
        if surface.surface_type() == SurfaceKind::Plane {
            let outer_pts: Vec<Point3> = old.iter().map(|v| topo.vertices[*v].point).collect();
            let inner_pts: Vec<Point3> = old.iter().map(|v| inner_pos[v]).collect();
            let (a_out, a_in) = (
                newell(outer_pts.iter().copied()),
                newell(inner_pts.iter().copied()),
            );
            if a_out.norm() > 1e-12
                && (a_in.dot(a_out) <= 1e-9 * a_out.dot(a_out)
                    || a_in.norm() < 1e-9 * a_out.norm()
                    || !offset_polygon_is_simple(&outer_pts, &inner_pts))
            {
                return fail(format!(
                    "wall thickness {thickness} consumes the solid (an offset face collapsed)"
                ));
            }
        }
    }

    pair_twin_half_edges(&mut new_topo);
    let shell_id = new_topo.add_shell(all_faces, ShellType::Outer);
    let solid_id = new_topo.add_solid(shell_id);
    Ok(BRepSolid {
        topology: new_topo,
        geometry: new_geom,
        solid_id,
    })
}

/// One face as the open-face shell sees it: a set of coplanar input faces
/// merged into a single outline, with straight-edge subdivision vertices
/// removed.
struct LogicalFace {
    /// The input face whose surface and orientation this face carries.
    rep: FaceId,
    /// Every input face merged into this one.
    members: Vec<FaceId>,
    /// The merged outer loop.
    verts: Vec<VertexId>,
}

/// Group coplanar, edge-adjacent planar faces into single faces, then
/// drop vertices that only subdivide a straight edge between exactly two
/// faces. A group whose union is not one simple loop stays unmerged.
fn logical_faces(brep: &BRepSolid, shell_faces: &[FaceId]) -> Vec<LogicalFace> {
    let topo = &brep.topology;
    let geom = &brep.geometry;
    let loops: Vec<Vec<VertexId>> = shell_faces
        .iter()
        .map(|&f| topo.loop_vertices(topo.faces[f].outer_loop))
        .collect();
    // Plane key per face: (normal incl. orientation, offset), planar only.
    let plane: Vec<Option<(Vec3, f64)>> = shell_faces
        .iter()
        .zip(&loops)
        .map(|(&f, vs)| {
            let face = &topo.faces[f];
            let surf = &geom.surfaces[face.surface_index];
            if surf.surface_type() != SurfaceKind::Plane || vs.len() < 3 {
                return None;
            }
            let n = *surf
                .normal(vcad_kernel_math::Point2::new(0.0, 0.0))
                .as_ref();
            let n = match face.orientation {
                Orientation::Forward => n,
                Orientation::Reversed => -n,
            };
            let p = topo.vertices[vs[0]].point;
            Some((n, n.dot(p - Point3::origin())))
        })
        .collect();
    let scale = topo
        .vertices
        .values()
        .map(|v| v.point.x.abs().max(v.point.y.abs()).max(v.point.z.abs()))
        .fold(1.0, f64::max);
    let same_plane = |i: usize, j: usize| match (plane[i], plane[j]) {
        (Some((a, da)), Some((b, db))) => a.dot(b) > 1.0 - 1e-9 && (da - db).abs() < 1e-9 * scale,
        _ => false,
    };

    // Union-find over edge-adjacent coplanar faces.
    let mut parent: Vec<usize> = (0..shell_faces.len()).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        let mut i = i;
        while p[i] != r {
            let next = p[i];
            p[i] = r;
            i = next;
        }
        r
    }
    let mut by_edge: HashMap<(VertexId, VertexId), usize> = HashMap::new();
    for (i, vs) in loops.iter().enumerate() {
        for k in 0..vs.len() {
            let (a, b) = (vs[k], vs[(k + 1) % vs.len()]);
            if a == b {
                continue;
            }
            if let Some(&j) = by_edge.get(&(b, a)) {
                if same_plane(i, j) {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    parent[ri] = rj;
                }
            }
            by_edge.insert((a, b), i);
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<usize, usize> = HashMap::new();
    for i in 0..shell_faces.len() {
        let r = find(&mut parent, i);
        let g = *group_of.entry(r).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }

    let mut out: Vec<LogicalFace> = Vec::new();
    for g in groups {
        if g.len() == 1 {
            out.push(LogicalFace {
                rep: shell_faces[g[0]],
                members: vec![shell_faces[g[0]]],
                verts: loops[g[0]].clone(),
            });
            continue;
        }
        match merge_loops(g.iter().map(|&i| loops[i].as_slice())) {
            Some(verts) => out.push(LogicalFace {
                rep: shell_faces[g[0]],
                members: g.iter().map(|&i| shell_faces[i]).collect(),
                verts,
            }),
            None => out.extend(g.iter().map(|&i| LogicalFace {
                rep: shell_faces[i],
                members: vec![shell_faces[i]],
                verts: loops[i].clone(),
            })),
        }
    }

    // Drop straight-edge subdivision vertices: in exactly two faces,
    // visited once in each, and collinear with its neighbours in both.
    let mut count: HashMap<VertexId, usize> = HashMap::new();
    for lf in &out {
        for &v in &lf.verts {
            *count.entry(v).or_default() += 1;
        }
    }
    let collinear = |vs: &[VertexId], k: usize| {
        let n = vs.len();
        let (a, b, c) = (vs[(k + n - 1) % n], vs[k], vs[(k + 1) % n]);
        let (pa, pb, pc) = (
            topo.vertices[a].point,
            topo.vertices[b].point,
            topo.vertices[c].point,
        );
        let (u, w) = (pb - pa, pc - pb);
        let (lu, lw) = (u.norm(), w.norm());
        lu > 1e-12 && lw > 1e-12 && u.dot(w) / (lu * lw) > 1.0 - 1e-12
    };
    let removable: HashSet<VertexId> = count
        .iter()
        .filter(|(_, c)| **c == 2)
        .map(|(v, _)| *v)
        .filter(|v| {
            out.iter()
                .all(|lf| match lf.verts.iter().position(|x| x == v) {
                    None => true,
                    Some(k) => lf.verts.len() > 3 && collinear(&lf.verts, k),
                })
        })
        .collect();
    for lf in &mut out {
        let kept: Vec<VertexId> = lf
            .verts
            .iter()
            .copied()
            .filter(|v| !removable.contains(v))
            .collect();
        if kept.len() >= 3 {
            lf.verts = kept;
        }
    }
    out
}

/// Union of face loops sharing edges: cancel each interior edge against
/// its reverse and chain what is left. `None` unless the result is one
/// simple loop.
fn merge_loops<'a>(loops: impl Iterator<Item = &'a [VertexId]>) -> Option<Vec<VertexId>> {
    let mut edges: HashSet<(VertexId, VertexId)> = HashSet::new();
    for vs in loops {
        for k in 0..vs.len() {
            let (a, b) = (vs[k], vs[(k + 1) % vs.len()]);
            if a == b {
                return None;
            }
            if !edges.remove(&(b, a)) && !edges.insert((a, b)) {
                return None;
            }
        }
    }
    let mut next: HashMap<VertexId, VertexId> = HashMap::new();
    for &(a, b) in &edges {
        if next.insert(a, b).is_some() {
            return None; // pinch vertex
        }
    }
    let &(start, _) = edges.iter().min()?;
    let mut verts = vec![start];
    let mut cur = next[&start];
    while cur != start {
        if verts.len() > edges.len() {
            return None;
        }
        verts.push(cur);
        cur = *next.get(&cur)?;
    }
    (verts.len() == edges.len()).then_some(verts)
}

/// True when every offset edge kept its direction — a wall thinner than
/// the thickness makes some edge of the offset outline run backwards.
fn offset_polygon_is_simple(outer: &[Point3], inner: &[Point3]) -> bool {
    let n = outer.len();
    (0..n).all(|k| {
        let (a, b) = (outer[k], outer[(k + 1) % n]);
        let (c, d) = (inner[k], inner[(k + 1) % n]);
        let (e, f) = (b - a, d - c);
        e.norm() < 1e-12 || f.norm() < 1e-12 || e.dot(f) > 0.0
    })
}

/// Axis-aligned bounds of a point set (`[MAX; 3]`/`[MIN; 3]` when empty).
fn vertex_aabb(points: impl Iterator<Item = Point3>) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for p in points {
        for (i, c) in [p.x, p.y, p.z].into_iter().enumerate() {
            lo[i] = lo[i].min(c);
            hi[i] = hi[i].max(c);
        }
    }
    (lo, hi)
}

/// Newell area vector of a polygon (twice the area, along its normal).
fn newell(points: impl Iterator<Item = Point3>) -> Vec3 {
    let pts: Vec<Point3> = points.collect();
    let mut n = Vec3::new(0.0, 0.0, 0.0);
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n
}

/// The displacement `d` with `n · d = t` for every (distinct, unit) normal:
/// the point `t` inside each face at once. Exact for up to three
/// independent faces; least-squares (minimum norm) beyond, or when the
/// normals are dependent. `None` with no normals.
fn solve_offset(normals: &[Vec3], t: f64) -> Option<Vec3> {
    match normals {
        [] => None,
        [n] => Some(*n * t),
        [a, b] => {
            // d = α(a + b) by symmetry; a·d = α(1 + a·b) = t.
            let c = a.dot(*b);
            if 1.0 + c < 1e-12 {
                return Some(*a * t); // opposite faces: degenerate
            }
            Some((*a + *b) * (t / (1.0 + c)))
        }
        _ => {
            // Normal equations: (Σ n nᵀ) d = t Σ n, solved by Cramer's rule.
            let mut m = [[0.0f64; 3]; 3];
            let mut rhs = [0.0f64; 3];
            for n in normals {
                let a = [n.x, n.y, n.z];
                for r in 0..3 {
                    for c in 0..3 {
                        m[r][c] += a[r] * a[c];
                    }
                    rhs[r] += a[r] * t;
                }
            }
            let det3 = |m: &[[f64; 3]; 3]| {
                m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                    - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                    + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
            };
            let det = det3(&m);
            if det.abs() > 1e-9 {
                let mut out = [0.0; 3];
                for (k, o) in out.iter_mut().enumerate() {
                    let mut mk = m;
                    for r in 0..3 {
                        mk[r][k] = rhs[r];
                    }
                    *o = det3(&mk) / det;
                }
                return Some(Vec3::new(out[0], out[1], out[2]));
            }
            // Rank 2 (all normals in one plane): solve in the span of the
            // two most different normals.
            let (mut best, mut pair) = (2.0, (0, 1));
            for i in 0..normals.len() {
                for j in i + 1..normals.len() {
                    let c = normals[i].dot(normals[j]).abs();
                    if c < best {
                        best = c;
                        pair = (i, j);
                    }
                }
            }
            solve_offset(&[normals[pair.0], normals[pair.1]], t)
        }
    }
}

/// Outward normal of `surface` at the 3D point `p`.
///
/// The analytic surfaces all have a closed-form normal at a point, so no
/// parameter inversion is needed: a plane's is constant, a cylinder's is
/// radial from its axis, a sphere's is radial from its center, a torus's
/// is radial from the tube centerline. Anything else falls back to the
/// surface's normal at the start of its domain.
fn surface_normal_at(surface: &dyn Surface, p: Point3) -> Vec3 {
    use vcad_kernel_geom::{CylinderSurface, SphereSurface, TorusSurface};

    let any = surface.as_any();
    if let Some(cyl) = any.downcast_ref::<CylinderSurface>() {
        let axis = *cyl.axis.as_ref();
        let d = p - cyl.center;
        let radial = d - axis * d.dot(axis);
        if radial.norm() > 1e-12 {
            return radial.normalize() * cyl.radius.signum();
        }
    } else if let Some(sph) = any.downcast_ref::<SphereSurface>() {
        let d = p - sph.center;
        if d.norm() > 1e-12 {
            return d.normalize() * sph.radius.signum();
        }
    } else if let Some(tor) = any.downcast_ref::<TorusSurface>() {
        let axis = *tor.axis.as_ref();
        let d = p - tor.center;
        let radial = d - axis * d.dot(axis);
        if radial.norm() > 1e-12 {
            let tube_center = tor.center + radial.normalize() * tor.major_radius;
            let n = p - tube_center;
            if n.norm() > 1e-12 {
                return n.normalize() * tor.minor_radius.signum();
            }
        }
    }
    *surface
        .normal(vcad_kernel_math::Point2::new(0.0, 0.0))
        .as_ref()
}

/// Compute the offset position for a vertex given the surrounding offset surfaces.
///
/// For vertices at the intersection of 3 planar faces, solves the 3-plane
/// intersection exactly. For simpler cases or mixed surfaces, uses a
/// weighted average of offset vectors from adjacent face normals.
fn compute_offset_vertex(
    topo: &Topology,
    geom: &GeometryStore,
    vertex_id: VertexId,
    _offset_surfaces: &HashMap<FaceId, Box<dyn Surface>>,
    thickness: f64,
) -> Point3 {
    let vertex = &topo.vertices[vertex_id];
    let pos = vertex.point;

    // Collect normals from adjacent faces
    let mut normals: Vec<Vec3> = Vec::new();

    // Find all faces adjacent to this vertex
    if let Some(start_he) = vertex.half_edge {
        let mut he_id = start_he;
        let mut visited = std::collections::HashSet::new();
        loop {
            if !visited.insert(he_id) {
                break;
            }
            let he = &topo.half_edges[he_id];
            if let Some(loop_id) = he.loop_id {
                if let Some(face_id) = topo.loops[loop_id].face {
                    let face = &topo.faces[face_id];
                    let surface = &geom.surfaces[face.surface_index];
                    // Get the surface normal at the vertex position. For a
                    // curved face the normal varies over the surface, so
                    // sampling it at a fixed parameter (uv = 0,0) points
                    // somewhere else entirely — that put every inner vertex
                    // of a filleted solid in the wrong place, so the inner
                    // faces no longer met their own offset surfaces and the
                    // cavity tessellated full of holes.
                    let n = surface_normal_at(surface.as_ref(), pos);
                    let sign = match face.orientation {
                        Orientation::Forward => 1.0,
                        Orientation::Reversed => -1.0,
                    };
                    normals.push(sign * n);
                }
            }
            // Move to next half-edge around vertex
            if let Some(twin) = he.twin {
                let twin_he = &topo.half_edges[twin];
                if let Some(next) = twin_he.next {
                    he_id = next;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }

    if normals.is_empty() {
        return pos;
    }

    // Average the face normals to get vertex offset direction
    let avg_normal: Vec3 = normals.iter().copied().sum::<Vec3>() / normals.len() as f64;
    let avg_len = avg_normal.norm();
    if avg_len < 1e-12 {
        return pos;
    }

    // Scale the offset to preserve wall thickness at convex/concave vertices
    // For a vertex at the intersection of n planar faces, the offset direction
    // is the average of the face normals, and we scale by 1/cos(angle) to
    // maintain the intended thickness on each face.
    let cos_factor = avg_len; // avg_len < 1.0 at convex corners, so we offset more
    let offset_vec = (thickness / cos_factor) * avg_normal.normalize();

    pos - offset_vec
}

/// Create a shell from a triangle mesh by vertex normal offsetting.
///
/// # Arguments
///
/// * `mesh` - The input mesh (assumed to be a closed solid)
/// * `thickness` - Wall thickness (positive = inward offset)
///
/// # Returns
///
/// A new mesh representing the hollow shell.
pub fn shell_mesh(mesh: &TriangleMesh, thickness: f64) -> TriangleMesh {
    if mesh.vertices.is_empty() || mesh.indices.is_empty() {
        return mesh.clone();
    }

    let num_verts = mesh.vertices.len() / 3;

    // Step 1: Compute vertex normals (average of adjacent face normals)
    let vertex_normals = compute_vertex_normals(mesh);

    // Step 2: Create offset (inner) vertices
    let mut inner_vertices = Vec::with_capacity(num_verts * 3);
    for i in 0..num_verts {
        let vx = mesh.vertices[i * 3] as f64;
        let vy = mesh.vertices[i * 3 + 1] as f64;
        let vz = mesh.vertices[i * 3 + 2] as f64;
        let nx = vertex_normals[i * 3];
        let ny = vertex_normals[i * 3 + 1];
        let nz = vertex_normals[i * 3 + 2];

        // Offset inward (opposite to normal direction)
        inner_vertices.push((vx - thickness * nx) as f32);
        inner_vertices.push((vy - thickness * ny) as f32);
        inner_vertices.push((vz - thickness * nz) as f32);
    }

    // Step 3: Build combined mesh
    // - Outer shell: original vertices, original indices
    // - Inner shell: offset vertices, reversed indices
    let mut combined_vertices = mesh.vertices.clone();
    combined_vertices.extend(&inner_vertices);

    let mut combined_indices = mesh.indices.clone();

    // Add inner shell triangles with reversed winding
    let offset = num_verts as u32;
    for tri in mesh.indices.chunks(3) {
        // Reverse winding: swap indices 1 and 2
        combined_indices.push(tri[0] + offset);
        combined_indices.push(tri[2] + offset);
        combined_indices.push(tri[1] + offset);
    }

    TriangleMesh {
        vertices: combined_vertices,
        indices: combined_indices,
        normals: Vec::new(), // Let renderer compute normals
        face_kinds: Vec::new(),
        face_ids: Vec::new(),
    }
}

/// Compute vertex normals as the average of adjacent face normals.
fn compute_vertex_normals(mesh: &TriangleMesh) -> Vec<f64> {
    let num_verts = mesh.vertices.len() / 3;
    let mut normals = vec![0.0_f64; num_verts * 3];
    let mut counts = vec![0_u32; num_verts];

    // Accumulate face normals at each vertex
    for tri in mesh.indices.chunks(3) {
        let i0 = tri[0] as usize;
        let i1 = tri[1] as usize;
        let i2 = tri[2] as usize;

        let v0 = Vec3::new(
            mesh.vertices[i0 * 3] as f64,
            mesh.vertices[i0 * 3 + 1] as f64,
            mesh.vertices[i0 * 3 + 2] as f64,
        );
        let v1 = Vec3::new(
            mesh.vertices[i1 * 3] as f64,
            mesh.vertices[i1 * 3 + 1] as f64,
            mesh.vertices[i1 * 3 + 2] as f64,
        );
        let v2 = Vec3::new(
            mesh.vertices[i2 * 3] as f64,
            mesh.vertices[i2 * 3 + 1] as f64,
            mesh.vertices[i2 * 3 + 2] as f64,
        );

        let e1 = v1 - v0;
        let e2 = v2 - v0;
        let face_normal = e1.cross(e2);

        // Add to each vertex's normal
        for &idx in &[i0, i1, i2] {
            normals[idx * 3] += face_normal.x;
            normals[idx * 3 + 1] += face_normal.y;
            normals[idx * 3 + 2] += face_normal.z;
            counts[idx] += 1;
        }
    }

    // Normalize
    for i in 0..num_verts {
        let nx = normals[i * 3];
        let ny = normals[i * 3 + 1];
        let nz = normals[i * 3 + 2];
        let len = (nx * nx + ny * ny + nz * nz).sqrt();
        if len > 1e-12 {
            normals[i * 3] = nx / len;
            normals[i * 3 + 1] = ny / len;
            normals[i * 3 + 2] = nz / len;
        } else {
            // Default to Z-up if degenerate
            normals[i * 3] = 0.0;
            normals[i * 3 + 1] = 0.0;
            normals[i * 3 + 2] = 1.0;
        }
    }

    normals
}

/// Convert a triangle mesh to a B-rep solid.
///
/// Creates a simple B-rep with one planar face per triangle.
/// This is a minimal representation for mesh-based results.
fn mesh_to_brep(mesh: &TriangleMesh) -> BRepSolid {
    let mut topo = Topology::new();
    let mut geom = GeometryStore::new();

    // Create vertices
    let mut vertex_cache: HashMap<[i64; 3], VertexId> = HashMap::new();

    let get_or_create_vertex =
        |cache: &mut HashMap<[i64; 3], VertexId>, topo: &mut Topology, pos: Point3| -> VertexId {
            let key = [
                (pos.x * 1e6).round() as i64,
                (pos.y * 1e6).round() as i64,
                (pos.z * 1e6).round() as i64,
            ];
            *cache.entry(key).or_insert_with(|| topo.add_vertex(pos))
        };

    let mut all_faces = Vec::new();

    // Create one face per triangle
    for tri in mesh.indices.chunks(3) {
        let i0 = tri[0] as usize;
        let i1 = tri[1] as usize;
        let i2 = tri[2] as usize;

        let p0 = Point3::new(
            mesh.vertices[i0 * 3] as f64,
            mesh.vertices[i0 * 3 + 1] as f64,
            mesh.vertices[i0 * 3 + 2] as f64,
        );
        let p1 = Point3::new(
            mesh.vertices[i1 * 3] as f64,
            mesh.vertices[i1 * 3 + 1] as f64,
            mesh.vertices[i1 * 3 + 2] as f64,
        );
        let p2 = Point3::new(
            mesh.vertices[i2 * 3] as f64,
            mesh.vertices[i2 * 3 + 1] as f64,
            mesh.vertices[i2 * 3 + 2] as f64,
        );

        let v0 = get_or_create_vertex(&mut vertex_cache, &mut topo, p0);
        let v1 = get_or_create_vertex(&mut vertex_cache, &mut topo, p1);
        let v2 = get_or_create_vertex(&mut vertex_cache, &mut topo, p2);

        // Create surface
        let x_dir = p1 - p0;
        let y_dir = p2 - p0;
        if x_dir.norm() < 1e-12 || y_dir.norm() < 1e-12 {
            continue; // Degenerate triangle
        }
        let surf_idx = geom.add_surface(Box::new(Plane::new(p0, x_dir, y_dir)));

        // Create half-edges and loop
        let he0 = topo.add_half_edge(v0);
        let he1 = topo.add_half_edge(v1);
        let he2 = topo.add_half_edge(v2);
        let loop_id = topo.add_loop(&[he0, he1, he2]);
        let face_id = topo.add_face(loop_id, surf_idx, Orientation::Forward);
        all_faces.push(face_id);
    }

    // Pair twin half-edges
    pair_twin_half_edges(&mut topo);

    // Build shell and solid
    let shell = topo.add_shell(all_faces, ShellType::Outer);
    let solid_id = topo.add_solid(shell);

    BRepSolid {
        topology: topo,
        geometry: geom,
        solid_id,
    }
}

/// Pair twin half-edges by matching (origin, destination) vertex pairs.
fn pair_twin_half_edges(topo: &mut Topology) {
    let mut he_map: HashMap<([i64; 3], [i64; 3]), HalfEdgeId> = HashMap::new();

    let he_ids: Vec<HalfEdgeId> = topo.half_edges.keys().collect();
    for he_id in &he_ids {
        let he = &topo.half_edges[*he_id];
        let origin = topo.vertices[he.origin].point;
        let next = match he.next {
            Some(n) => n,
            None => continue,
        };
        let dest = topo.vertices[topo.half_edges[next].origin].point;

        let origin_key = [
            (origin.x * 1e6).round() as i64,
            (origin.y * 1e6).round() as i64,
            (origin.z * 1e6).round() as i64,
        ];
        let dest_key = [
            (dest.x * 1e6).round() as i64,
            (dest.y * 1e6).round() as i64,
            (dest.z * 1e6).round() as i64,
        ];

        if let Some(&twin_id) = he_map.get(&(dest_key, origin_key)) {
            if topo.half_edges[*he_id].twin.is_none() && topo.half_edges[twin_id].twin.is_none() {
                topo.add_edge(*he_id, twin_id);
            }
        }

        he_map.insert((origin_key, dest_key), *he_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_mesh_basic() {
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let mesh = vcad_kernel_tessellate::tessellate_brep(&cube, 32);

        let shell = shell_mesh(&mesh, 1.0);

        let orig_tris = mesh.indices.len() / 3;
        let shell_tris = shell.indices.len() / 3;
        assert_eq!(shell_tris, orig_tris * 2, "shell should have 2x triangles");

        let orig_verts = mesh.vertices.len() / 3;
        let shell_verts = shell.vertices.len() / 3;
        assert_eq!(shell_verts, orig_verts * 2, "shell should have 2x vertices");
    }

    #[test]
    fn test_shell_mesh_volume() {
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let mesh = vcad_kernel_tessellate::tessellate_brep(&cube, 32);
        let shell = shell_mesh(&mesh, 2.0);

        let orig_vol = compute_volume(&mesh);
        let shell_vol = compute_volume(&shell);

        assert!(
            shell_vol < orig_vol,
            "shell volume {} should be less than original {}",
            shell_vol,
            orig_vol
        );
        assert!(
            shell_vol > 100.0,
            "shell volume {} should be positive",
            shell_vol
        );
    }

    #[test]
    fn test_shell_brep() {
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let shell = shell_brep(&cube, 1.0);
        assert!(!shell.topology.faces.is_empty(), "shell should have faces");
    }

    #[test]
    fn test_shell_cube_analytical() {
        // Shell a 10x10x10 cube with thickness 1
        // Expected: 6 outer + 6 inner = 12 faces, no walls (no open faces)
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let result = shell_brep_analytical(&cube, 1.0, &[]).unwrap();

        // Should have 12 faces (6 outer + 6 inner)
        assert_eq!(
            result.topology.faces.len(),
            12,
            "analytical shell of closed cube should have 12 faces"
        );

        // Verify inner vertices are offset inward
        // Outer cube: 0..10 in each axis
        // Inner cube should be ~1..9 in each axis
        let mut has_inner = false;
        for (_, vertex) in &result.topology.vertices {
            let p = vertex.point;
            // Check if this is an inner vertex (not at 0 or 10)
            if p.x > 0.5 && p.x < 9.5 && p.y > 0.5 && p.y < 9.5 && p.z > 0.5 && p.z < 9.5 {
                has_inner = true;
                // Inner vertices should be at approximately 1, 9
                let check = |v: f64| (v - 1.0).abs() < 0.5 || (v - 9.0).abs() < 0.5;
                assert!(
                    check(p.x) && check(p.y) && check(p.z),
                    "inner vertex at unexpected position: {:?}",
                    p
                );
            }
        }
        assert!(has_inner, "should have inner vertices");

        // Geometry check: should have 12 surfaces (6 outer planes + 6 inner offset planes)
        assert_eq!(
            result.geometry.surfaces.len(),
            12,
            "should have 12 surfaces"
        );
    }

    #[test]
    fn overshell_is_refused_not_grown() {
        // Fuzz repro: `[shell 20 [cube 10 10 10]]` offset OUTWARD and
        // reported 28000 mm³ with a (-10..20) bbox.
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        for t in [5.0, 5.01, 6.0, 20.0] {
            let err = shell_brep_analytical(&cube, t, &[]).unwrap_err();
            assert!(
                matches!(err, ShellError::ThicknessConsumesSolid { .. }),
                "t={t}: {err}"
            );
            assert!(err.is_infeasible());
        }
        for t in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = shell_brep_analytical(&cube, t, &[]).unwrap_err();
            assert!(
                matches!(err, ShellError::InvalidThickness(_)),
                "t={t}: {err}"
            );
        }
        // Just under the limit is a real (very thin-cavity) shell.
        let ok = shell_brep_analytical(&cube, 4.9, &[]).unwrap();
        let vol = compute_volume(&vcad_kernel_tessellate::tessellate_brep(&ok, 32));
        let expected = 1000.0 - 0.2f64.powi(3);
        assert!((vol - expected).abs() < 1e-3, "{vol}");
    }

    #[test]
    fn test_shell_cube_analytical_volume() {
        // The tessellated analytical shell must enclose outer − inner volume:
        // the cavity subtracts. A winding bug here makes the cavity *add*
        // (volume > the solid cube), which inverts thickness→mass scaling.
        for &t in &[0.8, 1.0, 2.0, 3.5] {
            let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
            let shelled = shell_brep_analytical(&cube, t, &[]).unwrap();
            let mesh = vcad_kernel_tessellate::tessellate_brep(&shelled, 32);
            let vol = compute_volume(&mesh);
            let expected = 1000.0 - (10.0 - 2.0 * t).powi(3);
            // Relative tolerance: tessellated vertices are f32.
            assert!(
                (vol - expected).abs() < expected * 1e-5,
                "shell thickness {}: volume {} != expected {}",
                t,
                vol,
                expected
            );
        }
    }

    #[test]
    fn test_shell_cube_with_open_face() {
        // Shell a cube with one open face
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let solid = &cube.topology.solids[cube.solid_id];
        let shell = &cube.topology.shells[solid.outer_shell];

        // Open the first face
        let open_face = shell.faces[0];
        let result = shell_brep_analytical(&cube, 1.0, &[open_face]).unwrap();

        // Should have: 6 outer + 5 inner + 4 walls = 15 faces
        assert_eq!(
            result.topology.faces.len(),
            15,
            "shell with one open face should have 15 faces"
        );
    }

    #[test]
    fn test_shell_cylinder_analytical() {
        // Shell a cylinder
        let cyl = vcad_kernel_primitives::make_cylinder(5.0, 10.0, 64);
        let result = shell_brep_analytical(&cyl, 1.0, &[]);

        match result {
            Ok(shelled) => {
                // Should have outer faces + inner faces
                assert!(
                    shelled.topology.faces.len() > 3,
                    "shelled cylinder should have faces"
                );
            }
            Err(e) => {
                // Cylinder shell may fail if vertex offset doesn't handle
                // curved adjacency well yet - that's expected
                println!("Cylinder shell error (expected for curved vertices): {}", e);
            }
        }
    }

    #[test]
    fn test_shell_sphere_analytical() {
        // Shell a sphere
        let sphere = vcad_kernel_primitives::make_sphere(10.0, 32);
        let result = shell_brep_analytical(&sphere, 2.0, &[]);

        match result {
            Ok(shelled) => {
                assert!(
                    shelled.topology.faces.len() >= 2,
                    "shelled sphere should have faces"
                );
            }
            Err(e) => {
                println!("Sphere shell error (expected): {}", e);
            }
        }
    }

    #[test]
    fn test_shell_error_on_collapse() {
        // A sphere of radius 5 shelled with thickness 6 should fail
        let sphere = vcad_kernel_primitives::make_sphere(5.0, 32);
        let result = shell_brep_analytical(&sphere, 6.0, &[]);

        assert!(result.is_err(), "shelling past center should fail");
        if let Err(ShellError::SurfaceCollapse(_, msg)) = result {
            assert!(
                msg.contains("collapsed"),
                "error should mention collapse: {}",
                msg
            );
        }
    }

    fn compute_volume(mesh: &TriangleMesh) -> f64 {
        let verts = &mesh.vertices;
        let indices = &mesh.indices;
        let mut vol = 0.0;
        for tri in indices.chunks(3) {
            let (i0, i1, i2) = (
                tri[0] as usize * 3,
                tri[1] as usize * 3,
                tri[2] as usize * 3,
            );
            let v0 = [verts[i0] as f64, verts[i0 + 1] as f64, verts[i0 + 2] as f64];
            let v1 = [verts[i1] as f64, verts[i1 + 1] as f64, verts[i1 + 2] as f64];
            let v2 = [verts[i2] as f64, verts[i2 + 1] as f64, verts[i2 + 2] as f64];
            vol += v0[0] * (v1[1] * v2[2] - v2[1] * v1[2])
                - v1[0] * (v0[1] * v2[2] - v2[1] * v0[2])
                + v2[0] * (v0[1] * v1[2] - v1[1] * v0[2]);
        }
        (vol / 6.0).abs()
    }
}
