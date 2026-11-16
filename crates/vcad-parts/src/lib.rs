//! Built-in parametric parts (stdlib) for vcad.
//!
//! Each part is a Rust module exporting a [`PartMetadata`] constant and a
//! `build` function. The registry is assembled at compile time; the engine
//! resolves `std:*` paths in [`vcad_ir::CsgOp::PartInstance`] by calling
//! [`build_part`].
//!
//! New parts live under `src/<category>/<slug>.rs` and are registered in
//! [`all_parts`].

#![warn(missing_docs)]

use std::collections::HashMap;

pub mod builder;
pub mod types;

pub mod bearings;
pub mod fasteners;
pub mod vacuum;

pub use builder::Builder;
pub use types::{
    BuildFn, InfoFn, Param, Params, PartEntry, PartInfo, PartMetadata, PortInfo, Xref,
};

use vcad_ir::{CsgOp, Document, Node, NodeId};

/// All registered built-in parts.
pub fn all_parts() -> &'static [PartEntry] {
    &[
        fasteners::washer_flat::ENTRY,
        fasteners::bolt_socket_head::ENTRY,
        fasteners::nut_hex::ENTRY,
        bearings::bearing_608::ENTRY,
        vacuum::flanges::CF_FLANGE,
        vacuum::flanges::CF_GASKET,
        vacuum::flanges::CF_BOLT_SET,
        vacuum::flanges::CF_REDUCER,
        vacuum::flanges::KF_FLANGE,
        vacuum::flanges::KF_CENTERING_RING,
        vacuum::flanges::KF_CLAMP,
        vacuum::fittings::NIPPLE,
        vacuum::fittings::TEE,
        vacuum::fittings::CROSS,
        vacuum::fittings::ADAPTER_CF_KF,
        vacuum::chambers::SPHERICAL_CUBE,
        vacuum::chambers::SPHERICAL_OCTAGON,
        vacuum::instruments::VIEWPORT,
        vacuum::instruments::HV_FEEDTHROUGH,
        vacuum::instruments::ELECTRICAL_FEEDTHROUGH,
        vacuum::instruments::NEEDLE_VALVE,
        vacuum::instruments::GAUGE,
    ]
}

/// Look up a part by its `std:` path.
pub fn find_part(path: &str) -> Option<&'static PartEntry> {
    let stripped = path.strip_prefix("std:").unwrap_or(path);
    all_parts().iter().find(|e| e.meta.id == stripped)
}

/// Build a part's geometry given its path and params.
///
/// Returns the full sub-document the part produces. The caller is expected to
/// splice this into a parent document and keep the [`vcad_ir::CsgOp::PartInstance`]
/// node around as the edit surface.
pub fn build_part(
    path: &str,
    params: &HashMap<String, serde_json::Value>,
) -> Result<Document, String> {
    let entry = find_part(path).ok_or_else(|| format!("unknown part: {path}"))?;
    let p = Params::new(entry.meta.params, params);
    (entry.build)(&p)
}

/// Describe a part: mate ports (part-local) and metadata, for parts that
/// declare an [`InfoFn`]. Parts without one return no ports and `{}`.
pub fn part_info(
    path: &str,
    params: &HashMap<String, serde_json::Value>,
) -> Result<PartInfo, String> {
    let entry = find_part(path).ok_or_else(|| format!("unknown part: {path}"))?;
    let p = Params::new(entry.meta.params, params);
    match entry.info {
        Some(f) => f(&p),
        None => Ok(PartInfo {
            ports: Vec::new(),
            metadata: serde_json::json!({}),
        }),
    }
}

/// Copy `sub`'s nodes into `nodes` under fresh ids drawn from `alloc`, and
/// return the id standing for `sub`'s scene (its first root, or a union of
/// all roots). Errors if `sub` has no root.
pub fn splice(
    nodes: &mut HashMap<NodeId, Node>,
    sub: &Document,
    alloc: &mut dyn FnMut() -> NodeId,
) -> Result<NodeId, String> {
    if sub.roots.is_empty() {
        return Err("part produced no scene root".into());
    }
    let mut ids: Vec<NodeId> = sub.nodes.keys().copied().collect();
    ids.sort_unstable();
    let map: HashMap<NodeId, NodeId> = ids.iter().map(|&id| (id, alloc())).collect();
    for id in &ids {
        let mut node = sub.nodes[id].clone();
        for c in node.op.child_ids_mut() {
            if let Some(&n) = map.get(c) {
                *c = n;
            }
        }
        node.id = map[id];
        nodes.insert(node.id, node);
    }
    let mut root = *map
        .get(&sub.roots[0].root)
        .ok_or("part root is not one of its nodes")?;
    for r in &sub.roots[1..] {
        let right = *map
            .get(&r.root)
            .ok_or("part root is not one of its nodes")?;
        let id = alloc();
        nodes.insert(
            id,
            Node {
                id,
                name: None,
                op: CsgOp::Union { left: root, right },
            },
        );
        root = id;
    }
    Ok(root)
}

/// Expand every `std:` [`CsgOp::PartInstance`] in `doc` in place of the
/// engine's TypeScript expansion, so Rust-side evaluators (vcad.app, the
/// CLI, vcad-render) see geometry. Each instance becomes a zero `Translate`
/// over its spliced sub-graph, keeping its node id (and therefore every
/// reference to it). Returns `None` when there is nothing to expand. A part
/// that fails to build becomes `Empty` — mirroring the engine — and its
/// error is returned alongside, keyed by the instance's node id.
pub fn expand_part_instances(doc: &Document) -> Option<(Document, Vec<(NodeId, String)>)> {
    let mut targets: Vec<NodeId> = doc
        .nodes
        .iter()
        .filter(
            |(_, n)| matches!(&n.op, CsgOp::PartInstance { path, .. } if !path.starts_with('@')),
        )
        .map(|(&id, _)| id)
        .collect();
    // Deterministic order: spliced ids must not shuffle between evaluations
    // (the incremental cache keys on subtree content).
    targets.sort_unstable();
    if targets.is_empty() {
        return None;
    }
    let mut out = doc.clone();
    let mut next = out.nodes.keys().copied().max().unwrap_or(0) + 1;
    let mut errors = Vec::new();
    for id in targets {
        let CsgOp::PartInstance { path, params, .. } = out.nodes[&id].op.clone() else {
            continue;
        };
        let built = build_part(&path, &params).and_then(|sub| {
            let mut alloc = || {
                let n = next;
                next += 1;
                n
            };
            splice(&mut out.nodes, &sub, &mut alloc)
        });
        let op = match built {
            Ok(child) => CsgOp::Translate {
                child,
                offset: vcad_ir::Vec3::new(0.0, 0.0, 0.0),
            },
            Err(e) => {
                errors.push((id, format!("{path}: {e}")));
                CsgOp::Empty
            }
        };
        if let Some(n) = out.nodes.get_mut(&id) {
            n.op = op;
        }
    }
    Some((out, errors))
}

/// Emit the manifest JSON the app uses for palette + Cmd+K indexing.
pub fn manifest_json() -> String {
    let entries: Vec<_> = all_parts().iter().map(|e| e.manifest_entry()).collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}
