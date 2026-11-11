//! Island-to-island connections: what is still unrouted on a board, stated
//! the way KiCad and every serious router state it.
//!
//! A net's existing copper — pads, traces, vias, pours, joined by the same
//! galvanic connectivity the DRC uses ([`net_copper_islands`]) — falls into
//! islands. The remaining work is a minimum spanning set over those ISLANDS
//! (closest-point distance between islands), not over pads, and each
//! connection may start anywhere on its source island and end anywhere on its
//! target island ([`IslandTerminals`]).
//!
//! This is what makes hand-placed fan-out work: a fine-pitch pad walled in by
//! its neighbours, with a fixed stub out to a via in open space, is ONE island
//! with the via — so the router starts from the free via instead of proving
//! the buried pad unreachable.

use std::collections::BTreeSet;

use vcad_ir::ecad::{Pcb, PcbLayer};
use vcad_ir::Vec2;

use super::complete::{closest_on_segment, Anchor, IslandTerminals};
use crate::drc::{net_copper_islands, CopperIsland};

/// One unrouted island-to-island connection.
#[derive(Debug, Clone)]
pub struct IslandConnection {
    /// The net.
    pub net: String,
    /// Closest point of the `from` island to the `to` island — the
    /// representative terminal (window placement, certificates).
    pub from: Vec2,
    /// Closest point of the `to` island to the `from` island.
    pub to: Vec2,
    /// Every piece of copper of both islands, as attachable anchors.
    pub terminals: IslandTerminals,
}

/// Every connection still needed to join each net's copper islands, nets in
/// name order. Islands holding no pad (dead copper the prune will remove) are
/// not terminals; nets in `skip` (plane nets, joined through their pour) are
/// left out. `layers` restricts which copper the anchors may attach on.
///
/// Deterministic: islands are ordered by their first pad in board order, and
/// the spanning set is Prim's from island 0 with strict `<` tie-breaking.
pub fn island_connections(
    pcb: &Pcb,
    layers: &[PcbLayer],
    skip: &BTreeSet<&str>,
) -> Vec<IslandConnection> {
    let mut pads_per_net: std::collections::BTreeMap<&str, usize> = Default::default();
    for fp in &pcb.footprints {
        for pad in &fp.pads {
            if let Some(net) = pad.net.as_deref().filter(|n| !n.is_empty()) {
                *pads_per_net.entry(net).or_default() += 1;
            }
        }
    }
    let board_order: std::collections::HashMap<(String, String), usize> = pcb
        .footprints
        .iter()
        .flat_map(|fp| {
            fp.pads
                .iter()
                .map(move |p| (fp.reference.clone(), p.number.clone()))
        })
        .enumerate()
        .map(|(i, k)| (k, i))
        .collect();
    let pad_key = |p: &crate::drc::IslandPad| (p.reference.clone(), p.number.clone());
    let mut out = Vec::new();
    for (net, n_pads) in pads_per_net {
        if n_pads < 2 || skip.contains(net) {
            continue;
        }
        let mut islands: Vec<CopperIsland> = net_copper_islands(pcb, net)
            .into_iter()
            .filter(|i| !i.pads.is_empty())
            .collect();
        if islands.len() < 2 {
            continue;
        }
        // Board order of each island's first pad (pads already arrive in board
        // order): the same node order the pad-level ratsnest used, so a board
        // with no copper yet gets exactly the connections it always did.
        islands.sort_by_key(|i| board_order.get(&pad_key(&i.pads[0])).copied());
        let prims: Vec<Vec<(Vec2, Vec2)>> = islands.iter().map(primitives).collect();
        for (p, c) in mst(&prims) {
            let (from, to, _) = island_gap(&prims[p], &prims[c]);
            out.push(IslandConnection {
                net: net.to_string(),
                from,
                to,
                terminals: IslandTerminals {
                    from: anchors(&islands[p], layers),
                    to: anchors(&islands[c], layers),
                    from_label: label(net, &islands[p]),
                    to_label: label(net, &islands[c]),
                },
            });
        }
    }
    out
}

/// `"island of net X at U1.12 (1 pad, 1 trace, 1 via)"`.
pub fn label(net: &str, isl: &CopperIsland) -> String {
    let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
    let mut parts = vec![n(isl.pads.len(), "pad", "pads")];
    if !isl.traces.is_empty() {
        parts.push(n(isl.traces.len(), "trace", "traces"));
    }
    if !isl.vias.is_empty() {
        parts.push(n(isl.vias.len(), "via", "vias"));
    }
    if isl.pours > 0 {
        parts.push(n(isl.pours, "pour", "pours"));
    }
    let at = isl
        .pads
        .first()
        .map(|p| format!(" at {}.{}", p.reference, p.number))
        .unwrap_or_default();
    format!("island of net {net}{at} ({})", parts.join(", "))
}

/// The island's copper as router anchors on `layers`: pads on their own
/// layers, vias on every layer they span, traces along their length.
pub fn anchors(isl: &CopperIsland, layers: &[PcbLayer]) -> Vec<Anchor> {
    let mut out = Vec::new();
    for pad in &isl.pads {
        let on: Vec<PcbLayer> = layers
            .iter()
            .copied()
            .filter(|l| pad.layers.contains(l))
            .collect();
        if !on.is_empty() {
            out.push(Anchor::Point {
                at: pad.position,
                layers: on,
            });
        }
    }
    for v in &isl.vias {
        let on: Vec<PcbLayer> = layers
            .iter()
            .copied()
            .filter(|l| l.spanned_by(v.start_layer, v.end_layer))
            .collect();
        if !on.is_empty() {
            out.push(Anchor::Point {
                at: v.position,
                layers: on,
            });
        }
    }
    for t in &isl.traces {
        if layers.contains(&t.layer) {
            out.push(Anchor::Segment {
                a: t.start,
                b: t.end,
                layer: t.layer,
            });
        }
    }
    out
}

/// The island's copper as planar segments (points are degenerate segments),
/// for closest-point distances.
fn primitives(isl: &CopperIsland) -> Vec<(Vec2, Vec2)> {
    isl.pads
        .iter()
        .map(|p| (p.position, p.position))
        .chain(isl.vias.iter().map(|v| (v.position, v.position)))
        .chain(isl.traces.iter().map(|t| (t.start, t.end)))
        .collect()
}

/// Closest points between two islands and their distance.
fn island_gap(a: &[(Vec2, Vec2)], b: &[(Vec2, Vec2)]) -> (Vec2, Vec2, f64) {
    let mut best = (a[0].0, b[0].0, f64::INFINITY);
    for &(a0, a1) in a {
        for &(b0, b1) in b {
            let (p, q) = segment_closest(a0, a1, b0, b1);
            let d = (p.x - q.x).hypot(p.y - q.y);
            if d < best.2 {
                best = (p, q, d);
            }
        }
    }
    best
}

/// Closest points between segments `a0a1` and `b0b1` (either may be a point).
fn segment_closest(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> (Vec2, Vec2) {
    // Proper crossing: the intersection point is on both.
    let (r, s) = (
        Vec2::new(a1.x - a0.x, a1.y - a0.y),
        Vec2::new(b1.x - b0.x, b1.y - b0.y),
    );
    let den = r.x * s.y - r.y * s.x;
    if den.abs() > 1e-12 {
        let (qx, qy) = (b0.x - a0.x, b0.y - a0.y);
        let t = (qx * s.y - qy * s.x) / den;
        let u = (qx * r.y - qy * r.x) / den;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            let p = Vec2::new(a0.x + t * r.x, a0.y + t * r.y);
            return (p, p);
        }
    }
    // Otherwise the minimum is at an endpoint of one of them.
    let cands = [
        (a0, closest_on_segment(a0, b0, b1)),
        (a1, closest_on_segment(a1, b0, b1)),
        (closest_on_segment(b0, a0, a1), b0),
        (closest_on_segment(b1, a0, a1), b1),
    ];
    let d = |(p, q): &(Vec2, Vec2)| (p.x - q.x).hypot(p.y - q.y);
    let mut best = cands[0];
    for c in &cands[1..] {
        if d(c) < d(&best) {
            best = *c;
        }
    }
    best
}

/// Prim's minimum spanning tree over islands by closest-point gap, from
/// island 0, strict `<` tie-breaking. Returns `(parent, child)` pairs.
fn mst(prims: &[Vec<(Vec2, Vec2)>]) -> Vec<(usize, usize)> {
    let n = prims.len();
    let mut in_tree = vec![false; n];
    let mut best = vec![f64::INFINITY; n];
    let mut parent = vec![0usize; n];
    in_tree[0] = true;
    for j in 1..n {
        best[j] = island_gap(&prims[0], &prims[j]).2;
    }
    let mut edges = Vec::with_capacity(n.saturating_sub(1));
    for _ in 1..n {
        let mut u = usize::MAX;
        let mut bd = f64::INFINITY;
        for j in 0..n {
            if !in_tree[j] && best[j] < bd {
                bd = best[j];
                u = j;
            }
        }
        if u == usize::MAX {
            break;
        }
        in_tree[u] = true;
        edges.push((parent[u], u));
        for j in 0..n {
            if !in_tree[j] {
                let d = island_gap(&prims[u], &prims[j]).2;
                if d < best[j] {
                    best[j] = d;
                    parent[j] = u;
                }
            }
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_segments_meet_at_the_crossing() {
        let (p, q) = segment_closest(
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 2.0),
            Vec2::new(0.0, 2.0),
            Vec2::new(2.0, 0.0),
        );
        assert!((p.x - 1.0).abs() < 1e-9 && (p.y - 1.0).abs() < 1e-9);
        assert_eq!(p, q);
    }

    #[test]
    fn point_to_segment_gap_is_perpendicular() {
        let (p, q) = segment_closest(
            Vec2::new(1.0, 3.0),
            Vec2::new(1.0, 3.0),
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
        );
        assert_eq!(p, Vec2::new(1.0, 3.0));
        assert!((q.x - 1.0).abs() < 1e-9 && q.y.abs() < 1e-9);
    }
}
