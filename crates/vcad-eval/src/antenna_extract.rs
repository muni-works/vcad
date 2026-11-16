//! Board → antenna extraction: the vcad side of the `vcad-kernel-antenna`
//! seam.
//!
//! `vcad-kernel-antenna` solves hand-built thin-wire grids
//! ([`AntennaSpec`]); its `ecad` module holds the strip ↔ round-wire
//! equivalence (`radius = width / 4`) and deliberately leaves board
//! extraction "to the vcad side of the seam". This module is that side: it
//! reads a document's [`CsgOp::PcbBoard`] node, takes the antenna net's
//! copper (trace centerlines + widths), the feed pad, and the ground pour,
//! and emits an `AntennaSpec` that `spec.resolve(&BTreeMap::new())` runs
//! as-is.
//!
//! No document evaluation happens: the board is IR data, read directly.
//!
//! # Ground model (the one approximation, always recorded)
//!
//! The solver's only ground is an **infinite PEC plane at z = 0 with every
//! wire at z ≥ 0** (image theory). A printed monopole's pour is *coplanar*
//! with the trace. So the extractor maps the board into the solver frame
//! about the pour edge nearest the feed:
//!
//! - solver `z` = distance outward from that edge line (into the antenna
//!   keepout), solver `x` = position along the edge, solver `y` = 0;
//! - the feed sits at `z = 0`, so its trace end gets the solver's
//!   grounded-end basis and the delta-gap drives exactly the pad.
//!
//! That is the textbook "printed monopole ≈ quarter-wave monopole over a
//! ground plane" model: exact for an antenna at the edge of a pour that is
//! large in wavelengths, optimistic for a small board (a 50 mm pour is
//! ~0.4 λ at 2.4 GHz). The result's [`GroundApproximation`] records the
//! edge, the pour's real extent in mm and in wavelengths at the band top,
//! and the feed gap. A finite (wire-mesh) ground is a follow-up: the
//! solver's single-basis delta-gap can't yet drive a junction node.
//!
//! # Fail-closed
//!
//! Every shape the model can't hold is an error naming the offending
//! trace/pad/zone, never a silent simplification: arcs, vias, zones or
//! extra pads on the antenna net; copper on more than one layer; copper
//! split into islands; a feed that isn't a free trace end on the feed pad;
//! antenna copper on or behind the pour edge's line; a pour that isn't
//! entirely behind that line; a feed gap beyond `max_feed_gap_mm`; a trace
//! too short to segment inside the thin-wire gates. After building, the
//! spec is resolved and the feed basis is checked to sit at the feed point
//! as a grounded end (`nearest_basis` always returns *something*, so this
//! is asserted, not assumed).
//!
//! # Dielectric (out of scope — gap G7)
//!
//! The solver is free-space. The board's ε_r and thicknesses are copied
//! into [`SubstrateMeta`] (null when the stackup doesn't say — never a
//! default), and the notes state the documented bias: free-space models of
//! FR-4 trace antennas read **high** in frequency; the fabricated board
//! resonates roughly 20–30 % lower (`docs/antenna-measurement-pack.md`).
//! No ε_eff-corrected number is emitted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vcad_ecad_pcb::geometry::{pad_radius, pad_world_position};
use vcad_ir::ecad::{Pcb, PcbLayer};
use vcad_ir::{CsgOp, Document, NodeId};
use vcad_kernel_antenna::constants::C0;
use vcad_kernel_antenna::ecad::strip_equivalent_radius_mm;
use vcad_kernel_antenna::geometry::NODE_TOL_MM;
use vcad_kernel_antenna::spec::{AntennaSpec, ElementSpec};

type P2 = [f64; 2];

fn sub2(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot2(a: P2, b: P2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn norm2(a: P2) -> f64 {
    dot2(a, a).sqrt()
}
fn dist2(a: P2, b: P2) -> f64 {
    norm2(sub2(a, b))
}
fn lerp2(a: P2, b: P2, t: f64) -> P2 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Point-to-segment distance and the clamped parameter `t ∈ [0, 1]`.
fn point_segment(p: P2, a: P2, b: P2) -> (f64, f64) {
    let d = sub2(b, a);
    let l2 = dot2(d, d);
    if l2 == 0.0 {
        return (dist2(p, a), 0.0);
    }
    let t = (dot2(sub2(p, a), d) / l2).clamp(0.0, 1.0);
    (dist2(p, lerp2(a, b, t)), t)
}

/// The feed: a pad of a placed footprint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedPad {
    /// Footprint reference designator (e.g. `"J1"`).
    pub footprint: String,
    /// Pad number (e.g. `"1"`).
    pub pad: String,
}

/// How the ground pour is represented in the solver.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum GroundModel {
    /// Infinite PEC plane at the pour edge nearest the feed (image
    /// theory). The only model the solver supports today.
    #[default]
    InfiniteImage,
}

/// What to extract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AntennaExtraction {
    /// The `PcbBoard` node. Omitted = the document's only board (error if
    /// it has none or several).
    #[serde(default)]
    pub board_node: Option<NodeId>,
    /// Antenna net. Omitted = the feed pad's net; when both are given they
    /// must agree.
    #[serde(default)]
    pub net: Option<String>,
    /// Feed pad (the delta-gap port, between this pad and the pour).
    pub feed: FeedPad,
    /// Net of the ground pour. Default `"GND"`.
    #[serde(default = "default_ground_net")]
    pub ground_net: String,
    /// Highest frequency that will be solved, Hz — sets segmentation and
    /// the pour's size in wavelengths.
    pub f_max_hz: f64,
    /// Target segments per wavelength at `f_max_hz`. Default 20 (must be ≥
    /// 8, the solver's λ/8 sampling gate).
    #[serde(default = "default_segments_per_wavelength")]
    pub segments_per_wavelength: f64,
    /// Largest feed-point-to-pour-edge gap accepted, mm. Default 2.
    #[serde(default = "default_max_feed_gap")]
    pub max_feed_gap_mm: f64,
    /// Ground representation.
    #[serde(default)]
    pub ground: GroundModel,
}

fn default_ground_net() -> String {
    "GND".to_string()
}
fn default_segments_per_wavelength() -> f64 {
    20.0
}
fn default_max_feed_gap() -> f64 {
    2.0
}

/// How the feed pad ↔ pour-edge gap was modelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedTreatment {
    /// The feed trace end lies on the pour edge (within 1 µm).
    OnEdge,
    /// The gap was shorter than the feed trace's width — too short for a
    /// thin-wire segment — so the feed end was moved onto the edge. The
    /// moved distance is `gap_mm`.
    Snapped,
    /// The gap was bridged by a lead wire of the feed trace's width from
    /// the edge to the trace end.
    LeadWire,
}

/// The feed, in board and solver frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedInfo {
    /// Footprint reference.
    pub footprint: String,
    /// Pad number.
    pub pad: String,
    /// Feed trace end on the board, mm.
    pub board_point_mm: P2,
    /// Feed point in the solver frame (on the ground plane), mm.
    pub solver_point_mm: [f64; 3],
    /// Distance from the feed trace end to the pour edge line, mm.
    pub gap_mm: f64,
    /// How that gap was modelled.
    pub treatment: FeedTreatment,
}

/// Board → solver frame: `solver = ((p−o)·x_axis, 0, (p−o)·z_axis)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SolverFrame {
    /// Board point mapped to the solver origin (feed projected onto the
    /// pour edge line), mm.
    pub origin_board_mm: P2,
    /// Board direction of solver +x (along the pour edge).
    pub x_axis_board: P2,
    /// Board direction of solver +z (outward from the pour).
    pub z_axis_board: P2,
}

/// The ground approximation, spelled out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroundApproximation {
    /// `"infinite_image"`.
    pub model: String,
    /// Net of the pour.
    pub ground_net: String,
    /// The pour edge the plane sits on, board mm.
    pub edge_board_mm: [P2; 2],
    /// Layer of the zone owning that edge.
    pub edge_layer: PcbLayer,
    /// Real pour extent along the edge, mm.
    pub pour_along_edge_mm: f64,
    /// Real pour depth behind the edge, mm.
    pub pour_depth_mm: f64,
    /// `pour_along_edge_mm` in wavelengths at `f_max_hz`.
    pub pour_along_edge_wavelengths: f64,
    /// `pour_depth_mm` in wavelengths at `f_max_hz`.
    pub pour_depth_wavelengths: f64,
    /// The approximation in words.
    pub approximation: String,
}

/// Substrate data copied from the board for a later dielectric model
/// (gap G7). Nulls mean the board doesn't say — nothing is defaulted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubstrateMeta {
    /// Board thickness (outline), mm.
    pub board_thickness_mm: f64,
    /// Relative permittivity, when the stackup declares exactly one value.
    pub dielectric_er: Option<f64>,
    /// Every distinct ε_r the stackup declares (several = mixed stackup).
    pub dielectric_er_values: Vec<f64>,
    /// Sum of declared dielectric thicknesses, mm (null when none).
    pub dielectric_thickness_mm: Option<f64>,
    /// Declared dielectric material names.
    pub materials: Vec<String>,
    /// Copper layer the antenna is on.
    pub antenna_layer: PcbLayer,
    /// Copper thickness of that layer, mm (null when undeclared; the
    /// strip model treats copper as zero-thickness either way).
    pub antenna_copper_thickness_mm: Option<f64>,
}

/// An extracted antenna, ready for `spec.resolve`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedAntenna {
    /// The solver spec (all literals, ground plane on).
    pub spec: AntennaSpec,
    /// Parameter bindings for `spec` (empty: geometry is literal).
    pub parameters: BTreeMap<String, f64>,
    /// The board node read.
    pub board_node: NodeId,
    /// The antenna net.
    pub net: String,
    /// Indices into `board.traces` of the antenna copper.
    pub trace_indices: Vec<usize>,
    /// Feed.
    pub feed: FeedInfo,
    /// Board → solver frame.
    pub frame: SolverFrame,
    /// Ground approximation.
    pub ground: GroundApproximation,
    /// Substrate metadata (not modelled).
    pub substrate: SubstrateMeta,
    /// Wire segments in the resolved mesh.
    pub segments: usize,
    /// Caveats a reader of any claim built from this spec must see.
    pub notes: Vec<String>,
}

/// Why extraction refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AntennaExtractError {
    /// Bad selection input.
    #[error("antenna extraction: {0}")]
    Selection(String),
    /// Board selection failed.
    #[error("antenna extraction: {0}")]
    Board(String),
    /// The antenna net holds geometry the model can't represent.
    #[error("antenna extraction: unsupported geometry: {0}")]
    Unsupported(String),
    /// Feed problem.
    #[error("antenna extraction: feed: {0}")]
    Feed(String),
    /// Ground pour problem.
    #[error("antenna extraction: ground: {0}")]
    Ground(String),
    /// The emitted spec failed to build in the solver.
    #[error("antenna extraction: solver rejected the extracted geometry: {0}")]
    Solver(String),
}

use AntennaExtractError as E;

fn find_board(doc: &Document, node: Option<NodeId>) -> Result<(NodeId, &Pcb), E> {
    if let Some(id) = node {
        let n = doc
            .nodes
            .get(&id)
            .ok_or_else(|| E::Board(format!("node {id} does not exist")))?;
        return match &n.op {
            CsgOp::PcbBoard { board } => Ok((id, board)),
            _ => Err(E::Board(format!("node {id} is not a PcbBoard"))),
        };
    }
    let mut boards: Vec<(NodeId, &Pcb)> = doc
        .nodes
        .iter()
        .filter_map(|(id, n)| match &n.op {
            CsgOp::PcbBoard { board } => Some((*id, board.as_ref())),
            _ => None,
        })
        .collect();
    boards.sort_by_key(|(id, _)| *id);
    match boards.len() {
        0 => Err(E::Board("the document has no PcbBoard node".into())),
        1 => Ok(boards[0]),
        n => Err(E::Board(format!(
            "the document has {n} PcbBoard nodes ({:?}); pass board_node",
            boards.iter().map(|b| b.0).collect::<Vec<_>>()
        ))),
    }
}

/// One straight piece of antenna copper after T-junction splitting.
struct Piece {
    a: P2,
    b: P2,
    width: f64,
    trace: usize,
}

/// Extract the antenna on `sel.net` of a document's board as a thin-wire
/// [`AntennaSpec`] over an infinite ground at the pour edge. See the
/// module docs for the model and every refusal.
pub fn extract_antenna_spec(
    doc: &Document,
    sel: &AntennaExtraction,
) -> Result<ExtractedAntenna, AntennaExtractError> {
    if !(sel.f_max_hz.is_finite() && sel.f_max_hz > 0.0) {
        return Err(E::Selection(format!(
            "f_max_hz must be positive, got {}",
            sel.f_max_hz
        )));
    }
    if !(sel.segments_per_wavelength >= 8.0 && sel.segments_per_wavelength.is_finite()) {
        return Err(E::Selection(format!(
            "segments_per_wavelength must be >= 8 (the solver's λ/8 gate), got {}",
            sel.segments_per_wavelength
        )));
    }
    if !(sel.max_feed_gap_mm >= 0.0 && sel.max_feed_gap_mm.is_finite()) {
        return Err(E::Selection("max_feed_gap_mm must be >= 0".into()));
    }
    let GroundModel::InfiniteImage = sel.ground;
    let (board_node, pcb) = find_board(doc, sel.board_node)?;
    let lambda_mm = C0 / sel.f_max_hz * 1e3;
    let tol = NODE_TOL_MM;

    // ── feed pad + net ────────────────────────────────────────────────
    let fp = pcb
        .footprints
        .iter()
        .find(|f| f.reference == sel.feed.footprint)
        .ok_or_else(|| {
            E::Feed(format!(
                "no footprint '{}' on the board",
                sel.feed.footprint
            ))
        })?;
    let pad = fp
        .pads
        .iter()
        .find(|p| p.number == sel.feed.pad)
        .ok_or_else(|| {
            E::Feed(format!(
                "footprint '{}' has no pad '{}'",
                sel.feed.footprint, sel.feed.pad
            ))
        })?;
    let pad_net = pad.net.clone().ok_or_else(|| {
        E::Feed(format!(
            "pad {}.{} has no net",
            sel.feed.footprint, sel.feed.pad
        ))
    })?;
    let net = match &sel.net {
        Some(n) if *n != pad_net => {
            return Err(E::Feed(format!(
                "feed pad {}.{} is on net '{pad_net}', not the antenna net '{n}'",
                sel.feed.footprint, sel.feed.pad
            )))
        }
        Some(n) => n.clone(),
        None => pad_net,
    };
    if net == sel.ground_net {
        return Err(E::Feed(format!(
            "the antenna net '{net}' is the ground net"
        )));
    }
    let pad_c = pad_world_position(fp, pad);
    let pad_c: P2 = [pad_c.x, pad_c.y];

    // ── antenna-net copper: refuse what the model can't hold ──────────
    if let Some(i) = pcb.trace_arcs.iter().position(|a| a.net == net) {
        return Err(E::Unsupported(format!(
            "trace arc {i} on net '{net}' (arcs are not extracted yet; route the radiator with straight segments)"
        )));
    }
    if let Some(i) = pcb.vias.iter().position(|v| v.net == net) {
        return Err(E::Unsupported(format!(
            "via {i} on net '{net}' (multi-layer antennas are not modelled)"
        )));
    }
    if let Some(i) = pcb.zones.iter().position(|z| z.net == net) {
        return Err(E::Unsupported(format!(
            "zone {i} on net '{net}' (copper-pour radiators are not modelled; only traces)"
        )));
    }
    for f in &pcb.footprints {
        for p in &f.pads {
            let is_feed = f.reference == sel.feed.footprint && p.number == sel.feed.pad;
            if !is_feed && p.net.as_deref() == Some(net.as_str()) {
                return Err(E::Unsupported(format!(
                    "pad {}.{} is on the antenna net '{net}' besides the feed (lumped/matching components are not modelled)",
                    f.reference, p.number
                )));
            }
        }
    }
    let trace_indices: Vec<usize> = pcb
        .traces
        .iter()
        .enumerate()
        .filter(|(_, t)| t.net == net)
        .map(|(i, _)| i)
        .collect();
    if trace_indices.is_empty() {
        return Err(E::Unsupported(format!("net '{net}' has no traces")));
    }
    let layer = pcb.traces[trace_indices[0]].layer;
    for &i in &trace_indices {
        let t = &pcb.traces[i];
        if t.layer != layer {
            return Err(E::Unsupported(format!(
                "trace {i} is on {:?} but trace {} is on {layer:?} (single-layer antennas only)",
                t.layer, trace_indices[0]
            )));
        }
        if !(t.width.is_finite() && t.width > 0.0) {
            return Err(E::Unsupported(format!("trace {i} has width {}", t.width)));
        }
        if dist2([t.start.x, t.start.y], [t.end.x, t.end.y]) <= tol {
            return Err(E::Unsupported(format!("trace {i} has zero length")));
        }
    }

    // ── split at T-junctions (an end landing mid-segment) ─────────────
    let ends: Vec<P2> = trace_indices
        .iter()
        .flat_map(|&i| {
            let t = &pcb.traces[i];
            [[t.start.x, t.start.y], [t.end.x, t.end.y]]
        })
        .collect();
    let mut pieces = Vec::new();
    for &i in &trace_indices {
        let t = &pcb.traces[i];
        let (a, b) = ([t.start.x, t.start.y], [t.end.x, t.end.y]);
        let len = dist2(a, b);
        let mut cuts: Vec<f64> = ends
            .iter()
            .filter_map(|&p| {
                let (d, s) = point_segment(p, a, b);
                (d <= tol && s * len > tol && (1.0 - s) * len > tol).then_some(s)
            })
            .collect();
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|x, y| (*x - *y).abs() * len <= tol);
        let mut prev = 0.0;
        for s in cuts.into_iter().chain(std::iter::once(1.0)) {
            pieces.push(Piece {
                a: lerp2(a, b, prev),
                b: lerp2(a, b, s),
                width: t.width,
                trace: i,
            });
            prev = s;
        }
    }

    // ── graph: merged nodes, connectivity, feed end ───────────────────
    let mut nodes: Vec<P2> = Vec::new();
    let mut node_at = |p: P2| -> usize {
        if let Some(i) = nodes.iter().position(|n| dist2(*n, p) <= tol) {
            return i;
        }
        nodes.push(p);
        nodes.len() - 1
    };
    let edges: Vec<(usize, usize)> = pieces
        .iter()
        .map(|p| (node_at(p.a), node_at(p.b)))
        .collect();
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut degree = vec![0usize; nodes.len()];
    for &(u, v) in &edges {
        degree[u] += 1;
        degree[v] += 1;
        let (ru, rv) = (find(&mut parent, u), find(&mut parent, v));
        parent[ru] = rv;
    }
    let roots: std::collections::BTreeSet<usize> =
        (0..nodes.len()).map(|i| find(&mut parent, i)).collect();
    if roots.len() > 1 {
        return Err(E::Unsupported(format!(
            "net '{net}' copper is {} disconnected islands (floating copper on the antenna net is not modelled)",
            roots.len()
        )));
    }
    let reach = pad_radius(&pad.shape).max(tol);
    let feed_node = (0..nodes.len())
        .filter(|&i| dist2(nodes[i], pad_c) <= reach)
        .min_by(|&i, &j| dist2(nodes[i], pad_c).total_cmp(&dist2(nodes[j], pad_c)))
        .ok_or_else(|| {
            E::Feed(format!(
                "no antenna trace ends on pad {}.{} (pad centre {:?}, reach {reach:.3} mm)",
                sel.feed.footprint, sel.feed.pad, pad_c
            ))
        })?;
    if degree[feed_node] != 1 {
        return Err(E::Feed(format!(
            "the feed point {:?} joins {} trace pieces; the feed must be a free trace end",
            nodes[feed_node], degree[feed_node]
        )));
    }
    let feed_p = nodes[feed_node];

    // ── ground pour edge ──────────────────────────────────────────────
    let gzones: Vec<(usize, &vcad_ir::ecad::Zone)> = pcb
        .zones
        .iter()
        .enumerate()
        .filter(|(_, z)| z.net == sel.ground_net && z.outline.len() >= 3)
        .collect();
    if gzones.is_empty() {
        return Err(E::Ground(format!(
            "no zone on ground net '{}' (the image model needs a pour to put the plane on)",
            sel.ground_net
        )));
    }
    let mut best: Option<(f64, usize, P2, P2)> = None;
    for &(zi, z) in &gzones {
        let n = z.outline.len();
        for k in 0..n {
            let a = [z.outline[k].x, z.outline[k].y];
            let b = [z.outline[(k + 1) % n].x, z.outline[(k + 1) % n].y];
            if dist2(a, b) <= tol {
                continue;
            }
            let (d, _) = point_segment(feed_p, a, b);
            if best.is_none_or(|bb| d < bb.0) {
                best = Some((d, zi, a, b));
            }
        }
    }
    let (_, edge_zone, ea, eb) = best.expect("ground zones have edges");
    let l = dist2(ea, eb);
    let x_axis = [(eb[0] - ea[0]) / l, (eb[1] - ea[1]) / l];
    // Outward normal: away from the owning zone's interior.
    let mut z_axis = [-x_axis[1], x_axis[0]];
    let owner = &pcb.zones[edge_zone];
    let far = owner
        .outline
        .iter()
        .map(|v| dot2(sub2([v.x, v.y], ea), z_axis))
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(0.0);
    if far > 0.0 {
        z_axis = [-z_axis[0], -z_axis[1]];
    }
    // The whole pour (every ground zone, every layer) must lie behind the
    // edge line, or the half-space plane misrepresents it.
    let mut depth: f64 = 0.0;
    let (mut x_lo, mut x_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &(zi, z) in &gzones {
        for v in &z.outline {
            let d = sub2([v.x, v.y], ea);
            let zz = dot2(d, z_axis);
            if zz > tol {
                return Err(E::Ground(format!(
                    "zone {zi} on '{}' extends {zz:.3} mm past the feed's pour edge line (a pour wrapping the antenna is not an edge-of-plane monopole)",
                    sel.ground_net
                )));
            }
            depth = depth.max(-zz);
            let xx = dot2(d, x_axis);
            x_lo = x_lo.min(xx);
            x_hi = x_hi.max(xx);
        }
    }
    let origin = {
        let s = dot2(sub2(feed_p, ea), x_axis);
        [ea[0] + x_axis[0] * s, ea[1] + x_axis[1] * s]
    };
    let to_solver = |p: P2| -> (f64, f64) {
        let d = sub2(p, origin);
        (dot2(d, x_axis), dot2(d, z_axis))
    };

    // ── feed gap ──────────────────────────────────────────────────────
    let (_, gap) = to_solver(feed_p);
    if gap < -tol {
        return Err(E::Feed(format!(
            "the feed point lies {:.3} mm inside the pour outline; pull the pour edge back to the feed (pour cutouts around the feed are not modelled)",
            -gap
        )));
    }
    if gap > sel.max_feed_gap_mm {
        return Err(E::Feed(format!(
            "feed gap {gap:.3} mm to the pour edge exceeds max_feed_gap_mm = {}",
            sel.max_feed_gap_mm
        )));
    }
    let feed_width = pieces
        .iter()
        .zip(&edges)
        .find(|(_, &(u, v))| u == feed_node || v == feed_node)
        .map(|(p, _)| p.width)
        .expect("feed node has a piece");
    let treatment = if gap <= tol {
        FeedTreatment::OnEdge
    } else if gap < feed_width {
        FeedTreatment::Snapped
    } else {
        FeedTreatment::LeadWire
    };

    // ── antenna copper must sit in front of the edge ──────────────────
    for (i, n) in nodes.iter().enumerate() {
        if i == feed_node {
            continue;
        }
        let (_, zz) = to_solver(*n);
        if zz <= tol {
            let tr = pieces
                .iter()
                .find(|p| dist2(p.a, *n) <= tol || dist2(p.b, *n) <= tol)
                .map(|p| p.trace)
                .unwrap_or(usize::MAX);
            return Err(E::Unsupported(format!(
                "trace {tr} reaches {:.3} mm {} the pour edge line at {:?} (copper over or alongside the pour is a feedline, not a radiator)",
                zz.abs(),
                if zz < 0.0 { "behind" } else { "onto" },
                n
            )));
        }
    }

    // ── emit wires ────────────────────────────────────────────────────
    let max_seg = lambda_mm / sel.segments_per_wavelength;
    let solver_pt = |p: P2, snap: bool| -> [f64; 3] {
        let (x, z) = to_solver(p);
        [x, 0.0, if snap { 0.0 } else { z }]
    };
    let mut elements = Vec::new();
    let mut add = |a: [f64; 3], b: [f64; 3], width: f64, what: String| -> Result<(), E> {
        let len = ((b[0] - a[0]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
        // 4·radius = width is the thin-wire floor per segment.
        if len < width {
            return Err(E::Unsupported(format!(
                "{what} is {len:.3} mm long, shorter than its width {width:.3} mm — no segment fits the thin-wire gate (length ≥ 4·radius = width)"
            )));
        }
        let n_max = (len / width).floor() as usize;
        let n = ((len / max_seg).ceil() as usize).clamp(1, n_max.max(1));
        elements.push(ElementSpec::Wire {
            start_mm: [a[0].into(), a[1].into(), a[2].into()],
            end_mm: [b[0].into(), b[1].into(), b[2].into()],
            radius_mm: strip_equivalent_radius_mm(width).into(),
            segments: n,
        });
        Ok(())
    };
    let snap_feed = treatment == FeedTreatment::Snapped;
    let feed_solver = solver_pt(feed_p, true);
    if treatment == FeedTreatment::LeadWire {
        add(
            feed_solver,
            solver_pt(feed_p, false),
            feed_width,
            "the feed lead (pour edge → feed trace end)".into(),
        )?;
    }
    for (p, &(u, v)) in pieces.iter().zip(&edges) {
        let a = solver_pt(p.a, snap_feed && u == feed_node);
        let b = solver_pt(p.b, snap_feed && v == feed_node);
        add(a, b, p.width, format!("trace {}", p.trace))?;
    }

    let spec = AntennaSpec {
        ground_plane: true,
        elements,
        feed_mm: [
            feed_solver[0].into(),
            feed_solver[1].into(),
            feed_solver[2].into(),
        ],
    };

    // ── resolve and assert the feed basis is the grounded feed end ────
    let parameters = BTreeMap::new();
    let (mesh, feed_basis) = spec
        .resolve(&parameters)
        .map_err(|e| E::Solver(e.to_string()))?;
    let basis = &mesh.bases[feed_basis];
    let fnode = mesh.nodes[basis.node];
    let off = ((fnode[0] * 1e3 - feed_solver[0]).powi(2)
        + (fnode[1] * 1e3).powi(2)
        + (fnode[2] * 1e3).powi(2))
    .sqrt();
    if off > tol || basis.halves.len() != 1 {
        return Err(E::Solver(format!(
            "feed basis {feed_basis} is not the grounded feed end ({off:.4} mm off, {} halves)",
            basis.halves.len()
        )));
    }

    // ── metadata ──────────────────────────────────────────────────────
    let mut ers: Vec<f64> = Vec::new();
    let mut materials: Vec<String> = Vec::new();
    let mut diel_t = 0.0;
    let mut have_t = false;
    let mut cu_t = None;
    for s in &pcb.stackup.layers {
        if let Some(er) = s.dielectric_er {
            if !ers.iter().any(|e| (e - er).abs() < 1e-9) {
                ers.push(er);
            }
        }
        if let Some(t) = s.dielectric_thickness {
            diel_t += t;
            have_t = true;
        }
        if let Some(m) = &s.material {
            if !materials.contains(m) {
                materials.push(m.clone());
            }
        }
        if s.layer == layer {
            cu_t = s.copper_thickness;
        }
    }
    let substrate = SubstrateMeta {
        board_thickness_mm: pcb.outline.thickness,
        dielectric_er: (ers.len() == 1).then(|| ers[0]),
        dielectric_er_values: ers,
        dielectric_thickness_mm: have_t.then_some(diel_t),
        materials,
        antenna_layer: layer,
        antenna_copper_thickness_mm: cu_t,
    };
    let along = x_hi - x_lo;
    let ground = GroundApproximation {
        model: "infinite_image".into(),
        ground_net: sel.ground_net.clone(),
        edge_board_mm: [ea, eb],
        edge_layer: owner.layer,
        pour_along_edge_mm: along,
        pour_depth_mm: depth,
        pour_along_edge_wavelengths: along / lambda_mm,
        pour_depth_wavelengths: depth / lambda_mm,
        approximation: format!(
            "infinite PEC ground plane at the '{}' pour edge (image theory): the coplanar pour \
             ({along:.1} × {depth:.1} mm = {:.2} × {:.2} λ at {:.0} MHz) is replaced by an infinite \
             plane perpendicular to the antenna. Exact only for a pour large in wavelengths; a \
             sub-wavelength pour shifts resonance and radiates from the ground currents too",
            sel.ground_net,
            along / lambda_mm,
            depth / lambda_mm,
            sel.f_max_hz / 1e6
        ),
    };
    let mut notes = vec![
        ground.approximation.clone(),
        "free-space model: no substrate. FR-4 trace antennas resonate roughly 20–30 % BELOW a \
         free-space prediction (docs/antenna-measurement-pack.md: f_meas/f_pred ≈ 1/√ε_eff, \
         ε_eff ≈ 1.5–2 for a trace at a pour edge). Treat predicted frequencies as an upper \
         bound; the substrate is recorded in `substrate` for the dielectric model (G7)"
            .into(),
        "pour geometry is the zone's requested outline, not its filled copper (clearance \
         cutouts are not seen)"
            .into(),
        format!(
            "traces → round wires of radius width/4 (zero-thickness strip equivalence); \
             {} wire elements, {} segments, ≤ λ/{} at {:.0} MHz",
            spec.elements.len(),
            mesh.segments.len(),
            sel.segments_per_wavelength,
            sel.f_max_hz / 1e6
        ),
    ];
    match treatment {
        FeedTreatment::OnEdge => {}
        FeedTreatment::Snapped => notes.push(format!(
            "feed trace end moved {gap:.3} mm onto the pour edge (gap shorter than the trace width — no thin-wire segment fits)"
        )),
        FeedTreatment::LeadWire => notes.push(format!(
            "feed gap {gap:.3} mm bridged by a lead wire of the feed trace's width"
        )),
    }
    // Other copper in front of the edge is not in the model — say so.
    let stray = pcb
        .traces
        .iter()
        .filter(|t| t.net != net)
        .filter(|t| {
            to_solver([t.start.x, t.start.y]).1 > tol || to_solver([t.end.x, t.end.y]).1 > tol
        })
        .count();
    if stray > 0 {
        notes.push(format!(
            "{stray} trace(s) of other nets lie in front of the pour edge and are NOT in the model"
        ));
    }

    Ok(ExtractedAntenna {
        segments: mesh.segments.len(),
        spec,
        parameters,
        board_node,
        net,
        trace_indices,
        feed: FeedInfo {
            footprint: sel.feed.footprint.clone(),
            pad: sel.feed.pad.clone(),
            board_point_mm: feed_p,
            solver_point_mm: feed_solver,
            gap_mm: gap,
            treatment,
        },
        frame: SolverFrame {
            origin_board_mm: origin,
            x_axis_board: x_axis,
            z_axis_board: z_axis,
        },
        ground,
        substrate,
        notes,
    })
}
