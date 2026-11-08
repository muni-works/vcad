//! Drawings workspace for the native app: 2D shop drawings on
//! `vcad-kernel-drafting`.
//!
//! One request (view direction, hidden-line/dimension toggles, detail views,
//! GD&T frames) plus the document's persisted `drawing` settings (title block,
//! section lines, BOM toggle) compose a [`DrawingSheet`] with the same layout
//! the web app's PDF export uses (`packages/app/src/lib/drawing-sheet.ts`).
//! The app draws that sheet's display list directly, so what you see is the
//! PDF. DXF goes through [`projected_view_to_dxf`] — the writer behind the web
//! app's "Export DXF" — so the two exports are byte-identical for one view.

use std::ffi::c_char;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vcad_eval::EvaluatedMesh;
use vcad_ir::{Document, DrawingSectionLine, DrawingSettings, DrawingTitleBlock};
use vcad_kernel_drafting::{
    compose_sheet, create_detail_view, offset_section_mesh, project_mesh, projected_view_to_dxf,
    sheet_to_pdf, AnnotationLayer, BomRow, BoundingBox2D, DetailViewParams, DrawingSheet,
    HatchPattern, OffsetSectionPlane, OffsetSectionStep, PlacedDetailView, PlacedProjectedView,
    PlacedSectionView, Point2D, ProjectedView, RenderedDimension, RenderedText, SectionPlane,
    SheetSize, SheetSpec, TextAlignment, TitleBlockFields, ViewDirection, Visibility,
};
use vcad_kernel_tessellate::TriangleMesh;

use crate::err;
use crate::json::{borrow_str, guard_ptr, json_out};

/// DXF of the active view's projection (web parity).
pub const VCAD_DRAWING_DXF: u32 = 0;
/// PDF of the composed sheet.
pub const VCAD_DRAWING_PDF: u32 = 1;

/// A detail view request (magnified region of the main view).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DetailRequest {
    /// Label letter ("A" → "DETAIL A").
    pub label: String,
    /// Region center in view coordinates (mm).
    pub center: [f64; 2],
    /// Magnification.
    #[serde(default = "two")]
    pub scale: f64,
    /// Region width in view mm.
    pub width: f64,
    /// Region height in view mm.
    pub height: f64,
}

fn two() -> f64 {
    2.0
}

/// A GD&T annotation request, anchored in view coordinates.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GdtRequest {
    /// `flatness`, `position`, `perpendicularity`, or `datum`.
    pub kind: String,
    /// Tolerance zone (mm); ignored for `datum`.
    #[serde(default)]
    pub tolerance: f64,
    /// Datum letter (reference for position/perpendicularity, the label for `datum`).
    #[serde(default)]
    pub datum: Option<String>,
    /// Anchor in view coordinates (mm).
    pub at: [f64; 2],
}

/// What to draw. Section lines, title block and BOM come from the document's
/// `drawing` settings unless `drawing` overrides them (unsaved edits).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DrawingRequest {
    /// `front`, `back`, `top`, `bottom`, `left`, `right`, `isometric`.
    #[serde(default)]
    pub view: Option<String>,
    /// Draw hidden edges dashed (default true).
    #[serde(default)]
    pub show_hidden: Option<bool>,
    /// Overall dimensions (default true).
    #[serde(default)]
    pub show_dimensions: Option<bool>,
    /// Detail views.
    #[serde(default)]
    pub details: Vec<DetailRequest>,
    /// GD&T frames and datum symbols.
    #[serde(default)]
    pub gdt: Vec<GdtRequest>,
    /// Override for the document's drawing settings.
    #[serde(default)]
    pub drawing: Option<DrawingSettings>,
    /// Directory relative mesh paths resolve against (the document's folder).
    #[serde(default)]
    pub base_dir: Option<String>,
}

/// View → sheet mapping for the main view, so the app can turn a click on
/// the sheet into view coordinates (placing details, sections, GD&T).
#[derive(Debug, Clone, Serialize)]
pub struct ViewPlacement {
    /// Sheet position of the view-bounds center (mm).
    pub sheet_center: [f64; 2],
    /// View-bounds center in view coordinates.
    pub view_center: [f64; 2],
    /// View → sheet scale.
    pub scale: f64,
    /// Scale note ("1:2").
    pub scale_note: String,
    /// View bounds.
    pub bounds: BoundingBox2D,
}

/// Composed sheet plus the facts the app needs to interact with it.
#[derive(Debug, Clone, Serialize)]
pub struct DrawingResult {
    /// Sheet width (mm).
    pub width: f64,
    /// Sheet height (mm).
    pub height: f64,
    /// The display list (sheet mm, origin bottom-left).
    pub sheet: DrawingSheet,
    /// Main view mapping.
    pub main: ViewPlacement,
    /// Visible / hidden edge counts of the main view.
    pub visible_edges: usize,
    /// Hidden edge count.
    pub hidden_edges: usize,
    /// Entities a DXF export of this view will contain.
    pub dxf_entities: usize,
    /// Section views that resolved to geometry.
    pub sections: usize,
}

fn view_direction(name: &str) -> ViewDirection {
    match name.to_lowercase().as_str() {
        "back" => ViewDirection::Back,
        "top" => ViewDirection::Top,
        "bottom" => ViewDirection::Bottom,
        "left" => ViewDirection::Left,
        "right" => ViewDirection::Right,
        "isometric" => ViewDirection::ISOMETRIC_STANDARD,
        _ => ViewDirection::Front,
    }
}

type Vec3 = [f64; 3];

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: Vec3) -> Vec3 {
    let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if n > 1e-12 {
        [a[0] / n, a[1] / n, a[2] / n]
    } else {
        a
    }
}

/// Orthographic basis `(right, up, forward)`, mirroring the web's `viewBasis`.
fn view_basis(dir: &str) -> Option<(Vec3, Vec3, Vec3)> {
    let (forward, world_up): (Vec3, Vec3) = match dir {
        "front" => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        "back" => ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
        "top" => ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        "bottom" => ([0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
        "right" => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        "left" => ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        _ => return None,
    };
    let right = normalize(cross(world_up, forward));
    let up = normalize(cross(forward, right));
    Some((right, up, forward))
}

/// Offset section plane from a cut polyline (port of `sectionPlaneFromLine`).
fn section_plane_from_line(dir: &str, points: &[[f64; 2]]) -> Option<OffsetSectionPlane> {
    let (right, _up, forward) = view_basis(dir)?;
    if points.len() < 2 {
        return None;
    }
    let mut steps = Vec::new();
    let mut base_x: Option<f64> = None;
    for w in points.windows(2) {
        let ([x1, y1], [x2, y2]) = (w[0], w[1]);
        if (y2 - y1).abs() <= (x2 - x1).abs() {
            continue; // jog segment
        }
        let x = (x1 + x2) / 2.0;
        let base = *base_x.get_or_insert(x);
        steps.push(OffsetSectionStep::new(y1.min(y2), y1.max(y2), x - base));
    }
    let base_x = base_x?;
    let all_flat = steps.iter().all(|s| s.offset == 0.0);
    Some(OffsetSectionPlane::new(
        SectionPlane::from_arrays(
            [right[0] * base_x, right[1] * base_x, right[2] * base_x],
            right,
            forward,
        ),
        if all_flat { Vec::new() } else { steps },
    ))
}

/// Conventional drawing scale at or below `s` (port of `niceScale`).
fn nice_scale(s: f64) -> (f64, String) {
    const STANDARDS: [f64; 13] = [
        100.0, 50.0, 20.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1, 0.05, 0.02, 0.01,
    ];
    let value = STANDARDS
        .iter()
        .copied()
        .find(|v| *v <= s)
        .unwrap_or(STANDARDS[STANDARDS.len() - 1]);
    let note = if value >= 1.0 {
        format!("{value}:1")
    } else {
        format!("1:{}", (1.0 / value).round())
    };
    (value, note)
}

fn kernel_title_block(tb: &DrawingTitleBlock, scale_note: &str) -> TitleBlockFields {
    let or = |s: &str, d: &str| {
        if s.is_empty() {
            d.to_string()
        } else {
            s.to_string()
        }
    };
    TitleBlockFields {
        part_name: or(&tb.part_name, "UNTITLED"),
        material: tb.material.clone(),
        finish: String::new(),
        scale: or(&tb.scale, scale_note),
        drawn_by: tb.author.clone(),
        date: tb.date.clone(),
        revision: or(&tb.revision, "A"),
        units: "MM".into(),
        tolerance_note: "±0.1 UNLESS NOTED".into(),
    }
}

/// Map a rendered dimension from view to sheet coordinates (port of
/// `dimensionToSheet`): geometry scales, text/arrow sizes stay in sheet mm.
fn dimension_to_sheet(rd: &RenderedDimension, place: &ViewPlacement) -> RenderedDimension {
    let map = |p: Point2D| place.to_sheet(p);
    let mut out = rd.clone();
    for l in &mut out.lines {
        *l = (map(l.0), map(l.1));
    }
    for a in &mut out.arcs {
        a.center = map(a.center);
        a.radius *= place.scale;
    }
    for a in &mut out.arrows {
        a.tip = map(a.tip);
    }
    for t in &mut out.texts {
        t.position = map(t.position);
    }
    out
}

impl ViewPlacement {
    fn to_sheet(&self, p: Point2D) -> Point2D {
        Point2D::new(
            self.sheet_center[0] + (p.x - self.view_center[0]) * self.scale,
            self.sheet_center[1] + (p.y - self.view_center[1]) * self.scale,
        )
    }
}

/// Cut-line annotation (polyline + end labels), view coordinates.
fn cut_line(section: &DrawingSectionLine) -> RenderedDimension {
    let pts: Vec<Point2D> = section
        .points
        .iter()
        .map(|p| Point2D::new(p[0], p[1]))
        .collect();
    let mut rd = RenderedDimension::new();
    for w in pts.windows(2) {
        rd.lines.push((w[0], w[1]));
    }
    if let (Some(a), Some(b)) = (pts.first(), pts.last()) {
        for p in [a, b] {
            let mut t = RenderedText::new(*p, section.label.clone(), 4.0);
            t.alignment = TextAlignment::MiddleCenter;
            rd.texts.push(t);
        }
    }
    rd
}

/// Concatenate meshes into one (web: `combineSceneMeshes`).
fn combine(meshes: &[EvaluatedMesh]) -> TriangleMesh {
    let mut out = TriangleMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        normals: Vec::new(),
        face_kinds: Vec::new(),
        face_ids: Vec::new(),
    };
    for m in meshes {
        let base = (out.vertices.len() / 3) as u32;
        out.vertices.extend_from_slice(&m.positions);
        out.indices.extend(m.indices.iter().map(|i| i + base));
    }
    out
}

/// Evaluated root/instance meshes, cached on the document text: typing in
/// the title block must not re-evaluate a part that takes seconds to solve.
type NamedMeshes = Vec<(String, EvaluatedMesh)>;
static MESH_CACHE: Mutex<Option<(u64, NamedMeshes)>> = Mutex::new(None);

fn cached_meshes(doc_text: &str, doc: &Document) -> Result<Vec<(String, EvaluatedMesh)>, String> {
    use std::hash::{Hash, Hasher};
    // Hash the document minus its drawing settings, which don't change geometry.
    let mut geom = doc.clone();
    geom.drawing = None;
    let key_src = serde_json::to_string(&geom).unwrap_or_else(|_| doc_text.to_string());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key_src.hash(&mut h);
    let key = h.finish();
    if let Ok(slot) = MESH_CACHE.lock() {
        if let Some((k, m)) = slot.as_ref() {
            if *k == key {
                return Ok(m.clone());
            }
        }
    }
    let meshes = crate::export::named_meshes(doc)?;
    if let Ok(mut slot) = MESH_CACHE.lock() {
        *slot = Some((key, meshes.clone()));
    }
    Ok(meshes)
}

/// The composed sheet spec plus the main view (for DXF) and its placement.
pub struct Composed {
    /// Sheet spec (what the PDF draws).
    pub spec: SheetSpec,
    /// Main projected view, all edges (what the DXF writes).
    pub view: ProjectedView,
    /// Main view placement.
    pub main: ViewPlacement,
    /// Resolved section count.
    pub sections: usize,
}

/// Compose the drawing for `doc` (layout = `buildDrawingSheetSpec`, plus
/// detail views and GD&T, which the web sheet does not place yet).
pub fn compose(doc_text: &str, doc: &Document, req: &DrawingRequest) -> Result<Composed, String> {
    let meshes: Vec<EvaluatedMesh> = cached_meshes(doc_text, doc)?
        .into_iter()
        .map(|(_, m)| m)
        .collect();
    let mesh = combine(&meshes);
    let dir_name = req
        .view
        .clone()
        .unwrap_or_else(|| "front".into())
        .to_lowercase();
    let view = project_mesh(&mesh, view_direction(&dir_name));
    if !view.bounds.is_valid() {
        return Err("nothing to draw: the projection is empty".into());
    }

    let drawing = req
        .drawing
        .clone()
        .or_else(|| doc.drawing.clone())
        .unwrap_or_default();
    let sections: Vec<&DrawingSectionLine> = drawing
        .sections
        .iter()
        .flatten()
        .filter(|s| s.view == dir_name)
        .collect();
    let has_side = !sections.is_empty() || !req.details.is_empty();

    // A4 landscape; reserve a right strip for sections/details.
    // With a title block or BOM in the bottom-right corner, a full-width main
    // view is lifted and shortened so its caption clears them (the web
    // layout let them collide).
    let corner = drawing.title_block.is_some() || drawing.show_bom == Some(true);
    let (box_w, box_h, sheet_center) = match (has_side, corner) {
        (true, _) => (140.0, 130.0, [85.0, 120.0]),
        (false, true) => (200.0, 85.0, [120.0, 148.0]),
        (false, false) => (200.0, 130.0, [120.0, 120.0]),
    };
    let b = view.bounds;
    let vw = if b.width() > 0.0 { b.width() } else { 1.0 };
    let vh = if b.height() > 0.0 { b.height() } else { 1.0 };
    let (s, scale_note) = nice_scale((box_w / vw).min(box_h / vh));
    let main = ViewPlacement {
        sheet_center,
        view_center: [(b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0],
        scale: s,
        scale_note: scale_note.clone(),
        bounds: b,
    };

    let mut shown = view.clone();
    if req.show_hidden == Some(false) {
        shown.edges.retain(|e| e.visibility == Visibility::Visible);
    }
    let mut spec = SheetSpec {
        size: Some(SheetSize::A4),
        views: vec![PlacedProjectedView {
            view: shown,
            center: sheet_center,
            scale: s,
            label: Some(format!("{} VIEW  ({scale_note})", dir_name.to_uppercase())),
        }],
        ..Default::default()
    };

    if req.show_dimensions != Some(false) {
        let mut layer = AnnotationLayer::new();
        layer.add_horizontal_dimension(
            Point2D::new(b.min_x, b.min_y),
            Point2D::new(b.max_x, b.min_y),
            -10.0 / s,
        );
        layer.add_vertical_dimension(
            Point2D::new(b.max_x, b.min_y),
            Point2D::new(b.max_x, b.max_y),
            10.0 / s,
        );
        for rd in layer.render_all(None) {
            spec.annotations.push(dimension_to_sheet(&rd, &main));
        }
    }

    // Right strip: sections first, then details, stacked downward.
    // Stack top-down from just under the border, each item sized by its own
    // height plus room for its caption (the web's fixed 160/65 mm pitch let a
    // tall section run off the sheet).
    // The title block + BOM own the bottom-right corner; anything that won't
    // fit above them flows into the free strip under the main view.
    let floor = if corner { 92.0 } else { 20.0 };
    let mut side_top = 195.0;
    let mut low_x = 30.0;
    let mut place = |w: f64, h: f64| -> [f64; 2] {
        if side_top - h - 14.0 >= floor {
            let c = [225.0, side_top - h / 2.0];
            side_top -= h + 26.0;
            return c;
        }
        let c = [low_x + w / 2.0, 50.0];
        low_x += w + 14.0;
        c
    };
    let mut resolved = 0;
    for section in &sections {
        let Some(plane) = section_plane_from_line(&dir_name, &section.points) else {
            continue;
        };
        let sv = offset_section_mesh(
            &mesh,
            &plane,
            Some(&HatchPattern::new(3.0, std::f64::consts::FRAC_PI_4)),
        );
        if !sv.bounds.is_valid() {
            continue;
        }
        resolved += 1;
        let at = place(
            (sv.bounds.max_x - sv.bounds.min_x) * s,
            (sv.bounds.max_y - sv.bounds.min_y) * s,
        );
        spec.sections.push(PlacedSectionView {
            view: sv,
            center: at,
            scale: s,
            label: Some(format!("SECTION {0}-{0}  ({scale_note})", section.label)),
        });
        spec.annotations
            .push(dimension_to_sheet(&cut_line(section), &main));
    }

    for d in &req.details {
        let params = DetailViewParams::new(
            Point2D::new(d.center[0], d.center[1]),
            d.scale * s,
            d.width,
            d.height,
            d.label.clone(),
        );
        let dv = create_detail_view(&view, &params);
        // Callout circle on the main view, in sheet coordinates.
        let c = main.to_sheet(Point2D::new(d.center[0], d.center[1]));
        let r = d.width.max(d.height) / 2.0 * s;
        let mut callout = RenderedDimension::new();
        callout.arcs.push(vcad_kernel_drafting::RenderedArc::new(
            c,
            r,
            0.0,
            std::f64::consts::TAU,
        ));
        callout.texts.push(RenderedText::new(
            Point2D::new(c.x + r * 0.75 + 2.0, c.y + r * 0.75 + 2.0),
            d.label.clone(),
            4.0,
        ));
        spec.annotations.push(callout);
        let ratio = d.scale * s;
        let note = if ratio >= 1.0 {
            format!("{}:1", (ratio * 100.0).round() / 100.0)
        } else {
            format!("1:{}", (1.0 / ratio).round())
        };
        let at = place(
            dv.bounds.max_x - dv.bounds.min_x,
            dv.bounds.max_y - dv.bounds.min_y,
        );
        spec.details.push(PlacedDetailView {
            view: dv,
            center: at,
            label: Some(format!("DETAIL {}  ({note})", d.label)),
        });
    }

    // GD&T: placed directly at the anchor's sheet position so frame boxes
    // and text stay readable at any view scale.
    let mut gdt = AnnotationLayer::new();
    for g in &req.gdt {
        let at = main.to_sheet(Point2D::new(g.at[0], g.at[1]));
        let datum = g
            .datum
            .as_deref()
            .and_then(|d| d.chars().next())
            .unwrap_or('A');
        match g.kind.as_str() {
            "flatness" => {
                gdt.add_flatness_tolerance(g.tolerance, at);
            }
            "position" => {
                gdt.add_position_tolerance(g.tolerance, at, datum);
            }
            "perpendicularity" => {
                gdt.add_perpendicularity_tolerance(g.tolerance, at, datum);
            }
            "datum" => {
                gdt.add_datum_symbol(datum, at);
            }
            other => return Err(format!("unknown GD&T kind `{other}`")),
        }
    }
    spec.annotations.extend(gdt.render_all(None));

    if let Some(tb) = &drawing.title_block {
        spec.title_block = Some(kernel_title_block(tb, &scale_note));
    }
    if drawing.show_bom == Some(true) {
        let materials = &doc.part_materials;
        let rows: Vec<BomRow> = doc
            .roots
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let name = doc
                    .nodes
                    .get(&r.root)
                    .and_then(|n| n.name.clone())
                    .unwrap_or_else(|| format!("part_{}", i + 1));
                let material = materials.get(&name).cloned().unwrap_or_default();
                BomRow {
                    item: (i + 1) as u32,
                    name,
                    qty: 1,
                    material,
                }
            })
            .collect();
        if !rows.is_empty() {
            spec.bom = Some(rows);
        }
    }

    Ok(Composed {
        spec,
        view,
        main,
        sections: resolved,
    })
}

fn parse(
    doc_json: *const c_char,
    req_json: *const c_char,
) -> Option<(String, Document, DrawingRequest)> {
    let text = borrow_str("document", doc_json)?.to_string();
    let mut doc = err::ctx("parse document", || Document::from_json(&text))?;
    let req: DrawingRequest = if req_json.is_null() {
        DrawingRequest::default()
    } else {
        let r = borrow_str("request", req_json)?;
        if r.trim().is_empty() {
            DrawingRequest::default()
        } else {
            err::ctx("parse request", || serde_json::from_str(r))?
        }
    };
    if let Some(dir) = req.base_dir.as_deref().filter(|d| !d.is_empty()) {
        vcad_eval::resolve_mesh_paths(&mut doc, std::path::Path::new(dir));
    }
    Some((text, doc, req))
}

/// Compose the drawing sheet for a document and return it as JSON
/// ([`DrawingResult`]). `doc_json` is the `.vcad` document, `request_json` a
/// [`DrawingRequest`] (null/empty = defaults). Returns null with
/// `vcad_last_error` set on failure. Free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_drawing_sheet_json(
    doc_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    err::clear_error();
    guard_ptr("drawing", || {
        let Some((text, doc, req)) = parse(doc_json, request_json) else {
            return std::ptr::null_mut();
        };
        let Some(c) = err::ctx("drawing", || compose(&text, &doc, &req)) else {
            return std::ptr::null_mut();
        };
        let sheet = compose_sheet(&c.spec);
        let (width, height) = sheet.size.dimensions_mm();
        let result = DrawingResult {
            width,
            height,
            sheet,
            visible_edges: c.view.num_visible(),
            hidden_edges: c.view.num_hidden(),
            dxf_entities: c.view.edges.len(),
            sections: c.sections,
            main: c.main,
        };
        json_out(&result)
    })
}

/// Export the drawing to `out_path` as DXF ([`VCAD_DRAWING_DXF`], the active
/// view's projection — same writer as the web export) or PDF
/// ([`VCAD_DRAWING_PDF`], the composed sheet). Returns the DXF entity count /
/// PDF byte count (≥ 0), or -1 with `vcad_last_error` set.
#[no_mangle]
pub extern "C" fn vcad_drawing_export(
    doc_json: *const c_char,
    request_json: *const c_char,
    out_path: *const c_char,
    format: u32,
) -> i64 {
    err::clear_error();
    let run = || -> Option<i64> {
        let (text, doc, req) = parse(doc_json, request_json)?;
        let path = borrow_str("out_path", out_path)?.to_string();
        let c = err::ctx("drawing", || compose(&text, &doc, &req))?;
        let (bytes, n) = match format {
            VCAD_DRAWING_DXF => (projected_view_to_dxf(&c.view), c.view.edges.len() as i64),
            VCAD_DRAWING_PDF => {
                let pdf = sheet_to_pdf(&compose_sheet(&c.spec));
                let n = pdf.len() as i64;
                (pdf, n)
            }
            other => {
                err::set_error(format!("unknown drawing export format {other}"));
                return None;
            }
        };
        err::ctx("write", || {
            crate::export::write_atomic(std::path::Path::new(&path), &bytes)
        })?;
        Some(n)
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Some(n)) => n,
        Ok(None) => -1,
        Err(_) => {
            err::set_error("drawing export: kernel panicked");
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn cube_doc() -> String {
        // A 40×20×10 block with a through-hole: visible + hidden edges.
        r#"{"version":"0.1","nodes":{
            "1":{"id":1,"name":"Block","op":{"type":"Cube","size":{"x":40,"y":20,"z":10}}},
            "2":{"id":2,"name":null,"op":{"type":"Cylinder","radius":4,"height":30,"segments":32}},
            "3":{"id":3,"name":null,"op":{"type":"Translate","child":2,"offset":{"x":20,"y":10,"z":-10}}},
            "4":{"id":4,"name":"Block","op":{"type":"Difference","left":1,"right":3}}},
            "materials":{},"part_materials":{},"roots":[{"root":4,"material":"default"}],
            "drawing":{"titleBlock":{"partName":"Bracket","author":"cam","date":"2026-11-06",
              "scale":"","material":"6061","revision":"B"},
              "sections":[{"id":"s1","view":"top","label":"A","points":[[-20,-5],[-20,25]]}],
              "showBom":true}}"#
            .to_string()
    }

    fn call(req: &str) -> serde_json::Value {
        let d = CString::new(cube_doc()).unwrap();
        let r = CString::new(req).unwrap();
        let p = vcad_drawing_sheet_json(d.as_ptr(), r.as_ptr());
        assert!(!p.is_null(), "{:?}", err::last_error_string());
        serde_json::from_str(&crate::json::take(p)).unwrap()
    }

    #[test]
    fn front_sheet_has_views_dims_title() {
        let v = call(r#"{"view":"front"}"#);
        assert_eq!(v["width"], 297.0);
        assert!(v["visible_edges"].as_u64().unwrap() > 0);
        assert!(
            v["hidden_edges"].as_u64().unwrap() > 0,
            "through-hole is hidden from the front"
        );
        let texts: Vec<String> = v["sheet"]["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["text"].as_str().unwrap().to_string())
            .collect();
        assert!(texts.iter().any(|t| t.contains("FRONT VIEW")), "{texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("40")),
            "width dimension: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("Bracket") || t.contains("BRACKET")),
            "{texts:?}"
        );
    }

    #[test]
    fn top_view_resolves_section_detail_and_gdt() {
        let v = call(
            r#"{"view":"top","details":[{"label":"B","center":[20,10],"width":12,"height":12}],
                "gdt":[{"kind":"flatness","tolerance":0.05,"at":[0,25]},
                       {"kind":"datum","datum":"A","at":[-5,0]}]}"#,
        );
        assert_eq!(v["sections"], 1);
        let texts: Vec<String> = v["sheet"]["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["text"].as_str().unwrap().to_string())
            .collect();
        assert!(texts.iter().any(|t| t.contains("SECTION A-A")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("DETAIL B")), "{texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("0.05")),
            "flatness value: {texts:?}"
        );
        let hatch = v["sheet"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l["class"] == "hatch")
            .count();
        assert!(hatch > 0, "section is hatched");
    }

    #[test]
    fn hidden_toggle_drops_dashed_lines() {
        let count = |v: &serde_json::Value| {
            v["sheet"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|l| l["class"] == "hidden")
                .count()
        };
        assert!(count(&call(r#"{"view":"front"}"#)) > 0);
        assert_eq!(count(&call(r#"{"view":"front","show_hidden":false}"#)), 0);
    }

    /// Dogfood repro: a 60×40×10 plate, all edges filleted r=3, Ø8
    /// through-hole. The front view used to draw every mesh edge
    /// (4,506 visible + 6,374 hidden); it must draw feature edges only.
    #[test]
    fn filleted_plate_front_view_draws_feature_edges_not_mesh() {
        let doc = vcad_loon::eval_vcad(
            "[difference [translate 30 20 -1 [cylinder 4 12]] [fillet 3 [cube 60 40 10]]]",
            None,
        )
        .expect("loon");
        let meshes: Vec<EvaluatedMesh> = crate::export::named_meshes(&doc)
            .expect("evaluate")
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        let view = project_mesh(&combine(&meshes), ViewDirection::Front);
        let (vis, hid) = (view.num_visible(), view.num_hidden());
        eprintln!("plate front view: {vis} visible, {hid} hidden");
        assert!(vis + hid < 200, "{vis} visible + {hid} hidden");
        assert!(hid >= 2, "the hole shows as hidden lines");

        // No triangle diagonals: away from the rounded corners (|x|,|z|
        // within 3 of the outline), every drawn edge is axis-aligned.
        for e in &view.edges {
            let (dx, dz) = (e.end.x - e.start.x, e.end.y - e.start.y);
            let (mx, mz) = ((e.start.x + e.end.x) / 2.0, (e.start.y + e.end.y) / 2.0);
            let interior = mx > 3.5 && mx < 56.5 && mz > 0.5 && mz < 9.5;
            let axis_aligned = dx.abs() < 1e-3 || dz.abs() < 1e-3;
            assert!(
                !interior || axis_aligned,
                "diagonal across a flat face: {e:?}"
            );
        }
    }

    #[test]
    fn dxf_matches_web_writer_and_pdf_writes() {
        let dir = std::env::temp_dir().join(format!("vcad-drawing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = CString::new(cube_doc()).unwrap();
        let r = CString::new(r#"{"view":"front"}"#).unwrap();
        let dxf = dir.join("front.dxf");
        let p = CString::new(dxf.to_str().unwrap()).unwrap();
        let n = vcad_drawing_export(d.as_ptr(), r.as_ptr(), p.as_ptr(), VCAD_DRAWING_DXF);
        assert!(n > 0, "{:?}", err::last_error_string());
        let text = std::fs::read_to_string(&dxf).unwrap();
        assert_eq!(text.matches("\nLINE\n").count() as i64, n);
        // Same entities as the web path: projectMesh(mesh) → exportProjectedViewToDxf.
        // (Edge order follows a HashMap, so compare the sorted LINE records.)
        let doc = Document::from_json(&cube_doc()).unwrap();
        let meshes: Vec<EvaluatedMesh> = crate::export::named_meshes(&doc)
            .unwrap()
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        let web = projected_view_to_dxf(&project_mesh(&combine(&meshes), ViewDirection::Front));
        let lines = |t: &str| {
            let mut v: Vec<String> = t
                .split("\n0\nLINE\n")
                .skip(1)
                .map(|r| r.split("\n0\n").next().unwrap().to_string())
                .collect();
            v.sort();
            v
        };
        let web = String::from_utf8(web).unwrap();
        assert_eq!(lines(&text), lines(&web));
        assert_eq!(text.len(), web.len());

        let pdf = dir.join("sheet.pdf");
        let p = CString::new(pdf.to_str().unwrap()).unwrap();
        assert!(vcad_drawing_export(d.as_ptr(), r.as_ptr(), p.as_ptr(), VCAD_DRAWING_PDF) > 0);
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    }

    #[test]
    fn section_plane_matches_web_port() {
        let p = section_plane_from_line("front", &[[5.0, -1.0], [5.0, 9.0]]).unwrap();
        assert!(p.steps.is_empty());
        // Same basis as the web: front right axis = worldUp × forward = -X.
        assert_eq!(p.base.normal, [-1.0, 0.0, 0.0]);
        assert!(section_plane_from_line("isometric", &[[0.0, 0.0], [0.0, 1.0]]).is_none());
        assert_eq!(nice_scale(3.7), (2.0, "2:1".into()));
        assert_eq!(nice_scale(0.3), (0.2, "1:5".into()));
    }

    /// Gate: every sample the app ships draws in every orthographic view, and
    /// its DXF holds exactly the LINE records the web export writes for the
    /// same mesh (`projectMesh` → `exportProjectedViewToDxf`).
    #[test]
    fn shipped_samples_draw_and_match_web_dxf() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apple/VcadApp/Samples");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("vcad") {
                continue;
            }
            // Through the app's own loader: samples may be JSON or compact text.
            let doc = crate::load_document_file(&path).unwrap();
            let text = serde_json::to_string(&doc).unwrap();
            let meshes: Vec<EvaluatedMesh> = crate::export::named_meshes(&doc)
                .unwrap()
                .into_iter()
                .map(|(_, m)| m)
                .collect();
            let mesh = combine(&meshes);
            for view in ["front", "isometric"] {
                let req = DrawingRequest {
                    view: Some(view.into()),
                    ..Default::default()
                };
                let c =
                    compose(&text, &doc, &req).unwrap_or_else(|e| panic!("{path:?} {view}: {e}"));
                let sheet = compose_sheet(&c.spec);
                assert!(sheet.lines.len() > 4, "{path:?} {view}: empty sheet");
                let ours = String::from_utf8(projected_view_to_dxf(&c.view)).unwrap();
                let web = String::from_utf8(projected_view_to_dxf(&project_mesh(
                    &mesh,
                    view_direction(view),
                )))
                .unwrap();
                let lines = |t: &str| {
                    let mut v: Vec<&str> = t.split("\n0\nLINE\n").skip(1).collect();
                    v.iter_mut()
                        .for_each(|r| *r = r.split("\n0\n").next().unwrap());
                    v.sort_unstable();
                    v.into_iter().map(str::to_owned).collect::<Vec<_>>()
                };
                assert_eq!(
                    lines(&ours),
                    lines(&web),
                    "{path:?} {view}: DXF differs from the web export"
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} sample views checked");
    }
}
