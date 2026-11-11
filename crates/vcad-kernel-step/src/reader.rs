//! STEP file reader: converts parsed STEP data to BRepSolid.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use crate::entities::{
    curves::{parse_curve, StepCurve},
    parse_advanced_face, parse_edge_curve, parse_edge_loop, parse_manifold_solid_brep,
    parse_oriented_edge, parse_shell, parse_surface_oriented, parse_vertex_point, EntityArgs,
};
use crate::error::StepError;
use stepperoni::{Parser, StepFile, StepValue};

use vcad_kernel_geom::{Curve3d, GeometryStore};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_topo::{EdgeId, HalfEdgeId, LoopId, Orientation, ShellType, Topology, VertexId};

/// Number of intermediate points to sample along a curved edge (arc or B-spline).
const CURVE_SAMPLE_COUNT: usize = 8;

/// Angular resolution for sampling circular arcs: aim for 64 segments per
/// full turn (matching the kernel's usual tessellation density), so a
/// semicircular arc doesn't collapse to a 9-gon.
const ARC_SEGMENTS_PER_TURN: f64 = 64.0;

/// Intermediate sample count for a circular arc of the given sweep (radians).
fn arc_sample_count(sweep: f64) -> usize {
    let segments = (sweep.abs() * ARC_SEGMENTS_PER_TURN / std::f64::consts::TAU).ceil() as usize;
    segments.saturating_sub(1).clamp(CURVE_SAMPLE_COUNT, 128)
}

/// Hard ceilings on how much topology we will try to parse from a single
/// STEP solid. They exist purely to keep a malicious file from pinning the
/// reader in a multi-hour allocation loop; legitimate CAD models come
/// nowhere near these numbers.
const MAX_FACES_PER_SOLID: usize = 200_000;
const MAX_EDGES_PER_LOOP: usize = 100_000;

/// Compute the absolute enclosed area of a topology loop using Newell's method.
/// Works in 3D — the magnitude of the cross-product sum gives twice the area.
fn loop_area_3d(topo: &Topology, loop_id: LoopId) -> f64 {
    loop_newell_normal(topo, loop_id).norm() * 0.5
}

/// Signed area of a loop pulled back into a surface's (u, v) parameters:
/// positive when the loop winds counter-clockwise about `∂u × ∂v`.
///
/// `None` when the answer is not trustworthy: a vertex that does not invert
/// onto the surface, a loop that crosses a periodic seam (its parameter
/// polygon would wrap), or a degenerate polygon.
fn loop_winding_in_uv(
    topo: &Topology,
    loop_id: LoopId,
    surface: &dyn vcad_kernel_geom::Surface,
) -> Option<f64> {
    use vcad_kernel_math::Point2;
    let mut pts: Vec<vcad_kernel_math::Point3> = topo
        .loop_half_edges(loop_id)
        .map(|he| topo.vertices[topo.half_edges[he].origin].point)
        .collect();
    pts.dedup_by(|a, b| (*a - *b).norm() < 1e-12);
    if pts.len() < 3 {
        return None;
    }
    let (mut lo, mut hi) = (pts[0], pts[0]);
    for p in &pts {
        lo = vcad_kernel_math::Point3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = vcad_kernel_math::Point3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    let tol = (1e-4 * (hi - lo).norm()).max(1e-3);
    let domain = surface.domain();
    let ((u0, u1), (v0, v1)) = domain;
    let grid = vcad_kernel_geom::surface_sample_grid(surface, domain, 16);
    let mut guess: Option<Point2> = None;
    let mut uv = Vec::with_capacity(pts.len());
    for p in &pts {
        let (w, d) = vcad_kernel_geom::invert_surface_point(surface, domain, p, guess, &grid, tol);
        if d > 10.0 * tol {
            return None;
        }
        guess = Some(w);
        uv.push(w);
    }
    // A seam crossing shows as a half-domain jump; only a closed direction
    // can do that, and there the polygon's sign would be meaningless.
    let closed =
        |a: Point2, b: Point2| (surface.evaluate(a) - surface.evaluate(b)).norm() <= 10.0 * tol;
    let closed_u = closed(
        Point2::new(u0, 0.5 * (v0 + v1)),
        Point2::new(u1, 0.5 * (v0 + v1)),
    );
    let closed_v = closed(
        Point2::new(0.5 * (u0 + u1), v0),
        Point2::new(0.5 * (u0 + u1), v1),
    );
    let n = uv.len();
    let mut area = 0.0;
    for i in 0..n {
        let (a, b) = (uv[i], uv[(i + 1) % n]);
        if (closed_u && (a.x - b.x).abs() > 0.5 * (u1 - u0))
            || (closed_v && (a.y - b.y).abs() > 0.5 * (v1 - v0))
        {
            return None;
        }
        area += a.x * b.y - b.x * a.y;
    }
    let area = 0.5 * area;
    let box_area = (u1 - u0).abs() * (v1 - v0).abs();
    (area.is_finite() && area.abs() > 1e-9 * box_area.max(1e-300)).then_some(area)
}

/// Newell's normal of a loop: its direction follows the loop's winding
/// (counter-clockwise about it), its length is twice the enclosed area.
fn loop_newell_normal(topo: &Topology, loop_id: LoopId) -> vcad_kernel_math::Vec3 {
    let verts: Vec<vcad_kernel_math::Point3> = topo
        .loop_half_edges(loop_id)
        .map(|he| topo.vertices[topo.half_edges[he].origin].point)
        .collect();
    let n = verts.len();
    let mut cross = vcad_kernel_math::Vec3::zeros();
    if n < 3 {
        return cross;
    }
    for i in 0..n {
        let c = verts[i];
        let nx = verts[(i + 1) % n];
        cross.x += (c.y - nx.y) * (c.z + nx.z);
        cross.y += (c.z - nx.z) * (c.x + nx.x);
        cross.z += (c.x - nx.x) * (c.y + nx.y);
    }
    cross
}

/// Compute the angle parameter of a point on a circle.
fn point_angle_on_circle(
    circle: &vcad_kernel_geom::Circle3d,
    pt: &vcad_kernel_math::Point3,
) -> f64 {
    let d = *pt - circle.center;
    let x = d.dot(circle.x_dir.as_ref());
    let y = d.dot(circle.y_dir.as_ref());
    y.atan2(x)
}

/// A face omitted during import because its surface could not be parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedFace {
    /// STEP `ADVANCED_FACE` entity id of the omitted face.
    pub face_id: u64,
    /// STEP entity id of the surface the face referenced.
    pub surface_id: u64,
    /// Why parsing failed — typically the unsupported surface type name.
    pub reason: String,
}

/// Per-solid record of silent degradations applied during import.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SolidImportReport {
    /// STEP `MANIFOLD_SOLID_BREP` entity id of the solid.
    pub solid_id: u64,
    /// Total faces the solid's shell declared in the file.
    pub total_faces: usize,
    /// Faces omitted because their surface type is unsupported. A non-empty
    /// list means the imported solid has holes where these faces were.
    pub skipped_faces: Vec<SkippedFace>,
    /// Non-fatal approximations that don't remove geometry (e.g. curved
    /// edges subdivided into chords).
    pub notes: Vec<String>,
}

impl SolidImportReport {
    /// True when the solid imported without dropping any faces.
    pub fn is_clean(&self) -> bool {
        self.skipped_faces.is_empty()
    }
}

/// Whole-file import report: one entry per imported solid.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepImportReport {
    /// Per-solid reports, in the order the solids were returned.
    pub solids: Vec<SolidImportReport>,
}

impl StepImportReport {
    /// True when no solid dropped any faces.
    pub fn is_clean(&self) -> bool {
        self.solids.iter().all(|s| s.is_clean())
    }

    /// Total number of faces dropped across all solids.
    pub fn total_skipped_faces(&self) -> usize {
        self.solids.iter().map(|s| s.skipped_faces.len()).sum()
    }

    /// Human-readable warning summary, or `None` when the import is clean.
    pub fn summary(&self) -> Option<String> {
        if self.is_clean() {
            return None;
        }
        let mut lines = Vec::new();
        for s in &self.solids {
            if s.skipped_faces.is_empty() {
                continue;
            }
            lines.push(format!(
                "solid #{}: skipped {} of {} faces (imported geometry has holes):",
                s.solid_id,
                s.skipped_faces.len(),
                s.total_faces
            ));
            for f in &s.skipped_faces {
                lines.push(format!(
                    "  face #{} (surface #{}): {}",
                    f.face_id, f.surface_id, f.reason
                ));
            }
        }
        Some(lines.join("\n"))
    }
}

/// Result of a STEP read: the solids plus a report of any degradations.
#[derive(Debug)]
pub struct ReadResult {
    /// The imported B-rep solids.
    pub solids: Vec<BRepSolid>,
    /// Per-solid import report (skipped faces, approximation notes).
    pub report: StepImportReport,
    /// Per-solid surface colour (`STYLED_ITEM`), same order as `solids`:
    /// the solid's own style, else its shell's, else the colour on most of
    /// its faces. `None` when the file styles nothing on it.
    pub colors: Vec<Option<[f64; 3]>>,
    /// Per-solid name, same order as `solids`: the `PRODUCT` it belongs to,
    /// else the solid entity's own name. `None` when neither is set.
    pub names: Vec<Option<String>>,
    /// Every placed copy of every solid, with world transforms through the
    /// assembly tree (see [`crate::solid_occurrences`]). Empty when the
    /// product structure can't be read; then each solid sits once at its
    /// own coordinates.
    pub occurrences: Vec<crate::SolidOccurrence>,
}

/// Give every half-edge of a loop-to-be to that loop alone. STEP files
/// that use one ORIENTED_EDGE direction in two loops (KiCad boards via
/// OCCT do it on every hole: 124 of 620 in one board) would otherwise have
/// `add_loop` rewire the shared half-edge's `next` into the second loop,
/// leaving the first loop's chain cycling without reaching its start — the
/// loop walk then never ends. A claimed half-edge is replaced by a fresh
/// copy (same origin, edge and twin) for this loop. Returns how many were
/// copied.
fn claim_loop_half_edges(topo: &mut Topology, hes: &mut [HalfEdgeId]) -> usize {
    let mut seen: HashSet<HalfEdgeId> = HashSet::new();
    let mut copied = 0;
    for he in hes.iter_mut() {
        let taken = topo.half_edges[*he].loop_id.is_some() || !seen.insert(*he);
        if !taken {
            continue;
        }
        let src = topo.half_edges[*he].clone();
        let dup = topo.add_half_edge(src.origin);
        topo.half_edges[dup].twin = src.twin;
        topo.half_edges[dup].edge = src.edge;
        seen.insert(dup);
        *he = dup;
        copied += 1;
    }
    copied
}

/// Read STEP file from a path.
///
/// # Arguments
///
/// * `path` - Path to the STEP file
///
/// # Returns
///
/// A vector of B-rep solids found in the file. Faces with unsupported
/// surface types are silently omitted — use [`read_step_with_report`] to
/// find out about them.
pub fn read_step(path: impl AsRef<Path>) -> Result<Vec<BRepSolid>, StepError> {
    read_step_with_report(path).map(|r| r.solids)
}

/// Read STEP file from a path, reporting skipped faces and other
/// degradations alongside the solids.
pub fn read_step_with_report(path: impl AsRef<Path>) -> Result<ReadResult, StepError> {
    let data = std::fs::read(path)?;
    read_step_from_buffer_with_report(&data)
}

/// Read STEP file from a byte buffer.
///
/// # Arguments
///
/// * `data` - Raw STEP file contents
///
/// # Returns
///
/// A vector of B-rep solids found in the file. Faces with unsupported
/// surface types are silently omitted — use
/// [`read_step_from_buffer_with_report`] to find out about them.
pub fn read_step_from_buffer(data: &[u8]) -> Result<Vec<BRepSolid>, StepError> {
    read_step_from_buffer_with_report(data).map(|r| r.solids)
}

/// Read STEP file from a byte buffer, reporting skipped faces and other
/// degradations alongside the solids.
pub fn read_step_from_buffer_with_report(data: &[u8]) -> Result<ReadResult, StepError> {
    let step_file = Parser::parse(data)?;
    let mut reader = StepReader::new(&step_file);
    reader.read_all_solids()
}

/// Read a STEP file from a path, publishing progress to `progress` as it
/// goes and stopping with [`StepError::Cancelled`] once
/// [`StepProgress::cancel`] has been called (checked once per face).
pub fn read_step_with_progress(
    path: impl AsRef<Path>,
    progress: &StepProgress,
) -> Result<ReadResult, StepError> {
    progress.set_stage(StepStage::Reading, 0, 0);
    let data = std::fs::read(path)?;
    progress.check()?;
    progress.set_stage(StepStage::Parsing, 0, 0);
    let step_file = Parser::parse(&data)?;
    progress.check()?;
    let mut reader = StepReader::new(&step_file);
    reader.progress = Some(progress);
    let r = reader.read_all_solids()?;
    let total = progress.total.load(Ordering::Relaxed);
    progress.set_stage(StepStage::Done, total, total);
    Ok(r)
}

/// What a progress-reporting STEP read is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StepStage {
    /// Reading the file's bytes.
    Reading = 0,
    /// Tokenising entities (one pass, no count).
    Parsing = 1,
    /// Building B-rep faces: `done` of `total` across every solid.
    Faces = 2,
    /// Finished.
    Done = 3,
}

impl StepStage {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Reading,
            1 => Self::Parsing,
            2 => Self::Faces,
            _ => Self::Done,
        }
    }
}

/// Lock-free progress + cancel flag shared between a STEP read and whoever
/// is watching it (another thread polls [`StepProgress::snapshot`]).
#[derive(Debug, Default)]
pub struct StepProgress {
    stage: AtomicU8,
    done: AtomicUsize,
    total: AtomicUsize,
    cancelled: AtomicBool,
}

impl StepProgress {
    /// A fresh tracker (stage `Reading`, 0 of 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// `(stage, done, total)` right now.
    pub fn snapshot(&self) -> (StepStage, usize, usize) {
        (
            StepStage::from_u8(self.stage.load(Ordering::Acquire)),
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    /// Ask the read to stop at the next face.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// True once [`StepProgress::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn set_stage(&self, stage: StepStage, done: usize, total: usize) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.stage.store(stage as u8, Ordering::Release);
    }

    fn check(&self) -> Result<(), StepError> {
        if self.is_cancelled() {
            Err(StepError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn tick(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }
}

/// Read a single solid by its `MANIFOLD_SOLID_BREP` entity ID.
///
/// Creates a fresh reader state so maps from other solids don't bleed in.
pub(crate) fn read_solid_from_file(file: &StepFile, solid_id: u64) -> Result<BRepSolid, StepError> {
    let mut reader = StepReader::new(file);
    reader.read_solid(solid_id).map(|(solid, _)| solid)
}

/// STEP entity types that define a solid: `MANIFOLD_SOLID_BREP` and its
/// `BREP_WITH_VOIDS` subtype (Onshape writes the latter for hollow bodies).
pub(crate) const SOLID_BREP_TYPES: [&str; 2] = ["MANIFOLD_SOLID_BREP", "BREP_WITH_VOIDS"];

pub(crate) fn is_solid_brep_type(type_name: &str) -> bool {
    SOLID_BREP_TYPES.contains(&type_name)
}

/// Millimetres per length unit declared in the file's `LENGTH_UNIT` complex
/// entity: an `SI_UNIT` with an optional prefix, or a `CONVERSION_BASED_UNIT`
/// named inch/foot. Returns 1.0 (millimetres) when no length unit is declared.
pub(crate) fn length_unit_mm(file: &StepFile) -> f64 {
    let mut ids: Vec<u64> = file
        .entities
        .values()
        .filter(|e| {
            e.type_name == "LENGTH_UNIT"
                || e.args.iter().any(
                    |a| matches!(a, StepValue::Typed { type_name, .. } if type_name == "LENGTH_UNIT"),
                )
        })
        .map(|e| e.id)
        .collect();
    ids.sort_unstable();
    for id in ids {
        let Some(e) = file.get(id) else { continue };
        let parts: Vec<(&str, &[StepValue])> =
            std::iter::once((e.type_name.as_str(), e.args.as_slice()))
                .chain(e.args.iter().filter_map(|a| match a {
                    StepValue::Typed { type_name, args } => {
                        Some((type_name.as_str(), args.as_slice()))
                    }
                    _ => None,
                }))
                .collect();
        for (type_name, args) in &parts {
            match *type_name {
                "SI_UNIT" => {
                    let prefix = args
                        .first()
                        .and_then(|v| v.as_enum())
                        .map(|p| p.trim_matches('.'));
                    return match prefix {
                        None => 1000.0,
                        Some("MILLI") => 1.0,
                        Some("CENTI") => 10.0,
                        Some("DECI") => 100.0,
                        Some("MICRO") => 1e-3,
                        Some("KILO") => 1e6,
                        Some(_) => 1.0,
                    };
                }
                "CONVERSION_BASED_UNIT" => {
                    let name = args.first().and_then(|v| v.as_string()).unwrap_or("");
                    match name.to_ascii_uppercase().as_str() {
                        "INCH" | "IN" => return 25.4,
                        "FOOT" | "FT" => return 304.8,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    1.0
}

/// Context for reading STEP files and building B-rep solids.
pub(crate) struct StepReader<'a> {
    pub(crate) file: &'a StepFile,
    /// Maps STEP vertex ID to vcad VertexId.
    vertex_map: HashMap<u64, VertexId>,
    /// Maps STEP edge ID to vcad EdgeId (pair of half-edges).
    edge_map: HashMap<u64, EdgeId>,
    /// Maps (STEP edge ID, orientation) to vcad HalfEdgeId.
    half_edge_map: HashMap<(u64, bool), HalfEdgeId>,
    /// Maps STEP surface ID to vcad geometry store index, plus a flag that
    /// is true when the stored surface's natural normal is flipped relative
    /// to the STEP surface normal (see `parse_surface_oriented`).
    surface_map: HashMap<u64, (usize, bool)>,
    /// For subdivided curved edges: maps (STEP edge ID, orientation) to
    /// an ordered chain of half-edge IDs that replace the single original.
    subdivided_edges: HashMap<(u64, bool), Vec<HalfEdgeId>>,
    /// Millimetres per file length unit (1.0 when the file declares mm or
    /// declares nothing).
    length_scale: f64,
    /// Optional progress/cancel sink (see [`read_step_with_progress`]).
    progress: Option<&'a StepProgress>,
}

impl<'a> StepReader<'a> {
    pub(crate) fn new(file: &'a StepFile) -> Self {
        Self {
            file,
            vertex_map: HashMap::new(),
            edge_map: HashMap::new(),
            half_edge_map: HashMap::new(),
            surface_map: HashMap::new(),
            subdivided_edges: HashMap::new(),
            length_scale: length_unit_mm(file),
            progress: None,
        }
    }

    /// Collect `MANIFOLD_SOLID_BREP` ids reachable through the product anchor:
    /// SHAPE_DEFINITION_REPRESENTATION -> representation -> items. Returns
    /// `None` when the file has no SDR entities at all (anchor-less file).
    ///
    /// Conforming importers only see geometry through this chain; when it is
    /// present we honor it, so a vcad-written file is read the same way
    /// Shapr3D/SolidWorks/Fusion read it — an anchor that references nothing
    /// becomes a loud error instead of a silent direct-scan pass.
    ///
    /// Assemblies add one hop: the anchored `SHAPE_REPRESENTATION` holds only
    /// placements, and the geometry hangs off it through a
    /// `SHAPE_REPRESENTATION_RELATIONSHIP` pointing at an
    /// `ADVANCED_BREP_SHAPE_REPRESENTATION`. Creo, SolidWorks and NX all write
    /// their assemblies this way, so the walk follows those links transitively.
    fn anchored_solid_ids(&self) -> Option<Vec<u64>> {
        let sdrs = self
            .file
            .entities_of_type("SHAPE_DEFINITION_REPRESENTATION");
        if sdrs.is_empty() {
            return None;
        }

        // rep -> related reps, via SHAPE_REPRESENTATION_RELATIONSHIP (both
        // directions: the relationship's orientation varies by writer).
        let mut linked: HashMap<u64, Vec<u64>> = HashMap::new();
        for rel in self
            .file
            .entities_of_type("SHAPE_REPRESENTATION_RELATIONSHIP")
        {
            let (Ok(a), Ok(b)) = (rel.entity_ref(2), rel.entity_ref(3)) else {
                continue;
            };
            linked.entry(a).or_default().push(b);
            linked.entry(b).or_default().push(a);
        }
        for next in linked.values_mut() {
            next.sort_unstable();
        }

        let mut ids = Vec::new();
        let mut visited: HashSet<u64> = HashSet::new();
        // Seed the walk in entity-id order: `entities_of_type` iterates a
        // HashMap, so without the sort the imported solid order would differ
        // from run to run.
        let mut seeds: Vec<u64> = sdrs.iter().filter_map(|s| s.entity_ref(1).ok()).collect();
        seeds.sort_unstable();
        let mut queue: std::collections::VecDeque<u64> = seeds.into();
        while let Some(rep_id) = queue.pop_front() {
            if !visited.insert(rep_id) {
                continue;
            }
            let Some(rep) = self.file.get(rep_id) else {
                continue;
            };
            if let Ok(items) = rep.entity_ref_list(1) {
                for item_id in items {
                    let is_solid = self
                        .file
                        .get(item_id)
                        .map(|e| is_solid_brep_type(&e.type_name))
                        .unwrap_or(false);
                    if is_solid && !ids.contains(&item_id) {
                        ids.push(item_id);
                    }
                }
            }
            if let Some(next) = linked.get(&rep_id) {
                queue.extend(next.iter().copied());
            }
        }
        Some(ids)
    }

    fn read_all_solids(&mut self) -> Result<ReadResult, StepError> {
        let solid_ids: Vec<u64> = match self.anchored_solid_ids() {
            // Anchor present: read exactly what it references. Empty means the
            // product structure points at no solids — fail rather than rescue
            // unreachable geometry a conforming importer would never show.
            Some(ids) => {
                if ids.is_empty() {
                    return Err(StepError::NoSolids);
                }
                ids
            }
            // Anchor-less file (legacy vcad exports, minimal foreign files):
            // fall back to scanning for solids directly.
            None => {
                let mut ids: Vec<u64> = SOLID_BREP_TYPES
                    .iter()
                    .flat_map(|t| self.file.entities_of_type(t))
                    .map(|e| e.id)
                    .collect();
                if ids.is_empty() {
                    return Err(StepError::NoSolids);
                }
                ids.sort_unstable();
                ids
            }
        };

        if let Some(p) = self.progress {
            // Total faces across every solid, so the bar spans the file.
            let total: usize = solid_ids
                .iter()
                .filter_map(|&id| parse_manifold_solid_brep(self.file, id).ok())
                .filter_map(|s| parse_shell(self.file, s.outer_shell_id).ok())
                .map(|sh| sh.face_ids.len())
                .sum();
            p.set_stage(StepStage::Faces, 0, total);
        }
        let mut solids = Vec::new();
        let mut report = StepImportReport::default();
        for entity_id in solid_ids {
            // Reset maps for each solid
            self.vertex_map.clear();
            self.edge_map.clear();
            self.half_edge_map.clear();
            self.surface_map.clear();
            self.subdivided_edges.clear();

            let (solid, solid_report) = self.read_solid(entity_id)?;
            solids.push(solid);
            report.solids.push(solid_report);
        }

        let styled = crate::style::styled_colors(self.file);
        let occurrences = crate::assembly::solid_occurrences(self.file).unwrap_or_default();
        let mut colors = Vec::with_capacity(solids.len());
        let mut names = Vec::with_capacity(solids.len());
        for r in &report.solids {
            let id = r.solid_id;
            let msb = parse_manifold_solid_brep(self.file, id).ok();
            let shells: Vec<u64> = msb.iter().map(|m| m.outer_shell_id).collect();
            let faces: Vec<u64> = shells
                .iter()
                .filter_map(|&sh| parse_shell(self.file, sh).ok())
                .flat_map(|sh| sh.face_ids)
                .collect();
            colors.push(crate::style::solid_color(&styled, id, &shells, &faces));
            // The product names the part — unless several solids share one
            // product (a multi-body part, vcad's own multi-solid export),
            // where only the solid's own name tells them apart.
            let product = occurrences
                .iter()
                .find(|o| o.solid_id == id)
                .filter(|o| {
                    let mut siblings = occurrences
                        .iter()
                        .filter(|p| p.part_name == o.part_name)
                        .map(|p| p.solid_id);
                    siblings.all(|s| s == id)
                })
                .map(|o| o.part_name.clone());
            let own = self
                .file
                .get(id)
                .and_then(|e| e.args.first())
                .and_then(|a| a.as_string())
                .map(str::to_owned);
            names.push(
                product
                    .into_iter()
                    .chain(own)
                    .find(|n| !n.trim().is_empty()),
            );
        }

        Ok(ReadResult {
            solids,
            report,
            colors,
            names,
            occurrences,
        })
    }

    fn check_cancelled(&self) -> Result<(), StepError> {
        self.progress.map_or(Ok(()), StepProgress::check)
    }

    pub(crate) fn read_solid(
        &mut self,
        solid_id: u64,
    ) -> Result<(BRepSolid, SolidImportReport), StepError> {
        use std::collections::HashSet;

        let mut topo = Topology::new();
        let mut geom = GeometryStore::new();

        let step_solid = parse_manifold_solid_brep(self.file, solid_id)?;
        let step_shell = parse_shell(self.file, step_solid.outer_shell_id)?;

        if step_shell.face_ids.len() > MAX_FACES_PER_SOLID {
            return Err(StepError::UnsupportedEntity(format!(
                "shell #{} has {} faces (cap {})",
                step_solid.outer_shell_id,
                step_shell.face_ids.len(),
                MAX_FACES_PER_SOLID
            )));
        }

        // Track faces we skip due to unsupported surface types
        let mut skipped_faces: HashSet<u64> = HashSet::new();
        let mut solid_report = SolidImportReport {
            solid_id,
            total_faces: step_shell.face_ids.len(),
            ..Default::default()
        };
        if !step_solid.void_shell_ids.is_empty() {
            // Only the outer shell is imported: internal cavities are not
            // visible, but the imported solid is filled where they were.
            solid_report.notes.push(format!(
                "BREP_WITH_VOIDS: {} internal void shell(s) not imported; volume excludes the cavities",
                step_solid.void_shell_ids.len()
            ));
        }

        // First pass: collect all vertices and surfaces
        for &face_id in &step_shell.face_ids {
            self.check_cancelled()?;
            let step_face = parse_advanced_face(self.file, face_id)?;

            // Parse and store surface - skip face if surface type unsupported
            if !self.surface_map.contains_key(&step_face.surface_id) {
                match parse_surface_oriented(self.file, step_face.surface_id) {
                    Ok((surface, sense_flipped)) => {
                        let idx = geom.add_surface(surface.into_box());
                        self.surface_map
                            .insert(step_face.surface_id, (idx, sense_flipped));
                    }
                    Err(StepError::UnsupportedEntity(reason)) => {
                        // Skip this face — surface type not supported. Recorded
                        // in the report so the omission is never silent.
                        skipped_faces.insert(face_id);
                        solid_report.skipped_faces.push(SkippedFace {
                            face_id,
                            surface_id: step_face.surface_id,
                            reason,
                        });
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            // Parse vertices from face bounds
            for bound in &step_face.bounds {
                let loop_ = parse_edge_loop(self.file, bound.loop_id)?;
                if loop_.edge_ids.len() > MAX_EDGES_PER_LOOP {
                    return Err(StepError::UnsupportedEntity(format!(
                        "loop #{} has {} edges (cap {})",
                        bound.loop_id,
                        loop_.edge_ids.len(),
                        MAX_EDGES_PER_LOOP
                    )));
                }
                for &oe_id in &loop_.edge_ids {
                    let oe = parse_oriented_edge(self.file, oe_id)?;
                    let edge = parse_edge_curve(self.file, oe.edge_id)?;

                    // Add vertices
                    if !self.vertex_map.contains_key(&edge.start_vertex_id) {
                        let v = parse_vertex_point(self.file, edge.start_vertex_id)?;
                        let vid = topo.add_vertex(v.point);
                        self.vertex_map.insert(edge.start_vertex_id, vid);
                    }
                    if !self.vertex_map.contains_key(&edge.end_vertex_id) {
                        let v = parse_vertex_point(self.file, edge.end_vertex_id)?;
                        let vid = topo.add_vertex(v.point);
                        self.vertex_map.insert(edge.end_vertex_id, vid);
                    }
                }
            }
        }

        // Second pass: create half-edges and edges.
        // For CIRCLE edges, sample intermediate points along the arc.
        // For B-spline/trimmed-curve edges, sample intermediate points too.
        for &face_id in &step_shell.face_ids {
            self.check_cancelled()?;
            if skipped_faces.contains(&face_id) {
                continue;
            }
            let step_face = parse_advanced_face(self.file, face_id)?;

            for bound in &step_face.bounds {
                let loop_ = parse_edge_loop(self.file, bound.loop_id)?;
                for &oe_id in &loop_.edge_ids {
                    let oe = parse_oriented_edge(self.file, oe_id)?;
                    let edge = parse_edge_curve(self.file, oe.edge_id)?;

                    // Skip if already processed
                    if self.half_edge_map.contains_key(&(edge.id, oe.orientation)) {
                        continue;
                    }

                    // The loop walks the edge start→end when the ORIENTED_EDGE
                    // says .T., end→start otherwise. `same_sense` relates the
                    // underlying curve's parameter to the edge and has no say
                    // in vertex order (it only steers which way an arc sweeps,
                    // in `sample_circle_arc`); folding it in here walked every
                    // same_sense=.F. edge backwards — collapsed slivers on
                    // lines, the complementary arc on circles.
                    let (start_v, end_v) = if oe.orientation {
                        (edge.start_vertex_id, edge.end_vertex_id)
                    } else {
                        (edge.end_vertex_id, edge.start_vertex_id)
                    };

                    // Try to parse the edge curve
                    let curve = parse_curve(self.file, edge.curve_id).ok();

                    // Determine whether this is a curved edge that needs subdivision
                    let needs_subdivision = matches!(
                        &curve,
                        Some(StepCurve::Circle(_))
                            | Some(StepCurve::BSpline(_))
                            | Some(StepCurve::Nurbs(_))
                            | Some(StepCurve::Trimmed(_))
                    );

                    if needs_subdivision {
                        self.subdivide_curved_edge(
                            &mut topo,
                            &edge,
                            &oe,
                            &curve.unwrap(),
                            start_v,
                            end_v,
                        );
                    } else {
                        // Simple edge: single half-edge per direction
                        let start_vid = self.vertex_map[&start_v];
                        let he_id = topo.add_half_edge(start_vid);
                        self.half_edge_map.insert((edge.id, oe.orientation), he_id);

                        let twin_start = self.vertex_map[&end_v];
                        let twin_he_id = topo.add_half_edge(twin_start);
                        self.half_edge_map
                            .insert((edge.id, !oe.orientation), twin_he_id);

                        use std::collections::hash_map::Entry;
                        if let Entry::Vacant(e) = self.edge_map.entry(edge.id) {
                            let he1 = self.half_edge_map[&(edge.id, true)];
                            let he2 = self.half_edge_map[&(edge.id, false)];
                            let edge_id = topo.add_edge(he1, he2);
                            e.insert(edge_id);
                        }
                    }
                }
            }
        }

        // Third pass: create loops, faces, and shell
        let mut vcad_face_ids = Vec::new();
        // Plane surface index -> index of its normal-flipped copy.
        let mut flipped_planes: HashMap<usize, usize> = HashMap::new();
        let mut reoriented_faces = 0usize;
        let mut reused_half_edges = 0usize;

        for &face_id in &step_shell.face_ids {
            self.check_cancelled()?;
            if let Some(p) = self.progress {
                p.tick();
            }
            if skipped_faces.contains(&face_id) {
                continue;
            }
            let step_face = parse_advanced_face(self.file, face_id)?;
            let (surface_idx, sense_flipped) = self.surface_map[&step_face.surface_id];

            let mut outer_loop: Option<LoopId> = None;
            let mut inner_loops = Vec::new();

            for bound in &step_face.bounds {
                let loop_ = parse_edge_loop(self.file, bound.loop_id)?;

                // Skip empty loops (VERTEX_LOOP returns empty edge list)
                if loop_.edge_ids.is_empty() {
                    continue;
                }

                // Collect half-edges for this loop.
                // For subdivided curved edges, expand to the chain of sub-half-edges.
                //
                // `FACE_BOUND(…, .F.)` uses its loop reversed: the edges in
                // the opposite order, each walked the other way. KiCad's
                // StepUp and OCCT write every bound of a part this way;
                // ignoring the flag wound each face against its neighbours
                // (656 doubly-claimed half-edges and an inside-out solid on
                // a SOIC-8 model).
                let mut loop_hes = Vec::new();
                let mut oe_ids = loop_.edge_ids.clone();
                if !bound.orientation {
                    oe_ids.reverse();
                }
                for &oe_id in &oe_ids {
                    let oe = parse_oriented_edge(self.file, oe_id)?;
                    let key = (oe.edge_id, oe.orientation == bound.orientation);
                    if let Some(chain) = self.subdivided_edges.get(&key) {
                        loop_hes.extend_from_slice(chain);
                    } else if let Some(&he_id) = self.half_edge_map.get(&key) {
                        loop_hes.push(he_id);
                    } else {
                        return Err(StepError::MissingEntity(oe.edge_id));
                    }
                }
                reused_half_edges += claim_loop_half_edges(&mut topo, &mut loop_hes);

                let loop_id = topo.add_loop(&loop_hes);

                if bound.is_outer {
                    outer_loop = Some(loop_id);
                } else {
                    inner_loops.push(loop_id);
                }
            }

            // If no explicit FACE_OUTER_BOUND, pick the loop with the largest
            // enclosed area as the outer loop. Many STEP exporters (e.g. Shapr3D)
            // use FACE_BOUND for everything, and the first bound listed is not
            // necessarily the enclosing boundary.
            if outer_loop.is_none() && !inner_loops.is_empty() {
                let mut best_idx = 0;
                let mut best_area = 0.0f64;
                for (i, &lid) in inner_loops.iter().enumerate() {
                    let area = loop_area_3d(&topo, lid);
                    if area > best_area {
                        best_area = area;
                        best_idx = i;
                    }
                }
                outer_loop = Some(inner_loops.remove(best_idx));
            }

            // Create face - skip if no bounds at all
            let outer = match outer_loop {
                Some(l) => l,
                None => continue,
            };

            // `same_sense` is expressed against the STEP surface normal;
            // XOR with the parse-time flag when our surface's natural
            // normal points the other way.
            let mut orientation = if step_face.same_sense != sense_flipped {
                Orientation::Forward
            } else {
                Orientation::Reversed
            };

            // The planar tessellator triangulates the outer loop as wound and
            // flips the result for a Reversed face, so it needs the loop to
            // wind about the plane's own normal. Conforming STEP winds loops
            // about the FACE normal instead — on a reversed face that is the
            // other way, and the plane would mesh facing into the solid.
            // (vcad's own writer winds about the surface normal.) Measure the
            // winding rather than assume a convention: when the loop runs
            // against the plane's normal, give the face a normal-flipped copy
            // of the plane and toggle its orientation — the same face normal,
            // now agreeing with its loop.
            let mut surface_idx = surface_idx;
            let flipped = geom.surfaces[surface_idx]
                .as_any()
                .downcast_ref::<vcad_kernel_geom::Plane>()
                .filter(|plane| {
                    loop_newell_normal(&topo, outer).dot(*plane.normal_dir.as_ref()) < 0.0
                })
                .map(|plane| {
                    vcad_kernel_geom::Plane::new(
                        plane.origin,
                        *plane.y_dir.as_ref(),
                        *plane.x_dir.as_ref(),
                    )
                });
            if let Some(flipped) = flipped {
                surface_idx = *flipped_planes
                    .entry(surface_idx)
                    .or_insert_with(|| geom.add_surface(Box::new(flipped)));
                orientation = match orientation {
                    Orientation::Forward => Orientation::Reversed,
                    Orientation::Reversed => Orientation::Forward,
                };
            }

            // A B-spline face's `same_sense` is checked against its own
            // boundary. Conforming STEP winds the outer loop counter-
            // clockwise about the FACE normal, and the loops are what the
            // neighbouring faces agree on (every edge is used once each way).
            // SolidWorks has been seen writing `same_sense` backwards on a
            // handful of fillet patches (411080037.STEP: 8 of 113), whose
            // meshes then faced into the solid against all their neighbours
            // — OCCT's reader repairs the same faces. Where the loop's
            // parameter-space winding is unambiguous, it decides.
            if geom.surfaces[surface_idx].surface_type() == vcad_kernel_geom::SurfaceKind::BSpline {
                if let Some(area) =
                    loop_winding_in_uv(&topo, outer, geom.surfaces[surface_idx].as_ref())
                {
                    let loop_says_forward = area > 0.0;
                    if loop_says_forward != (orientation == Orientation::Forward) {
                        orientation = if loop_says_forward {
                            Orientation::Forward
                        } else {
                            Orientation::Reversed
                        };
                        reoriented_faces += 1;
                    }
                }
            }

            let face_id = topo.add_face(outer, surface_idx, orientation);
            for inner in inner_loops {
                topo.add_inner_loop(face_id, inner);
            }
            vcad_face_ids.push(face_id);
        }

        if reused_half_edges > 0 {
            solid_report.notes.push(format!(
                "{reused_half_edges} half-edge{} used by two loops in the same direction (non-manifold or mis-oriented faces in the file); duplicated so each loop stays closed",
                if reused_half_edges == 1 { "" } else { "s" }
            ));
        }

        if reoriented_faces > 0 {
            solid_report.notes.push(format!(
                "{reoriented_faces} B-spline face{} oriented by its boundary loop: same_sense contradicted the loop winding",
                if reoriented_faces == 1 { "" } else { "s" }
            ));
        }

        // Create shell and solid
        // Note: vcad doesn't distinguish open/closed shells yet, so we always use Outer
        let _ = step_shell.is_closed; // acknowledged but unused
        let shell_id = topo.add_shell(vcad_face_ids, ShellType::Outer);
        let solid_id = topo.add_solid(shell_id);

        // Chord subdivision loses the analytic curve but not the geometry —
        // note it once per solid rather than per face.
        let subdivided = self.subdivided_edges.len() / 2;
        if subdivided > 0 {
            solid_report.notes.push(format!(
                "{subdivided} curved edge{} approximated by chordal subdivision",
                if subdivided == 1 { "" } else { "s" }
            ));
        }

        // STEP coordinates are in the file's declared length unit; vcad is
        // millimetres. Onshape (and many others) write metres.
        if self.length_scale != 1.0 {
            let t = vcad_kernel_math::Transform::scale(
                self.length_scale,
                self.length_scale,
                self.length_scale,
            );
            for (_id, v) in &mut topo.vertices {
                v.point = t.apply_point(&v.point);
            }
            for surface in &mut geom.surfaces {
                *surface = surface.transform(&t);
            }
            solid_report.notes.push(format!(
                "scaled from file length unit by {} to millimetres",
                self.length_scale
            ));
        }

        Ok((
            BRepSolid {
                topology: topo,
                geometry: geom,
                solid_id,
            },
            solid_report,
        ))
    }

    /// Subdivide a curved edge (circle, B-spline, or trimmed variant) into multiple
    /// segments by sampling intermediate points. This ensures loops with only 2
    /// vertices get enough points for tessellation.
    fn subdivide_curved_edge(
        &mut self,
        topo: &mut Topology,
        edge: &crate::entities::topology::StepEdge,
        oe: &crate::entities::topology::StepOrientedEdge,
        curve: &StepCurve,
        start_v: u64,
        end_v: u64,
    ) {
        // Compute sample parameters [t_start, ..., t_end]
        let mid_pts = self.sample_curve_intermediates(topo, curve, edge, oe, start_v, end_v);

        let start_vid = self.vertex_map[&start_v];
        let end_vid = self.vertex_map[&end_v];
        let n = mid_pts.len();

        // Build forward chain: start → m0 → m1 → ... → mn-1 → (end = next edge's start)
        let mut fwd_origins = vec![start_vid];
        fwd_origins.extend_from_slice(&mid_pts);

        // Reverse chain: end → mn-1 → ... → m0 → (start = next edge's start)
        let mut rev_origins = vec![end_vid];
        for &vid in mid_pts.iter().rev() {
            rev_origins.push(vid);
        }

        let mut fwd_chain = Vec::with_capacity(n + 1);
        let mut rev_chain = Vec::with_capacity(n + 1);

        for &origin in &fwd_origins {
            let he = topo.add_half_edge(origin);
            fwd_chain.push(he);
        }
        for &origin in &rev_origins {
            let he = topo.add_half_edge(origin);
            rev_chain.push(he);
        }

        // Pair forward[i] with reverse[n-i] into edges
        for i in 0..fwd_chain.len() {
            let fwd_he = fwd_chain[i];
            let rev_he = rev_chain[fwd_chain.len() - 1 - i];
            topo.add_edge(fwd_he, rev_he);
        }

        self.subdivided_edges
            .insert((edge.id, oe.orientation), fwd_chain);
        self.subdivided_edges
            .insert((edge.id, !oe.orientation), rev_chain);
    }

    /// Compute intermediate 3D sample points along a curved edge.
    ///
    /// Returns a list of `VertexId`s (not including the start/end vertices)
    /// ordered from start to end.
    fn sample_curve_intermediates(
        &mut self,
        topo: &mut Topology,
        curve: &StepCurve,
        edge: &crate::entities::topology::StepEdge,
        oe: &crate::entities::topology::StepOrientedEdge,
        start_v: u64,
        end_v: u64,
    ) -> Vec<VertexId> {
        let resolved = curve.resolve();
        match resolved {
            StepCurve::Circle(circle) => {
                self.sample_circle_arc(topo, circle, curve, edge, oe, start_v, end_v)
            }
            StepCurve::BSpline(_) | StepCurve::Nurbs(_) => {
                self.sample_bspline(topo, curve, edge, oe, start_v, end_v)
            }
            // Line or other simple curves don't need intermediate points
            _ => vec![],
        }
    }

    /// Sample a circle arc using either trim parameters (if available via TRIMMED_CURVE)
    /// or vertex endpoint projection.
    #[allow(clippy::too_many_arguments)]
    fn sample_circle_arc(
        &mut self,
        topo: &mut Topology,
        circle: &vcad_kernel_geom::Circle3d,
        curve: &StepCurve,
        edge: &crate::entities::topology::StepEdge,
        oe: &crate::entities::topology::StepOrientedEdge,
        start_v: u64,
        end_v: u64,
    ) -> Vec<VertexId> {
        let start_pt = topo.vertices[self.vertex_map[&start_v]].point;
        let end_pt = topo.vertices[self.vertex_map[&end_v]].point;

        // Prefer explicit trim parameters from TRIMMED_CURVE wrapper
        let (t_start, t_end, forward) = if let StepCurve::Trimmed(tc) = curve {
            let fwd = oe.orientation == edge.same_sense;
            if fwd == tc.sense_agreement {
                (tc.trim1, tc.trim2, true)
            } else {
                (tc.trim2, tc.trim1, false)
            }
        } else {
            // Fall back to projecting vertex endpoints onto the circle
            let ts = point_angle_on_circle(circle, &start_pt);
            let te = point_angle_on_circle(circle, &end_pt);
            (ts, te, oe.orientation == edge.same_sense)
        };

        // Compute the arc sweep (always in the direction of traversal)
        let sweep = if forward {
            let mut s = t_end - t_start;
            if s <= 1e-10 {
                s += std::f64::consts::TAU;
            }
            s
        } else {
            let mut s = t_start - t_end;
            if s <= 1e-10 {
                s += std::f64::consts::TAU;
            }
            -s
        };

        let n = arc_sample_count(sweep);
        let mut mid_vids = Vec::with_capacity(n);
        for i in 1..=n {
            let frac = i as f64 / (n + 1) as f64;
            let t = t_start + sweep * frac;
            let pt = circle.evaluate(t);
            let vid = topo.add_vertex(pt);
            mid_vids.push(vid);
        }
        mid_vids
    }

    /// Sample a B-spline (or trimmed B-spline) curve at `CURVE_SAMPLE_COUNT` points,
    /// ordered in the direction the loop traverses the edge (`start_v` →
    /// `end_v`).
    ///
    /// The samples come out in curve-parameter order, which is the traversal
    /// order only when the oriented edge walks the curve forwards. Emitting
    /// them unreversed on a backwards edge made the loop jump to the far end,
    /// run back, and jump again — a self-crossing UV polygon that rejected
    /// every ray hit on the face (Onshape's torus fillets).
    #[allow(clippy::too_many_arguments)]
    fn sample_bspline(
        &mut self,
        topo: &mut Topology,
        curve: &StepCurve,
        edge: &crate::entities::topology::StepEdge,
        oe: &crate::entities::topology::StepOrientedEdge,
        start_v: u64,
        end_v: u64,
    ) -> Vec<VertexId> {
        let (t_min, t_max) = curve.domain();
        let n = CURVE_SAMPLE_COUNT;
        let pts: Vec<_> = (1..=n)
            .map(|i| curve.evaluate(t_min + (t_max - t_min) * (i as f64 / (n + 1) as f64)))
            .collect();

        let start_pt = topo.vertices[self.vertex_map[&start_v]].point;
        let end_pt = topo.vertices[self.vertex_map[&end_v]].point;
        let backwards = if (start_pt - end_pt).norm() > 1e-9 {
            // Whichever endpoint the first sample sits nearer is where the
            // curve starts.
            (pts[0] - end_pt).norm() < (pts[0] - start_pt).norm()
        } else {
            // A closed curve has no nearer end; fall back to the senses.
            oe.orientation != edge.same_sense
        };

        let mut mid_vids: Vec<VertexId> = pts.into_iter().map(|pt| topo.add_vertex(pt)).collect();
        if backwards {
            mid_vids.reverse();
        }
        mid_vids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One face of a cube rewritten to walk its edges the same way as its
    /// neighbours do (every ORIENTED_EDGE flag flipped, loop reversed): each
    /// of those half-edges is then claimed by two loops. KiCad boards (via
    /// OCCT) do this on every hole, and the shared half-edge used to be
    /// rewired into a `next` cycle that never returned — the import hung.
    #[test]
    fn half_edge_used_by_two_loops_does_not_hang() {
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let content =
            String::from_utf8(crate::writer::write_step_to_buffer(&cube).unwrap()).unwrap();
        let file = Parser::parse(content.as_bytes()).unwrap();
        let mut loops = file.entities_of_type("EDGE_LOOP");
        loops.sort_by_key(|e| e.id);
        let oes = loops[0].entity_ref_list(1).unwrap();
        let mut out = String::new();
        for line in content.lines() {
            let id: Option<u64> = line
                .strip_prefix('#')
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse().ok());
            let line = match id {
                Some(id) if oes.contains(&id) => {
                    if line.contains(".T.") {
                        line.replace(".T.", ".F.")
                    } else {
                        line.replace(".F.", ".T.")
                    }
                }
                _ => line.to_string(),
            };
            out.push_str(&line);
            out.push('\n');
        }
        let r = read_step_from_buffer_with_report(out.as_bytes()).unwrap();
        assert_eq!(r.solids[0].topology.faces.len(), 6);
        assert!(
            r.report.solids[0]
                .notes
                .iter()
                .any(|n| n.contains("used by two loops")),
            "{:?}",
            r.report.solids[0].notes
        );
        // Every loop walk terminates and closes.
        let topo = &r.solids[0].topology;
        for (_, face) in topo.faces.iter() {
            let n = topo.loop_half_edges(face.outer_loop).count();
            assert!((3..=64).contains(&n), "loop of {n}");
        }
    }

    /// The same cube, one face written the KiCad/OCCT way: its loop listed
    /// backwards with every edge flipped, and `FACE_OUTER_BOUND(…, .F.)` to
    /// say "use it reversed". That is the identical face; reading it must
    /// claim no half-edge twice.
    #[test]
    fn reversed_face_bound_is_honoured() {
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let content =
            String::from_utf8(crate::writer::write_step_to_buffer(&cube).unwrap()).unwrap();
        let file = Parser::parse(content.as_bytes()).unwrap();
        let mut bounds = file.entities_of_type("FACE_OUTER_BOUND");
        bounds.sort_by_key(|e| e.id);
        let bound_id = bounds[0].id;
        let loop_id = bounds[0].entity_ref(1).unwrap();
        let oes = file.get(loop_id).unwrap().entity_ref_list(1).unwrap();
        let id_of = |line: &str| -> Option<u64> {
            line.strip_prefix('#')
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse().ok())
        };
        let mut out = String::new();
        for line in content.lines() {
            let line = match id_of(line) {
                Some(id) if oes.contains(&id) => {
                    if line.contains(".T.") {
                        line.replace(".T.", ".F.")
                    } else {
                        line.replace(".F.", ".T.")
                    }
                }
                Some(id) if id == loop_id => {
                    let rev: Vec<String> = oes.iter().rev().map(|o| format!("#{o}")).collect();
                    format!("#{loop_id} = EDGE_LOOP('',({}));", rev.join(","))
                }
                Some(id) if id == bound_id => line.replace(".T.", ".F."),
                _ => line.to_string(),
            };
            out.push_str(&line);
            out.push('\n');
        }
        assert_ne!(out, content);
        let r = read_step_from_buffer_with_report(out.as_bytes()).unwrap();
        assert!(
            !r.report.solids[0]
                .notes
                .iter()
                .any(|n| n.contains("used by two loops")),
            "{:?}",
            r.report.solids[0].notes
        );
        let topo = &r.solids[0].topology;
        for (_, face) in topo.faces.iter() {
            assert_eq!(topo.loop_half_edges(face.outer_loop).count(), 4);
        }
    }

    #[test]
    fn colours_and_names_survive_a_round_trip() {
        let a = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let b = vcad_kernel_primitives::make_cube(5.0, 5.0, 5.0);
        let buf = crate::writer::write_step_solids_styled_to_buffer(&[
            (&a, "red block", Some([0.8, 0.1, 0.2])),
            (&b, "plain block", None),
        ])
        .unwrap();
        let r = read_step_from_buffer_with_report(&buf).unwrap();
        assert_eq!(r.solids.len(), 2);
        assert_eq!(r.colors, vec![Some([0.8, 0.1, 0.2]), None]);
        assert_eq!(
            r.names,
            vec![
                Some("red block".to_string()),
                Some("plain block".to_string())
            ]
        );
        assert_eq!(r.occurrences.len(), 2);
    }

    /// An anchor (SDR -> representation) that references no solids must fail
    /// loudly, even when unreachable MANIFOLD_SOLID_BREPs exist in the file —
    /// that is exactly what a conforming importer sees as "no 3D geometry".
    #[test]
    fn test_broken_anchor_is_detected() {
        let cube = vcad_kernel_primitives::make_cube(10.0, 10.0, 10.0);
        let buffer = crate::writer::write_step_to_buffer(&cube).unwrap();
        let content = String::from_utf8_lossy(&buffer);

        // Empty the ABSR's item list, orphaning the solid.
        let start = content
            .find("ADVANCED_BREP_SHAPE_REPRESENTATION")
            .expect("writer must emit an ABSR");
        let open = content[start..].find(",(").unwrap() + start;
        let close = content[open..].find(')').unwrap() + open;
        let broken = format!("{},(){}", &content[..open], &content[close + 1..]);
        assert!(
            broken.contains("MANIFOLD_SOLID_BREP"),
            "solid still present"
        );

        let err = read_step_from_buffer(broken.as_bytes()).unwrap_err();
        assert!(matches!(err, StepError::NoSolids), "got {err:?}");
    }

    /// Writer-emitted files are read through the anchor, not the fallback scan.
    #[test]
    fn test_anchored_read_finds_solids() {
        let cube = vcad_kernel_primitives::make_cube(10.0, 20.0, 30.0);
        let buffer = crate::writer::write_step_to_buffer(&cube).unwrap();
        let step_file = Parser::parse(&buffer).unwrap();
        let reader = StepReader::new(&step_file);
        let anchored = reader
            .anchored_solid_ids()
            .expect("writer output must contain an SDR anchor");
        assert_eq!(anchored.len(), 1);
        let solids = read_step_from_buffer(&buffer).unwrap();
        assert_eq!(solids.len(), 1);
    }

    /// A minimal STEP file representing a 10 mm box.
    const BOX_STEP: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''), '2;1');
FILE_NAME('box.step', '2024-01-01', (''), (''), '', '', '');
FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));
ENDSEC;
DATA;
/* Points */
#1 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#2 = CARTESIAN_POINT('', (10.0, 0.0, 0.0));
#3 = CARTESIAN_POINT('', (10.0, 10.0, 0.0));
#4 = CARTESIAN_POINT('', (0.0, 10.0, 0.0));
#5 = CARTESIAN_POINT('', (0.0, 0.0, 10.0));
#6 = CARTESIAN_POINT('', (10.0, 0.0, 10.0));
#7 = CARTESIAN_POINT('', (10.0, 10.0, 10.0));
#8 = CARTESIAN_POINT('', (0.0, 10.0, 10.0));

/* Vertices */
#11 = VERTEX_POINT('', #1);
#12 = VERTEX_POINT('', #2);
#13 = VERTEX_POINT('', #3);
#14 = VERTEX_POINT('', #4);
#15 = VERTEX_POINT('', #5);
#16 = VERTEX_POINT('', #6);
#17 = VERTEX_POINT('', #7);
#18 = VERTEX_POINT('', #8);

/* Directions for placements */
#20 = DIRECTION('', (0.0, 0.0, 1.0));
#21 = DIRECTION('', (1.0, 0.0, 0.0));
#22 = DIRECTION('', (0.0, 0.0, -1.0));
#23 = DIRECTION('', (0.0, 1.0, 0.0));
#24 = DIRECTION('', (0.0, -1.0, 0.0));
#25 = DIRECTION('', (-1.0, 0.0, 0.0));

/* Axis placements for surfaces */
#30 = AXIS2_PLACEMENT_3D('', #1, #22, #21);
#31 = AXIS2_PLACEMENT_3D('', #5, #20, #21);
#32 = AXIS2_PLACEMENT_3D('', #1, #24, #21);
#33 = AXIS2_PLACEMENT_3D('', #4, #23, #21);
#34 = AXIS2_PLACEMENT_3D('', #1, #25, #23);
#35 = AXIS2_PLACEMENT_3D('', #2, #21, #23);

/* Surfaces */
#40 = PLANE('', #30);
#41 = PLANE('', #31);
#42 = PLANE('', #32);
#43 = PLANE('', #33);
#44 = PLANE('', #34);
#45 = PLANE('', #35);

/* Directions for line vectors */
#50 = DIRECTION('', (1.0, 0.0, 0.0));
#51 = DIRECTION('', (0.0, 1.0, 0.0));
#52 = DIRECTION('', (0.0, 0.0, 1.0));
#53 = DIRECTION('', (-1.0, 0.0, 0.0));
#54 = DIRECTION('', (0.0, -1.0, 0.0));
#55 = DIRECTION('', (0.0, 0.0, -1.0));

/* Vectors for lines */
#60 = VECTOR('', #50, 10.0);
#61 = VECTOR('', #51, 10.0);
#62 = VECTOR('', #52, 10.0);
#63 = VECTOR('', #53, 10.0);
#64 = VECTOR('', #54, 10.0);
#65 = VECTOR('', #55, 10.0);

/* Lines */
#70 = LINE('', #1, #60);
#71 = LINE('', #2, #61);
#72 = LINE('', #3, #63);
#73 = LINE('', #4, #64);
#74 = LINE('', #1, #61);
#75 = LINE('', #5, #60);
#76 = LINE('', #6, #61);
#77 = LINE('', #7, #63);
#78 = LINE('', #8, #64);
#79 = LINE('', #1, #62);
#80 = LINE('', #2, #62);
#81 = LINE('', #3, #62);
#82 = LINE('', #4, #62);

/* Edges - bottom face */
#100 = EDGE_CURVE('', #11, #12, #70, .T.);
#101 = EDGE_CURVE('', #12, #13, #71, .T.);
#102 = EDGE_CURVE('', #13, #14, #72, .T.);
#103 = EDGE_CURVE('', #14, #11, #73, .T.);

/* Edges - top face */
#104 = EDGE_CURVE('', #15, #16, #75, .T.);
#105 = EDGE_CURVE('', #16, #17, #76, .T.);
#106 = EDGE_CURVE('', #17, #18, #77, .T.);
#107 = EDGE_CURVE('', #18, #15, #78, .T.);

/* Edges - vertical */
#108 = EDGE_CURVE('', #11, #15, #79, .T.);
#109 = EDGE_CURVE('', #12, #16, #80, .T.);
#110 = EDGE_CURVE('', #13, #17, #81, .T.);
#111 = EDGE_CURVE('', #14, #18, #82, .T.);

/* Oriented edges - bottom face (CCW from below = CW from above) */
#120 = ORIENTED_EDGE('', *, *, #100, .F.);
#121 = ORIENTED_EDGE('', *, *, #103, .F.);
#122 = ORIENTED_EDGE('', *, *, #102, .F.);
#123 = ORIENTED_EDGE('', *, *, #101, .F.);

/* Oriented edges - top face (CCW from above) */
#124 = ORIENTED_EDGE('', *, *, #104, .T.);
#125 = ORIENTED_EDGE('', *, *, #105, .T.);
#126 = ORIENTED_EDGE('', *, *, #106, .T.);
#127 = ORIENTED_EDGE('', *, *, #107, .T.);

/* Oriented edges - front face */
#130 = ORIENTED_EDGE('', *, *, #100, .T.);
#131 = ORIENTED_EDGE('', *, *, #109, .T.);
#132 = ORIENTED_EDGE('', *, *, #104, .F.);
#133 = ORIENTED_EDGE('', *, *, #108, .F.);

/* Oriented edges - back face */
#134 = ORIENTED_EDGE('', *, *, #102, .T.);
#135 = ORIENTED_EDGE('', *, *, #111, .T.);
#136 = ORIENTED_EDGE('', *, *, #106, .F.);
#137 = ORIENTED_EDGE('', *, *, #110, .F.);

/* Oriented edges - left face */
#138 = ORIENTED_EDGE('', *, *, #103, .T.);
#139 = ORIENTED_EDGE('', *, *, #108, .T.);
#140 = ORIENTED_EDGE('', *, *, #107, .F.);
#141 = ORIENTED_EDGE('', *, *, #111, .F.);

/* Oriented edges - right face */
#142 = ORIENTED_EDGE('', *, *, #101, .T.);
#143 = ORIENTED_EDGE('', *, *, #110, .T.);
#144 = ORIENTED_EDGE('', *, *, #105, .F.);
#145 = ORIENTED_EDGE('', *, *, #109, .F.);

/* Edge loops */
#150 = EDGE_LOOP('', (#120, #121, #122, #123));
#151 = EDGE_LOOP('', (#124, #125, #126, #127));
#152 = EDGE_LOOP('', (#130, #131, #132, #133));
#153 = EDGE_LOOP('', (#134, #135, #136, #137));
#154 = EDGE_LOOP('', (#138, #139, #140, #141));
#155 = EDGE_LOOP('', (#142, #143, #144, #145));

/* Face bounds */
#160 = FACE_OUTER_BOUND('', #150, .T.);
#161 = FACE_OUTER_BOUND('', #151, .T.);
#162 = FACE_OUTER_BOUND('', #152, .T.);
#163 = FACE_OUTER_BOUND('', #153, .T.);
#164 = FACE_OUTER_BOUND('', #154, .T.);
#165 = FACE_OUTER_BOUND('', #155, .T.);

/* Faces */
#170 = ADVANCED_FACE('', (#160), #40, .T.);
#171 = ADVANCED_FACE('', (#161), #41, .T.);
#172 = ADVANCED_FACE('', (#162), #42, .T.);
#173 = ADVANCED_FACE('', (#163), #43, .T.);
#174 = ADVANCED_FACE('', (#164), #44, .T.);
#175 = ADVANCED_FACE('', (#165), #45, .T.);

/* Shell */
#180 = CLOSED_SHELL('', (#170, #171, #172, #173, #174, #175));

/* Solid */
#190 = MANIFOLD_SOLID_BREP('Box', #180);

ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn test_read_simple_box() {
        // A minimal STEP file representing a simple box
        let step_content = BOX_STEP;

        let solids = read_step_from_buffer(step_content.as_bytes()).unwrap();
        assert_eq!(solids.len(), 1);

        let solid = &solids[0];
        assert_eq!(solid.topology.vertices.len(), 8);
        assert_eq!(solid.topology.faces.len(), 6);
        assert_eq!(solid.geometry.surfaces.len(), 6);
    }

    #[test]
    fn same_sense_false_edge_keeps_vertex_order() {
        // same_sense only says the LINE's direction runs against the edge;
        // the loop still walks #11 -> #12. Reading it as a vertex-order flip
        // repeated a corner in both faces that use the edge.
        let step_content = BOX_STEP.replace(
            "#100 = EDGE_CURVE('', #11, #12, #70, .T.);",
            "#100 = EDGE_CURVE('', #11, #12, #70, .F.);",
        );
        let solids = read_step_from_buffer(step_content.as_bytes()).unwrap();
        let topo = &solids[0].topology;
        for (_, face) in &topo.faces {
            let verts = topo.loop_vertices(face.outer_loop);
            let mut distinct = verts.clone();
            distinct.sort();
            distinct.dedup();
            assert_eq!(distinct.len(), 4, "face loop {verts:?}");
        }
    }

    #[test]
    fn test_read_brep_with_voids() {
        // Onshape writes hollow bodies as BREP_WITH_VOIDS; the outer shell is
        // imported and the dropped voids are reported, not silently lost.
        let step_content = BOX_STEP
            .replace(
                "#190 = MANIFOLD_SOLID_BREP('Box', #180);",
                "#185 = ORIENTED_CLOSED_SHELL('', *, #180, .F.);\n#190 = BREP_WITH_VOIDS('Box', #180, (#185));",
            );
        let result = read_step_from_buffer_with_report(step_content.as_bytes()).unwrap();
        assert_eq!(result.solids.len(), 1);
        assert_eq!(result.solids[0].topology.faces.len(), 6);
        let notes = &result.report.solids[0].notes;
        assert!(
            notes
                .iter()
                .any(|n| n.contains("BREP_WITH_VOIDS: 1 internal void")),
            "{notes:?}"
        );
    }

    #[test]
    fn test_metre_length_unit_scales_to_mm() {
        let step_content = BOX_STEP.replace(
            "/* Solid */",
            "#200 = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT($,.METRE.) );\n/* Solid */",
        );
        let solids = read_step_from_buffer(step_content.as_bytes()).unwrap();
        let max_x = solids[0]
            .topology
            .vertices
            .values()
            .map(|v| v.point.x)
            .fold(f64::MIN, f64::max);
        assert!((max_x - 10_000.0).abs() < 1e-9, "max_x = {max_x}");
    }

    #[test]
    fn test_no_solids() {
        let step_content = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''), '2;1');
ENDSEC;
DATA;
#1 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
ENDSEC;
END-ISO-10303-21;
"#;
        let result = read_step_from_buffer(step_content.as_bytes());
        assert!(matches!(result, Err(StepError::NoSolids)));
    }
}
