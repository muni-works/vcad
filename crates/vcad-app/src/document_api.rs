//! High-level document API — typed mutations over the CRDT.
//!
//! `DocumentApi` wraps `CrdtDocument` and provides typed methods that accept
//! `FeatureInput` values. It manages stable ID mapping and materializes after
//! each mutation.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use vcad_crdt::{CrdtDocument, FeatureId, FractionalIndex, ReplicaId, Value};
use vcad_ir::Document;

use crate::feature::FeatureInput;
use crate::command::CommandError;
use crate::materializer::{feature_input_ids, materialize, MaterializeResult};
use crate::part_info::PartInfo;

/// Bidirectional mapping between stable string IDs and CRDT FeatureIds.
///
/// Stable IDs are formatted as `"replica:seq"` (e.g. `"1738000000:0"`).
/// This struct is intentionally simple — it's just the string formatting
/// that the rest of the system already uses.
#[derive(Debug, Clone, Default)]
pub struct StableIdMap {
    /// FeatureId → stable string.
    fid_to_stable: HashMap<FeatureId, String>,
    /// Stable string → FeatureId.
    stable_to_fid: HashMap<String, FeatureId>,
}

impl StableIdMap {
    /// Create an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a mapping.
    pub fn insert(&mut self, fid: FeatureId) -> String {
        let stable = format!("{}:{}", fid.0 .0, fid.1);
        self.fid_to_stable.insert(fid, stable.clone());
        self.stable_to_fid.insert(stable.clone(), fid);
        stable
    }

    /// Look up a FeatureId by its stable string.
    pub fn resolve(&self, stable_id: &str) -> Option<FeatureId> {
        self.stable_to_fid.get(stable_id).copied()
    }

    /// Look up a stable string by FeatureId.
    pub fn stable_id(&self, fid: FeatureId) -> Option<&str> {
        self.fid_to_stable.get(&fid).map(|s| s.as_str())
    }

    /// Rebuild the map from the CRDT's current features.
    pub fn rebuild(&mut self, crdt: &CrdtDocument) {
        self.fid_to_stable.clear();
        self.stable_to_fid.clear();
        for (fid, _) in crdt.ordered_features() {
            self.insert(fid);
        }
        // Also include deleted features that have been seen
        // (they may still be referenced by other features).
    }
}

/// Result returned by every `DocumentApi` mutation.
#[derive(Debug, Clone)]
pub struct ApiResult {
    /// The materialized IR document.
    pub document: Document,
    /// Part metadata for each materialized feature.
    pub parts: Vec<PartInfo>,
    /// Stable IDs of parts consumed by other features (e.g. boolean inputs).
    pub consumed_part_ids: Vec<String>,
    /// Stable ID of the newly created feature, if any.
    pub created_feature_id: Option<String>,
    /// Non-fatal issues detected during materialization (e.g. dangling
    /// input references). Consumers should surface these to the user.
    pub warnings: Vec<String>,
    /// Features removed by this call; empty except for a delete.
    pub deleted: Vec<DeletedFeature>,
}

/// One feature a delete removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct DeletedFeature {
    /// Stable id.
    pub id: String,
    /// Feature name, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub name: Option<String>,
    /// Feature kind (`cube`, `boolean`, ...).
    pub kind: String,
}

impl ApiResult {
    /// Build from a MaterializeResult, computing consumed parts.
    fn from_materialize(result: MaterializeResult, created_id: Option<String>) -> Self {
        let consumed = compute_consumed_part_ids(&result.parts);
        Self {
            document: result.document,
            parts: result.parts,
            consumed_part_ids: consumed,
            created_feature_id: created_id,
            warnings: result.warnings,
            deleted: Vec::new(),
        }
    }
}

/// High-level document API.
///
/// Wraps a `CrdtDocument` with typed mutation methods. Each mutation
/// materializes the document and returns the full state.
pub struct DocumentApi {
    crdt: CrdtDocument,
    stable_ids: StableIdMap,
    name: String,
    dirty: bool,
    /// Node ids of the last imported IR document → the stable id of the
    /// feature each became. Valid only until the next applied command:
    /// a host that imported a session IR (e.g. the MCP server) addresses
    /// nodes by *those* ids until it adopts the engine's renumbered output.
    import_nodes: Option<HashMap<vcad_ir::NodeId, String>>,
}

impl DocumentApi {
    /// Create a new empty document.
    pub fn new(replica_id: ReplicaId) -> Self {
        Self {
            crdt: CrdtDocument::new(replica_id),
            stable_ids: StableIdMap::new(),
            name: "Untitled".into(),
            dirty: false,
            import_nodes: None,
        }
    }

    /// Add a feature with typed input.
    pub fn add_feature(&mut self, input: FeatureInput) -> ApiResult {
        let (kind, params) = input.to_crdt_params();
        let position = self.append_position();
        let (fid, _cs) = self.crdt.create_feature(kind, position, params);
        self.advance_marker_to(fid);
        let stable_id = self.stable_ids.insert(fid);
        self.dirty = true;
        self.build_result(Some(stable_id))
    }

    /// Where a new feature goes: the end of the history, or — while the
    /// history is rolled back — right after the marker, so it is built.
    fn append_position(&self) -> FractionalIndex {
        let ordered = self.crdt.ordered_features();
        let marker = ordered.iter().position(|(_, f)| {
            matches!(
                f.params.get(crate::materializer::ROLLBACK_MARKER),
                Some((Value::Bool(true), _))
            )
        });
        match marker {
            Some(i) => FractionalIndex::between(
                Some(&ordered[i].1.position),
                ordered.get(i + 1).map(|(_, f)| &f.position),
            ),
            None => match ordered.last() {
                Some(last) => FractionalIndex::between(Some(&last.1.position), None),
                None => FractionalIndex::between(None, None),
            },
        }
    }

    /// Move the rollback marker (if set) onto `fid`, a feature just
    /// inserted at it, so it stays the last built step.
    fn advance_marker_to(&mut self, fid: FeatureId) {
        let key = crate::materializer::ROLLBACK_MARKER;
        let Some(old) = self
            .crdt
            .ordered_features()
            .iter()
            .find(|(id, f)| *id != fid && matches!(f.params.get(key), Some((Value::Bool(true), _))))
            .map(|(id, _)| *id)
        else {
            return;
        };
        self.crdt.set_param(old, key, Value::Bool(false));
        self.crdt.set_param(fid, key, Value::Bool(true));
    }

    /// Create a feature from a raw kind + params map, for features that have no
    /// typed [`FeatureInput`] constructor (e.g. `pcb-board`, which carries a
    /// large serialized `board` param). Mirrors [`add_feature`](Self::add_feature)
    /// — crucially it registers a stable id, so the feature can later be
    /// transformed (`set_translation`/`set_rotation`/`set_scale`), updated, or
    /// deleted by id. Without this a board would be unmovable.
    pub fn create_feature_raw(&mut self, kind: &str, params: HashMap<String, Value>) -> ApiResult {
        let (fid, _cs) = self.crdt.create_feature(kind, self.append_position(), params);
        self.advance_marker_to(fid);
        let stable_id = self.stable_ids.insert(fid);
        self.dirty = true;
        self.build_result(Some(stable_id))
    }

    /// Update all params on an existing feature by replacing with new input.
    pub fn update_feature(&mut self, stable_id: &str, input: FeatureInput) -> ApiResult {
        if let Some(fid) = self.stable_ids.resolve(stable_id) {
            let hoist = matches!(input, FeatureInput::Hull { .. });
            let (_kind, params) = input.to_crdt_params();
            for (key, value) in params {
                self.crdt.set_param(fid, &key, value);
            }
            if hoist {
                self.hoist_dependencies(fid);
            }
            self.dirty = true;
        }
        self.build_result(None)
    }

    /// Move everything `target` now depends on to just before it. Features
    /// materialize in list order, so a hull that gains a part made after it
    /// would otherwise lose that input.
    fn hoist_dependencies(&mut self, target: FeatureId) {
        let ordered = self.crdt.ordered_features();
        let Some(at) = ordered.iter().position(|(id, _)| *id == target) else {
            return;
        };
        let refs_of = |i: usize| -> Vec<String> {
            ordered[i]
                .1
                .params
                .values()
                .flat_map(|(v, _)| match v {
                    Value::FeatureRef(r) => vec![r.clone()],
                    Value::FeatureRefList(rs) => rs.clone(),
                    _ => Vec::new(),
                })
                .collect()
        };
        let mut late = std::collections::BTreeSet::new();
        let mut stack = refs_of(at);
        while let Some(r) = stack.pop() {
            let Some(fid) = self.stable_ids.resolve(&r) else {
                continue;
            };
            let Some(i) = ordered.iter().position(|(id, _)| *id == fid) else {
                continue;
            };
            if i > at && late.insert(i) {
                stack.extend(refs_of(i));
            }
        }
        let moves: Vec<FeatureId> = late.iter().map(|&i| ordered[i].0).collect();
        for fid in moves {
            let now = self.crdt.ordered_features();
            let Some(t) = now.iter().position(|(id, _)| *id == target) else {
                return;
            };
            let before = t.checked_sub(1).map(|b| &now[b].1.position);
            let pos = FractionalIndex::between(before, Some(&now[t].1.position));
            self.crdt.move_feature(fid, pos);
        }
    }

    /// Delete a feature.
    ///
    /// Inputs that only this feature used go with it (a boolean's operands,
    /// and theirs), as one undo step; otherwise they would resurface as new
    /// scene parts. An input another feature still references stays. The
    /// result's `deleted` lists everything that went.
    pub fn delete_feature(&mut self, stable_id: &str) -> ApiResult {
        let Some(fid) = self.stable_ids.resolve(stable_id) else {
            return self.build_result(None);
        };
        let doomed = self.exclusive_inputs_closure(fid);
        let mut deleted = Vec::with_capacity(doomed.len());
        let depth = self.crdt.undo_depth();
        for id in doomed {
            if let Some(feature) = self.crdt.get_feature(id) {
                let name = match feature.params.get("name") {
                    Some((Value::String(s), _)) => Some(s.clone()),
                    _ => None,
                };
                deleted.push(DeletedFeature {
                    id: crate::materializer::fid_to_string(id),
                    name,
                    kind: feature.kind.clone(),
                });
            }
            self.crdt.delete_feature(id);
        }
        self.crdt.coalesce_undo_since(depth);
        self.dirty = true;
        let mut result = self.build_result(None);
        result.deleted = deleted;
        result
    }

    /// `root` and every input reachable from it that nothing outside the set
    /// references, root first.
    fn exclusive_inputs_closure(&self, root: FeatureId) -> Vec<FeatureId> {
        use crate::materializer::{feature_input_ids, fid_to_string};
        let features = self.crdt.ordered_features();
        let by_id: HashMap<String, FeatureId> = features
            .iter()
            .map(|(f, _)| (fid_to_string(*f), *f))
            .collect();
        // Referencing features per feature, part-def sources and colliders
        // included: a part def keeps its geometry alive.
        let mut users: HashMap<String, Vec<String>> = HashMap::new();
        for (fid, feature) in &features {
            let me = fid_to_string(*fid);
            let mut refs = feature_input_ids(feature);
            if feature.kind == "part-def" {
                if let Some((Value::FeatureRefList(ids), _)) = feature.params.get("colliders") {
                    refs.extend(ids.iter().cloned());
                }
                if let Some((Value::FeatureRef(id), _)) = feature.params.get("source_feature") {
                    refs.push(id.clone());
                }
            }
            for r in refs {
                users.entry(r).or_default().push(me.clone());
            }
        }
        let root_id = fid_to_string(root);
        let mut doomed: HashSet<String> = HashSet::from([root_id.clone()]);
        let mut order = vec![root];
        // Repeat until stable: an input shared by two doomed features only
        // qualifies once both are in the set.
        loop {
            let mut grew = false;
            for fid in order.clone() {
                let Some(feature) = self.crdt.get_feature(fid) else {
                    continue;
                };
                for input in feature_input_ids(feature) {
                    let Some(&input_fid) = by_id.get(&input) else {
                        continue;
                    };
                    let exclusive = users
                        .get(&input)
                        .is_none_or(|u| u.iter().all(|x| doomed.contains(x)));
                    if exclusive && doomed.insert(input) {
                        order.push(input_fid);
                        grew = true;
                    }
                }
            }
            if !grew {
                return order;
            }
        }
    }

    /// Set translation offset.
    pub fn set_translation(&mut self, stable_id: &str, offset: [f64; 3]) -> ApiResult {
        self.set_vec3_param(stable_id, "offset", offset)
    }

    /// Set rotation angles.
    pub fn set_rotation(&mut self, stable_id: &str, angles: [f64; 3]) -> ApiResult {
        self.set_vec3_param(stable_id, "rotation", angles)
    }

    /// Set scale factor.
    pub fn set_scale(&mut self, stable_id: &str, factor: [f64; 3]) -> ApiResult {
        self.set_vec3_param(stable_id, "scale", factor)
    }

    /// Set material.
    pub fn set_material(&mut self, stable_id: &str, material: &str) -> ApiResult {
        self.set_str_param(stable_id, "material", material)
    }

    /// Add (or replace) the definition of material `key` in the document's
    /// `materials` table, so a preset picked by key renders and survives a
    /// save/reload. The table lives on a `doc-extras` feature (see
    /// `materialize_doc_extras`); one is created when the document has none.
    pub fn define_material(&mut self, key: &str, def: &vcad_ir::MaterialDef) -> ApiResult {
        let def = serde_json::to_value(def).unwrap_or_default();
        let extras_of = |f: &vcad_crdt::FeatureState| match f.params.get("extras") {
            Some((Value::String(s), _)) => serde_json::from_str::<serde_json::Value>(s)
                .ok()
                .and_then(|v| match v {
                    serde_json::Value::Object(m) => Some(m),
                    _ => None,
                }),
            _ => None,
        };
        // The materializer fills `materials` from the first doc-extras that
        // carries the key, so that is the one to edit.
        let ordered = self.crdt.ordered_features();
        let candidates: Vec<_> = ordered
            .iter()
            .filter(|(_, f)| f.kind == "doc-extras")
            .map(|(fid, f)| (*fid, extras_of(f).unwrap_or_default()))
            .collect();
        let target = candidates
            .iter()
            .find(|(_, m)| m.contains_key("materials"))
            .or(candidates.first())
            .cloned();
        match target {
            Some((fid, mut extras)) => {
                let materials = extras
                    .entry("materials")
                    .or_insert_with(|| serde_json::Value::Object(Default::default()));
                if !materials.is_object() {
                    *materials = serde_json::Value::Object(Default::default());
                }
                materials[key] = def;
                let blob = serde_json::Value::Object(extras).to_string();
                self.crdt.set_param(fid, "extras", Value::String(blob));
                self.dirty = true;
                self.build_result(None)
            }
            None => {
                let blob = serde_json::json!({ "materials": { key: def } }).to_string();
                let params = HashMap::from([("extras".to_string(), Value::String(blob))]);
                let mut r = self.create_feature_raw("doc-extras", params);
                r.created_feature_id = None;
                r
            }
        }
    }

    /// Set visibility.
    pub fn set_visible(&mut self, stable_id: &str, visible: bool) -> ApiResult {
        if let Some(fid) = self.stable_ids.resolve(stable_id) {
            self.crdt.set_param(fid, "visible", Value::Bool(visible));
            self.dirty = true;
        }
        self.build_result(None)
    }

    /// Rename a feature.
    pub fn rename_feature(&mut self, stable_id: &str, name: &str) -> ApiResult {
        self.set_str_param(stable_id, "name", name)
    }

    /// Why moving `stable_id` to `new_index` would break the history, if it
    /// would: the feature has to stay after every input it is built on, and
    /// before every feature built on it. `None` when the move is legal (or
    /// the id is unknown — the caller reports that separately).
    pub fn reorder_violation(&self, stable_id: &str, new_index: u32) -> Option<CommandError> {
        let fid = self.stable_ids.resolve(stable_id)?;
        let ordered = self.crdt.ordered_features();
        let me = ordered.iter().find(|(id, _)| *id == fid)?.1;
        let rest: Vec<_> = ordered.iter().filter(|(id, _)| *id != fid).collect();
        let idx = (new_index as usize).min(rest.len());
        let sid = |id: FeatureId| {
            self.stable_ids
                .stable_id(id)
                .map(str::to_string)
                .unwrap_or_else(|| crate::materializer::fid_to_string(id))
        };
        let inputs = feature_input_ids(me);
        // An input that would land after the moved feature.
        if let Some((id, _)) = rest[idx..].iter().find(|(id, _)| inputs.contains(&sid(*id))) {
            let other = sid(*id);
            return Some(CommandError::OrderViolation {
                id: stable_id.to_string(),
                reason: format!("it is built on `{other}`, so it has to stay after it"),
                conflicts_with: other,
            });
        }
        // A consumer that would land before it.
        if let Some((id, _)) = rest[..idx]
            .iter()
            .find(|(_, f)| feature_input_ids(f).iter().any(|i| i == stable_id))
        {
            let other = sid(*id);
            return Some(CommandError::OrderViolation {
                id: stable_id.to_string(),
                reason: format!("`{other}` is built on it, so it has to stay before `{other}`"),
                conflicts_with: other,
            });
        }
        None
    }

    /// Reorder a feature to a new position.
    pub fn reorder_feature(&mut self, stable_id: &str, new_index: u32) -> ApiResult {
        if let Some(fid) = self.stable_ids.resolve(stable_id) {
            // Neighbours are computed without the moving feature, so `index`
            // is its final position in the list.
            let ordered: Vec<_> = self
                .crdt
                .ordered_features()
                .into_iter()
                .filter(|(id, _)| *id != fid)
                .collect();
            let idx = new_index as usize;

            let before = if idx > 0 {
                ordered.get(idx - 1).map(|(_, f)| &f.position)
            } else {
                None
            };
            let after = ordered.get(idx).map(|(_, f)| &f.position);

            let pos = FractionalIndex::between(before, after);
            self.crdt.move_feature(fid, pos);
            self.dirty = true;
        }
        self.build_result(None)
    }

    /// Duplicate features.
    pub fn duplicate_features(&mut self, stable_ids: &[String]) -> ApiResult {
        let mut last_created = None;

        for sid in stable_ids {
            if let Some(fid) = self.stable_ids.resolve(sid) {
                // Read the feature's kind and params
                let ordered = self.crdt.ordered_features();
                if let Some((_, feature)) = ordered.iter().find(|(id, _)| *id == fid) {
                    let kind = feature.kind.clone();
                    let params: HashMap<String, Value> = feature
                        .params
                        .iter()
                        .map(|(k, (v, _))| (k.clone(), v.clone()))
                        .collect();

                    // Insert at end
                    let ordered = self.crdt.ordered_features();
                    let position = if let Some(last) = ordered.last() {
                        FractionalIndex::between(Some(&last.1.position), None)
                    } else {
                        FractionalIndex::between(None, None)
                    };

                    let (new_fid, _) = self.crdt.create_feature(&kind, position, params);
                    last_created = Some(self.stable_ids.insert(new_fid));
                }
            }
        }
        self.dirty = true;
        self.build_result(last_created)
    }

    /// Set joint state value.
    pub fn set_joint_state(&mut self, stable_id: &str, state: f64) -> ApiResult {
        if let Some(fid) = self.stable_ids.resolve(stable_id) {
            self.crdt.set_param(fid, "state", Value::F64(state));
            self.dirty = true;
        }
        self.build_result(None)
    }

    /// Undo the last action.
    pub fn undo(&mut self) -> ApiResult {
        self.crdt.undo();
        self.dirty = true;
        self.build_result(None)
    }

    /// Redo the last undone action.
    pub fn redo(&mut self) -> ApiResult {
        self.crdt.redo();
        self.dirty = true;
        self.build_result(None)
    }

    /// Whether undo is available.
    pub fn can_undo(&self) -> bool {
        self.crdt.can_undo()
    }

    /// Whether redo is available.
    pub fn can_redo(&self) -> bool {
        self.crdt.can_redo()
    }

    /// Save the document to bytes.
    pub fn save(&self) -> Vec<u8> {
        self.crdt.save()
    }

    /// Load a document from bytes.
    pub fn load(bytes: &[u8], replica_id: ReplicaId) -> Result<Self, String> {
        let crdt = CrdtDocument::load(bytes).map_err(|e| e.to_string())?;
        let mut api = Self {
            crdt,
            stable_ids: StableIdMap::new(),
            name: "Untitled".into(),
            dirty: false,
            import_nodes: None,
        };
        let _ = replica_id;
        api.stable_ids.rebuild(&api.crdt);
        Ok(api)
    }

    /// Build a fresh document (empty history) from an IR `Document` JSON —
    /// how a native host opens a `.vcad` file. Fails closed on JSON that
    /// isn't an IR document, unlike [`import_ir`](Self::import_ir).
    pub fn from_ir(ir_json: &str, replica_id: ReplicaId) -> Result<Self, String> {
        serde_json::from_str::<vcad_ir::Document>(ir_json).map_err(|e| e.to_string())?;
        let mut api = Self::new(replica_id);
        api.import_ir(ir_json);
        api.dirty = false;
        Ok(api)
    }

    /// Replace the document contents (including undo/redo history) with a
    /// snapshot from [`save`](Self::save). Fails closed on bad bytes.
    pub fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<(), String> {
        let crdt = CrdtDocument::load(bytes).map_err(|e| e.to_string())?;
        self.crdt = crdt;
        self.stable_ids.rebuild(&self.crdt);
        self.dirty = true;
        self.import_nodes = None;
        Ok(())
    }

    /// Replace the document contents with a snapshot from [`save`](Self::save),
    /// keeping the name. Used to roll back a failed command batch.
    pub(crate) fn restore(&mut self, bytes: &[u8]) {
        if let Ok(crdt) = CrdtDocument::load(bytes) {
            self.crdt = crdt;
            self.stable_ids.rebuild(&self.crdt);
        }
    }

    /// Whether `stable_id` names a live (not deleted) feature.
    pub fn feature_exists(&self, stable_id: &str) -> bool {
        self.stable_ids.resolve(stable_id).is_some_and(|fid| {
            self.crdt
                .ordered_features()
                .iter()
                .any(|(id, _)| *id == fid)
        })
    }

    /// A feature's `Vec3` param (e.g. `offset`, `rotation`), if set.
    pub fn feature_vec3(&self, stable_id: &str, key: &str) -> Option<[f64; 3]> {
        let fid = self.stable_ids.resolve(stable_id)?;
        let ordered = self.crdt.ordered_features();
        let (_, feature) = ordered.iter().find(|(id, _)| *id == fid)?;
        match &feature.params.get(key)?.0 {
            Value::Vec3(v) => Some(*v),
            _ => None,
        }
    }

    /// World-space bounding box `(min, max)` of a part, by evaluating its
    /// root node with the full `vcad-eval` evaluator (transforms included).
    /// `None` for an unknown id, a part that failed to evaluate, or one
    /// whose solid is empty (no sentinel `±f64::MAX` box ever escapes).
    /// Use [`DocumentApi::part_evaluation`] to tell those apart.
    pub fn part_world_bbox(&self, stable_id: &str) -> Option<([f64; 3], [f64; 3])> {
        self.part_evaluation(stable_id)?.ok()??.try_bounding_box()
    }

    /// Evaluate one part's root. `None` for an unknown id; `Some(Err)` with
    /// the evaluator's reason when the part failed (an overlarge fillet, an
    /// impossible shell, …); `Some(Ok(None))` when it produced no solid.
    pub fn part_evaluation(
        &self,
        stable_id: &str,
    ) -> Option<Result<Option<vcad_kernel::Solid>, String>> {
        let state = self.get_state();
        let part = state.parts.iter().find(|p| p.id() == stable_id)?;
        let mut cache = HashMap::new();
        Some(
            vcad_eval::evaluate::evaluate_node(
                part.root_node_id(),
                &state.document.nodes,
                &mut cache,
            )
            .map_err(|e| e.to_string()),
        )
    }

    /// One line per part whose geometry failed to evaluate, `"<id>
    /// \"<name>\": <reason>"`. Empty when every part evaluates. Loading a
    /// document never fails on these (the user may be mid-fix), so a caller
    /// that wants fail-closed loading — the agent's `create_cad_loon` —
    /// checks this after the load.
    pub fn evaluation_errors(&self) -> Vec<String> {
        let state = self.get_state();
        state
            .parts
            .iter()
            .filter_map(|part| {
                let id = part.id();
                let err = self.part_evaluation(id)?.err()?;
                let name = serde_json::to_value(part)
                    .ok()
                    .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
                    .unwrap_or_default();
                Some(format!("{id} \"{name}\": {err}"))
            })
            .collect()
    }

    /// Deterministic fingerprint of the materialized state (IR document +
    /// parts): FNV-1a 64 over key-sorted JSON, as 16 hex digits. The parity
    /// harness compares it across native, WASM and C-ABI hosts, so it must
    /// not depend on map iteration order or serde feature unification.
    pub fn state_hash(&self) -> String {
        let state = self.get_state();
        let value = serde_json::json!({ "document": state.document, "parts": state.parts });
        let mut canonical = String::new();
        write_canonical(&value, &mut canonical);
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in canonical.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{h:016x}")
    }

    /// Import IR JSON into the document.
    pub fn import_ir(&mut self, ir_json: &str) -> ApiResult {
        if let Ok(doc) = serde_json::from_str::<vcad_ir::Document>(ir_json) {
            let (migrated, map) = crate::migrate::migrate_v1_mapped(&doc);
            let ops = migrated.ops_since(&std::collections::BTreeMap::new());
            self.crdt.merge(ops);
            self.stable_ids.rebuild(&self.crdt);
            self.dirty = true;
            self.import_nodes = Some(
                map.mappings
                    .into_iter()
                    .map(|m| (m.old_node_id, m.feature_id))
                    .collect(),
            );
        }
        self.build_result(None)
    }

    /// The feature an IR node of the last imported document became, while
    /// that import is still the host's view (see `import_nodes`).
    pub fn imported_node_feature(&self, node: vcad_ir::NodeId) -> Option<&str> {
        self.import_nodes.as_ref()?.get(&node).map(String::as_str)
    }

    /// Whether node ids from the last import are still the host's view.
    pub fn has_import_view(&self) -> bool {
        self.import_nodes.is_some()
    }

    /// Forget the import's node ids: after an applied command the host
    /// adopts the engine's materialized document and its node ids.
    pub(crate) fn clear_import_view(&mut self) {
        self.import_nodes = None;
    }

    /// Get the current document state.
    pub fn get_state(&self) -> ApiResult {
        self.build_result(None)
    }

    /// Get a raw param value by key.
    pub fn set_param(&mut self, stable_id: &str, key: &str, value: Value) -> ApiResult {
        if let Some(fid) = self.stable_ids.resolve(stable_id) {
            self.crdt.set_param(fid, key, value);
            self.dirty = true;
        }
        self.build_result(None)
    }

    // -- Metadata --

    /// Get the document name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Set the document name.
    pub fn set_name(&mut self, name: &str) {
        self.name = name.into();
    }

    /// Whether the document has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the document as saved.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Access the underlying CRDT document (for sync, ordered features, etc.).
    pub fn crdt(&self) -> &CrdtDocument {
        &self.crdt
    }

    /// Mutable access to the CRDT (for sync operations).
    pub fn crdt_mut(&mut self) -> &mut CrdtDocument {
        &mut self.crdt
    }

    /// Access the stable ID map.
    pub fn stable_ids(&self) -> &StableIdMap {
        &self.stable_ids
    }

    /// Rebuild the stable-id map from the current CRDT state.
    ///
    /// Must be called after loading or replacing the CRDT document so that
    /// existing features can be resolved by their stable IDs.
    pub fn rebuild_stable_ids(&mut self) {
        self.stable_ids.rebuild(&self.crdt);
    }

    // -- Internal helpers --

    fn set_vec3_param(&mut self, stable_id: &str, key: &str, value: [f64; 3]) -> ApiResult {
        if let Some(fid) = self.stable_ids.resolve(stable_id) {
            self.crdt.set_param(fid, key, Value::Vec3(value));
            self.dirty = true;
        }
        self.build_result(None)
    }

    fn set_str_param(&mut self, stable_id: &str, key: &str, value: &str) -> ApiResult {
        if let Some(fid) = self.stable_ids.resolve(stable_id) {
            self.crdt.set_param(fid, key, Value::String(value.into()));
            self.dirty = true;
        }
        self.build_result(None)
    }

    fn build_result(&self, created_id: Option<String>) -> ApiResult {
        let result = materialize(&self.crdt);
        ApiResult::from_materialize(result, created_id)
    }
}

/// Serialize `v` as JSON with object keys sorted.
fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value as J;
    match v {
        J::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&J::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        J::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Compute which part IDs are consumed (referenced as inputs) by other parts.
///
/// A part is "consumed" if it's used as an input to a boolean, fillet, chamfer,
/// shell, pattern, or mirror operation.
fn compute_consumed_part_ids(parts: &[PartInfo]) -> Vec<String> {
    let mut consumed = Vec::new();
    for part in parts {
        match part {
            PartInfo::Boolean {
                source_part_ids, ..
            } => {
                consumed.extend(source_part_ids.iter().cloned());
            }
            PartInfo::Hull {
                source_part_ids, ..
            } => {
                consumed.extend(source_part_ids.iter().cloned());
            }
            PartInfo::SweepRotate { source_part_id, .. }
            | PartInfo::Fillet { source_part_id, .. }
            | PartInfo::Chamfer { source_part_id, .. }
            | PartInfo::Shell { source_part_id, .. }
            | PartInfo::LinearPattern { source_part_id, .. }
            | PartInfo::CircularPattern { source_part_id, .. }
            | PartInfo::Mirror { source_part_id, .. } => {
                consumed.push(source_part_id.clone());
            }
            _ => {}
        }
    }
    consumed.sort();
    consumed.dedup();
    consumed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::BooleanType;

    fn test_api() -> DocumentApi {
        DocumentApi::new(ReplicaId(1))
    }

    #[test]
    fn test_add_cube() {
        let mut api = test_api();
        let result = api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 20.0,
            size_z: 30.0,
        });
        assert_eq!(result.parts.len(), 1);
        assert!(result.created_feature_id.is_some());
        assert!(api.is_dirty());
    }

    #[test]
    fn test_delete_feature() {
        let mut api = test_api();
        let result = api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        });
        let id = result.created_feature_id.unwrap();

        let result = api.delete_feature(&id);
        assert_eq!(result.parts.len(), 0);
    }

    fn cube_feature(api: &mut DocumentApi) -> String {
        api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        })
        .created_feature_id
        .unwrap()
    }

    fn difference_of(api: &mut DocumentApi, a: &str, b: &str) -> String {
        api.add_feature(FeatureInput::Boolean {
            boolean_type: BooleanType::Difference,
            input_a: a.to_string(),
            input_b: b.to_string(),
        })
        .created_feature_id
        .unwrap()
    }

    #[test]
    fn deleting_a_boolean_takes_its_exclusive_inputs_in_one_undo() {
        let mut api = test_api();
        let a = cube_feature(&mut api);
        let b = cube_feature(&mut api);
        let c = cube_feature(&mut api);
        let inner = difference_of(&mut api, &a, &b);
        let outer = difference_of(&mut api, &inner, &c);
        let r = api.delete_feature(&outer);
        assert!(r.document.roots.is_empty(), "children must not resurface");
        let mut gone: Vec<_> = r.deleted.iter().map(|d| d.id.clone()).collect();
        gone.sort();
        let mut want = vec![a, b, c, inner, outer];
        want.sort();
        assert_eq!(gone, want);
        assert_eq!(
            api.undo().document.roots.len(),
            1,
            "one undo restores it all"
        );
    }

    #[test]
    fn deleting_a_boolean_keeps_an_input_something_else_uses() {
        let mut api = test_api();
        let shared = cube_feature(&mut api);
        let b = cube_feature(&mut api);
        let c = cube_feature(&mut api);
        let keep = difference_of(&mut api, &shared, &b);
        let doomed = difference_of(&mut api, &shared, &c);
        let r = api.delete_feature(&doomed);
        let gone: Vec<_> = r.deleted.iter().map(|d| d.id.as_str()).collect();
        assert!(gone.contains(&c.as_str()));
        assert!(!gone.contains(&shared.as_str()));
        assert_eq!(r.document.roots.len(), 1);
        assert!(r.parts.iter().any(|p| p.id() == keep));
    }

    #[test]
    fn hull_can_gain_a_part_made_after_it() {
        let mut api = DocumentApi::new(ReplicaId(1));
        let cube = |api: &mut DocumentApi, x: f64| {
            api.add_feature(FeatureInput::Cube {
                size_x: x,
                size_y: 10.0,
                size_z: 10.0,
            })
            .created_feature_id
            .unwrap()
        };
        let a = cube(&mut api, 10.0);
        let b = cube(&mut api, 20.0);
        let hull = api
            .add_feature(FeatureInput::Hull {
                inputs: vec![a.clone(), b.clone()],
                points: Vec::new(),
            })
            .created_feature_id
            .unwrap();
        let later = cube(&mut api, 30.0);
        let r = api.update_feature(
            &hull,
            FeatureInput::Hull {
                inputs: vec![a, b, later.clone()],
                points: Vec::new(),
            },
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(r.consumed_part_ids.contains(&later));
        assert_eq!(r.document.roots.len(), 1);
    }

    #[test]
    fn test_consumed_parts() {
        let mut api = test_api();

        let r1 = api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        });
        let id1 = r1.created_feature_id.unwrap();

        let r2 = api.add_feature(FeatureInput::Cylinder {
            radius: 5.0,
            height: 10.0,
            segments: None,
        });
        let id2 = r2.created_feature_id.unwrap();

        let result = api.add_feature(FeatureInput::Boolean {
            boolean_type: BooleanType::Difference,
            input_a: id1.clone(),
            input_b: id2.clone(),
        });

        assert_eq!(result.consumed_part_ids.len(), 2);
        assert!(result.consumed_part_ids.contains(&id1));
        assert!(result.consumed_part_ids.contains(&id2));
    }

    #[test]
    fn test_undo_redo() {
        let mut api = test_api();
        assert!(!api.can_undo());

        api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        });
        assert!(api.can_undo());

        let result = api.undo();
        assert_eq!(result.parts.len(), 0);
        assert!(api.can_redo());

        let result = api.redo();
        assert_eq!(result.parts.len(), 1);
    }

    #[test]
    fn test_set_translation() {
        let mut api = test_api();
        let r = api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        });
        let id = r.created_feature_id.unwrap();

        let result = api.set_translation(&id, [5.0, 10.0, 15.0]);
        assert_eq!(result.parts.len(), 1);
        // Verify the translate node has the right offset
        let translate_id = result.parts[0].root_node_id();
        let node = result.document.nodes.get(&translate_id).unwrap();
        match &node.op {
            vcad_ir::CsgOp::Translate { offset, .. } => {
                assert_eq!(offset.x, 5.0);
                assert_eq!(offset.y, 10.0);
                assert_eq!(offset.z, 15.0);
            }
            _ => panic!("expected Translate"),
        }
    }

    #[test]
    fn create_feature_raw_is_movable() {
        // A feature created via the raw path (e.g. a PCB board) must register a
        // stable id so it can be transformed like any other part. Regression
        // for boards being unmovable.
        let mut api = test_api();
        let r = api.create_feature_raw("pcb-board", HashMap::new());
        let id = r
            .created_feature_id
            .expect("create_feature_raw should return a registered stable id");

        let result = api.set_translation(&id, [5.0, 10.0, 15.0]);
        assert_eq!(result.parts.len(), 1);
        let translate_id = result.parts[0].root_node_id();
        match &result.document.nodes.get(&translate_id).unwrap().op {
            vcad_ir::CsgOp::Translate { offset, .. } => {
                assert_eq!((offset.x, offset.y, offset.z), (5.0, 10.0, 15.0));
            }
            other => panic!("expected Translate, got {other:?}"),
        }
    }

    #[test]
    fn test_save_load_roundtrip() {
        let mut api = test_api();
        api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 20.0,
            size_z: 30.0,
        });

        let bytes = api.save();
        let api2 = DocumentApi::load(&bytes, ReplicaId(2)).unwrap();
        let state = api2.get_state();
        assert_eq!(state.parts.len(), 1);
    }

    #[test]
    fn test_pork_chop_fillet_chat_flow_produces_single_root() {
        // End-to-end regression through the high-level DocumentApi — exactly
        // the path the TS side exercises via `engine.add_feature(JSON)`. A
        // fillet's input-consumption logic must ensure only the fillet, not
        // its pre-fillet extrude source, ends up in `doc.roots`. Otherwise
        // both near-identical shells render and z-fight ("the pork-chop
        // sawtooth").
        let sketch_json = serde_json::to_string(&vcad_ir::CsgOp::Sketch2D {
            holes: None,
            origin: vcad_ir::Vec3::new(0.0, 0.0, 0.0),
            x_dir: vcad_ir::Vec3::new(1.0, 0.0, 0.0),
            y_dir: vcad_ir::Vec3::new(0.0, 1.0, 0.0),
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
        })
        .unwrap();

        let mut api = test_api();

        // 1. Extrude the sketch ("Chop Meat").
        let r1 = api.add_feature(FeatureInput::Extrude {
            sketch: sketch_json.clone(),
            depth: 20.0,
            direction: [0.0, 0.0, 1.0],
            twist_angle: None,
            scale_end: None,
        });
        let extrude_id = r1.created_feature_id.clone().unwrap();

        // 2. Fillet that extrude ("Meat Rounding") — chat path passes the
        //    stable id of the extrude as `input`.
        let r2 = api.add_feature(FeatureInput::Fillet {
            input: extrude_id.clone(),
            radius: 5.0,
        });

        // Extrude is reported as consumed.
        assert!(
            r2.consumed_part_ids.contains(&extrude_id),
            "fillet must report its source as consumed; got consumed={:?}",
            r2.consumed_part_ids
        );

        // 3. Set material on the pre-fillet extrude (as the AI did).
        let r3 = api.set_material(&extrude_id, "abs-red");

        // The document after all of this must have exactly ONE scene root —
        // the fillet. The pre-fillet extrude is consumed and not rooted.
        assert_eq!(
            r3.document.roots.len(),
            1,
            "expected exactly 1 root (fillet); got {} roots: {:#?}",
            r3.document.roots.len(),
            r3.document.roots
        );
    }

    #[test]
    fn test_update_feature() {
        let mut api = test_api();
        let r = api.add_feature(FeatureInput::Cube {
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        });
        let id = r.created_feature_id.unwrap();

        let result = api.update_feature(
            &id,
            FeatureInput::Cube {
                size_x: 20.0,
                size_y: 30.0,
                size_z: 40.0,
            },
        );
        assert_eq!(result.parts.len(), 1);

        // Verify the prim node uses new dimensions
        let prim_id = match &result.parts[0] {
            PartInfo::Cube {
                primitive_node_id, ..
            } => *primitive_node_id,
            _ => panic!("expected cube"),
        };
        match &result.document.nodes.get(&prim_id).unwrap().op {
            vcad_ir::CsgOp::Cube { size } => {
                assert_eq!(size.x, 20.0);
                assert_eq!(size.y, 30.0);
                assert_eq!(size.z, 40.0);
            }
            _ => panic!("expected Cube op"),
        }
    }
}
