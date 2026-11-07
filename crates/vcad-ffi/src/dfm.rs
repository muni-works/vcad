//! Design-for-Manufacturing for the native app (`vcad_dfm_*`): run a
//! `vcad-kernel-dfm` rule pack over every part of a document, annotate the
//! offending faces with viewport face ids, and resolve each issue's
//! suggested fix to a concrete IR patch the app applies as an undoable edit.
//!
//! # Fix resolution
//!
//! The kernel attributes faces to IR nodes coarsely (one root per part) and
//! most rules suggest a `manual` fix ("increase the wall to 0.8 mm"). That is
//! the right shape for a human but not something the app can apply. So each
//! issue whose measurement is a length is resolved here: the primitive field
//! under the part that *is* the measured dimension (a cube side equal to the
//! wall thickness, a cylinder radius equal to the corner radius) is found and
//! patched to the rule limit. When that field is driven by a named document
//! parameter, the parameter is patched instead, so the fix stays parametric.
//! Ambiguous or formula-bound targets are left manual — a fix the app
//! guesses at is worse than one it admits it can't make.

use std::ffi::c_char;
use std::ptr;

use serde::Serialize;
use serde_json::{json, Value};
use vcad_ir::{BindingKey, CsgOp, Document, Expr, NodeId};
use vcad_kernel::vcad_kernel_dfm::{
    self as dfm, geom::provenance::ProvenanceMap, DfmFix, DfmIssue, DfmReport, Process, RulePack,
};

use crate::analysis::{
    evaluate, kernel_index_to_ordinal, parse_doc, parse_request, part_name, part_roots,
};
use crate::err;
use crate::json::{guard_ptr, into_c, json_out};

/// A concrete, applyable IR patch.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DfmPatch {
    /// Set a named document parameter.
    SetParameter {
        /// Parameter name.
        name: String,
        /// Current value.
        from: f64,
        /// New value.
        to: f64,
    },
    /// Set a numeric field on a node's op.
    SetField {
        /// Target node.
        node: NodeId,
        /// Dotted path into the op (`"radius"`, `"size.z"`).
        path: String,
        /// Current value.
        from: f64,
        /// New value.
        to: f64,
    },
}

/// One issue as the app consumes it.
#[derive(Debug, Clone, Serialize)]
pub struct AppIssue {
    /// The kernel issue, verbatim.
    #[serde(flatten)]
    pub issue: DfmIssue,
    /// Part (evaluated scene index) the issue belongs to.
    pub part_index: usize,
    /// Part display name.
    pub part_name: String,
    /// Offending faces as viewport face ids (shell ordinals).
    pub face_ids: Vec<u32>,
    /// Resolved patch, when the fix can be applied automatically.
    pub patch: Option<DfmPatch>,
    /// Why there is no patch, when there isn't one.
    pub patch_note: Option<String>,
}

/// Whole-document report.
#[derive(Debug, Clone, Serialize)]
pub struct AppReport {
    /// Process the pack targets.
    pub process: Process,
    /// Rule pack name.
    pub rule_pack_name: String,
    /// Rule pack version.
    pub rule_pack_version: String,
    /// Issues across all parts.
    pub issues: Vec<AppIssue>,
    /// Per-rule pass/fail (rulesets that report it).
    pub rule_results: Vec<dfm::RuleResult>,
    /// Error / warning / info counts.
    pub counts: Counts,
    /// 0–100, same formula as the MCP `dfm_check` score.
    pub score: u32,
    /// Parts skipped (mesh-only parts under a BRep pack), by name.
    pub skipped_parts: Vec<String>,
}

/// Severity counts.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Counts {
    /// Errors.
    pub error: usize,
    /// Warnings.
    pub warning: usize,
    /// Infos.
    pub info: usize,
}

/// Mirrors `dfmScore` in `packages/mcp/src/tools/dfm.ts`.
fn score(c: Counts) -> u32 {
    100 - (c.error * 30 + c.warning * 10).min(100) as u32
}

/// Resolve the rule pack: explicit TOML > named ruleset > process default.
fn pack_for(req: &Value) -> Result<(Process, RulePack), String> {
    let process_name = req["process"].as_str().unwrap_or("fdm");
    let process =
        Process::from_str(process_name).ok_or_else(|| format!("unknown process: {process_name}"))?;
    if let Some(toml) = req["rule_pack_toml"].as_str().filter(|s| !s.trim().is_empty()) {
        return Ok((
            process,
            RulePack::from_toml(toml).map_err(|e| format!("rule pack: {e}"))?,
        ));
    }
    if let Some(name) = req["ruleset"].as_str().filter(|s| !s.is_empty()) {
        let toml = dfm::DefaultPacks::by_name(name).ok_or_else(|| format!("unknown ruleset: {name}"))?;
        return Ok((
            process,
            RulePack::from_toml(toml).map_err(|e| format!("ruleset {name}: {e}"))?,
        ));
    }
    Ok((process, RulePack::default_for(process)))
}

/// Run DFM over every evaluated part of `doc`.
pub fn check_document(doc: &Document, req: &Value) -> Result<AppReport, String> {
    let (process, pack) = pack_for(req)?;
    let scene = evaluate(doc)?;
    let roots = part_roots(doc, &scene);
    let mut issues = Vec::new();
    let mut rule_results = Vec::new();
    let mut skipped_parts = Vec::new();
    let mut name = pack.name.clone();
    for (i, part) in scene.parts.iter().enumerate() {
        let root = roots[i];
        let pname = part_name(doc, root, i);
        let Some(solid) = part.solid.as_ref() else {
            skipped_parts.push(pname);
            continue;
        };
        let (report, ordinals): (DfmReport, Vec<Option<u32>>) = match solid.as_brep() {
            Some(brep) => {
                let prov = root.map(|r| ProvenanceMap::single_root(brep, r));
                (
                    dfm::run_dfm(brep, prov.as_ref(), process, &pack),
                    kernel_index_to_ordinal(brep),
                )
            }
            None if !pack.ruleset.is_empty() => {
                let mesh = solid.to_mesh(32);
                let tris = dfm::rules::hobby_mill::tris_from_indexed(&mesh.vertices, &mesh.indices);
                (dfm::run_dfm_mesh(&tris, &pack), Vec::new())
            }
            None => {
                skipped_parts.push(pname);
                continue;
            }
        };
        name = report.rule_pack_name.clone();
        rule_results.extend(report.rule_results);
        for issue in report.issues {
            let face_ids = issue
                .face_indices
                .iter()
                .filter_map(|&k| ordinals.get(k).copied().flatten())
                .collect();
            let (patch, patch_note) = match resolve_patch(doc, root, &issue) {
                Ok(p) => (Some(p), None),
                Err(n) => (None, Some(n)),
            };
            issues.push(AppIssue {
                issue,
                part_index: i,
                part_name: pname.clone(),
                face_ids,
                patch,
                patch_note,
            });
        }
    }
    let mut counts = Counts::default();
    for i in &issues {
        match i.issue.severity {
            dfm::DfmSeverity::Error => counts.error += 1,
            dfm::DfmSeverity::Warning => counts.warning += 1,
            dfm::DfmSeverity::Info => counts.info += 1,
        }
    }
    Ok(AppReport {
        process,
        rule_pack_name: name,
        rule_pack_version: pack.version.clone(),
        issues,
        rule_results,
        counts,
        score: score(counts),
        skipped_parts,
    })
}

// ─── fix resolution ──────────────────────────────────────────────────────

fn op_value(doc: &Document, node: NodeId) -> Option<Value> {
    serde_json::to_value(&doc.nodes.get(&node)?.op).ok()
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, k| cur.get(k))
}

/// Numeric fields of the primitives under `start` that a length rule can
/// sensibly drive: `(node, path, current, scale)` where the measured
/// dimension is `scale * current` (2 for a cylinder radius measured as a
/// diameter).
fn length_fields(doc: &Document, start: NodeId) -> Vec<(NodeId, String, f64, f64)> {
    let mut out = Vec::new();
    let mut stack = vec![start];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = doc.nodes.get(&id) else {
            continue;
        };
        match &node.op {
            CsgOp::Cube { size } => {
                out.push((id, "size.x".into(), size.x, 1.0));
                out.push((id, "size.y".into(), size.y, 1.0));
                out.push((id, "size.z".into(), size.z, 1.0));
            }
            CsgOp::Cylinder { radius, height, .. } => {
                out.push((id, "radius".into(), *radius, 1.0));
                out.push((id, "radius".into(), *radius, 2.0));
                out.push((id, "height".into(), *height, 1.0));
            }
            op => stack.extend(op.child_ids()),
        }
    }
    out
}

/// Turn a field target into a patch, routing through a named parameter when
/// the field is bound to one.
fn field_patch(doc: &Document, node: NodeId, path: &str, from: f64, to: f64) -> Result<DfmPatch, String> {
    match doc.bindings.0.get(&BindingKey::new(node, path)) {
        None | Some(Expr::Number(_)) => Ok(DfmPatch::SetField {
            node,
            path: path.to_string(),
            from,
            to,
        }),
        Some(Expr::Formula(f)) => {
            let refuse = || format!("{path} on node {node} is driven by the formula `{f}` — edit it by hand");
            let (name, a, b) = affine(f).ok_or_else(refuse)?;
            match doc.parameters.get(name) {
                Some(p) if p.value.as_number().is_some() && b != 0.0 => Ok(DfmPatch::SetParameter {
                    name: name.to_string(),
                    from: p.value.as_number().unwrap_or(from),
                    // field = a + b·param  ⇒  param = (field − a) / b
                    to: (to - a) / b,
                }),
                _ => Err(refuse()),
            }
        }
    }
}

/// `name`, or loon's lowered `(a) + (b) * name`, as `(name, a, b)`.
fn affine(f: &str) -> Option<(&str, f64, f64)> {
    fn ident(s: &str) -> Option<&str> {
        let s = s.trim();
        (!s.is_empty()
            && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'))
        .then_some(s)
    }
    let f = f.trim();
    if let Some(n) = ident(f) {
        return Some((n, 0.0, 1.0));
    }
    let num = |s: &str| s.trim().trim_start_matches('(').trim_end_matches(')').trim().parse::<f64>().ok();
    let (lhs, rhs) = f.split_once('+')?;
    let (coef, name) = rhs.split_once('*')?;
    Some((ident(name)?, num(lhs)?, num(coef)?))
}

/// Resolve the issue's suggested fix to an applyable patch, or say why not.
pub fn resolve_patch(doc: &Document, root: Option<NodeId>, issue: &DfmIssue) -> Result<DfmPatch, String> {
    // A kernel set_param fix: honour it when the field really is numeric.
    if let Some(DfmFix::SetParam { node, path, value }) = &issue.suggested_fix {
        if let (Some(to), Some(from)) = (
            value.as_f64(),
            op_value(doc, *node).and_then(|v| get_path(&v, path).and_then(Value::as_f64)),
        ) {
            return field_patch(doc, *node, path, from, to);
        }
    }
    if issue.units != "mm" || !(issue.measured > 0.0) || !(issue.limit > 0.0) {
        return Err("no automatic fix for this rule".into());
    }
    let start = issue
        .origin_op
        .or(root)
        .ok_or("part provenance unavailable — fix manually")?;
    let tol = (issue.measured * 0.01).max(1e-3);
    let mut hits: Vec<(NodeId, String, f64, f64)> = length_fields(doc, start)
        .into_iter()
        .filter(|(_, _, v, s)| (v * s - issue.measured).abs() <= tol)
        .collect();
    hits.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    hits.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    match hits.as_slice() {
        [(node, path, from, scale)] => field_patch(doc, *node, path, *from, issue.limit / scale),
        [] => Err(format!(
            "no primitive dimension equals the measured {:.3} mm — fix manually",
            issue.measured
        )),
        many => Err(format!(
            "{} dimensions equal the measured {:.3} mm — ambiguous, fix manually",
            many.len(),
            issue.measured
        )),
    }
}

/// Apply a patch to a copy of `doc`.
pub fn apply_patch(doc: &Document, patch: &DfmPatch) -> Result<Document, String> {
    let mut out = doc.clone();
    match patch {
        DfmPatch::SetParameter { name, to, .. } => {
            let p = out
                .parameters
                .get_mut(name)
                .ok_or_else(|| format!("no parameter `{name}`"))?;
            p.value = Expr::Number(*to);
        }
        DfmPatch::SetField { node, path, to, .. } => {
            let n = out
                .nodes
                .get_mut(node)
                .ok_or_else(|| format!("no node {node}"))?;
            let mut v = serde_json::to_value(&n.op).map_err(|e| e.to_string())?;
            let mut cur = &mut v;
            for k in path.split('.') {
                cur = cur
                    .get_mut(k)
                    .ok_or_else(|| format!("{path} not on node {node}"))?;
            }
            if !cur.is_number() {
                return Err(format!("{path} on node {node} is not numeric"));
            }
            *cur = json!(to);
            n.op = serde_json::from_value(v).map_err(|e| format!("patched op: {e}"))?;
            // A literal binding would re-assert the old value at eval.
            let key = BindingKey::new(*node, path.as_str());
            if let Some(Expr::Number(_)) = out.bindings.0.get(&key) {
                out.bindings.0.insert(key, Expr::Number(*to));
            }
        }
    }
    Ok(out)
}

// ─── C ABI ───────────────────────────────────────────────────────────────

/// Run DFM over a document. `doc_json` is IR JSON; `request_json` (nullable)
/// is `{"process": "fdm"|"cnc_3axis"|…, "rule_pack_toml"?, "ruleset"?}`.
/// Returns the report JSON (free with `vcad_string_free`), or null with
/// `vcad_last_error` set.
#[no_mangle]
pub extern "C" fn vcad_dfm_check(doc_json: *const c_char, request_json: *const c_char) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_dfm_check", || {
        let run = || -> Result<AppReport, String> {
            let doc = parse_doc("document", doc_json)?;
            check_document(&doc, &parse_request(request_json)?)
        };
        match run() {
            Ok(r) => json_out(&r),
            Err(e) => {
                err::set_error(e);
                ptr::null_mut()
            }
        }
    })
}

/// Apply a resolved patch (`issue.patch` from [`vcad_dfm_check`]) to a
/// document. Returns the patched IR JSON, or null with `vcad_last_error`.
#[no_mangle]
pub extern "C" fn vcad_dfm_apply_patch(doc_json: *const c_char, patch_json: *const c_char) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_dfm_apply_patch", || {
        let run = || -> Result<String, String> {
            let doc = parse_doc("document", doc_json)?;
            let patch: DfmPatch = serde_json::from_value(parse_request(patch_json)?)
                .map_err(|e| format!("patch: {e}"))?;
            let out = apply_patch(&doc, &patch)?;
            serde_json::to_string(&out).map_err(|e| e.to_string())
        };
        match run() {
            Ok(s) => into_c(s),
            Err(e) => {
                err::set_error(e);
                ptr::null_mut()
            }
        }
    })
}

/// The processes and named rulesets the app can offer, as JSON
/// `{"processes": [...], "rulesets": [...]}`.
#[no_mangle]
pub extern "C" fn vcad_dfm_processes() -> *mut c_char {
    let processes = [
        "fdm",
        "sla",
        "cnc_3axis",
        "injection",
        "sheet_metal",
        "casting_sand",
        "casting_investment",
    ];
    json_out(&json!({ "processes": processes, "rulesets": ["hobby-3axis-mill"] }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;
    use std::ffi::CString;

    fn doc(src: &str) -> Document {
        vcad_loon::eval_vcad(src, None).expect("loon")
    }

    #[test]
    fn thin_wall_fix_clears_the_violation() {
        // A 0.5 mm plate under FDM (min wall 0.8 mm).
        let d = doc("[root [cube 40 30 0.5] \"pla\"]");
        let req = json!({ "process": "fdm" });
        let before = check_document(&d, &req).unwrap();
        let thin: Vec<_> = before
            .issues
            .iter()
            .filter(|i| i.issue.rule == "fdm.thin_wall")
            .collect();
        assert!(!thin.is_empty(), "plate should be flagged: {before:#?}");
        assert!(!thin[0].face_ids.is_empty());
        let patch = thin[0].patch.clone().expect("thin wall resolves to a patch");
        assert!(
            matches!(&patch, DfmPatch::SetField { path, to, .. } if path == "size.z" && (*to - 0.8).abs() < 1e-9),
            "{patch:?}"
        );
        let fixed = apply_patch(&d, &patch).unwrap();
        let after = check_document(&fixed, &req).unwrap();
        assert!(
            after.issues.iter().all(|i| i.issue.rule != "fdm.thin_wall"),
            "violation must clear: {:#?}",
            after.issues
        );
    }

    #[test]
    fn parameter_bound_wall_patches_the_parameter() {
        let d = doc("[defparam t 0.5]\n[root [cube 40 30 t] \"pla\"]");
        let r = check_document(&d, &json!({ "process": "fdm" })).unwrap();
        let issue = r
            .issues
            .iter()
            .find(|i| i.issue.rule == "fdm.thin_wall")
            .expect("flagged");
        match issue.patch.clone() {
            Some(p @ DfmPatch::SetParameter { .. }) => {
                let fixed = apply_patch(&d, &p).unwrap();
                let after = check_document(&fixed, &json!({ "process": "fdm" })).unwrap();
                assert!(after.issues.iter().all(|i| i.issue.rule != "fdm.thin_wall"));
            }
            // Loon may bake the literal without a binding; a field patch is fine then.
            Some(p @ DfmPatch::SetField { .. }) => {
                let fixed = apply_patch(&d, &p).unwrap();
                let after = check_document(&fixed, &json!({ "process": "fdm" })).unwrap();
                assert!(after.issues.iter().all(|i| i.issue.rule != "fdm.thin_wall"));
            }
            None => panic!("no patch: {:?}", issue.patch_note),
        }
    }

    #[test]
    fn c_abi_round_trip() {
        let d = doc("[root [cube 40 30 0.5] \"pla\"]");
        let dj = CString::new(serde_json::to_string(&d).unwrap()).unwrap();
        let rq = CString::new(r#"{"process":"fdm"}"#).unwrap();
        let report: Value = serde_json::from_str(&take(vcad_dfm_check(dj.as_ptr(), rq.as_ptr()))).unwrap();
        let patch = report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|i| i.get("patch").filter(|p| !p.is_null()))
            .expect("a patch")
            .to_string();
        let pj = CString::new(patch).unwrap();
        let fixed = take(vcad_dfm_apply_patch(dj.as_ptr(), pj.as_ptr()));
        assert!(Document::from_json(&fixed).is_ok());
        assert!(vcad_dfm_check(dj.as_ptr(), CString::new(r#"{"process":"nope"}"#).unwrap().as_ptr()).is_null());
        let procs: Value = serde_json::from_str(&take(vcad_dfm_processes())).unwrap();
        assert!(procs["processes"].as_array().unwrap().len() >= 7);
    }
}
