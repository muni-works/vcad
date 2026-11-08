//! The verdict ladder: put every still-unrouted connection in front of the
//! COMPLETE window router and demand an answer — `Routed` (commit-quality
//! paths that survive an oracle probe), `ProvedInfeasible` (a bottleneck-cut
//! certificate), or `BudgetExhausted` (an honest unknown).
//!
//! Lifted out of `vcad-ecad-pcb/examples/cm5_verdict.rs` so the fab-prep
//! pipeline and the example driver run the same code rather than two copies
//! that drift.

use std::collections::{BTreeSet, HashMap};

use vcad_ecad_pcb::router::complete::{
    live_layers, path_vias, route_window_complete_islands, route_window_complete_pinned,
    CompleteOutcome, IslandTerminals, TerminalLayers, ViaClass, WindowBudget,
};
use vcad_ecad_pcb::router::islands::{island_connections, IslandConnection};
use vcad_ecad_pcb::session::RouteSession;
use vcad_ecad_pcb::spatial::{CopperElement, CopperGeom};
use vcad_ir::ecad::{Pcb, PcbLayer, Trace, Via, ViaType};
use vcad_ir::Vec2;

/// Search knobs for one pass of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerdictOptions {
    /// Maximum DFS node expansions per cluster before the router returns an
    /// honest unknown.
    pub budget: usize,
    /// Maximum connections coalesced into one joint search window.
    pub max_cluster: usize,
    /// Permit blind/buried (layer-pair) vias even when the board does not
    /// declare them. **Off by default**: a standard 2/4-layer process (JLC,
    /// PCBWay, OSH Park) drills through vias only, and a layer-pair via the
    /// fab cannot build either gets drilled through — shorting every layer it
    /// crosses — or bounces the order. A board that already carries blind,
    /// buried or micro vias has declared the process and keeps them
    /// regardless (see [`declares_blind_vias`]).
    #[serde(default)]
    pub allow_blind_vias: bool,
}

impl Default for VerdictOptions {
    fn default() -> Self {
        Self {
            budget: 5_000_000,
            max_cluster: 6,
            allow_blind_vias: false,
        }
    }
}

/// Whether `pcb` declares a blind/buried-capable process: it already carries a
/// via typed blind or micro, or one whose span stops short of the full copper
/// stack. An imported HDI board (the CM5) says so through its own copper; a
/// freshly placed board says nothing and gets through vias.
pub fn declares_blind_vias(pcb: &Pcb) -> bool {
    let copper: Vec<PcbLayer> = copper_layers(pcb);
    let (Some(&top), Some(&bottom)) = (copper.first(), copper.last()) else {
        return false;
    };
    pcb.vias.iter().any(|v| {
        matches!(v.via_type, Some(ViaType::Blind) | Some(ViaType::Micro))
            || !((v.start_layer == top && v.end_layer == bottom)
                || (v.start_layer == bottom && v.end_layer == top))
    })
}

fn copper_layers(pcb: &Pcb) -> Vec<PcbLayer> {
    pcb.stackup
        .layers
        .iter()
        .map(|l| l.layer)
        .filter(|l| l.is_copper())
        .collect()
}

/// What one pass of the ladder concluded.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VerdictSummary {
    /// Unrouted connections the pass was asked about (plane nets excluded).
    pub connections: usize,
    /// Joint search windows the connections were clustered into.
    pub clusters: usize,
    /// Connections routed and committed to the board.
    pub routed: usize,
    /// Connections carrying a proved-infeasible certificate.
    pub proved_infeasible: usize,
    /// Connections that ended as honest unknowns (budget exhausted, or a path
    /// the session oracle rejected).
    pub unknown: usize,
    /// Infeasibility certificates, verbatim — capped so a large board's
    /// receipt stays readable.
    pub certificates: Vec<String>,
    /// True when every via this pass committed was a through via (the
    /// default unless the board declares blind/buried vias or the caller
    /// opted in with [`VerdictOptions::allow_blind_vias`]).
    #[serde(default)]
    pub through_vias_only: bool,
}

/// How many certificates a summary carries before it stops collecting.
const MAX_CERTIFICATES: usize = 64;

/// A joint search window: its bounds and the indices of its connections.
type Cluster = (Vec2, Vec2, Vec<usize>);

/// Merged windows wider than this coarsen the router's grid pitch until
/// unrelated terminals artificially collide.
const MAX_WINDOW_MM: f64 = 20.0;

/// Route every still-unrouted connection on `pcb`, committing only copper that
/// survives the session oracle. Mutates `pcb` in place.
pub fn route_remaining(pcb: &mut Pcb, opts: VerdictOptions) -> VerdictSummary {
    // Nets that own a filled zone are connected THROUGH the plane, not by
    // pad-to-pad traces — the router intentionally stitches them with vias.
    // Their air-wires are not unrouted work and must not enter the verdict.
    let plane_nets: BTreeSet<&str> = pcb
        .zones
        .iter()
        .filter(|z| !z.net.is_empty())
        .map(|z| z.net.as_str())
        .collect();
    let layers = copper_layers(pcb);
    // Unrouted connections = a spanning set over each net's copper ISLANDS as
    // the board stands (the DRC's own connectivity): a connection may start on
    // any copper already joined to its pad — a fan-out stub, a via, an
    // earlier trace — not only on the pad itself.
    let rats: Vec<IslandConnection> = island_connections(pcb, &layers, &plane_nets);
    // Via policy: through vias only, unless the board declares a blind/buried
    // process or the caller opted in. `through` is the span every barrel is
    // widened to.
    let through = (!(opts.allow_blind_vias || declares_blind_vias(pcb)))
        .then(|| Some((*layers.first()?, *layers.last()?)))
        .flatten();
    let mut summary = VerdictSummary {
        connections: rats.len(),
        through_vias_only: through.is_some(),
        ..Default::default()
    };
    if rats.is_empty() {
        return summary;
    }

    let mut session = RouteSession::from_pcb(pcb);
    let width = pcb.rules.default_rules.trace_width;
    // Per-net class widths: an SI-class net must be routed AND committed at its
    // class width, because the DRC's MinTraceWidth rule is per-net. A joint
    // path at the default width lands hundreds of width violations on a board
    // whose pairs are classed wider.
    let net_width: HashMap<String, f64> = pcb
        .rules
        .class_rules
        .iter()
        .flat_map(|rule| {
            pcb.rules
                .net_class_assignments
                .get(&rule.name)
                .into_iter()
                .flatten()
                .map(move |net| (net.clone(), rule.trace_width))
        })
        .collect();

    // Cluster connections whose bboxes (inflated 2mm) overlap. Two rules keep
    // the certificates honest: a cluster never holds two connections of the
    // same net (per-connection node-disjointness can't model same-net cell
    // sharing), and the merged window is capped.
    let mut clusters: Vec<Cluster> = Vec::new();
    'c: for (li, l) in rats.iter().enumerate() {
        let (lo, hi) = (
            Vec2::new(l.from.x.min(l.to.x) - 2.0, l.from.y.min(l.to.y) - 2.0),
            Vec2::new(l.from.x.max(l.to.x) + 2.0, l.from.y.max(l.to.y) + 2.0),
        );
        for (clo, chi, cc) in clusters.iter_mut() {
            let merged_w = (chi.x.max(hi.x) - clo.x.min(lo.x)).abs();
            let merged_h = (chi.y.max(hi.y) - clo.y.min(lo.y)).abs();
            if lo.x <= chi.x
                && clo.x <= hi.x
                && lo.y <= chi.y
                && clo.y <= hi.y
                && cc.len() < opts.max_cluster
                && merged_w <= MAX_WINDOW_MM
                && merged_h <= MAX_WINDOW_MM
                && cc.iter().all(|&j| rats[j].net != l.net)
            {
                clo.x = clo.x.min(lo.x);
                clo.y = clo.y.min(lo.y);
                chi.x = chi.x.max(hi.x);
                chi.y = chi.y.max(hi.y);
                cc.push(li);
                continue 'c;
            }
        }
        clusters.push((lo, hi, vec![li]));
    }
    summary.clusters = clusters.len();

    for (lo, hi, members) in &clusters {
        let conns: Vec<(String, Vec2, Vec2)> = members
            .iter()
            .map(|&j| (rats[j].net.clone(), rats[j].from, rats[j].to))
            .collect();
        let conns = &conns;
        // Each terminal is the whole copper island on that end — pads on
        // their own layers (an unpinned F.Cu SMD pad would otherwise start
        // its trace on an inner layer with no via back to the pad), vias on
        // every layer they span, traces along their length.
        let islands: Vec<IslandTerminals> =
            members.iter().map(|&j| rats[j].terminals.clone()).collect();
        // Search at the widest class width in the cluster (conservative:
        // guarantees the found corridors fit every member's committed width).
        let cluster_width = conns
            .iter()
            .map(|(n, _, _)| net_width.get(n).copied().unwrap_or(width))
            .fold(width, f64::max);
        // Hand the router the via geometry the commit below actually writes, so
        // its grid pitch carries the hole-to-hole floor and its barrels are
        // drill-probed in-search. Without it the search spaces vias by copper
        // clearance alone and proposes paths the fail-closed commit has to
        // throw away as unknowns.
        //
        // Per-span sizing (`vcad_ecad_pcb::via_sizing`): the window router
        // hops one layer pair at a time, so it is handed the LARGEST
        // single-hop via of the cluster (a microvia on an HDI board). A
        // stacked descent it emits is judged at its real, drilled size by
        // `vias_legal` before anything is committed.
        //
        // Through-only: every barrel is drilled, so search at the drilled size.
        let sz = session.via_sizing();
        let via_class = conns
            .iter()
            .map(|(n, _, _)| match through {
                Some(_) => sz.drilled_size(n),
                None => sz.micro_size(n).unwrap_or_else(|| sz.drilled_size(n)),
            })
            .fold((0.0_f64, 0.0_f64), |(ad, adr), (d, dr)| {
                (ad.max(d), adr.max(dr))
            });
        match route_window_complete_islands(
            &session,
            (*lo, *hi),
            &layers,
            conns,
            &islands,
            cluster_width,
            Some(ViaClass {
                pad_diameter: via_class.0,
                drill: via_class.1,
            }),
            WindowBudget::new(opts.budget),
        ) {
            CompleteOutcome::Routed(paths) => {
                // Probe-then-commit PER PATH, in order. Cluster paths are
                // node-disjoint on the coarse window grid, but the grid pitch
                // can hide sub-clearance gaps BETWEEN two paths of the same
                // cluster. Probing each path against the session AFTER its
                // clustermates committed makes mutual legality exact; a path
                // the oracle rejects downgrades only itself to an unknown.
                for ((net, _, _), path) in conns.iter().zip(&paths) {
                    let w = net_width.get(net).copied().unwrap_or(width);
                    let legal = path.iter().all(|&(a, b, l)| {
                        let g = CopperGeom::Segment {
                            a,
                            b,
                            half_w: w / 2.0,
                        };
                        session.probe(&g, l, net, session.clearance_for(net)).legal
                    }) && vias_legal(pcb, &session, net, path, through);
                    if !legal {
                        summary.unknown += 1;
                        continue;
                    }
                    summary.routed += 1;
                    commit_path(pcb, &mut session, net, w, path, through);
                }
                // A cluster the router answered for fewer paths than it holds
                // (the zip is short) leaves the remainder unaccounted; charge
                // them as unknowns rather than silently dropping them.
                summary.unknown += conns.len().saturating_sub(paths.len());
            }
            CompleteOutcome::ProvedInfeasible { reason } => {
                summary.proved_infeasible += conns.len();
                if summary.certificates.len() < MAX_CERTIFICATES {
                    // Name the islands, not just the nets: "which copper of
                    // net X could not be joined to which".
                    let names: Vec<String> = members
                        .iter()
                        .map(|&j| {
                            let t = &rats[j].terminals;
                            format!("{} [{} -> {}]", rats[j].net, t.from_label, t.to_label)
                        })
                        .collect();
                    summary
                        .certificates
                        .push(format!("{}: {reason}", names.join(", ")));
                }
            }
            CompleteOutcome::BudgetExhausted => summary.unknown += conns.len(),
        }
    }
    summary
}

/// The copper layers a barrel from `start` to `end` occupies, in stackup
/// order — endpoints *and* every interior layer.
///
/// A barrel is a hole through all of them: probing or committing only the two
/// endpoint layers leaves the interior invisible to the oracle, and a later
/// path is then routed straight through the barrel on an inner layer (measured
/// on the CM5: two 0.000mm shorts, both a FCu..In2Cu barrel against an In1Cu
/// trace).
fn spanned_layers(pcb: &Pcb, start: PcbLayer, end: PcbLayer) -> Vec<PcbLayer> {
    let mut out: Vec<PcbLayer> = pcb
        .stackup
        .layers
        .iter()
        .map(|l| l.layer)
        .filter(|l| l.is_copper() && l.spanned_by(start, end))
        .collect();
    for endpoint in [start, end] {
        if !out.contains(&endpoint) {
            out.push(endpoint);
        }
    }
    out
}

/// Fail-closed via legality for a candidate path, against the session as it
/// stands (clustermates already committed).
///
/// Two rules the segment probe structurally cannot answer, and which
/// `commit_path` used to commit blind:
///
/// * the barrel's annulus must clear foreign copper on **every** layer it
///   spans, not just its endpoints;
/// * the *drill* must keep `hole_to_hole` from every other hole on the board —
///   layer- and net-agnostic, so two vias whose copper never meets still
///   collide in the drill file. The window router spaces vias inside ONE
///   window by a drill-aware grid pitch; nothing spaced them against the vias
///   of earlier windows, earlier rounds, or the arriving board (measured on the
///   CM5: four hole-to-hole violations, every one of them copper-legal and
///   drill-illegal — the 0.29..0.37mm window between the two rules).
fn vias_legal(
    pcb: &Pcb,
    session: &RouteSession,
    net: &str,
    path: &[(Vec2, Vec2, PcbLayer)],
    through: Option<(PcbLayer, PcbLayer)>,
) -> bool {
    let clearance = session.clearance_for(net);
    let vias = barrels(path, through);
    vias.iter().all(|&(p, l0, l1)| {
        // Each barrel at its own span's size.
        let spec = session.via_spec(net, l0, l1);
        let via_d = spec.diameter;
        if !session.probe_via_span(p, net, l0, l1).legal {
            return false;
        }
        // The path's own barrels must clear each other too — the session
        // cannot judge them until they are committed.
        let self_clear = vias.iter().all(|&(q, q0, q1)| {
            let (dx, dy) = (q.x - p.x, q.y - p.y);
            dx.abs() + dy.abs() < 1e-9
                || (dx * dx + dy * dy).sqrt()
                    - (spec.drill + session.via_spec(net, q0, q1).drill) / 2.0
                    >= pcb.rules.hole_to_hole - 1e-6
        });
        if !self_clear {
            return false;
        }
        let disc = CopperGeom::Disc {
            center: p,
            r: via_d / 2.0,
        };
        spanned_layers(pcb, l0, l1)
            .into_iter()
            .all(|l| session.probe(&disc, l, net, clearance).legal)
    })
}

/// The barrels a path commits: [`path_vias`], each widened to `through`
/// (outer to outer) when the via policy is through-only, with coincident
/// barrels merged — two through vias at one point are one hole.
fn barrels(
    path: &[(Vec2, Vec2, PcbLayer)],
    through: Option<(PcbLayer, PcbLayer)>,
) -> Vec<(Vec2, PcbLayer, PcbLayer)> {
    let vias = path_vias(path);
    let Some((top, bottom)) = through else {
        return vias;
    };
    let mut out: Vec<(Vec2, PcbLayer, PcbLayer)> = Vec::new();
    for (p, _, _) in vias {
        if !out
            .iter()
            .any(|(q, _, _)| (q.x - p.x).abs() + (q.y - p.y).abs() < 1e-9)
        {
            out.push((p, top, bottom));
        }
    }
    out
}

/// Commit one accepted path to both the legality oracle and the board.
fn commit_path(
    pcb: &mut Pcb,
    session: &mut RouteSession,
    net: &str,
    w: f64,
    path: &[(Vec2, Vec2, vcad_ir::ecad::PcbLayer)],
    through: Option<(PcbLayer, PcbLayer)>,
) {
    for (a, b, layer) in path {
        session.commit(CopperElement {
            min: [a.x.min(b.x) - w, a.y.min(b.y) - w],
            max: [a.x.max(b.x) + w, a.y.max(b.y) + w],
            net: net.to_string(),
            layer: *layer,
            geom: CopperGeom::Segment {
                a: *a,
                b: *b,
                half_w: w / 2.0,
            },
        });
        pcb.traces.push(Trace {
            start: *a,
            end: *b,
            width: w,
            layer: *layer,
            net: net.to_string(),
            source: None,
        });
    }
    // Layer changes are barrels. `path_vias` merges a multi-step transition at
    // one point into a single barrel — two stacked vias at the same position
    // are a hole-to-hole violation against each other.
    for (p, l0, l1) in barrels(path, through) {
        let spec = session.via_spec(net, l0, l1);
        let (via_d, via_drill, r) = (spec.diameter, spec.drill, spec.r());
        // Copper on every spanned layer, and the drill in the hole index, so
        // the next path sees the whole barrel — both the annulus it must clear
        // and the hole it must keep `hole_to_hole` from.
        for layer in spanned_layers(pcb, l0, l1) {
            session.commit(CopperElement {
                min: [p.x - r, p.y - r],
                max: [p.x + r, p.y + r],
                net: net.to_string(),
                layer,
                geom: CopperGeom::Disc { center: p, r },
            });
        }
        session.commit_drill(p, via_drill);
        pcb.vias.push(Via {
            via_type: Some(spec.via_type),
            position: p,
            diameter: via_d,
            drill: via_drill,
            start_layer: l0,
            end_layer: l1,
            net: net.to_string(),
            source: None,
        });
    }
}

/// Remove all board-level copper (traces, trace arcs, vias) belonging to
/// `nets`. Returns `(traces, arcs, vias)` removed.
pub fn strip_nets(pcb: &mut Pcb, nets: &BTreeSet<String>) -> (usize, usize, usize) {
    let before = (pcb.traces.len(), pcb.trace_arcs.len(), pcb.vias.len());
    pcb.traces.retain(|t| !nets.contains(&t.net));
    pcb.trace_arcs.retain(|t| !nets.contains(&t.net));
    pcb.vias.retain(|v| !nets.contains(&v.net));
    (
        before.0 - pcb.traces.len(),
        before.1 - pcb.trace_arcs.len(),
        before.2 - pcb.vias.len(),
    )
}

/// Nets owning board-level copper within `radius` of `at`.
///
/// The attribution channel of last resort. Three rules — hole-to-hole, annular
/// ring, minimum drill — report a measurement and a position but name no net at
/// all, so a message-based census cannot see them and the fix loop would declare
/// them unreachable when they are in fact ordinary re-routing work: move the
/// offending via and the violation goes away. Reading the nets off the copper
/// sitting at the reported position closes that gap.
pub fn nets_near(pcb: &Pcb, at: [f64; 2], radius: f64) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for via in &pcb.vias {
        let (dx, dy) = (via.position.x - at[0], via.position.y - at[1]);
        if (dx * dx + dy * dy).sqrt() <= radius + via.diameter / 2.0 {
            out.insert(via.net.clone());
        }
    }
    for t in &pcb.traces {
        if point_segment_distance(at, [t.start.x, t.start.y], [t.end.x, t.end.y])
            <= radius + t.width / 2.0
        {
            out.insert(t.net.clone());
        }
    }
    out
}

/// Distance from a point to a segment.
fn point_segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = abx * abx + aby * aby;
    let t = if len2 <= f64::EPSILON {
        0.0
    } else {
        (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0)
    };
    let (dx, dy) = (p[0] - (a[0] + t * abx), p[1] - (a[1] + t * aby));
    (dx * dx + dy * dy).sqrt()
}

/// Every net that currently owns board-level copper.
pub fn nets_with_copper(pcb: &Pcb) -> BTreeSet<String> {
    pcb.traces
        .iter()
        .map(|t| t.net.clone())
        .chain(pcb.trace_arcs.iter().map(|t| t.net.clone()))
        .chain(pcb.vias.iter().map(|v| v.net.clone()))
        .collect()
}

/// One attempt at the interactive route from a start point to the cursor.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InteractiveRoute {
    /// `routed`, `blocked` (proved, or rejected by the oracle) or `unknown`
    /// (search budget ran out).
    pub status: String,
    /// Why it is not `routed`.
    pub reason: Option<String>,
    /// The path as `(a, b, layer)` segments; empty unless `routed`.
    pub path: Vec<(Vec2, Vec2, PcbLayer)>,
    /// Barrel positions the path would drill.
    pub vias: Vec<Vec2>,
    /// Trace width the path commits at.
    pub width: f64,
    /// Existing copper this path pushes aside to fit.
    #[serde(default)]
    pub moved: Vec<TraceMove>,
}

/// New endpoints for an existing trace, by its index in the board's traces.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TraceMove {
    /// Index into `Pcb::traces`.
    pub index: usize,
    /// New start.
    pub start: Vec2,
    /// New end.
    pub end: Vec2,
}

/// Shove displacement step and cap (mm); smallest workable move wins.
const SHOVE_STEP_MM: f64 = 0.2;
const SHOVE_MAX_MM: f64 = 1.2;
/// Traces further than this from the corridor are left alone.
const SHOVE_REACH_MM: f64 = 1.5;
/// At most this many segments move for one route.
const SHOVE_MAX_VICTIMS: usize = 6;

/// A board held open for hand routing: an indexed session answering "route
/// this net from here to the cursor" fast enough to follow the mouse, with
/// the same fail-closed legality as the autorouter's commit — a preview that
/// comes back `routed` is copper DRC will accept.
pub struct InteractiveRouter {
    pcb: Pcb,
    session: RouteSession,
    layers: Vec<PcbLayer>,
    through: Option<(PcbLayer, PcbLayer)>,
}

impl InteractiveRouter {
    /// Index `pcb` for routing.
    pub fn new(pcb: Pcb) -> Self {
        let layers = copper_layers(&pcb);
        let through = (!declares_blind_vias(&pcb))
            .then(|| Some((*layers.first()?, *layers.last()?)))
            .flatten();
        let session = RouteSession::from_pcb(&pcb);
        Self {
            pcb,
            session,
            layers,
            through,
        }
    }

    fn width_for(&self, net: &str) -> f64 {
        self.pcb
            .rules
            .class_rules
            .iter()
            .find(|rule| {
                self.pcb
                    .rules
                    .net_class_assignments
                    .get(&rule.name)
                    .is_some_and(|nets| nets.iter().any(|n| n == net))
            })
            .map(|rule| rule.trace_width)
            .unwrap_or(self.pcb.rules.default_rules.trace_width)
    }

    /// Search for a legal path from `from` to `to` on `net`: on `preferred`
    /// if one layer will do, then on `preferred` pushing neighbouring traces
    /// aside, and only then changing layers.
    pub fn route(
        &self,
        net: &str,
        from: Vec2,
        to: Vec2,
        preferred: Option<PcbLayer>,
        budget: usize,
    ) -> InteractiveRoute {
        if let Some(layer) = preferred.filter(|p| self.layers.contains(p)) {
            let mut stack = vec![layer];
            for l in live_layers(&self.pcb, &self.layers, net, from) {
                if !stack.contains(&l) {
                    stack.push(l);
                }
            }
            let plain = self.best(net, from, to, layer, &stack, budget);
            if plain.status == "routed" {
                return plain;
            }
            if let Some(shoved) = self.shove(net, from, to, layer, &stack, budget) {
                return shoved;
            }
        }
        self.search(net, from, to, &self.layers, budget)
    }

    /// The shortest single-layer path if there is one on the search grid,
    /// else the complete search.
    fn best(
        &self,
        net: &str,
        from: Vec2,
        to: Vec2,
        layer: PcbLayer,
        stack: &[PcbLayer],
        budget: usize,
    ) -> InteractiveRoute {
        let width = self.width_for(net);
        let attaches = |p: Vec2| {
            let live = live_layers(&self.pcb, &self.layers, net, p);
            live.is_empty() || live.contains(&layer)
        };
        if attaches(from) && attaches(to) {
            if let Some(path) = [8.0, 30.0]
                .into_iter()
                .find_map(|margin| self.shortest_on_layer(net, width, from, to, layer, margin))
            {
                let path = self.tighten(net, width, path);
                if self.legal(net, width, &path) {
                    return InteractiveRoute {
                        status: "routed".into(),
                        reason: None,
                        path,
                        vias: Vec::new(),
                        width,
                        moved: Vec::new(),
                    };
                }
            }
        }
        self.search(net, from, to, stack, budget)
    }

    fn segment_clear(&self, net: &str, w: f64, a: Vec2, b: Vec2, layer: PcbLayer) -> bool {
        let g = CopperGeom::Segment {
            a,
            b,
            half_w: w / 2.0,
        };
        self.session
            .probe(&g, layer, net, self.session.clearance_for(net))
            .legal
    }

    /// Octilinear A* from `from` to `to` over a fine grid, each edge checked
    /// against the clearance oracle. Bends cost a little, so among equal
    /// lengths the straighter route wins. `None` when no path on this grid.
    fn shortest_on_layer(
        &self,
        net: &str,
        w: f64,
        from: Vec2,
        to: Vec2,
        layer: PcbLayer,
        margin: f64,
    ) -> Option<Vec<(Vec2, Vec2, PcbLayer)>> {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        const PITCH: f64 = 0.25;
        const MAX_EXPANSIONS: usize = 200_000;
        const BEND_COST: f64 = 0.15;
        const DIRS: [(i32, i32); 8] = [
            (1, 0),
            (1, 1),
            (0, 1),
            (-1, 1),
            (-1, 0),
            (-1, -1),
            (0, -1),
            (1, -1),
        ];
        let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for v in &self.pcb.outline.vertices {
            (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(v.x), lo_y.min(v.y), hi_x.max(v.x), hi_y.max(v.y));
        }
        let (x0, y0) = (
            (from.x.min(to.x) - margin).max(lo_x),
            (from.y.min(to.y) - margin).max(lo_y),
        );
        let (x1, y1) = (
            (from.x.max(to.x) + margin).min(hi_x),
            (from.y.max(to.y) + margin).min(hi_y),
        );
        let nx = ((x1 - x0) / PITCH).ceil() as i32 + 1;
        let ny = ((y1 - y0) / PITCH).ceil() as i32 + 1;
        let at =
            |c: (i32, i32)| Vec2::new(x0 + f64::from(c.0) * PITCH, y0 + f64::from(c.1) * PITCH);
        let cell = |p: Vec2| {
            (
                ((p.x - x0) / PITCH).round() as i32,
                ((p.y - y0) / PITCH).round() as i32,
            )
        };
        let inside = |c: (i32, i32)| c.0 >= 0 && c.1 >= 0 && c.0 < nx && c.1 < ny;
        let octile = |a: (i32, i32), b: (i32, i32)| {
            let (dx, dy) = (f64::from((a.0 - b.0).abs()), f64::from((a.1 - b.1).abs()));
            PITCH * (dx.max(dy) + (std::f64::consts::SQRT_2 - 1.0) * dx.min(dy))
        };
        // Terminals are off-grid: hook them to the nearby grid nodes they can reach.
        let around = |p: Vec2| -> Vec<(i32, i32)> {
            let c = cell(p);
            let mut out = Vec::new();
            for dx in -2..=2 {
                for dy in -2..=2 {
                    let q = (c.0 + dx, c.1 + dy);
                    if inside(q) && self.segment_clear(net, w, p, at(q), layer) {
                        out.push(q);
                    }
                }
            }
            out
        };
        let goals: BTreeSet<(i32, i32)> = around(to).into_iter().collect();
        if goals.is_empty() {
            return None;
        }
        let target = cell(to);
        type State = (i32, i32, u8);
        let key = |f: f64| (f * 1000.0).round() as i64;
        let mut open: BinaryHeap<Reverse<(i64, State)>> = BinaryHeap::new();
        let mut best: HashMap<State, f64> = HashMap::new();
        let mut came: HashMap<State, State> = HashMap::new();
        for c in around(from) {
            let s = (c.0, c.1, 8u8);
            best.insert(s, 0.0);
            open.push(Reverse((key(octile(c, target)), s)));
        }
        let mut edge: HashMap<(i32, i32, u8), bool> = HashMap::new();
        let mut expansions = 0;
        let mut end = None;
        while let Some(Reverse((_, s))) = open.pop() {
            expansions += 1;
            if expansions > MAX_EXPANSIONS {
                return None;
            }
            if goals.contains(&(s.0, s.1)) {
                end = Some(s);
                break;
            }
            let g = best[&s];
            for (d, &(dx, dy)) in DIRS.iter().enumerate() {
                let n = (s.0 + dx, s.1 + dy);
                if !inside(n) {
                    continue;
                }
                let clear = *edge
                    .entry((s.0, s.1, d as u8))
                    .or_insert_with(|| self.segment_clear(net, w, at((s.0, s.1)), at(n), layer));
                if !clear {
                    continue;
                }
                let step = PITCH
                    * if dx != 0 && dy != 0 {
                        std::f64::consts::SQRT_2
                    } else {
                        1.0
                    };
                let bend = if s.2 == 8 || s.2 == d as u8 {
                    0.0
                } else {
                    BEND_COST
                };
                let cost = g + step + bend;
                let ns = (n.0, n.1, d as u8);
                if best.get(&ns).is_none_or(|&b| cost < b - 1e-9) {
                    best.insert(ns, cost);
                    came.insert(ns, s);
                    open.push(Reverse((key(cost + octile(n, target)), ns)));
                }
            }
        }
        let mut cur = end?;
        let mut cells = vec![(cur.0, cur.1)];
        while let Some(&p) = came.get(&cur) {
            cur = p;
            if cells.last() != Some(&(cur.0, cur.1)) {
                cells.push((cur.0, cur.1));
            }
        }
        cells.reverse();
        let mut pts = vec![from];
        pts.extend(cells.into_iter().map(at));
        pts.push(to);
        pts.dedup_by(|a, b| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9);
        Some(pts.windows(2).map(|w| (w[0], w[1], layer)).collect())
    }

    /// Between `a` and `b`, octilinear: straight when it already is, else a
    /// diagonal plus a straight, whichever way clears. `None` when neither does.
    fn connector(&self, net: &str, w: f64, a: Vec2, b: Vec2, layer: PcbLayer) -> Option<Vec<Vec2>> {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let (ax, ay) = (dx.abs(), dy.abs());
        if ax < 1e-9 || ay < 1e-9 || (ax - ay).abs() < 1e-9 {
            return self.segment_clear(net, w, a, b, layer).then(|| vec![a, b]);
        }
        let diag = ax.min(ay);
        let knee = Vec2::new(a.x + diag * dx.signum(), a.y + diag * dy.signum());
        let other = Vec2::new(b.x - diag * dx.signum(), b.y - diag * dy.signum());
        [knee, other].into_iter().find_map(|k| {
            (self.segment_clear(net, w, a, k, layer) && self.segment_clear(net, w, k, b, layer))
                .then(|| vec![a, k, b])
        })
    }

    /// Pull each same-layer run of `path` taut: from every point, jump to the
    /// furthest later point an octilinear connector can reach. Run ends stay
    /// put, so vias and terminals are untouched.
    fn tighten(
        &self,
        net: &str,
        w: f64,
        path: Vec<(Vec2, Vec2, PcbLayer)>,
    ) -> Vec<(Vec2, Vec2, PcbLayer)> {
        let joined = |a: Vec2, b: Vec2| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9;
        let mut out = Vec::new();
        let mut i = 0;
        while i < path.len() {
            let layer = path[i].2;
            let mut j = i;
            while j + 1 < path.len() && path[j + 1].2 == layer && joined(path[j].1, path[j + 1].0) {
                j += 1;
            }
            let mut pts = vec![path[i].0];
            pts.extend(path[i..=j].iter().map(|s| s.1));
            let mut k = 0;
            let mut taut = vec![pts[0]];
            while k + 1 < pts.len() {
                let (next, link) = (k + 1..pts.len())
                    .rev()
                    .find_map(|m| {
                        self.connector(net, w, pts[k], pts[m], layer)
                            .map(|c| (m, c))
                    })
                    .unwrap_or((k + 1, vec![pts[k], pts[k + 1]]));
                taut.extend(link.into_iter().skip(1));
                k = next;
            }
            // Merge collinear runs so a straight line is one segment.
            let mut merged: Vec<Vec2> = Vec::new();
            for p in taut {
                if merged.len() >= 2 {
                    let (a, b) = (merged[merged.len() - 2], merged[merged.len() - 1]);
                    if ((b.x - a.x) * (p.y - b.y) - (b.y - a.y) * (p.x - b.x)).abs() < 1e-9
                        && (b.x - a.x) * (p.x - b.x) + (b.y - a.y) * (p.y - b.y) > 0.0
                    {
                        merged.pop();
                    }
                }
                merged.push(p);
            }
            out.extend(merged.windows(2).map(|s| (s[0], s[1], layer)));
            i = j + 1;
        }
        out
    }

    fn search(
        &self,
        net: &str,
        from: Vec2,
        to: Vec2,
        stack: &[PcbLayer],
        budget: usize,
    ) -> InteractiveRoute {
        let width = self.width_for(net);
        let conns = [(net.to_string(), from, to)];
        let (via_d, via_drill) = self.session.via_sizing().drilled_size(net);
        let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for v in &self.pcb.outline.vertices {
            (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(v.x), lo_y.min(v.y), hi_x.max(v.x), hi_y.max(v.y));
        }
        let mut last = ("no path".to_string(), "blocked");
        for margin in [1.5, 6.0] {
            // The router does not consult the outline: the window must.
            let window = (
                Vec2::new(
                    (from.x.min(to.x) - margin).max(lo_x),
                    (from.y.min(to.y) - margin).max(lo_y),
                ),
                Vec2::new(
                    (from.x.max(to.x) + margin).min(hi_x),
                    (from.y.max(to.y) + margin).min(hi_y),
                ),
            );
            let pins = TerminalLayers::for_connection(&self.pcb, stack, net, from, to);
            match route_window_complete_pinned(
                &self.session,
                window,
                stack,
                &conns,
                &[pins],
                width,
                Some(ViaClass {
                    pad_diameter: via_d,
                    drill: via_drill,
                }),
                WindowBudget::new(budget),
            ) {
                CompleteOutcome::Routed(mut paths) => {
                    let Some(path) = paths.pop() else { continue };
                    if !self.legal(net, width, &path) {
                        last = ("the path breaks a clearance rule".into(), "blocked");
                        continue;
                    }
                    let path = self.tighten(net, width, path);
                    let vias = barrels(&path, self.through)
                        .into_iter()
                        .map(|b| b.0)
                        .collect();
                    return InteractiveRoute {
                        status: "routed".into(),
                        reason: None,
                        path,
                        vias,
                        width,
                        moved: Vec::new(),
                    };
                }
                CompleteOutcome::ProvedInfeasible { reason } => last = (reason, "blocked"),
                CompleteOutcome::BudgetExhausted => {
                    last = ("search budget ran out".into(), "unknown")
                }
            }
        }
        InteractiveRoute {
            status: last.1.into(),
            reason: Some(last.0),
            path: Vec::new(),
            vias: Vec::new(),
            width,
            moved: Vec::new(),
        }
    }

    /// True when `p` is an end of a pad or via: a joint that cannot move.
    fn anchored(&self, net: &str, p: Vec2) -> bool {
        let near = |q: Vec2| (q.x - p.x).abs() < 0.05 && (q.y - p.y).abs() < 0.05;
        self.pcb
            .vias
            .iter()
            .any(|v| v.net == net && near(v.position))
            || self.pcb.footprints.iter().any(|f| {
                f.pads.iter().any(|pad| {
                    pad.net.as_deref() == Some(net)
                        && near(vcad_ecad_pcb::geometry::pad_world_position(f, pad))
                })
            })
    }

    /// Traces of other nets on `layer` that sit beside the corridor and can
    /// move: middle segments, both ends joined only to more trace. Each with
    /// its outward unit normal. Pads, vias and end stubs stay put.
    fn shove_victims(
        &self,
        net: &str,
        from: Vec2,
        to: Vec2,
        layer: PcbLayer,
    ) -> Vec<(usize, Vec2)> {
        let (cx, cy) = (to.x - from.x, to.y - from.y);
        let len = (cx * cx + cy * cy).sqrt().max(1e-9);
        let same = |a: Vec2, b: Vec2| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6;
        let mut found: Vec<(f64, usize, Vec2)> = Vec::new();
        for (i, t) in self.pcb.traces.iter().enumerate() {
            if t.layer != layer || t.net == net {
                continue;
            }
            let joined = |p: Vec2| {
                !self.anchored(&t.net, p)
                    && self.pcb.traces.iter().enumerate().any(|(j, o)| {
                        j != i
                            && o.net == t.net
                            && o.layer == layer
                            && (same(o.start, p) || same(o.end, p))
                    })
            };
            if !joined(t.start) || !joined(t.end) {
                continue;
            }
            let mid = Vec2::new((t.start.x + t.end.x) / 2.0, (t.start.y + t.end.y) / 2.0);
            let u = (((mid.x - from.x) * cx + (mid.y - from.y) * cy) / (len * len)).clamp(0.0, 1.0);
            let (px, py) = (from.x + u * cx, from.y + u * cy);
            let d = ((mid.x - px).powi(2) + (mid.y - py).powi(2)).sqrt();
            if d > SHOVE_REACH_MM {
                continue;
            }
            let (dx, dy) = (t.end.x - t.start.x, t.end.y - t.start.y);
            let seg = (dx * dx + dy * dy).sqrt().max(1e-9);
            // Only traces running along the corridor: one that crosses it
            // can't be pushed clear, however far it moves.
            if ((dx * cx + dy * cy) / (seg * len)).abs() < 0.8 {
                continue;
            }
            let (mut nx, mut ny) = (-dy / seg, dx / seg);
            if nx * (mid.x - px) + ny * (mid.y - py) < 0.0 {
                (nx, ny) = (-nx, -ny);
            }
            found.push((d, i, Vec2::new(nx, ny)));
        }
        found.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        found.truncate(SHOVE_MAX_VICTIMS);
        found.into_iter().map(|(_, i, n)| (i, n)).collect()
    }

    /// The moves that push `victims` out by `delta`: each segment and the
    /// joint end of every neighbour that shares it.
    fn shove_moves(&self, victims: &[(usize, Vec2)], delta: f64) -> Vec<TraceMove> {
        let same = |a: Vec2, b: Vec2| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6;
        let mut moved: HashMap<usize, (Vec2, Vec2)> = HashMap::new();
        for &(i, n) in victims {
            let t = &self.pcb.traces[i];
            let shift = |p: Vec2| Vec2::new(p.x + n.x * delta, p.y + n.y * delta);
            for (j, o) in self.pcb.traces.iter().enumerate() {
                if o.net != t.net || o.layer != t.layer {
                    continue;
                }
                let entry = moved.entry(j).or_insert((o.start, o.end));
                for corner in [t.start, t.end] {
                    if same(o.start, corner) {
                        entry.0 = shift(corner);
                    }
                    if same(o.end, corner) {
                        entry.1 = shift(corner);
                    }
                }
            }
        }
        let mut out: Vec<TraceMove> = moved
            .into_iter()
            .filter(|(j, (a, b))| {
                let o = &self.pcb.traces[*j];
                !same(*a, o.start) || !same(*b, o.end)
            })
            .map(|(index, (start, end))| TraceMove { index, start, end })
            .collect();
        out.sort_by_key(|m| m.index);
        out
    }

    /// Every moved trace clears everything else on the board with `moves` applied.
    fn moves_legal(session: &RouteSession, pcb: &Pcb, moves: &[TraceMove]) -> bool {
        moves.iter().all(|m| {
            let t = &pcb.traces[m.index];
            let g = CopperGeom::Segment {
                a: m.start,
                b: m.end,
                half_w: t.width / 2.0,
            };
            session
                .probe(&g, t.layer, &t.net, session.clearance_for(&t.net))
                .legal
        })
    }

    fn with_moves(&self, moves: &[TraceMove]) -> InteractiveRouter {
        let mut pcb = self.pcb.clone();
        for m in moves {
            pcb.traces[m.index].start = m.start;
            pcb.traces[m.index].end = m.end;
        }
        InteractiveRouter::new(pcb)
    }

    /// Push neighbouring traces aside, smallest move first, until the route
    /// fits. Depth one: moved copper must be legal where it lands and never
    /// pushes anything else. `None` when no move helps.
    fn shove(
        &self,
        net: &str,
        from: Vec2,
        to: Vec2,
        layer: PcbLayer,
        stack: &[PcbLayer],
        budget: usize,
    ) -> Option<InteractiveRoute> {
        let victims = self.shove_victims(net, from, to, layer);
        if victims.is_empty() {
            return None;
        }
        let steps = (SHOVE_MAX_MM / SHOVE_STEP_MM).round() as usize;
        for k in 1..=steps {
            // Away from the corridor first, then across it: the free side
            // of a tight channel can be either.
            for sign in [1.0, -1.0] {
                let delta = sign * k as f64 * SHOVE_STEP_MM;
                let moves = self.shove_moves(&victims, delta);
                let trial = self.with_moves(&moves);
                if !Self::moves_legal(&trial.session, &trial.pcb, &moves) {
                    continue;
                }
                let mut attempt = trial.best(net, from, to, layer, stack, budget);
                if attempt.status == "routed" {
                    attempt.moved = moves;
                    return Some(attempt);
                }
            }
        }
        None
    }

    fn legal(&self, net: &str, w: f64, path: &[(Vec2, Vec2, PcbLayer)]) -> bool {
        path.iter().all(|&(a, b, l)| {
            let g = CopperGeom::Segment {
                a,
                b,
                half_w: w / 2.0,
            };
            self.session
                .probe(&g, l, net, self.session.clearance_for(net))
                .legal
        }) && vias_legal(&self.pcb, &self.session, net, path, self.through)
    }

    /// Apply `moves`, re-check `path`, and commit it, returning the new traces
    /// and vias. `Err` (and no change) when anything is no longer legal.
    pub fn commit(
        &mut self,
        net: &str,
        path: &[(Vec2, Vec2, PcbLayer)],
        moves: &[TraceMove],
    ) -> Result<(Vec<Trace>, Vec<vcad_ir::ecad::Via>), String> {
        if !moves.is_empty() {
            if moves.iter().any(|m| m.index >= self.pcb.traces.len()) {
                return Err("the board changed under that shove".into());
            }
            let trial = self.with_moves(moves);
            if !Self::moves_legal(&trial.session, &trial.pcb, moves) {
                return Err("that shove is no longer legal".into());
            }
            *self = trial;
        }
        let w = self.width_for(net);
        if !self.legal(net, w, path) {
            return Err("that path is no longer legal".into());
        }
        let (t0, v0) = (self.pcb.traces.len(), self.pcb.vias.len());
        let through = self.through;
        commit_path(&mut self.pcb, &mut self.session, net, w, path, through);
        Ok((self.pcb.traces[t0..].to_vec(), self.pcb.vias[v0..].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_board, with_trace};

    #[test]
    fn strip_removes_only_the_named_nets() {
        let mut pcb = test_board();
        with_trace(&mut pcb, "A", 1.0, 1.0, 5.0, 1.0);
        with_trace(&mut pcb, "B", 1.0, 3.0, 5.0, 3.0);
        let nets: BTreeSet<String> = ["A".to_string()].into_iter().collect();
        let (t, _, _) = strip_nets(&mut pcb, &nets);
        assert_eq!(t, 1);
        assert_eq!(pcb.traces.len(), 1);
        assert_eq!(pcb.traces[0].net, "B");
    }

    #[test]
    fn nets_with_copper_sees_traces_and_vias() {
        let mut pcb = test_board();
        with_trace(&mut pcb, "A", 1.0, 1.0, 5.0, 1.0);
        pcb.vias.push(Via {
            via_type: None,
            position: Vec2::new(2.0, 2.0),
            diameter: 0.4,
            drill: 0.2,
            start_layer: vcad_ir::ecad::PcbLayer::FCu,
            end_layer: vcad_ir::ecad::PcbLayer::BCu,
            net: "B".into(),
            source: None,
        });
        let nets = nets_with_copper(&pcb);
        assert!(nets.contains("A") && nets.contains("B"));
    }

    /// A four-layer board, so a FCu..In2Cu barrel has an interior layer that a
    /// two-endpoint model cannot see.
    fn four_layer_board() -> Pcb {
        let mut pcb = test_board();
        pcb.stackup.layers = [
            PcbLayer::FCu,
            PcbLayer::In1Cu,
            PcbLayer::In2Cu,
            PcbLayer::BCu,
        ]
        .into_iter()
        .map(|layer| vcad_ir::ecad::StackupLayer {
            role: None,
            role_inferred: false,
            layer,
            copper_thickness: Some(0.035),
            dielectric_thickness: Some(0.2),
            dielectric_er: Some(4.5),
            material: Some("FR4".into()),
        })
        .collect();
        pcb
    }

    /// A two-step path FCu -> In2Cu through (5,5): one barrel, spanning In1Cu.
    fn transition_path() -> Vec<(Vec2, Vec2, PcbLayer)> {
        vec![
            (Vec2::new(3.0, 5.0), Vec2::new(5.0, 5.0), PcbLayer::FCu),
            (Vec2::new(5.0, 5.0), Vec2::new(7.0, 5.0), PcbLayer::In2Cu),
        ]
    }

    #[test]
    fn barrel_spans_interior_layers() {
        let pcb = four_layer_board();
        assert_eq!(
            spanned_layers(&pcb, PcbLayer::FCu, PcbLayer::In2Cu),
            vec![PcbLayer::FCu, PcbLayer::In1Cu, PcbLayer::In2Cu],
        );
        // Order-insensitive: the router emits (start, end) in path order.
        assert_eq!(
            spanned_layers(&pcb, PcbLayer::In2Cu, PcbLayer::FCu),
            vec![PcbLayer::FCu, PcbLayer::In1Cu, PcbLayer::In2Cu],
        );
    }

    /// A barrel overlapping foreign copper on an INTERIOR layer of its span
    /// must be refused. Committing only the endpoint layers is how two 0.000mm
    /// shorts reached the CM5 fab package: FCu..In2Cu barrels with an In1Cu
    /// trace driven through them.
    #[test]
    fn via_over_interior_layer_copper_is_refused() {
        let mut pcb = four_layer_board();
        let clear = RouteSession::from_pcb(&pcb);
        assert!(vias_legal(&pcb, &clear, "A", &transition_path(), None));

        pcb.traces.push(Trace {
            start: Vec2::new(5.0, 4.0),
            end: Vec2::new(5.0, 6.0),
            width: 0.2,
            layer: PcbLayer::In1Cu,
            net: "VICTIM".into(),
            source: None,
        });
        let session = RouteSession::from_pcb(&pcb);
        assert!(
            !vias_legal(&pcb, &session, "A", &transition_path(), None),
            "a barrel straight through an interior-layer foreign trace must be illegal"
        );
    }

    /// Hole-to-hole is a drill rule: two vias whose copper never shares a layer
    /// still collide in the drill file. The copper probe cannot see it, so the
    /// commit gate has to probe the drill index — the four CM5 hole-to-hole
    /// violations were all copper-legal.
    #[test]
    fn via_too_close_to_an_existing_hole_is_refused() {
        let mut pcb = four_layer_board();
        // Copper-legal by a mile (disjoint layers: In3Cu is not even in the
        // stackup used here), drill-illegal: centres 0.5mm, drills 0.3 → 0.2mm
        // hole-to-hole against a 0.25mm rule.
        pcb.vias.push(Via {
            via_type: None,
            position: Vec2::new(5.5, 5.0),
            diameter: 0.6,
            drill: 0.3,
            start_layer: PcbLayer::In2Cu,
            end_layer: PcbLayer::BCu,
            net: "OTHER".into(),
            source: None,
        });
        let session = RouteSession::from_pcb(&pcb);
        assert!(
            session
                .probe(
                    &CopperGeom::Disc {
                        center: Vec2::new(5.0, 5.0),
                        r: 0.3,
                    },
                    PcbLayer::FCu,
                    "A",
                    0.2,
                )
                .legal
        );
        assert!(
            !vias_legal(&pcb, &session, "A", &transition_path(), None),
            "a barrel 0.20mm hole-to-hole from an existing via must be illegal"
        );
    }

    /// Two barrels of ONE path have to clear each other, which the session
    /// cannot judge until they are committed.
    #[test]
    fn two_barrels_of_one_path_must_clear_each_other() {
        let pcb = four_layer_board();
        let session = RouteSession::from_pcb(&pcb);
        let path = vec![
            (Vec2::new(3.0, 5.0), Vec2::new(5.0, 5.0), PcbLayer::FCu),
            (Vec2::new(5.0, 5.0), Vec2::new(5.4, 5.0), PcbLayer::In2Cu),
            (Vec2::new(5.4, 5.0), Vec2::new(7.0, 5.0), PcbLayer::FCu),
        ];
        assert!(
            !vias_legal(&pcb, &session, "A", &path, None),
            "two barrels 0.10mm hole-to-hole apart must be illegal"
        );
    }

    /// The commit indexes the whole barrel — every spanned layer's copper AND
    /// the drill — so the next path sees it.
    #[test]
    fn commit_indexes_barrel_copper_and_drill() {
        let mut pcb = four_layer_board();
        let mut session = RouteSession::from_pcb(&pcb);
        commit_path(&mut pcb, &mut session, "A", 0.2, &transition_path(), None);
        assert_eq!(pcb.vias.len(), 1);
        assert_eq!(pcb.vias[0].position, Vec2::new(5.0, 5.0));
        // Interior layer now blocked for a foreign net...
        assert!(
            !session
                .probe(
                    &CopperGeom::Segment {
                        a: Vec2::new(5.0, 4.0),
                        b: Vec2::new(5.0, 6.0),
                        half_w: 0.1,
                    },
                    PcbLayer::In1Cu,
                    "VICTIM",
                    0.2,
                )
                .legal
        );
        // ...and the drill is in the hole index.
        assert!(!session.probe_drill(Vec2::new(5.5, 5.0), 0.3).legal);
        // A second path through the same spot is now refused rather than
        // stacking a coincident drill.
        assert!(!vias_legal(&pcb, &session, "B", &transition_path(), None));
    }

    #[test]
    fn interactive_router_previews_commits_and_refuses_a_stale_path() {
        use crate::test_support::{test_board, with_smd_pad};
        let mut pcb = test_board();
        with_smd_pad(&mut pcb, "R1", "1", 2.0, 5.0, "X");
        with_smd_pad(&mut pcb, "R2", "1", 8.0, 5.0, "X");
        let (a, b) = (Vec2::new(2.0, 5.0), Vec2::new(8.0, 5.0));
        let mut r = InteractiveRouter::new(pcb);
        let attempt = r.route("X", a, b, Some(PcbLayer::FCu), 150_000);
        assert_eq!(attempt.status, "routed", "{:?}", attempt.reason);
        assert!(
            attempt.path.iter().all(|s| s.2 == PcbLayer::FCu),
            "stays on the preferred layer"
        );
        let (traces, vias) = r.commit("X", &attempt.path, &[]).expect("commits");
        assert!(!traces.is_empty() && vias.is_empty());
        // Copper now occupies that path: another net can't take it.
        assert!(r.commit("Y", &attempt.path, &[]).is_err());
    }

    #[test]
    fn interactive_router_shoves_a_neighbour_rather_than_failing() {
        use crate::test_support::{test_board, with_smd_pad, with_trace};
        let mut pcb = test_board();
        with_smd_pad(&mut pcb, "R1", "1", 1.0, 5.0, "X");
        with_smd_pad(&mut pcb, "R2", "1", 9.0, 5.0, "X");
        // A hump of net Y with its middle run 0.6 mm under a fixed wall:
        // too narrow for X until the run moves down.
        with_smd_pad(&mut pcb, "Y1", "1", 3.0, 1.5, "Y");
        with_smd_pad(&mut pcb, "Y2", "1", 7.0, 1.5, "Y");
        with_trace(&mut pcb, "Y", 3.0, 1.5, 3.0, 5.4);
        with_trace(&mut pcb, "Y", 3.0, 5.4, 7.0, 5.4);
        with_trace(&mut pcb, "Y", 7.0, 5.4, 7.0, 1.5);
        with_trace(&mut pcb, "Z", 0.3, 6.0, 9.7, 6.0);
        with_trace(&mut pcb, "W", 0.3, 1.0, 9.7, 1.0);
        let (a, b) = (Vec2::new(1.0, 5.0), Vec2::new(9.0, 5.0));
        let mut r = InteractiveRouter::new(pcb);
        assert_ne!(
            r.search("X", a, b, &[PcbLayer::FCu], 150_000).status,
            "routed",
            "the fixture must be blocked without a shove"
        );
        let attempt = r.route("X", a, b, Some(PcbLayer::FCu), 150_000);
        assert_eq!(attempt.status, "routed", "{:?}", attempt.reason);
        assert!(!attempt.moved.is_empty(), "it should have pushed Y aside");
        assert!(
            attempt.path.iter().all(|s| s.2 == PcbLayer::FCu),
            "shove before vias"
        );
        let clearance = |p: &Pcb| {
            vcad_ecad_pcb::drc::check_drc(p)
                .iter()
                .filter(|v| format!("{:?}", v.rule).contains("Clearance"))
                .map(|v| v.message.clone())
                .collect::<BTreeSet<_>>()
        };
        let before = clearance(&r.pcb);
        r.commit("X", &attempt.path, &attempt.moved)
            .expect("commits");
        assert!(r.pcb.traces[1].start.y < 5.4, "the run moved down");
        let added: Vec<_> = clearance(&r.pcb).difference(&before).cloned().collect();
        assert!(added.is_empty(), "the shove added violations: {added:?}");
    }

    #[test]
    fn interactive_routes_are_short_and_simple() {
        use crate::test_support::{four_layer_board, with_smd_pad, with_trace};
        let len = |p: &[(Vec2, Vec2, PcbLayer)]| {
            p.iter()
                .map(|s| ((s.1.x - s.0.x).powi(2) + (s.1.y - s.0.y).powi(2)).sqrt())
                .sum::<f64>()
        };
        // Open board: one straight segment.
        let mut pcb = four_layer_board(40.0, 40.0);
        with_smd_pad(&mut pcb, "R1", "1", 5.0, 20.0, "X");
        with_smd_pad(&mut pcb, "R2", "1", 35.0, 20.0, "X");
        let (a, b) = (Vec2::new(5.0, 20.0), Vec2::new(35.0, 20.0));
        let r = InteractiveRouter::new(pcb.clone());
        let open = r.route("X", a, b, Some(PcbLayer::FCu), 150_000);
        assert_eq!(open.status, "routed");
        assert_eq!(open.path.len(), 1, "{:?}", open.path);
        // A wall with a gap off to one side: near the geometric optimum
        // (out to the gap's end and back), with few bends.
        with_trace(&mut pcb, "W", 20.0, 12.0, 20.0, 34.0);
        let r = InteractiveRouter::new(pcb);
        let around = r.route("X", a, b, Some(PcbLayer::FCu), 150_000);
        assert_eq!(around.status, "routed");
        assert!(around.vias.is_empty(), "no via needed");
        let best = 2.0 * (15.0_f64.powi(2) + 8.5_f64.powi(2)).sqrt();
        assert!(
            len(&around.path) < best * 1.15,
            "{} vs {best}",
            len(&around.path)
        );
        assert!(around.path.len() <= 8, "{} segments", around.path.len());
    }
}
