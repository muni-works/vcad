//! V1 file format migration.
//!
//! Converts legacy `.vcad` JSON documents (v1 format: IR `Document` with parts
//! array) into the new CRDT document format. Features are reconstructed by
//! walking the existing nodes and emitting CreateFeature + SetParam ops.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use vcad_crdt::{CrdtDocument, FeatureId, FractionalIndex, ReplicaId, Value};
use vcad_ir::{CsgOp, Document, JointKind, NodeId, Vec3};

/// Which node of a materialized feature's transform chain an old v1 node
/// corresponds to.
///
/// Migration collapses a `Translate → Rotate → Scale → primitive` chain into
/// one CRDT feature, and the materializer expands it again into four fresh
/// nodes with new ids. A v1 binding points at one specific node in the old
/// chain, so remapping it needs to know *which* — a `radius` binding belongs
/// on the primitive, an `offset` binding on the translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// The leaf: primitive, boolean, fillet — whatever the feature is *of*.
    Primitive,
    /// The scale wrapper.
    Scale,
    /// The rotation wrapper.
    Rotate,
    /// The translation wrapper (the feature's root node).
    Translate,
}

/// One old-node → (feature, role) correspondence produced by migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeMapping {
    /// Node id in the v1 document.
    pub old_node_id: NodeId,
    /// Serialized `FeatureId` (`"replica:seq"`), matching `PartInfo::id`.
    pub feature_id: String,
    /// Which node of the rebuilt chain this old node became.
    pub role: NodeRole,
}

/// The full old-node → feature correspondence for one migration.
///
/// Migration renumbers every node. Anything that referenced v1 node ids from
/// *outside* the document — parameter bindings, most importantly — is dangling
/// after a load unless it is remapped through this. Without it a binding like
/// `"2:radius"` silently lands on whatever node 2 happens to be in the rebuilt
/// document (in practice a `Scale` wrapper), and the document fails to
/// evaluate with a confusing "field path not valid on op Scale".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationMap {
    /// Every old node that has a correspondence. Nodes whose op has no CRDT
    /// feature representation are absent.
    pub mappings: Vec<NodeMapping>,
}

/// Part-def param: its source feature exists only to define the part, and is
/// not a scene root.
pub(crate) const PART_DEF_SOURCE_CONSUMED: &str = "source_consumed";

/// Migrate a v1 document (IR DAG + parts metadata) into a CRDT document.
///
/// Each scene entry's root node tree is converted recursively: primitives
/// become leaf features, boolean/modifier operations become features that
/// reference previously-migrated child features by id. Assembly metadata
/// (partDefs / instances / joints) migrates to its own feature kinds after
/// the regular scene roots.
pub fn migrate_v1(doc: &Document) -> CrdtDocument {
    migrate_v1_mapped(doc).0
}

/// [`migrate_v1`], also returning the old-node → feature correspondence.
///
/// Callers holding v1 node references outside the document (parameter
/// bindings) must remap them through the returned [`MigrationMap`], composed
/// with the materializer's per-feature node ids from `PartInfo`.
pub fn migrate_v1_mapped(doc: &Document) -> (CrdtDocument, MigrationMap) {
    let mut crdt = CrdtDocument::new(ReplicaId(0));
    let mut ctx = MigrationCtx::new();

    // Root feature → the stable id the materializer stamped on it, if any.
    let mut wanted: Vec<(FeatureId, FeatureId)> = Vec::new();
    // Feature history first: features in their saved order, including ones
    // that aren't built (no scene root reaches them), then their flags.
    if let Some(history) = &doc.history {
        for &node in &history.order {
            let fid = migrate_node(&mut crdt, &mut ctx, doc, node);
            let claim = doc
                .part_ids
                .as_ref()
                .and_then(|m| m.get(&node.to_string()))
                .and_then(|s| parse_fid(s));
            if let (Some(fid), Some(claim)) = (fid, claim) {
                if !wanted.iter().any(|(f, c)| *f == fid || *c == claim) {
                    wanted.push((fid, claim));
                }
            }
        }
        let flag = |crdt: &mut CrdtDocument, ctx: &MigrationCtx, node: &NodeId, key: &str| {
            if let Some(fid) = ctx.node_to_feature.get(node).copied() {
                crdt.set_param(fid, key, Value::Bool(true));
            }
        };
        for node in &history.suppressed {
            flag(&mut crdt, &ctx, node, crate::materializer::SUPPRESSED);
        }
        if let Some(node) = &history.rollback {
            flag(&mut crdt, &ctx, node, crate::materializer::ROLLBACK_MARKER);
        }
    }
    for entry in &doc.roots {
        let fid = migrate_node(&mut crdt, &mut ctx, doc, entry.root);
        // The scene entry's own material/visibility live on the part feature
        // (the materializer reads them back from there). Dropping them reset
        // every imported part to "default" on each IR round trip.
        if let Some(fid) = fid {
            if entry.material != "default" {
                crdt.set_param(fid, "material", Value::String(entry.material.clone()));
            }
            if entry.visible == Some(false) {
                crdt.set_param(fid, "visible", Value::Bool(false));
            }
        }
        let claim = doc
            .part_ids
            .as_ref()
            .and_then(|m| m.get(&entry.root.to_string()))
            .and_then(|s| parse_fid(s));
        if let (Some(fid), Some(claim)) = (fid, claim) {
            if !wanted.iter().any(|(f, c)| *f == fid || *c == claim) {
                wanted.push((fid, claim));
            }
        }
    }

    migrate_assembly(&mut crdt, &mut ctx, doc);
    migrate_orphan_boards(&mut crdt, &mut ctx, doc);

    // Atomic / molecular domain: one `molecule` feature carrying the whole
    // system as a JSON blob (mirrors how schematic/scene data round-trips).
    if let Some(mol) = &doc.molecule {
        if let Ok(json) = serde_json::to_string(mol) {
            let mut params = HashMap::new();
            params.insert("system".to_string(), Value::String(json));
            create(&mut crdt, &mut ctx, "molecule", params);
        }
    }

    // Animation timeline: singleton JSON-blob feature (same pattern), so
    // deep-linked / imported docs keep their motion through the CRDT.
    if let Some(tl) = &doc.timeline {
        if let Ok(json) = serde_json::to_string(tl) {
            let mut params = HashMap::new();
            params.insert("timeline".to_string(), Value::String(json));
            create(&mut crdt, &mut ctx, "timeline", params);
        }
    }

    migrate_doc_extras(&mut crdt, &mut ctx, doc);

    if wanted.iter().any(|(f, c)| f != c) {
        let remap = assign_ids(&crdt, &wanted);
        crdt = rekey(&crdt, &remap);
        for m in &mut ctx.node_roles {
            if let Some(to) = parse_fid(&m.feature_id).and_then(|f| remap.get(&f)) {
                m.feature_id = fid_to_stable(*to);
            }
        }
    }

    let map = MigrationMap {
        mappings: std::mem::take(&mut ctx.node_roles),
    };
    store_feature_bindings(&mut crdt, doc, &map);
    (crdt, map)
}

/// Outcome of remapping one document's parameter bindings across a
/// migration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemappedBindings {
    /// Binding keys rewritten to post-migration node ids.
    pub bindings: HashMap<String, String>,
    /// Keys that could not be remapped, with the reason. These are
    /// **dropped** rather than carried forward: a binding pointing at a node
    /// that no longer exists (or is now a different op) fails the whole
    /// document evaluation, which costs the user every other binding too.
    pub dropped: Vec<String>,
}

/// Which node of a feature's rebuilt chain a field path belongs on.
///
/// Transform fields live on the wrappers the materializer creates; anything
/// else belongs on the leaf. Bindings are authored against v1 nodes, so the
/// *field* is what tells us the role — the old node id alone cannot, since a
/// whole `Translate → … → primitive` chain collapses into one feature.
fn role_for_field(field_path: &str) -> NodeRole {
    let head = field_path.split('.').next().unwrap_or(field_path);
    match head {
        "offset" => NodeRole::Translate,
        "angles" | "rotation" => NodeRole::Rotate,
        "factor" | "scale" => NodeRole::Scale,
        _ => NodeRole::Primitive,
    }
}

/// Read one role's node id out of a serialized `PartInfo`.
///
/// Goes through JSON rather than a 20-variant match per role: every variant
/// names these fields identically in camelCase, and a variant that genuinely
/// lacks one (a feature with no transform chain) correctly yields `None`.
/// This runs once per document load, not per frame.
fn role_node_id(part: &serde_json::Value, role: NodeRole) -> Option<NodeId> {
    let key = match role {
        NodeRole::Primitive => "primitiveNodeId",
        NodeRole::Scale => "scaleNodeId",
        NodeRole::Rotate => "rotateNodeId",
        NodeRole::Translate => "translateNodeId",
    };
    part.get(key)?.as_u64()
}

/// Rewrite v1 parameter-binding keys (`"<nodeId>:<fieldPath>"`) onto the node
/// ids the materializer produced after migration.
///
/// Bindings live outside the CRDT schema, so they survive a load as raw
/// v1 node references while the document around them is renumbered. Left
/// alone they point at arbitrary nodes — the reported symptom was a
/// `"2:radius"` binding landing on a `Scale` wrapper and taking the whole
/// document's evaluation down with it.
///
/// `parts_json` is the materializer's `Vec<PartInfo>` as JSON, which carries
/// each feature's four node ids alongside its feature id.
pub fn remap_bindings(
    bindings: &HashMap<String, String>,
    map: &MigrationMap,
    parts_json: &serde_json::Value,
) -> RemappedBindings {
    // feature id -> its serialized PartInfo
    let mut by_feature: HashMap<&str, &serde_json::Value> = HashMap::new();
    if let Some(arr) = parts_json.as_array() {
        for p in arr {
            if let Some(id) = p.get("id").and_then(|v| v.as_str()) {
                by_feature.insert(id, p);
            }
        }
    }
    // old node id -> feature id (a node appears once; later wins, harmless)
    let mut old_to_feature: HashMap<NodeId, &str> = HashMap::new();
    for m in &map.mappings {
        old_to_feature.insert(m.old_node_id, m.feature_id.as_str());
    }

    let mut out = RemappedBindings::default();
    for (key, expr) in bindings {
        let Some((node_str, field)) = key.split_once(':') else {
            out.dropped.push(format!("{key}: malformed binding key"));
            continue;
        };
        let Ok(old_id) = node_str.parse::<NodeId>() else {
            out.dropped.push(format!("{key}: node id is not a number"));
            continue;
        };
        let Some(fid) = old_to_feature.get(&old_id) else {
            out.dropped.push(format!(
                "{key}: node {old_id} did not survive migration (its op has no feature)"
            ));
            continue;
        };
        let Some(part) = by_feature.get(fid) else {
            out.dropped
                .push(format!("{key}: feature {fid} was not materialized"));
            continue;
        };
        let role = role_for_field(field);
        let Some(new_id) = role_node_id(part, role) else {
            out.dropped.push(format!(
                "{key}: feature {fid} has no {role:?} node to carry field {field:?}"
            ));
            continue;
        };
        out.bindings
            .insert(format!("{new_id}:{field}"), expr.clone());
    }
    out
}

/// Tracks the position cursor so each new feature appends at the end of
/// the ordered feature list, plus a memoization map so a subtree that
/// appears under multiple v1 roots (e.g. the pre-fillet extrude and the
/// fillet itself both rooted) migrates to a single CRDT feature.
struct MigrationCtx {
    last_pos: Option<FractionalIndex>,
    node_to_feature: HashMap<NodeId, FeatureId>,
    /// Every old node that became part of a feature, with the role it plays
    /// in the rebuilt transform chain. Drives [`MigrationMap`].
    node_roles: Vec<NodeMapping>,
}

impl MigrationCtx {
    fn new() -> Self {
        Self {
            last_pos: None,
            node_to_feature: HashMap::new(),
            node_roles: Vec::new(),
        }
    }

    /// Record that `old` became the `role` node of `fid`.
    fn record_role(&mut self, old: NodeId, fid: FeatureId, role: NodeRole) {
        self.node_roles.push(NodeMapping {
            old_node_id: old,
            feature_id: format!("{}:{}", fid.0 .0, fid.1),
            role,
        });
    }

    /// Allocate the next ordered position (strictly greater than all prior).
    fn next_position(&mut self) -> FractionalIndex {
        let pos = FractionalIndex::between(self.last_pos.as_ref(), None);
        self.last_pos = Some(pos.clone());
        pos
    }
}

/// Recursively migrate a subtree rooted at `node_id` into one or more
/// CRDT features. Returns the `FeatureId` of the topmost feature created
/// for this subtree (the one callers should reference as an input).
fn migrate_node(
    crdt: &mut CrdtDocument,
    ctx: &mut MigrationCtx,
    doc: &Document,
    node_id: NodeId,
) -> Option<FeatureId> {
    if let Some(fid) = ctx.node_to_feature.get(&node_id).copied() {
        return Some(fid);
    }
    let top_id = node_id;
    let mut params: HashMap<String, Value> = HashMap::new();
    let mut current_id = node_id;
    // Old nodes consumed by the transform peel, with the role each played.
    // Recorded against the feature id once we know it.
    let mut chain: Vec<(NodeId, NodeRole)> = Vec::new();

    // Peel off the Translate → Rotate → Scale chain, accumulating
    // offset/rotation/scale/name params on the resulting feature.
    loop {
        let node = doc.nodes.get(&current_id)?;
        match &node.op {
            CsgOp::Translate { child, offset } => {
                if *offset != Vec3::new(0.0, 0.0, 0.0) {
                    params.insert(
                        "offset".to_string(),
                        Value::Vec3([offset.x, offset.y, offset.z]),
                    );
                }
                if let Some(name) = &node.name {
                    params.insert("name".to_string(), Value::String(name.clone()));
                }
                chain.push((current_id, NodeRole::Translate));
                current_id = *child;
            }
            CsgOp::Rotate { child, angles } => {
                if *angles != Vec3::new(0.0, 0.0, 0.0) {
                    params.insert(
                        "rotation".to_string(),
                        Value::Vec3([angles.x, angles.y, angles.z]),
                    );
                }
                chain.push((current_id, NodeRole::Rotate));
                current_id = *child;
            }
            CsgOp::Scale { child, factor } => {
                if *factor != Vec3::new(1.0, 1.0, 1.0) {
                    params.insert(
                        "scale".to_string(),
                        Value::Vec3([factor.x, factor.y, factor.z]),
                    );
                }
                chain.push((current_id, NodeRole::Scale));
                current_id = *child;
            }
            _ => break,
        }
    }
    // The node the peel stopped on is the feature's leaf.
    chain.push((current_id, NodeRole::Primitive));

    let leaf = doc.nodes.get(&current_id)?;
    // The materializer writes feature names onto the core/leaf node
    // (primitive, boolean, fillet, …), not the transform wrappers. Pick up
    // the name here if the Translate/Rotate/Scale walk above didn't already.
    if !params.contains_key("name") {
        if let Some(name) = &leaf.name {
            params.insert("name".to_string(), Value::String(name.clone()));
        }
    }
    // A sketch-based op over an attached sketch (`SketchOnFace`) can't be a
    // typed feature: those embed their sketch's op as JSON, and a
    // SketchOnFace op is only node references that materialize renumbers
    // out from under it. Keep the subgraph verbatim instead (the
    // materializer renumbers it consistently, face names included).
    let attached = |id: &NodeId| {
        matches!(
            doc.nodes.get(id).map(|n| &n.op),
            Some(CsgOp::SketchOnFace { .. })
        )
    };
    let over_attached_sketch = match &leaf.op {
        CsgOp::Extrude { sketch, .. }
        | CsgOp::Revolve { sketch, .. }
        | CsgOp::Sweep { sketch, .. } => attached(sketch),
        CsgOp::Loft { sketches, .. } => sketches.iter().any(attached),
        _ => false,
    };
    if over_attached_sketch {
        let fid = create_subgraph(crdt, ctx, doc, top_id);
        ctx.node_to_feature.insert(top_id, fid);
        return Some(fid);
    }
    let fid = match &leaf.op {
        CsgOp::Cube { size } => {
            params.insert("size_x".to_string(), Value::F64(size.x));
            params.insert("size_y".to_string(), Value::F64(size.y));
            params.insert("size_z".to_string(), Value::F64(size.z));
            Some(create(crdt, ctx, "cube", params))
        }
        CsgOp::Cylinder {
            radius,
            height,
            segments,
        } => {
            params.insert("radius".to_string(), Value::F64(*radius));
            params.insert("height".to_string(), Value::F64(*height));
            params.insert("segments".to_string(), Value::F64(*segments as f64));
            Some(create(crdt, ctx, "cylinder", params))
        }
        CsgOp::Sphere { radius, segments } => {
            params.insert("radius".to_string(), Value::F64(*radius));
            params.insert("segments".to_string(), Value::F64(*segments as f64));
            Some(create(crdt, ctx, "sphere", params))
        }
        CsgOp::Cone {
            radius_bottom,
            radius_top,
            height,
            segments,
        } => {
            params.insert("radius_bottom".to_string(), Value::F64(*radius_bottom));
            params.insert("radius_top".to_string(), Value::F64(*radius_top));
            params.insert("height".to_string(), Value::F64(*height));
            params.insert("segments".to_string(), Value::F64(*segments as f64));
            Some(create(crdt, ctx, "cone", params))
        }
        CsgOp::Torus {
            major_radius,
            minor_radius,
            segments,
        } => {
            params.insert("major_radius".to_string(), Value::F64(*major_radius));
            params.insert("minor_radius".to_string(), Value::F64(*minor_radius));
            params.insert("segments".to_string(), Value::F64(*segments as f64));
            Some(create(crdt, ctx, "torus", params))
        }
        CsgOp::Wedge { size } => {
            params.insert("size_x".to_string(), Value::F64(size.x));
            params.insert("size_y".to_string(), Value::F64(size.y));
            params.insert("size_z".to_string(), Value::F64(size.z));
            Some(create(crdt, ctx, "wedge", params))
        }
        CsgOp::Prism {
            sides,
            radius,
            height,
        } => {
            params.insert("sides".to_string(), Value::F64(*sides as f64));
            params.insert("radius".to_string(), Value::F64(*radius));
            params.insert("height".to_string(), Value::F64(*height));
            Some(create(crdt, ctx, "prism", params))
        }
        CsgOp::Hull { children, points } => {
            let mut inputs = Vec::with_capacity(children.len());
            for child in children {
                inputs.push(fid_to_stable(migrate_node(crdt, ctx, doc, *child)?));
            }
            params.insert("inputs".to_string(), Value::FeatureRefList(inputs));
            params.insert(
                "points_json".to_string(),
                Value::String(serde_json::to_string(points).ok()?),
            );
            Some(create(crdt, ctx, "hull", params))
        }
        CsgOp::SweepRotate {
            child,
            axis_origin,
            axis_dir,
            angle_lo_deg,
            angle_hi_deg,
            step_deg,
            clearance,
        } => {
            let input = migrate_node(crdt, ctx, doc, *child)?;
            params.insert("input".to_string(), Value::FeatureRef(fid_to_stable(input)));
            params.insert(
                "axis_origin".to_string(),
                Value::Vec3([axis_origin.x, axis_origin.y, axis_origin.z]),
            );
            params.insert(
                "axis_dir".to_string(),
                Value::Vec3([axis_dir.x, axis_dir.y, axis_dir.z]),
            );
            params.insert("angle_lo_deg".to_string(), Value::F64(*angle_lo_deg));
            params.insert("angle_hi_deg".to_string(), Value::F64(*angle_hi_deg));
            if let Some(v) = step_deg {
                params.insert("step_deg".to_string(), Value::F64(*v));
            }
            if let Some(v) = clearance {
                params.insert("clearance".to_string(), Value::F64(*v));
            }
            Some(create(crdt, ctx, "sweep-rotate", params))
        }
        // Boolean ops: migrate both operands first so their feature ids
        // exist before the boolean feature that references them.
        CsgOp::Union { left, right } => {
            migrate_boolean(crdt, ctx, doc, *left, *right, "union", params)
        }
        CsgOp::Difference { left, right } => {
            migrate_boolean(crdt, ctx, doc, *left, *right, "difference", params)
        }
        CsgOp::Intersection { left, right } => {
            migrate_boolean(crdt, ctx, doc, *left, *right, "intersection", params)
        }
        // Unary modifiers: migrate the child first, then reference it.
        CsgOp::Fillet { child, radius } => {
            let input = migrate_node(crdt, ctx, doc, *child)?;
            params.insert("input".to_string(), Value::FeatureRef(fid_to_stable(input)));
            params.insert("radius".to_string(), Value::F64(*radius));
            Some(create(crdt, ctx, "fillet", params))
        }
        CsgOp::Chamfer { child, distance } => {
            let input = migrate_node(crdt, ctx, doc, *child)?;
            params.insert("input".to_string(), Value::FeatureRef(fid_to_stable(input)));
            params.insert("distance".to_string(), Value::F64(*distance));
            Some(create(crdt, ctx, "chamfer", params))
        }
        CsgOp::Shell {
            child,
            thickness,
            open_faces,
        } => {
            let input = migrate_node(crdt, ctx, doc, *child)?;
            params.insert("input".to_string(), Value::FeatureRef(fid_to_stable(input)));
            params.insert("thickness".to_string(), Value::F64(*thickness));
            if !open_faces.is_empty() {
                params.insert(
                    "open_faces_json".to_string(),
                    Value::String(serde_json::to_string(open_faces).ok()?),
                );
            }
            Some(create(crdt, ctx, "shell", params))
        }
        CsgOp::LinearPattern {
            child,
            direction,
            count,
            spacing,
        } => {
            let input = migrate_node(crdt, ctx, doc, *child)?;
            params.insert("input".to_string(), Value::FeatureRef(fid_to_stable(input)));
            params.insert(
                "direction".to_string(),
                Value::Vec3([direction.x, direction.y, direction.z]),
            );
            params.insert("count".to_string(), Value::F64(*count as f64));
            params.insert("spacing".to_string(), Value::F64(*spacing));
            Some(create(crdt, ctx, "linear-pattern", params))
        }
        CsgOp::CircularPattern {
            child,
            axis_origin,
            axis_dir,
            count,
            angle_deg,
        } => {
            let input = migrate_node(crdt, ctx, doc, *child)?;
            params.insert("input".to_string(), Value::FeatureRef(fid_to_stable(input)));
            params.insert(
                "axis_origin".to_string(),
                Value::Vec3([axis_origin.x, axis_origin.y, axis_origin.z]),
            );
            params.insert(
                "axis_dir".to_string(),
                Value::Vec3([axis_dir.x, axis_dir.y, axis_dir.z]),
            );
            params.insert("count".to_string(), Value::F64(*count as f64));
            params.insert("angle_deg".to_string(), Value::F64(*angle_deg));
            Some(create(crdt, ctx, "circular-pattern", params))
        }
        CsgOp::Extrude {
            sketch,
            direction,
            twist_angle,
            scale_end,
        } => {
            let depth =
                (direction.x * direction.x + direction.y * direction.y + direction.z * direction.z)
                    .sqrt();
            if depth > 0.0 {
                params.insert("depth".to_string(), Value::F64(depth));
                params.insert(
                    "direction".to_string(),
                    Value::Vec3([
                        direction.x / depth,
                        direction.y / depth,
                        direction.z / depth,
                    ]),
                );
            }
            if let Some(sketch_node) = doc.nodes.get(sketch) {
                if let Ok(json) = serde_json::to_string(&sketch_node.op) {
                    params.insert("sketch".to_string(), Value::String(json));
                }
            }
            if let Some(ta) = twist_angle {
                params.insert("twist_angle".to_string(), Value::F64(*ta));
            }
            if let Some(se) = scale_end {
                params.insert("scale_end".to_string(), Value::F64(*se));
            }
            Some(create(crdt, ctx, "extrude", params))
        }
        CsgOp::Revolve {
            sketch,
            axis_origin,
            axis_dir,
            angle_deg,
        } => {
            if let Some(sketch_node) = doc.nodes.get(sketch) {
                if let Ok(json) = serde_json::to_string(&sketch_node.op) {
                    params.insert("sketch".to_string(), Value::String(json));
                }
            }
            params.insert(
                "axis_origin".to_string(),
                Value::Vec3([axis_origin.x, axis_origin.y, axis_origin.z]),
            );
            params.insert(
                "axis_dir".to_string(),
                Value::Vec3([axis_dir.x, axis_dir.y, axis_dir.z]),
            );
            params.insert("angle_deg".to_string(), Value::F64(*angle_deg));
            Some(create(crdt, ctx, "revolve", params))
        }
        CsgOp::Sweep {
            sketch,
            path,
            twist_angle,
            scale_start,
            scale_end,
            ..
        } => {
            if let Some(sketch_node) = doc.nodes.get(sketch) {
                if let Ok(json) = serde_json::to_string(&sketch_node.op) {
                    params.insert("sketch".to_string(), Value::String(json));
                }
            }
            if let Ok(path_json) = serde_json::to_string(path) {
                params.insert("path".to_string(), Value::String(path_json));
            }
            if let Some(ta) = twist_angle {
                params.insert("twist_angle".to_string(), Value::F64(*ta));
            }
            if let Some(ss) = scale_start {
                params.insert("scale_start".to_string(), Value::F64(*ss));
            }
            if let Some(se) = scale_end {
                params.insert("scale_end".to_string(), Value::F64(*se));
            }
            Some(create(crdt, ctx, "sweep", params))
        }
        CsgOp::Loft { sketches, closed } => {
            params.insert(
                "sketch_count".to_string(),
                Value::F64(sketches.len() as f64),
            );
            for (i, sketch_id) in sketches.iter().enumerate() {
                if let Some(sketch_node) = doc.nodes.get(sketch_id) {
                    if let Ok(json) = serde_json::to_string(&sketch_node.op) {
                        params.insert(format!("sketch_{i}"), Value::String(json));
                    }
                }
            }
            if let Some(true) = closed {
                params.insert("closed".to_string(), Value::Bool(true));
            }
            Some(create(crdt, ctx, "loft", params))
        }
        CsgOp::Text2D { text, height, .. } => {
            params.insert("text".to_string(), Value::String(text.clone()));
            params.insert("height".to_string(), Value::F64(*height));
            Some(create(crdt, ctx, "text", params))
        }
        CsgOp::ImportedMesh {
            positions,
            indices,
            normals,
            source,
        } => {
            if let Ok(json) = serde_json::to_string(positions) {
                params.insert("positions_json".to_string(), Value::String(json));
            }
            if let Ok(json) = serde_json::to_string(indices) {
                params.insert("indices_json".to_string(), Value::String(json));
            }
            if let Some(n) = normals {
                if let Ok(json) = serde_json::to_string(n) {
                    params.insert("normals_json".to_string(), Value::String(json));
                }
            }
            if let Some(s) = source {
                params.insert("source".to_string(), Value::String(s.clone()));
            }
            Some(create(crdt, ctx, "imported-mesh", params))
        }
        // MeshImport is a path reference (used by URDF imports before the
        // browser inlines the actual mesh data). Carry the path + scale
        // through so loaders that resolve meshes after migration still
        // know what to fetch; render as a 10 mm placeholder cube until
        // the data lands.
        // Mesh imports (URDF / STL references) have no materializer kind;
        // keep them verbatim like any other unmodelled op.
        CsgOp::MeshImport { .. } => {
            let fid = create_subgraph(crdt, ctx, doc, top_id);
            ctx.node_to_feature.insert(top_id, fid);
            return Some(fid);
        }
        // Ops the feature model has no kind for: keep the whole subgraph
        // (transform chain included) verbatim so import is lossless; the
        // materializer re-emits it with fresh node ids.
        _ => {
            let fid = create_subgraph(crdt, ctx, doc, top_id);
            ctx.node_to_feature.insert(top_id, fid);
            return Some(fid);
        }
    };
    if let Some(id) = fid {
        ctx.node_to_feature.insert(top_id, id);
        for (old, role) in chain {
            ctx.record_role(old, id, role);
        }
    }
    fid
}

/// Store the node subgraph reachable from `root` as one `ir-subgraph`
/// feature (`{"root": id, "nodes": {id: Node}}`, original ids).
fn create_subgraph(
    crdt: &mut CrdtDocument,
    ctx: &mut MigrationCtx,
    doc: &Document,
    root: NodeId,
) -> FeatureId {
    let mut nodes = std::collections::BTreeMap::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if nodes.contains_key(&id) {
            continue;
        }
        if let Some(node) = doc.nodes.get(&id) {
            stack.extend(node.op.child_ids());
            nodes.insert(id, node.clone());
        }
    }
    let mut params = HashMap::new();
    let blob = serde_json::json!({ "root": root, "nodes": nodes });
    params.insert("subgraph".to_string(), Value::String(blob.to_string()));
    if let Some(name) = doc.nodes.get(&root).and_then(|n| n.name.clone()) {
        params.insert("name".to_string(), Value::String(name));
    }
    create(crdt, ctx, "ir-subgraph", params)
}

/// Document-level fields with no dedicated CRDT feature: carried through
/// import as one `doc-extras` JSON blob (see `materialize_doc_extras`).
/// `bindings` are excluded — they name v1 node ids and are remapped
/// separately (`remap_bindings`).
pub const DOC_EXTRA_FIELDS: &[&str] = &[
    "materials",
    "part_materials",
    "scene",
    "mates",
    "schematic",
    "pcb",
    "parameters",
    "datums",
    "clearance_specs",
    "constraints",
    "analysis_studies",
    "drawing",
    "hardware",
    "source",
    "instrument",
    "pitch_locks",
];

fn migrate_doc_extras(crdt: &mut CrdtDocument, ctx: &mut MigrationCtx, doc: &Document) {
    let Ok(serde_json::Value::Object(all)) = serde_json::to_value(doc) else {
        return;
    };
    let extras: serde_json::Map<String, serde_json::Value> = all
        .into_iter()
        .filter(|(k, v)| DOC_EXTRA_FIELDS.contains(&k.as_str()) && !is_empty_json(v))
        .collect();
    // Bindings are written onto this feature once ids are final.
    if extras.is_empty() && doc.bindings.is_empty() {
        return;
    }
    let mut params = HashMap::new();
    params.insert(
        "extras".to_string(),
        Value::String(serde_json::Value::Object(extras).to_string()),
    );
    create(crdt, ctx, "doc-extras", params);
}

/// Key on the `doc-extras` blob holding bindings addressed by feature.
pub(crate) const FEATURE_BINDINGS: &str = "feature_bindings";

/// Persist `doc.bindings` against features instead of v1 node ids. Node ids
/// are reassigned on every materialize, so a stored id would go stale at the
/// first edit; the feature id and field path don't.
fn store_feature_bindings(crdt: &mut CrdtDocument, doc: &Document, map: &MigrationMap) {
    let Ok(serde_json::Value::Object(bindings)) = serde_json::to_value(&doc.bindings) else {
        return;
    };
    if bindings.is_empty() {
        return;
    }
    let features: HashMap<NodeId, &str> = map
        .mappings
        .iter()
        .map(|m| (m.old_node_id, m.feature_id.as_str()))
        .collect();
    let records: Vec<serde_json::Value> = bindings
        .iter()
        .filter_map(|(key, expr)| {
            let (node, field) = key.split_once(':')?;
            let feature = features.get(&node.parse::<NodeId>().ok()?)?;
            Some(serde_json::json!({ "feature": feature, "field": field, "expr": expr }))
        })
        .collect();
    let Some(fid) = crdt
        .ordered_features()
        .into_iter()
        .find(|(_, f)| f.kind == "doc-extras")
        .map(|(fid, _)| fid)
    else {
        return;
    };
    let mut extras = match crdt.get_feature(fid).and_then(|f| f.params.get("extras")) {
        Some((Value::String(s), _)) => serde_json::from_str(s).unwrap_or_default(),
        _ => serde_json::Map::new(),
    };
    extras.insert(FEATURE_BINDINGS.to_string(), records.into());
    crdt.set_param(
        fid,
        "extras",
        Value::String(serde_json::Value::Object(extras).to_string()),
    );
}

/// Turn feature-addressed bindings back into `"<nodeId>:<field>"` keys for
/// this materialize. Returns the bindings plus one warning per record whose
/// feature or node is gone.
pub(crate) fn resolve_feature_bindings(
    records: &[serde_json::Value],
    parts_json: &serde_json::Value,
) -> (serde_json::Map<String, serde_json::Value>, Vec<String>) {
    let mut out = serde_json::Map::new();
    let mut warnings = Vec::new();
    for record in records {
        let (Some(feature), Some(field), Some(expr)) = (
            record.get("feature").and_then(|v| v.as_str()),
            record.get("field").and_then(|v| v.as_str()),
            record.get("expr"),
        ) else {
            continue;
        };
        let node = parts_json.as_array().and_then(|parts| {
            let part = parts
                .iter()
                .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(feature))?;
            role_node_id(part, role_for_field(field))
        });
        match node {
            Some(id) => {
                out.insert(format!("{id}:{field}"), expr.clone());
            }
            None => warnings.push(format!("binding on {feature}.{field} has no target")),
        }
    }
    (out, warnings)
}

pub(crate) fn is_empty_json(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::Object(m) => m.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn migrate_boolean(
    crdt: &mut CrdtDocument,
    ctx: &mut MigrationCtx,
    doc: &Document,
    left: NodeId,
    right: NodeId,
    boolean_type: &str,
    mut params: HashMap<String, Value>,
) -> Option<FeatureId> {
    let a = migrate_node(crdt, ctx, doc, left)?;
    let b = migrate_node(crdt, ctx, doc, right)?;
    params.insert(
        "boolean_type".to_string(),
        Value::String(boolean_type.to_string()),
    );
    params.insert("input_a".to_string(), Value::FeatureRef(fid_to_stable(a)));
    params.insert("input_b".to_string(), Value::FeatureRef(fid_to_stable(b)));
    Some(create(crdt, ctx, "boolean", params))
}

fn create(
    crdt: &mut CrdtDocument,
    ctx: &mut MigrationCtx,
    kind: &str,
    params: HashMap<String, Value>,
) -> FeatureId {
    let position = ctx.next_position();
    let (fid, _) = crdt.create_feature(kind, position, params);
    fid
}

/// Parse a stable `"replica:seq"` id.
fn parse_fid(s: &str) -> Option<FeatureId> {
    let (r, q) = s.split_once(':')?;
    Some(FeatureId(ReplicaId(r.parse().ok()?), q.parse().ok()?))
}

/// The full old → new id map for [`rekey`]: claimed roots get their claimed
/// id; every other feature keeps its own unless a claim took it, in which
/// case it moves to the next id no one holds.
fn assign_ids(
    crdt: &CrdtDocument,
    wanted: &[(FeatureId, FeatureId)],
) -> HashMap<FeatureId, FeatureId> {
    let mut remap: HashMap<FeatureId, FeatureId> = wanted.iter().copied().collect();
    let mut taken: std::collections::HashSet<FeatureId> = wanted.iter().map(|(_, c)| *c).collect();
    let features = crdt.ordered_features();
    let mut displaced = Vec::new();
    for (fid, _) in &features {
        if remap.contains_key(fid) {
            continue;
        }
        if taken.insert(*fid) {
            remap.insert(*fid, *fid);
        } else {
            displaced.push(*fid);
        }
    }
    let mut seq = 0;
    for fid in displaced {
        while taken.contains(&FeatureId(ReplicaId(0), seq)) {
            seq += 1;
        }
        let to = FeatureId(ReplicaId(0), seq);
        taken.insert(to);
        remap.insert(fid, to);
    }
    remap
}

/// Rebuild a freshly migrated document under new feature ids, rewriting
/// every feature reference to match. Only valid on a migration result (no
/// history worth keeping, every feature live).
fn rekey(crdt: &CrdtDocument, remap: &HashMap<FeatureId, FeatureId>) -> CrdtDocument {
    let map_ref = |s: &str| {
        parse_fid(s)
            .and_then(|f| remap.get(&f))
            .map_or_else(|| s.to_string(), |f| fid_to_stable(*f))
    };
    let mut out = CrdtDocument::new(ReplicaId(0));
    for (fid, state) in crdt.ordered_features() {
        let params = state
            .params
            .iter()
            .map(|(k, (v, _))| {
                let v = match v {
                    Value::FeatureRef(r) => Value::FeatureRef(map_ref(r)),
                    Value::FeatureRefList(rs) => {
                        Value::FeatureRefList(rs.iter().map(|r| map_ref(r)).collect())
                    }
                    other => other.clone(),
                };
                (k.clone(), v)
            })
            .collect();
        out.create_feature_with_id(remap[&fid], &state.kind, state.position.clone(), params);
    }
    out
}

/// Format a `FeatureId` as the stable `"replica:seq"` string used throughout
/// the higher-level API (see `document_api::StableIdMap`).
fn fid_to_stable(fid: FeatureId) -> String {
    format!("{}:{}", fid.0 .0, fid.1)
}

/// A board node no root or part definition reaches is still the document's
/// board. Everything else is carried by walking from the roots, so without
/// this it vanished on load, and took the Electronics workspace with it.
fn migrate_orphan_boards(crdt: &mut CrdtDocument, ctx: &mut MigrationCtx, doc: &Document) {
    let mut reached = std::collections::HashSet::new();
    let part_roots = doc.part_defs.iter().flatten().map(|(_, pd)| pd.root);
    let mut stack: Vec<NodeId> = doc.roots.iter().map(|e| e.root).chain(part_roots).collect();
    while let Some(id) = stack.pop() {
        if reached.insert(id) {
            stack.extend(
                doc.nodes
                    .get(&id)
                    .map(|n| n.op.child_ids())
                    .unwrap_or_default(),
            );
        }
    }
    let mut orphans: Vec<NodeId> = doc
        .nodes
        .iter()
        .filter(|(id, node)| matches!(node.op, CsgOp::PcbBoard { .. }) && !reached.contains(id))
        .map(|(id, _)| *id)
        .collect();
    orphans.sort_unstable();
    for id in orphans {
        if let Some(fid) = migrate_node(crdt, ctx, doc, id) {
            crdt.set_param(fid, "material", Value::String("__pcb_fr4__".to_string()));
        }
    }
}

/// Migrate the assembly portion of a v1 document: part definitions, their
/// instances, and the joints connecting them. No-op when the document has
/// no `partDefs`.
fn migrate_assembly(crdt: &mut CrdtDocument, ctx: &mut MigrationCtx, doc: &Document) {
    let Some(part_defs) = &doc.part_defs else {
        return;
    };

    // partDef id (string in v1) -> stable feature id of the `part-def` feature.
    let mut part_def_stable: HashMap<String, String> = HashMap::new();
    // Sorted so feature ids are a pure function of the IR: an engine rebuilt
    // from the same document hands out the same stable part ids.
    let mut sorted: Vec<_> = part_defs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (key, pd) in sorted {
        let Some(source_fid) = migrate_node(crdt, ctx, doc, pd.root) else {
            continue;
        };
        let mut params = HashMap::new();
        params.insert(
            "source_feature".to_string(),
            Value::FeatureRef(fid_to_stable(source_fid)),
        );
        // A part definition's geometry reaches the scene through its instances.
        // Only a node the document itself lists as a root is also a scene root.
        if !doc.roots.iter().any(|entry| entry.root == pd.root) {
            params.insert(PART_DEF_SOURCE_CONSUMED.to_string(), Value::Bool(true));
        }
        if let Some(n) = &pd.name {
            params.insert("name".to_string(), Value::String(n.clone()));
        }
        if let Some(m) = &pd.default_material {
            params.insert("default_material".to_string(), Value::String(m.clone()));
        }
        if let Some(inertial) = pd
            .inertial
            .as_ref()
            .and_then(|i| serde_json::to_string(i).ok())
        {
            params.insert("inertial".to_string(), Value::String(inertial));
        }
        if let Some(colliders) = &pd.colliders {
            let refs = colliders
                .iter()
                .filter_map(|&n| migrate_node(crdt, ctx, doc, n))
                .map(fid_to_stable)
                .collect();
            params.insert("colliders".to_string(), Value::FeatureRefList(refs));
        }
        let pd_fid = create(crdt, ctx, "part-def", params);
        part_def_stable.insert(key.clone(), fid_to_stable(pd_fid));
        // Also key by the partDef's own id field, since instances reference
        // `partDefId` which matches `PartDef::id` (typically equal to the
        // HashMap key but not guaranteed).
        if pd.id != *key {
            part_def_stable.insert(pd.id.clone(), fid_to_stable(pd_fid));
        }
    }

    // instance.id -> stable feature id of the `instance` feature.
    let mut instance_stable: HashMap<String, String> = HashMap::new();
    if let Some(instances) = &doc.instances {
        for inst in instances {
            let Some(pd_stable) = part_def_stable.get(&inst.part_def_id) else {
                continue;
            };
            let mut params = HashMap::new();
            params.insert("part_def".to_string(), Value::FeatureRef(pd_stable.clone()));
            if let Some(n) = &inst.name {
                params.insert("name".to_string(), Value::String(n.clone()));
            }
            if let Some(t) = &inst.transform {
                if let Ok(json) = serde_json::to_string(t) {
                    params.insert("transform".to_string(), Value::String(json));
                }
            }
            if let Some(m) = &inst.material {
                params.insert("material".to_string(), Value::String(m.clone()));
            }
            if doc.ground_instance_id.as_deref() == Some(&inst.id) {
                params.insert("is_ground".to_string(), Value::Bool(true));
            }
            let inst_fid = create(crdt, ctx, "instance", params);
            instance_stable.insert(inst.id.clone(), fid_to_stable(inst_fid));
        }
    }

    if let Some(joints) = &doc.joints {
        for joint in joints {
            let Some(child_stable) = instance_stable.get(&joint.child_instance_id) else {
                continue;
            };
            let parent_stable = joint
                .parent_instance_id
                .as_ref()
                .and_then(|id| instance_stable.get(id));

            let (effort_limit, velocity_limit) = match &joint.kind {
                JointKind::Revolute {
                    effort_limit,
                    velocity_limit,
                    ..
                }
                | JointKind::Slider {
                    effort_limit,
                    velocity_limit,
                    ..
                } => (*effort_limit, *velocity_limit),
                _ => (None, None),
            };
            let (kind_str, axis, limits_json) = match &joint.kind {
                JointKind::Fixed => ("Fixed", None, None),
                JointKind::Revolute { axis, limits, .. } => (
                    "Revolute",
                    Some([axis.x, axis.y, axis.z]),
                    limits.as_ref().and_then(|l| serde_json::to_string(l).ok()),
                ),
                JointKind::Slider { axis, limits, .. } => (
                    "Slider",
                    Some([axis.x, axis.y, axis.z]),
                    limits.as_ref().and_then(|l| serde_json::to_string(l).ok()),
                ),
                JointKind::Cylindrical { axis } => {
                    ("Cylindrical", Some([axis.x, axis.y, axis.z]), None)
                }
                JointKind::Ball => ("Ball", None, None),
                JointKind::Free => ("Free", None, None),
            };

            let mut params = HashMap::new();
            params.insert("kind".to_string(), Value::String(kind_str.to_string()));
            params.insert(
                "instance_b".to_string(),
                Value::FeatureRef(child_stable.clone()),
            );
            if let Some(p) = parent_stable {
                params.insert("instance_a".to_string(), Value::FeatureRef(p.clone()));
            }
            params.insert(
                "anchor_a".to_string(),
                Value::Vec3([
                    joint.parent_anchor.x,
                    joint.parent_anchor.y,
                    joint.parent_anchor.z,
                ]),
            );
            params.insert(
                "anchor_b".to_string(),
                Value::Vec3([
                    joint.child_anchor.x,
                    joint.child_anchor.y,
                    joint.child_anchor.z,
                ]),
            );
            if let Some(a) = axis {
                params.insert("axis".to_string(), Value::Vec3(a));
            }
            if let Some(n) = &joint.name {
                params.insert("name".to_string(), Value::String(n.clone()));
            }
            if let Some(j) = limits_json {
                params.insert("limits".to_string(), Value::String(j));
            }
            params.insert("state".to_string(), Value::F64(joint.state));
            if let Some(v) = effort_limit {
                params.insert("effort_limit".to_string(), Value::F64(v));
            }
            if let Some(v) = velocity_limit {
                params.insert("velocity_limit".to_string(), Value::F64(v));
            }

            create(crdt, ctx, "joint", params);
        }
    }
}

/// Detect whether bytes represent a v1 (JSON) format or v2 (CRDT) format.
pub fn detect_format(bytes: &[u8]) -> FileFormat {
    // V1 format starts with '{' (JSON object).
    // V2 format starts with '{"replica_id"' (CRDT SavedDocument).
    if bytes.is_empty() {
        return FileFormat::Unknown;
    }

    // Try parsing as CRDT first (it's also JSON but has specific fields).
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if val.get("replica_id").is_some() && val.get("ops").is_some() {
            return FileFormat::V2Crdt;
        }
        if val.get("version").is_some() && val.get("nodes").is_some() {
            return FileFormat::V1Json;
        }
    }

    FileFormat::Unknown
}

/// File format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    /// Legacy JSON IR format.
    V1Json,
    /// New CRDT op-log format.
    V2Crdt,
    /// Unknown format.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_ir::{Document, Node, SceneEntry};

    #[test]
    fn test_migrate_empty_document() {
        let doc = Document::new();
        let crdt = migrate_v1(&doc);
        assert_eq!(crdt.ordered_features().len(), 0);
    }

    /// The exact document that broke: a plate with a through-hole, one
    /// binding on the cube's `size.z` and one on the cylinder's `radius`.
    fn plate_with_hole() -> Document {
        let mut doc = Document::new();
        doc.nodes.insert(
            1,
            Node {
                id: 1,
                name: Some("plate".into()),
                op: CsgOp::Cube {
                    size: Vec3::new(40.0, 40.0, 6.0),
                },
            },
        );
        doc.nodes.insert(
            2,
            Node {
                id: 2,
                name: Some("drill".into()),
                op: CsgOp::Cylinder {
                    radius: 6.0,
                    height: 40.0,
                    segments: 48,
                },
            },
        );
        doc.nodes.insert(
            3,
            Node {
                id: 3,
                name: Some("drill_at".into()),
                op: CsgOp::Translate {
                    child: 2,
                    offset: Vec3::new(20.0, 20.0, -10.0),
                },
            },
        );
        doc.nodes.insert(
            4,
            Node {
                id: 4,
                name: Some("drilled".into()),
                op: CsgOp::Difference { left: 1, right: 3 },
            },
        );
        doc.roots.push(SceneEntry {
            root: 4,
            material: "default".into(),
            visible: None,
        });
        doc
    }

    #[test]
    fn torus_wedge_and_prism_are_typed_after_a_load() {
        let doc: Document = serde_json::from_value(serde_json::json!({
            "version": "0.1",
            "nodes": {
                "1": {"id": 1, "op": {"type": "Torus", "major_radius": 20.0, "minor_radius": 5.0, "segments": 64}},
                "2": {"id": 2, "op": {"type": "Wedge", "size": {"x": 10.0, "y": 20.0, "z": 30.0}}},
                "3": {"id": 3, "op": {"type": "Prism", "sides": 6, "radius": 10.0, "height": 5.0}}
            },
            "materials": {}, "part_materials": {},
            "roots": [{"root": 1, "material": "default"}, {"root": 2, "material": "default"}, {"root": 3, "material": "default"}]
        })).unwrap();
        let got = crate::materializer::materialize(&migrate_v1(&doc));
        let kinds: Vec<_> = got
            .parts
            .iter()
            .map(|p| serde_json::to_value(p).unwrap()["kind"].clone())
            .collect();
        assert_eq!(kinds, vec!["torus", "wedge", "prism"]);
    }

    #[test]
    fn hull_and_sweep_rotate_migrate_typed_and_consume_children() {
        let doc: Document = serde_json::from_value(serde_json::json!({
            "version": "0.1",
            "nodes": {
                "1": {"id": 1, "op": {"type": "Cube", "size": {"x": 10.0, "y": 10.0, "z": 10.0}}},
                "2": {"id": 2, "op": {"type": "Sphere", "radius": 5.0, "segments": 16}},
                "3": {"id": 3, "op": {"type": "Hull", "children": [1, 2], "points": [0.0, 0.0, 40.0]}},
                "4": {"id": 4, "op": {"type": "Cube", "size": {"x": 2.0, "y": 2.0, "z": 2.0}}},
                "5": {"id": 5, "op": {"type": "SweepRotate", "child": 4,
                    "axis_origin": {"x": 0.0, "y": 0.0, "z": 0.0}, "axis_dir": {"x": 0.0, "y": 0.0, "z": 1.0},
                    "angle_lo_deg": 0.0, "angle_hi_deg": 90.0}}
            },
            "materials": {}, "part_materials": {},
            "roots": [{"root": 3, "material": "default"}, {"root": 5, "material": "default"}]
        })).unwrap();
        let got = crate::materializer::materialize(&migrate_v1(&doc));
        assert_eq!(got.document.roots.len(), 2, "{:?}", got.warnings);
        let hull = got
            .document
            .nodes
            .values()
            .find_map(|n| match &n.op {
                CsgOp::Hull { children, points } => Some((children.len(), points.clone())),
                _ => None,
            })
            .expect("hull");
        assert_eq!(hull, (2, vec![0.0, 0.0, 40.0]));
        assert!(got.document.nodes.values().any(
            |n| matches!(n.op, CsgOp::SweepRotate { angle_hi_deg, .. } if angle_hi_deg == 90.0)
        ));
    }

    #[test]
    fn an_unrooted_board_survives_a_load() {
        let doc: Document = serde_json::from_value(serde_json::json!({
            "version": "0.1",
            "nodes": {
                "1": {"id": 1, "op": {"type": "Cube", "size": {"x": 1.0, "y": 1.0, "z": 1.0}}},
                "2": {"id": 2, "name": "PCB", "op": {"type": "PcbBoard", "board": {
                    "outline": {"vertices": [], "thickness": 1.6},
                    "stackup": {"layers": []},
                    "nets": [],
                    "rules": {"defaultRules": {"name": "default", "traceWidth": 0.2,
                        "clearance": 0.2, "viaDiameter": 0.6, "viaDrill": 0.3},
                        "edgeClearance": 0.3, "holeToHole": 0.25,
                        "minAnnularRing": 0.1, "minDrill": 0.2},
                    "footprints": [], "traces": [], "vias": [], "zones": []
                }}}
            },
            "materials": {},
            "part_materials": {},
            "roots": [{"root": 1, "material": "default"}]
        }))
        .expect("fixture");
        let out = crate::materializer::materialize(&migrate_v1(&doc)).document;
        assert!(
            out.nodes
                .values()
                .any(|n| matches!(n.op, CsgOp::PcbBoard { .. })),
            "the board must be in the materialized document"
        );
        assert_eq!(out.roots.len(), 2, "and reachable from the scene");
    }

    #[test]
    fn torus_wedge_and_prism_survive_a_load() {
        let doc: Document = serde_json::from_value(serde_json::json!({
            "version": "0.1",
            "nodes": {
                "1": {"id": 1, "op": {"type": "Torus", "major_radius": 20.0, "minor_radius": 5.0, "segments": 64}},
                "2": {"id": 2, "op": {"type": "Wedge", "size": {"x": 10.0, "y": 20.0, "z": 30.0}}},
                "3": {"id": 3, "op": {"type": "Prism", "sides": 6, "radius": 10.0, "height": 5.0}}
            },
            "materials": {}, "part_materials": {},
            "roots": [{"root": 1, "material": "default"}, {"root": 2, "material": "default"}, {"root": 3, "material": "default"}]
        })).unwrap();
        let got = crate::materializer::materialize(&migrate_v1(&doc)).document;
        assert_eq!(got.roots.len(), 3);
        for found in [
            got.nodes
                .values()
                .any(|n| matches!(n.op, CsgOp::Torus { .. })),
            got.nodes
                .values()
                .any(|n| matches!(n.op, CsgOp::Wedge { .. })),
            got.nodes
                .values()
                .any(|n| matches!(n.op, CsgOp::Prism { .. })),
        ] {
            assert!(found);
        }
    }

    /// Real documents, through the same path the app opens them with.
    fn load(json: &str) -> Document {
        serde_json::from_str(json).expect("example parses")
    }

    #[test]
    fn a_loaded_document_keeps_its_bindings_on_the_right_nodes() {
        let original = load(include_str!("../../../examples/parametric-plate.vcad"));
        let once = crate::materializer::materialize(&migrate_v1(&original)).document;
        // Twice: a saved document must reload the same way.
        let twice = crate::materializer::materialize(&migrate_v1(&once)).document;
        for doc in [&once, &twice] {
            let json = serde_json::to_value(&doc.bindings).unwrap();
            let bindings = json.as_object().unwrap();
            assert_eq!(bindings.len(), 7, "{bindings:?}");
            for key in bindings.keys() {
                let (node, field) = key.split_once(':').unwrap();
                let op = &doc.nodes[&node.parse::<NodeId>().unwrap()].op;
                match field {
                    "size.x" | "size.y" | "size.z" => {
                        assert!(matches!(op, CsgOp::Cube { .. }), "{key}: {op:?}")
                    }
                    "radius" | "height" => {
                        assert!(matches!(op, CsgOp::Cylinder { .. }), "{key}: {op:?}")
                    }
                    "offset.x" | "offset.y" => {
                        assert!(matches!(op, CsgOp::Translate { .. }), "{key}: {op:?}")
                    }
                    other => panic!("unexpected field {other}"),
                }
            }
        }
    }

    #[test]
    fn a_loaded_document_keeps_its_actuator_limits() {
        let limits = |doc: &Document| -> Vec<(Option<f64>, Option<f64>)> {
            let mut v: Vec<_> = doc
                .joints
                .iter()
                .flatten()
                .filter_map(|j| match j.kind {
                    JointKind::Revolute {
                        effort_limit,
                        velocity_limit,
                        ..
                    } => Some((effort_limit, velocity_limit)),
                    _ => None,
                })
                .collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v
        };
        let original = load(include_str!("../../../examples/k1-floating.vcad"));
        let want = limits(&original);
        assert!(want.iter().any(|l| l.0.is_some()), "fixture has limits");
        let got = crate::materializer::materialize(&migrate_v1(&original)).document;
        assert_eq!(limits(&got), want);
    }

    /// Migration renumbers every node, so bindings authored against v1 ids
    /// must be rewritten. Before this existed, `"2:radius"` survived a load
    /// pointing at whatever node 2 became — a `Scale` wrapper — and the
    /// document failed to evaluate with "field path not valid on op Scale",
    /// taking every other binding down with it.
    #[test]
    fn bindings_are_remapped_onto_the_materialized_nodes() {
        let doc = plate_with_hole();
        let (crdt, map) = migrate_v1_mapped(&doc);
        let result = crate::materializer::materialize(&crdt);
        let parts_json = serde_json::to_value(&result.parts).unwrap();

        let mut bindings = HashMap::new();
        bindings.insert("1:size.z".to_string(), "plate_t".to_string());
        bindings.insert("2:radius".to_string(), "hole_r".to_string());

        let out = remap_bindings(&bindings, &map, &parts_json);
        assert!(
            out.dropped.is_empty(),
            "nothing should be dropped here: {:?}",
            out.dropped
        );
        assert_eq!(out.bindings.len(), 2);

        // Every remapped key must name a node that exists and whose op
        // actually carries that field — the property the old code violated.
        for (key, expr) in &out.bindings {
            let (node_str, field) = key.split_once(':').unwrap();
            let id: NodeId = node_str.parse().unwrap();
            let node = result
                .document
                .nodes
                .get(&id)
                .unwrap_or_else(|| panic!("{key} -> node {id} does not exist"));
            match (expr.as_str(), &node.op) {
                ("plate_t", CsgOp::Cube { .. }) => assert_eq!(field, "size.z"),
                ("hole_r", CsgOp::Cylinder { .. }) => assert_eq!(field, "radius"),
                (e, op) => panic!("{e} landed on the wrong op: {op:?}"),
            }
        }
    }

    /// A field on a transform wrapper follows the wrapper, not the leaf.
    #[test]
    fn transform_fields_remap_onto_their_wrapper() {
        let doc = plate_with_hole();
        let (crdt, map) = migrate_v1_mapped(&doc);
        let result = crate::materializer::materialize(&crdt);
        let parts_json = serde_json::to_value(&result.parts).unwrap();

        let mut bindings = HashMap::new();
        // Node 3 is the Translate wrapping the cylinder.
        bindings.insert("3:offset.x".to_string(), "hole_x".to_string());
        let out = remap_bindings(&bindings, &map, &parts_json);
        assert!(out.dropped.is_empty(), "{:?}", out.dropped);
        let key = out.bindings.keys().next().unwrap();
        let id: NodeId = key.split_once(':').unwrap().0.parse().unwrap();
        assert!(
            matches!(
                result.document.nodes.get(&id).map(|n| &n.op),
                Some(CsgOp::Translate { .. })
            ),
            "offset must land on a Translate, got {:?}",
            result.document.nodes.get(&id).map(|n| &n.op)
        );
    }

    /// A binding whose node did not survive is dropped with a reason, not
    /// carried forward to poison the evaluation.
    #[test]
    fn unmappable_bindings_are_dropped_with_a_reason() {
        let doc = plate_with_hole();
        let (crdt, map) = migrate_v1_mapped(&doc);
        let result = crate::materializer::materialize(&crdt);
        let parts_json = serde_json::to_value(&result.parts).unwrap();

        let mut bindings = HashMap::new();
        bindings.insert("999:radius".to_string(), "ghost".to_string());
        bindings.insert("nonsense".to_string(), "bad".to_string());
        let out = remap_bindings(&bindings, &map, &parts_json);
        assert!(out.bindings.is_empty());
        assert_eq!(out.dropped.len(), 2);
        assert!(out.dropped.iter().any(|d| d.contains("did not survive")));
        assert!(out.dropped.iter().any(|d| d.contains("malformed")));
    }

    #[test]
    fn test_migrate_single_cube() {
        let mut doc = Document::new();
        let cube_id = 1;
        doc.nodes.insert(
            cube_id,
            Node {
                id: cube_id,
                name: Some("Cube 1".to_string()),
                op: CsgOp::Cube {
                    size: Vec3::new(10.0, 20.0, 30.0),
                },
            },
        );
        // Add transform chain: scale → rotate → translate
        let scale_id = 2;
        doc.nodes.insert(
            scale_id,
            Node {
                id: scale_id,
                name: None,
                op: CsgOp::Scale {
                    child: cube_id,
                    factor: Vec3::new(1.0, 1.0, 1.0),
                },
            },
        );
        let rotate_id = 3;
        doc.nodes.insert(
            rotate_id,
            Node {
                id: rotate_id,
                name: None,
                op: CsgOp::Rotate {
                    child: scale_id,
                    angles: Vec3::new(0.0, 0.0, 0.0),
                },
            },
        );
        let translate_id = 4;
        doc.nodes.insert(
            translate_id,
            Node {
                id: translate_id,
                name: Some("Cube 1".to_string()),
                op: CsgOp::Translate {
                    child: rotate_id,
                    offset: Vec3::new(5.0, 0.0, 0.0),
                },
            },
        );
        doc.roots.push(SceneEntry {
            root: translate_id,
            material: "default".to_string(),
            visible: None,
        });

        let crdt = migrate_v1(&doc);
        let features = crdt.ordered_features();
        assert_eq!(features.len(), 1);

        let f = features[0].1;
        assert_eq!(f.kind, "cube");
        assert_eq!(f.params.get("size_x").unwrap().0, Value::F64(10.0));
        assert_eq!(f.params.get("size_y").unwrap().0, Value::F64(20.0));
        assert_eq!(f.params.get("size_z").unwrap().0, Value::F64(30.0));
        assert_eq!(
            f.params.get("offset").unwrap().0,
            Value::Vec3([5.0, 0.0, 0.0])
        );
        assert_eq!(
            f.params.get("name").map(|(v, _)| v),
            Some(&Value::String("Cube 1".to_string())),
        );
    }

    #[test]
    fn test_migrate_name_on_leaf() {
        // The materializer writes feature names onto the core/leaf node, not
        // the translate wrapper. Reloading a saved document must still
        // recover the name through migrate_v1.
        let mut doc = Document::new();
        let cube_id = 1;
        doc.nodes.insert(
            cube_id,
            Node {
                id: cube_id,
                name: Some("MyCube".to_string()),
                op: CsgOp::Cube {
                    size: Vec3::new(10.0, 10.0, 10.0),
                },
            },
        );
        let scale_id = 2;
        doc.nodes.insert(
            scale_id,
            Node {
                id: scale_id,
                name: None,
                op: CsgOp::Scale {
                    child: cube_id,
                    factor: Vec3::new(1.0, 1.0, 1.0),
                },
            },
        );
        let rotate_id = 3;
        doc.nodes.insert(
            rotate_id,
            Node {
                id: rotate_id,
                name: None,
                op: CsgOp::Rotate {
                    child: scale_id,
                    angles: Vec3::new(0.0, 0.0, 0.0),
                },
            },
        );
        let translate_id = 4;
        doc.nodes.insert(
            translate_id,
            Node {
                id: translate_id,
                name: None,
                op: CsgOp::Translate {
                    child: rotate_id,
                    offset: Vec3::new(0.0, 0.0, 0.0),
                },
            },
        );
        doc.roots.push(SceneEntry {
            root: translate_id,
            material: "default".to_string(),
            visible: None,
        });

        let crdt = migrate_v1(&doc);
        let features = crdt.ordered_features();
        assert_eq!(features.len(), 1);
        assert_eq!(
            features[0].1.params.get("name").map(|(v, _)| v),
            Some(&Value::String("MyCube".to_string())),
        );
    }

    #[test]
    fn test_migrate_bare_cube() {
        // A cube directly as a root (no transform chain)
        let mut doc = Document::new();
        let cube_id = 1;
        doc.nodes.insert(
            cube_id,
            Node {
                id: cube_id,
                name: Some("Box".to_string()),
                op: CsgOp::Cube {
                    size: Vec3::new(5.0, 5.0, 5.0),
                },
            },
        );
        doc.roots.push(SceneEntry {
            root: cube_id,
            material: "default".to_string(),
            visible: None,
        });

        let crdt = migrate_v1(&doc);
        assert_eq!(crdt.ordered_features().len(), 1);
        assert_eq!(crdt.ordered_features()[0].1.kind, "cube");
    }

    /// An assembly: `n` part defs, each instanced once, plus `roots`.
    fn assembly(n: u64, roots: &[u64]) -> Document {
        let mut value = serde_json::json!({
            "version": "0.1", "nodes": {}, "materials": {}, "part_materials": {},
            "roots": roots.iter().map(|r| serde_json::json!({"root": r, "material": "default"})).collect::<Vec<_>>(),
            "partDefs": {}, "instances": [],
        });
        for i in 1..=n {
            value["nodes"][i.to_string()] = serde_json::json!({
                "id": i, "name": format!("link{i}"),
                "op": {"type": "Cube", "size": {"x": 1.0, "y": 1.0, "z": i as f64}},
            });
            let id = format!("def{i}");
            value["partDefs"][&id] = serde_json::json!({"id": id, "root": i});
            value["instances"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "id": format!("inst{i}"), "partDefId": id,
                }));
        }
        serde_json::from_value(value).expect("assembly fixture")
    }

    #[test]
    fn part_definitions_do_not_become_scene_roots() {
        let doc = assembly(6, &[]);
        let result = crate::materializer::materialize(&migrate_v1(&doc));
        assert_eq!(
            result.document.roots.len(),
            0,
            "{:?}",
            result.document.roots
        );
        assert_eq!(result.document.part_defs.as_ref().unwrap().len(), 6);
        assert_eq!(result.document.instances.as_ref().unwrap().len(), 6);
    }

    #[test]
    fn a_root_that_is_also_a_part_definition_stays_one_root() {
        let doc = assembly(6, &[3]);
        let result = crate::materializer::materialize(&migrate_v1(&doc));
        assert_eq!(
            result.document.roots.len(),
            1,
            "{:?}",
            result.document.roots
        );
        // The surviving root is that part's geometry, under any transform wrappers.
        let mut id = result.document.roots[0].root;
        while let CsgOp::Translate { child, .. }
        | CsgOp::Rotate { child, .. }
        | CsgOp::Scale { child, .. } = &result.document.nodes[&id].op
        {
            id = *child;
        }
        match &result.document.nodes[&id].op {
            CsgOp::Cube { size } => assert_eq!(size.z, 3.0),
            other => panic!("expected the third part's cube, got {other:?}"),
        }
    }

    #[test]
    fn assembly_roots_are_stable_across_repeated_round_trips() {
        let mut doc = assembly(4, &[2]);
        for _ in 0..3 {
            doc = crate::materializer::materialize(&migrate_v1(&doc)).document;
            assert_eq!(doc.roots.len(), 1, "{:?}", doc.roots);
            assert_eq!(doc.part_defs.as_ref().unwrap().len(), 4);
        }
    }

    #[test]
    fn a_plain_document_round_trips_with_the_same_roots() {
        let doc: Document = serde_json::from_value(serde_json::json!({
            "version": "0.1", "materials": {}, "part_materials": {},
            "nodes": {
                "1": {"id": 1, "name": "a", "op": {"type": "Cube", "size": {"x": 1.0, "y": 1.0, "z": 1.0}}},
                "2": {"id": 2, "name": "b", "op": {"type": "Cube", "size": {"x": 2.0, "y": 1.0, "z": 1.0}}},
            },
            "roots": [{"root": 1, "material": "default"}, {"root": 2, "material": "default"}],
        })).expect("plain fixture");
        let result = crate::materializer::materialize(&migrate_v1(&doc));
        assert_eq!(result.document.roots.len(), 2);
    }

    #[test]
    fn tuned_instrument_metadata_survives_the_crdt_round_trip() {
        let doc: Document = serde_json::from_value(serde_json::json!({
            "version": "0.1", "materials": {}, "part_materials": {},
            "nodes": {"1": {"id": 1, "name": "bar_c6", "op": {"type": "Cube", "size": {"x": 130.0, "y": 25.0, "z": 6.0}}}},
            "roots": [{"root": 1, "material": "default"}],
            "instrument": {"kind": "glockenspiel", "tuning": "12-TET", "material": "aluminium_6061", "bars": [{
                "note": "C6", "part": "bar_c6", "target_hz": 1046.5, "predicted_hz": 1046.5,
                "tolerance_cents": 5.0, "length_mm": 130.0, "width_mm": 25.0, "thickness_mm": 6.0,
                "undercut_depth_mm": 2.7, "undercut_span": 0.2, "hole_dia_mm": 4.0, "nodes_mm": [29.0, 101.0],
                "measurement": {"measured_hz": 1047.0, "cents_error": 0.8, "instrument": "mic"}
            }]},
        })).expect("instrument fixture");
        let out = crate::materializer::materialize(&migrate_v1(&doc)).document;
        assert_eq!(out.instrument, doc.instrument);
    }

    #[test]
    fn test_detect_format() {
        let v1 = br#"{"version":"0.1","nodes":{},"materials":{},"part_materials":{},"roots":[]}"#;
        assert_eq!(detect_format(v1), FileFormat::V1Json);

        let v2 = br#"{"replica_id":1,"ops":[],"features":[]}"#;
        assert_eq!(detect_format(v2), FileFormat::V2Crdt);

        assert_eq!(detect_format(b""), FileFormat::Unknown);
        assert_eq!(detect_format(b"garbage"), FileFormat::Unknown);
    }

    /// The raw CRDT save output must be routed through the CRDT loader
    /// directly — not through `migrate_v1`. Running `migrate_v1` on a CRDT
    /// document is the lossy path that dropped feature `name` (and would
    /// drop any future param the materializer doesn't surface).
    #[test]
    fn test_crdt_bytes_detect_as_v2() {
        use vcad_crdt::{CrdtDocument, FractionalIndex, ReplicaId, Value};

        let mut crdt = CrdtDocument::new(ReplicaId(1));
        crdt.create_feature(
            "cube",
            FractionalIndex::between(None, None),
            std::collections::HashMap::from([
                ("size_x".to_string(), Value::F64(10.0)),
                ("name".to_string(), Value::String("MyCube".into())),
            ]),
        );
        let bytes = crdt.save();
        assert_eq!(
            detect_format(&bytes),
            FileFormat::V2Crdt,
            "CRDT save output must route as V2Crdt, not V1Json (otherwise migrate_v1 would drop params)",
        );

        // Round-trip via CrdtDocument::load (the non-lossy path) and confirm
        // the name survives — this is the contract this refactor protects.
        let loaded = CrdtDocument::load(&bytes).expect("v2 bytes must load");
        let features = loaded.ordered_features();
        assert_eq!(features.len(), 1);
        assert_eq!(
            features[0].1.params.get("name").map(|(v, _)| v),
            Some(&Value::String("MyCube".into())),
        );
    }

    #[test]
    fn test_migrate_fillet_wires_input_and_materializes_single_root() {
        // Regression: the pork-chop z-fighting came back via the v1 load path —
        // migrate_v1 flattened a Fillet root into one feature and dropped the
        // reference to the extrude it was rounding. After migration the fillet
        // had no `input` FeatureRef, so `collect_consumed_feature_ids` in the
        // materializer saw neither feature as consumed and both were scene
        // roots, producing the z-fighting sawtooth.
        use crate::materializer::materialize;
        use vcad_ir::SceneEntry;

        let mut doc = Document::new();
        // Sketch
        let sketch_id = 1;
        doc.nodes.insert(
            sketch_id,
            vcad_ir::Node {
                id: sketch_id,
                name: None,
                op: CsgOp::Sketch2D {
                    holes: None,
                    origin: Vec3::new(0.0, 0.0, 0.0),
                    x_dir: Vec3::new(1.0, 0.0, 0.0),
                    y_dir: Vec3::new(0.0, 1.0, 0.0),
                    segments: vec![
                        vcad_ir::SketchSegment2D::Line {
                            start: vcad_ir::Vec2 { x: 0.0, y: 0.0 },
                            end: vcad_ir::Vec2 { x: 10.0, y: 0.0 },
                        },
                        vcad_ir::SketchSegment2D::Line {
                            start: vcad_ir::Vec2 { x: 10.0, y: 0.0 },
                            end: vcad_ir::Vec2 { x: 10.0, y: 10.0 },
                        },
                        vcad_ir::SketchSegment2D::Line {
                            start: vcad_ir::Vec2 { x: 10.0, y: 10.0 },
                            end: vcad_ir::Vec2 { x: 0.0, y: 10.0 },
                        },
                        vcad_ir::SketchSegment2D::Line {
                            start: vcad_ir::Vec2 { x: 0.0, y: 10.0 },
                            end: vcad_ir::Vec2 { x: 0.0, y: 0.0 },
                        },
                    ],
                },
            },
        );
        // Extrude -> scale -> rotate -> translate (the materializer-produced chain)
        let extrude_id = 2;
        doc.nodes.insert(
            extrude_id,
            vcad_ir::Node {
                id: extrude_id,
                name: Some("Chop Meat".to_string()),
                op: CsgOp::Extrude {
                    sketch: sketch_id,
                    direction: Vec3::new(0.0, 0.0, 20.0),
                    twist_angle: None,
                    scale_end: None,
                },
            },
        );
        let scale_e = 3;
        doc.nodes.insert(
            scale_e,
            vcad_ir::Node {
                id: scale_e,
                name: None,
                op: CsgOp::Scale {
                    child: extrude_id,
                    factor: Vec3::new(1.0, 1.0, 1.0),
                },
            },
        );
        let rotate_e = 4;
        doc.nodes.insert(
            rotate_e,
            vcad_ir::Node {
                id: rotate_e,
                name: None,
                op: CsgOp::Rotate {
                    child: scale_e,
                    angles: Vec3::new(0.0, 0.0, 0.0),
                },
            },
        );
        let translate_e = 5;
        doc.nodes.insert(
            translate_e,
            vcad_ir::Node {
                id: translate_e,
                name: Some("Chop Meat".to_string()),
                op: CsgOp::Translate {
                    child: rotate_e,
                    offset: Vec3::new(0.0, 0.0, 0.0),
                },
            },
        );
        // Fillet -> scale -> rotate -> translate
        let fillet_id = 6;
        doc.nodes.insert(
            fillet_id,
            vcad_ir::Node {
                id: fillet_id,
                name: Some("Meat Rounding".to_string()),
                op: CsgOp::Fillet {
                    child: translate_e,
                    radius: 5.0,
                },
            },
        );
        let scale_f = 7;
        doc.nodes.insert(
            scale_f,
            vcad_ir::Node {
                id: scale_f,
                name: None,
                op: CsgOp::Scale {
                    child: fillet_id,
                    factor: Vec3::new(1.0, 1.0, 1.0),
                },
            },
        );
        let rotate_f = 8;
        doc.nodes.insert(
            rotate_f,
            vcad_ir::Node {
                id: rotate_f,
                name: None,
                op: CsgOp::Rotate {
                    child: scale_f,
                    angles: Vec3::new(0.0, 0.0, 0.0),
                },
            },
        );
        let translate_f = 9;
        doc.nodes.insert(
            translate_f,
            vcad_ir::Node {
                id: translate_f,
                name: Some("Meat Rounding".to_string()),
                op: CsgOp::Translate {
                    child: rotate_f,
                    offset: Vec3::new(0.0, 0.0, 0.0),
                },
            },
        );

        // v1 doc WITH BOTH the pre-fillet extrude AND the fillet as roots —
        // exactly the shape a "save-then-reload" would produce before the
        // consumed-roots fix landed. The migration must wire the fillet's
        // `input` FeatureRef so the extrude is consumed and not re-rooted.
        doc.roots.push(SceneEntry {
            root: translate_e,
            material: "abs-red".to_string(),
            visible: None,
        });
        doc.roots.push(SceneEntry {
            root: translate_f,
            material: "default".to_string(),
            visible: None,
        });

        let crdt = migrate_v1(&doc);
        // Two features: extrude + fillet (the migration must NOT create two
        // disconnected features plus a dangling fillet).
        let features = crdt.ordered_features();
        assert_eq!(features.len(), 2, "expected extrude + fillet");

        let fillet_feature = features.iter().find(|(_, f)| f.kind == "fillet").unwrap();
        let input = fillet_feature
            .1
            .params
            .get("input")
            .expect("fillet must have `input` param pointing at the extrude");
        match &input.0 {
            Value::FeatureRef(_) => {}
            other => panic!("fillet input must be FeatureRef, got {other:?}"),
        }

        let result = materialize(&crdt);
        assert_eq!(
            result.document.roots.len(),
            1,
            "v1 migration of extrude+fillet must produce exactly 1 root; got {:#?}",
            result.document.roots
        );
    }
}

#[cfg(test)]
mod timeline_roundtrip_tests {
    use super::*;
    use crate::materializer::materialize;

    #[test]
    fn timeline_survives_migrate_and_materialize() {
        let timeline = serde_json::from_value(serde_json::json!({
            "durationS": 2.0,
            "fps": 30.0,
            "tracks": [{
                "target": {"type": "Joint", "jointId": "j0"},
                "keys": [
                    {"t": 0.0, "value": 0.0, "ease": "linear"},
                    {"t": 2.0, "value": 360.0, "ease": "linear"}
                ]
            }],
            "camera": []
        }))
        .ok();
        let doc = Document {
            timeline,
            ..Document::default()
        };
        assert!(doc.timeline.is_some());

        let crdt = migrate_v1(&doc);
        let result = materialize(&crdt);
        let tl = result.document.timeline.expect("timeline materialized");
        assert_eq!(tl.tracks.len(), 1);
        assert!((tl.duration_s - 2.0).abs() < 1e-9);
    }
}
