//! The built-in desktop agent's contract: tool manifest, system prompt and
//! host-side read tools.
//!
//! The manifest is the OpenAI Chat Completions `tools` array the native app
//! sends with every request. Edit tools are exactly the calls
//! [`crate::tools::lower`] accepts, with schemas written next to that code;
//! read tools ([`read_tool`]) are answered here from the evaluated document.
//! The checked-in copy at `crates/vcad-app/agent/manifest.json` is versioned
//! and must equal [`manifest`] (see the drift test; regenerate with
//! `VCAD_UPDATE_AGENT_MANIFEST=1 cargo test -p vcad-app agent`).

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::command::CommandError;
use crate::DocumentApi;

/// Bump when tools or the system prompt change; it also keys the prompt cache.
pub const MANIFEST_VERSION: u32 = 2;

/// The checked-in manifest (what hosts ship).
pub const MANIFEST_JSON: &str = include_str!("../agent/manifest.json");

/// Tools answered by [`read_tool`] instead of the engine.
pub const READ_TOOLS: [&str; 2] = ["describe_scene", "inspect"];

/// `create` kinds the agent is offered (all lowered by `tools::lower`).
pub const CREATE_KINDS: [&str; 11] = [
    "cube", "cylinder", "sphere", "cone", "torus", "wedge", "prism", "tube", "fillet", "chamfer",
    "shell",
];

const SYSTEM_PROMPT: &str = "You are the modeling agent inside vcad, a parametric CAD app. \
You edit the document the user has open by calling tools; every edit appears live in their viewport.\n\
\n\
Conventions: units are millimetres. Z is up (X right, Y forward). A cube's corner sits at its \
origin and it extends to +size. Cylinders and cones stand along +Z from their base centre.\n\
\n\
Workflow:\n\
- Call describe_scene first when the request refers to existing parts. Part ids look like \"1:4\"; \
always use ids exactly as returned, never invent them.\n\
- create returns the new part id in its result. Use place to position parts relative to each other \
(e.g. put one part's bottom on another's top) instead of computing offsets by hand.\n\
- translate/rotate/scale set a part's absolute transform (not a delta).\n\
- Booleans (union/difference/intersection) consume their operands into a new part.\n\
- If a tool returns an error, read it, fix the arguments and try again. Do not repeat a failing call unchanged.\n\
- Use inspect to check volume, area or mass when the user asks about them.\n\
- When the request is done, reply with one or two short sentences describing what changed. No markdown headings.";

fn vec3_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "x": {"type": "number"}, "y": {"type": "number"}, "z": {"type": "number"}
        },
        "required": ["x", "y", "z"]
    })
}

fn part_id(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            }
        }
    })
}

fn anchor_schema(description: &str) -> Value {
    json!({
        "description": description,
        "anyOf": [
            {"type": "string", "enum": ["center", "min", "max", "top", "bottom", "front", "back", "left", "right"],
             "description": "Named bbox anchor on the moved part."},
            {"type": "object", "properties": {
                "part": {"type": "string"},
                "anchor": {"type": "string", "enum": ["center", "min", "max", "top", "bottom", "front", "back", "left", "right"]}
            }, "required": ["part", "anchor"], "description": "Named bbox anchor on another part."},
            vec3_schema("World point in mm.")
        ]
    })
}

/// Tools the agent may call, in OpenAI `tools` format.
pub fn tools() -> Vec<Value> {
    let anchor_desc = "One of: a named anchor string, {part, anchor}, or a world point {x,y,z}.";
    vec![
        tool(
            "describe_scene",
            "List every part in the open document: id, name, kind, material, world bounding box (mm).",
            json!({}),
            &[],
        ),
        tool(
            "inspect",
            "Measure one part: volume (mm^3), surface area (mm^2), bounding box, and mass when its material density is known.",
            json!({"part_id": part_id("Part id from describe_scene.")}),
            &["part_id"],
        ),
        tool(
            "create",
            "Create a part. Primitives: cube {size:{x,y,z}}, cylinder {radius, height}, sphere {radius}, \
             cone {radius_bottom, radius_top, height}, torus {major_radius, minor_radius}, \
             wedge {size:{x,y,z}}, prism {sides, radius, height}, tube {start:{x,y,z}, end:{x,y,z}, radius}. \
             Modifiers on an existing part (pass parent_part_id): fillet {radius}, chamfer {distance}, \
             shell {thickness, open_faces?} — open_faces removes faces to leave an opening (an open-top enclosure): \
             an array of {name} or {point:{x,y,z}, normal:{x,y,z}} on each face, e.g. the top face's center with normal {z:1}.",
            json!({
                "type": {"type": "string", "enum": CREATE_KINDS},
                "params": {"type": "object", "description": "Parameters for the chosen type (see description)."},
                "parent_part_id": part_id("Target part for fillet/chamfer/shell."),
                "name": {"type": "string", "description": "Short readable name, 1-3 words."}
            }),
            &["type", "params"],
        ),
        tool(
            "place",
            "Move a part so its `from` anchor (default center) lands on `to`. Keeps rotation and scale.",
            json!({
                "part_id": part_id("Part to move."),
                "from": anchor_schema(anchor_desc),
                "to": anchor_schema(anchor_desc)
            }),
            &["part_id", "to"],
        ),
        tool(
            "translate",
            "Set a part's absolute offset in mm.",
            json!({"child": part_id("Part id."), "offset": vec3_schema("Offset in mm.")}),
            &["child", "offset"],
        ),
        tool(
            "rotate",
            "Set a part's absolute rotation (degrees about X, Y, Z), pivoting about its bbox centre unless pivot is given.",
            json!({
                "child": part_id("Part id."),
                "angles": vec3_schema("Euler angles in degrees."),
                "pivot": vec3_schema("Optional world pivot in mm.")
            }),
            &["child", "angles"],
        ),
        tool(
            "scale",
            "Set a part's absolute scale factor per axis.",
            json!({"child": part_id("Part id."), "factor": vec3_schema("Scale factors (1 = unchanged).")}),
            &["child", "factor"],
        ),
        tool(
            "union",
            "Fuse two parts into one new part.",
            json!({"left": part_id("First part."), "right": part_id("Second part."), "name": {"type": "string"}}),
            &["left", "right"],
        ),
        tool(
            "difference",
            "Subtract `right` from `left`, producing a new part.",
            json!({"left": part_id("Part to cut from."), "right": part_id("Tool part removed."), "name": {"type": "string"}}),
            &["left", "right"],
        ),
        tool(
            "intersection",
            "Keep only the volume shared by two parts.",
            json!({"left": part_id("First part."), "right": part_id("Second part."), "name": {"type": "string"}}),
            &["left", "right"],
        ),
        tool(
            "linear_pattern",
            "Repeat a part `count` times along a direction, `spacing` mm apart.",
            json!({
                "child": part_id("Part id."),
                "direction": vec3_schema("Direction vector."),
                "count": {"type": "integer", "minimum": 1},
                "spacing": {"type": "number"}
            }),
            &["child", "direction", "count", "spacing"],
        ),
        tool(
            "circular_pattern",
            "Repeat a part `count` times around an axis over `angle_deg` degrees.",
            json!({
                "child": part_id("Part id."),
                "axis_origin": vec3_schema("Point on the axis."),
                "axis_dir": vec3_schema("Axis direction."),
                "count": {"type": "integer", "minimum": 1},
                "angle_deg": {"type": "number"}
            }),
            &["child", "axis_origin", "axis_dir", "count", "angle_deg"],
        ),
        tool(
            "mirror",
            "Mirror a part across a world plane.",
            json!({"child": part_id("Part id."), "plane": {"type": "string", "enum": ["XY", "XZ", "YZ"]}}),
            &["child", "plane"],
        ),
        tool(
            "update",
            "Change parameters of an existing part, e.g. {size:{x:40}} on a cube or {radius:8} on a cylinder.",
            json!({"node_id": part_id("Part id (or IR node id)."), "params": {"type": "object"}}),
            &["node_id", "params"],
        ),
        tool(
            "set_material",
            "Assign a material preset (e.g. aluminum, steel, brass, copper, oak, abs-white, pla-black) to one or more parts.",
            json!({
                "part_id": part_id("Single part."),
                "part_ids": {"type": "array", "items": {"type": "string"}},
                "material": {"type": "string"}
            }),
            &["material"],
        ),
        tool(
            "delete",
            "Delete a part.",
            json!({"part_id": part_id("Part id.")}),
            &["part_id"],
        ),
    ]
}

/// Tools offered only to endpoints that accept image input.
pub fn vision_tools() -> Vec<Value> {
    vec![tool(
        "render_view",
        "Look at the model: returns a rendered image of the scene from a preset view, or from a \
         camera angle given in degrees (azimuth about +Z from the front, elevation above the XY plane). \
         The user's own camera is left where it was.",
        json!({
            "view": {"type": "string", "enum": ["current", "iso", "top", "front", "right"],
                     "description": "Preset view; ignored when azimuth_deg/elevation_deg are given."},
            "azimuth_deg": {"type": "number"},
            "elevation_deg": {"type": "number", "minimum": -85, "maximum": 85}
        }),
        &[],
    )]
}

/// The agent manifest hosts load (`vcad_agent_manifest`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct AgentManifest {
    /// Manifest schema version.
    pub version: u32,
    /// Prompt-cache key; changes with the version.
    pub cache_key: String,
    /// System prompt.
    pub system: String,
    /// Tool definitions (JSON-schema tool specs).
    #[cfg_attr(feature = "ts-rs", ts(type = "Array<Record<string, unknown>>"))]
    pub tools: Vec<Value>,
    /// Tools only offered to vision-capable models.
    #[cfg_attr(feature = "ts-rs", ts(type = "Array<Record<string, unknown>>"))]
    pub vision_tools: Vec<Value>,
}

/// Text answer of a read tool inside an [`AgentReadEnvelope`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct AgentReadResult {
    /// The answer, as plain text for the model.
    pub text: String,
}

/// The read-tool envelope: `{"ok":true,"result":{"text":…}}` or an error.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export, export_to = "bindings/"))]
pub struct AgentReadEnvelope {
    /// Whether the read succeeded.
    pub ok: bool,
    /// Present when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub result: Option<AgentReadResult>,
    /// Present when not `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(optional))]
    pub error: Option<crate::command::EngineError>,
}

/// The full manifest: version, system prompt, tools, vision-only tools.
pub fn manifest() -> Value {
    serde_json::to_value(AgentManifest {
        version: MANIFEST_VERSION,
        cache_key: format!("vcad-agent-v{MANIFEST_VERSION}"),
        system: SYSTEM_PROMPT.to_string(),
        tools: tools(),
        vision_tools: vision_tools(),
    })
    .expect("manifest serializes")
}

/// Pretty JSON for the checked-in file (trailing newline).
pub fn manifest_pretty() -> String {
    let mut s = serde_json::to_string_pretty(&manifest()).expect("manifest serializes");
    s.push('\n');
    s
}

/// Answer a read tool as plain text for the model.
pub fn read_tool(tool: &str, args: &Value, doc: &DocumentApi) -> Result<String, CommandError> {
    match tool {
        "describe_scene" => Ok(describe_scene(doc)),
        // Host-only (not in the agent manifest): the Mac app calls it
        // after `create_cad_loon` loads to fail closed on parts that
        // cannot be built — or that build to no geometry at all.
        "evaluation_errors" => Ok(evaluation_errors(doc)),
        // Host-only: the feature tree's rows (state per feature, broken
        // references with re-pick candidates) as JSON.
        "feature_history" => serde_json::to_string(&doc.feature_history()).map_err(|e| {
            CommandError::Invalid {
                message: e.to_string(),
            }
        }),
        "inspect" => {
            let id = args
                .get("part_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CommandError::Invalid {
                    message: "inspect requires `part_id`".into(),
                })?;
            inspect(id, doc)
        }
        other => Err(CommandError::Unsupported {
            tool: other.to_string(),
        }),
    }
}

/// The read-tool envelope (`{"ok":true,"result":{"text":…}}` or an error).
pub fn read_tool_envelope(doc: &DocumentApi, tool: &str, args_json: &str) -> String {
    let out = serde_json::from_str::<Value>(args_json)
        .map_err(|e| CommandError::Invalid {
            message: e.to_string(),
        })
        .and_then(|args| read_tool(tool, &args, doc));
    let env = match out {
        Ok(text) => AgentReadEnvelope {
            ok: true,
            result: Some(AgentReadResult { text }),
            error: None,
        },
        Err(e) => AgentReadEnvelope {
            ok: false,
            result: None,
            error: Some(e.into()),
        },
    };
    serde_json::to_value(env).unwrap_or_default().to_string()
}

fn part_field(doc: &DocumentApi, id: &str, key: &str) -> Option<String> {
    let fid = doc.stable_ids().resolve(id)?;
    let feature = doc.crdt().get_feature(fid)?;
    match &feature.params.get(key)?.0 {
        vcad_crdt::Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn fmt3(v: [f64; 3]) -> String {
    format!("({:.2}, {:.2}, {:.2})", v[0], v[1], v[2])
}

/// Parts that evaluate to no geometry, one per line; empty when every part
/// built. A feature the kernel can't build (a blend whose edge reference
/// resolves to the wrong edge, an over-size radius) often yields an empty
/// solid rather than an error — the app uses this to refuse such an edit.
pub fn evaluation_errors(doc: &DocumentApi) -> String {
    let state = doc.get_state();
    let mut out = String::new();
    for p in &state.parts {
        let mut cache = std::collections::HashMap::new();
        let why = match vcad_eval::evaluate::evaluate_node(
            p.root_node_id(),
            &state.document.nodes,
            &mut cache,
        ) {
            Ok(Some(_)) => continue,
            Ok(None) => "produced no geometry".to_string(),
            Err(e) => e.to_string(),
        };
        let name = serde_json::to_value(p)
            .ok()
            .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        out.push_str(&format!("- {} \"{name}\": {why}\n", p.id()));
    }
    out
}

/// One line per part: id, name, kind, material, bbox.
pub fn describe_scene(doc: &DocumentApi) -> String {
    let state = doc.get_state();
    // What the feature tree shows: scene roots. A boolean's operands and a
    // part definition's source are features, not parts.
    let roots: HashSet<&String> = state
        .document
        .part_ids
        .iter()
        .flat_map(|m| m.values())
        .collect();
    let parts: Vec<_> = state
        .parts
        .iter()
        .filter(|p| roots.contains(&p.id().to_string()))
        .collect();
    let instances = state.document.instances.as_deref().unwrap_or_default();
    if parts.is_empty() && instances.is_empty() {
        return "The document is empty (no parts).".into();
    }
    let mut out = format!(
        "{} part(s), units mm, Z up. Ids are current as of this call; a document \
         pushed from an MCP session can renumber them, so re-read after one.\n",
        parts.len() + instances.len()
    );
    for inst in instances {
        let name = inst.name.as_deref().unwrap_or(&inst.id);
        out.push_str(&format!(
            "- instance {} \"{name}\" of {}\n",
            inst.id, inst.part_def_id
        ));
    }
    for part in parts {
        let v = serde_json::to_value(part).unwrap_or(Value::Null);
        let id = part.id();
        let name = v.get("name").and_then(Value::as_str).unwrap_or("");
        let kind = v.get("kind").and_then(Value::as_str).unwrap_or("?");
        let material = part_field(doc, id, "material").unwrap_or_else(|| "default".into());
        let bbox = match doc.part_evaluation(id) {
            Some(Err(e)) => format!("evaluation failed: {e}"),
            Some(Ok(Some(solid))) => match solid.try_bounding_box() {
                Some((lo, hi)) => format!("bbox min {} max {}", fmt3(lo), fmt3(hi)),
                None => "empty (no volume)".into(),
            },
            Some(Ok(None)) | None => "empty (no volume)".into(),
        };
        out.push_str(&format!(
            "- {id} \"{name}\" [{kind}] material={material} {bbox}\n"
        ));
    }
    out
}

/// Density in kg/m^3 for a material key: the document's own definition
/// first, then common presets.
fn density(doc: &DocumentApi, material: &str) -> Option<f64> {
    let state = doc.get_state();
    if let Some(d) = state
        .document
        .materials
        .get(material)
        .and_then(|m| m.density)
    {
        return Some(d);
    }
    let m = material.to_lowercase();
    let table: [(&str, f64); 10] = [
        ("steel", 7850.0),
        ("stainless", 8000.0),
        ("aluminum", 2700.0),
        ("brass", 8500.0),
        ("copper", 8960.0),
        ("titanium", 4430.0),
        ("abs", 1050.0),
        ("pla", 1240.0),
        ("petg", 1270.0),
        ("oak", 750.0),
    ];
    table.iter().find(|(k, _)| m.contains(k)).map(|(_, d)| *d)
}

fn inspect(id: &str, doc: &DocumentApi) -> Result<String, CommandError> {
    let state = doc.get_state();
    let part = state
        .parts
        .iter()
        .find(|p| p.id() == id)
        .ok_or_else(|| CommandError::UnknownId { id: id.to_string() })?;
    // Measure what the viewport draws: parameters applied through bindings.
    let (resolved, _) =
        vcad_eval::resolve::resolve_document_cloned(&state.document).map_err(|e| {
            CommandError::Invalid {
                message: format!("parameters could not be applied: {e}"),
            }
        })?;
    let mut cache = std::collections::HashMap::new();
    let solid =
        vcad_eval::evaluate::evaluate_node(part.root_node_id(), &resolved.nodes, &mut cache)
            .map_err(|e| CommandError::Invalid {
                message: format!("part {id} could not be evaluated: {e}"),
            })?;
    let Some(solid) = solid else {
        return Ok(format!("{id}: empty (no volume)"));
    };
    let Some((lo, hi)) = solid.try_bounding_box() else {
        return Ok(format!("{id}: empty (no volume)"));
    };
    let volume = solid.volume();
    let area = solid.surface_area();
    let material = part_field(doc, id, "material");
    let mut out = format!(
        "{id}: volume {volume:.3} mm^3, surface area {area:.3} mm^2, bbox min {} max {}, size {}",
        fmt3(lo),
        fmt3(hi),
        fmt3([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]])
    );
    match material
        .as_deref()
        .and_then(|m| density(doc, m).map(|d| (m, d)))
    {
        // mm^3 → m^3 is 1e-9; kg → g is 1e3.
        Some((m, d)) => out.push_str(&format!(
            ", material {m} ({d} kg/m^3), mass {:.3} g",
            volume * 1e-9 * d * 1e3
        )),
        None => out.push_str(&format!(
            ", material {}, mass unknown (no density)",
            material.as_deref().unwrap_or("default")
        )),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_crdt::ReplicaId;

    fn apply(api: &mut DocumentApi, tool: &str, args: Value) -> Value {
        let v: Value =
            serde_json::from_str(&api.apply_tool_envelope(tool, &args.to_string())).unwrap();
        assert_eq!(v["ok"], true, "{tool} {args}: {v}");
        v
    }

    /// A one-root document: `nodes` is `[(id, name, op)]`, root = last.
    fn ir_doc(nodes: &[(u64, &str, Value)]) -> DocumentApi {
        let mut map = serde_json::Map::new();
        for (id, name, op) in nodes {
            map.insert(id.to_string(), json!({"id": id, "name": name, "op": op}));
        }
        let root = nodes.last().unwrap().0;
        let doc = json!({"version": "0.1", "nodes": map, "materials": {},
            "part_materials": {}, "roots": [{"root": root, "material": "default"}]});
        let mut api = DocumentApi::new(ReplicaId(1));
        api.import_ir(&doc.to_string());
        api
    }

    /// Stable id of the part named `name`.
    fn part_named(api: &DocumentApi, name: &str) -> String {
        api.get_state()
            .parts
            .iter()
            .find(|p| serde_json::to_value(p).unwrap()["name"] == name)
            .unwrap()
            .id()
            .to_string()
    }

    fn cube_op(z: f64) -> Value {
        json!({"type": "Cube", "size": {"x": 10.0, "y": 10.0, "z": z}})
    }

    // Fuzz repro: `[fillet 20 [cube 10 10 10]]` showed "no geometry" and
    // the load reported success. The reason must reach the agent.
    #[test]
    fn overfillet_reports_the_kernel_reason() {
        let api = ir_doc(&[
            (1, "cube", cube_op(10.0)),
            (
                2,
                "fillet",
                json!({"type": "Fillet", "child": 1, "radius": 20.0}),
            ),
        ]);
        let errors = api.evaluation_errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("fillet") && errors[0].contains("radius"),
            "{errors:?}"
        );
        let scene = read_tool("describe_scene", &json!({}), &api).unwrap();
        assert!(scene.contains("evaluation failed"), "{scene}");
        assert!(!scene.contains("no geometry"), "{scene}");
        let id = part_named(&api, "fillet");
        let err = read_tool("inspect", &json!({"part_id": id}), &api).unwrap_err();
        assert!(err.to_string().contains("radius"), "{err}");
        let text = read_tool("evaluation_errors", &json!({}), &api).unwrap();
        assert!(text.contains("radius"), "{text}");
    }

    #[test]
    fn overshell_reports_an_error_not_a_bigger_part() {
        let api = ir_doc(&[
            (1, "cube", cube_op(10.0)),
            (
                2,
                "shell",
                json!({"type": "Shell", "child": 1, "thickness": 20.0}),
            ),
        ]);
        let errors = api.evaluation_errors();
        assert!(
            errors.iter().any(|e| e.contains("consumes the solid")),
            "{errors:?}"
        );
        let ok = ir_doc(&[
            (1, "cube", cube_op(10.0)),
            (
                2,
                "shell",
                json!({"type": "Shell", "child": 1, "thickness": 1.0}),
            ),
        ]);
        assert!(ok.evaluation_errors().is_empty());
        let id = part_named(&ok, "shell");
        let text = read_tool("inspect", &json!({"part_id": id}), &ok).unwrap();
        assert!(text.contains("volume 488.000"), "{text}");
    }

    // Fuzz repro: loon `[difference [cube 10 10 10] [cube 10 10 5]]` is
    // B − A = empty, and printed a `1.797e308` sentinel bbox.
    #[test]
    fn empty_result_says_empty_not_a_sentinel_bbox() {
        let api = ir_doc(&[
            (1, "a", cube_op(10.0)),
            (2, "b", cube_op(5.0)),
            (
                3,
                "difference",
                json!({"type": "Difference", "left": 2, "right": 1}),
            ),
        ]);
        let scene = read_tool("describe_scene", &json!({}), &api).unwrap();
        assert!(scene.contains("empty (no volume)"), "{scene}");
        assert!(!scene.contains("e308"), "{scene}");
        assert!(
            scene.contains("\"difference\" [boolean] material=default empty (no volume)"),
            "{scene}"
        );
        let id = part_named(&api, "difference");
        let text = read_tool("inspect", &json!({"part_id": id}), &api).unwrap();
        assert!(
            text.contains("empty (no volume)") && !text.contains("e308"),
            "{text}"
        );
        assert_eq!(api.part_world_bbox(&id), None);
    }

    #[test]
    fn checked_in_manifest_matches_generated() {
        let generated = manifest_pretty();
        if std::env::var_os("VCAD_UPDATE_AGENT_MANIFEST").is_some() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/agent/manifest.json");
            std::fs::write(path, &generated).unwrap();
            return;
        }
        assert_eq!(
            MANIFEST_JSON, generated,
            "crates/vcad-app/agent/manifest.json drifted; regenerate with \
             VCAD_UPDATE_AGENT_MANIFEST=1 cargo test -p vcad-app agent"
        );
    }

    fn names(tools: &[Value]) -> Vec<String> {
        tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// Every edit tool in the manifest is one `tools::lower` accepts (with
    /// arguments shaped exactly like its schema), and every read tool is
    /// answered by `read_tool`.
    #[test]
    fn every_manifest_tool_is_executable() {
        let mut api = DocumentApi::new(ReplicaId(7));
        let cube = |api: &mut DocumentApi| {
            apply(
                api,
                "create",
                json!({"type": "cube", "params": {"size": {"x": 10, "y": 10, "z": 10}}}),
            )["result"]["createdFeatureId"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let a = cube(&mut api);
        let v = |x: f64, y: f64, z: f64| json!({"x": x, "y": y, "z": z});
        let mut covered = vec![];
        for name in names(&tools()) {
            if READ_TOOLS.contains(&name.as_str()) {
                let text = read_tool(&name, &json!({"part_id": a}), &api).unwrap();
                assert!(text.contains(&a), "{name}: {text}");
                covered.push(name);
                continue;
            }
            let b = cube(&mut api);
            let args = match name.as_str() {
                "create" => json!({"type": "sphere", "params": {"radius": 3}}),
                "place" => {
                    json!({"part_id": b, "from": "bottom", "to": {"part": a, "anchor": "top"}})
                }
                "translate" => json!({"child": b, "offset": v(1.0, 2.0, 3.0)}),
                "rotate" => json!({"child": b, "angles": v(0.0, 0.0, 45.0)}),
                "scale" => json!({"child": b, "factor": v(2.0, 1.0, 1.0)}),
                "union" | "difference" | "intersection" => json!({"left": a, "right": b}),
                "linear_pattern" => {
                    json!({"child": b, "direction": v(1.0, 0.0, 0.0), "count": 3, "spacing": 12})
                }
                "circular_pattern" => {
                    json!({"child": b, "axis_origin": v(0.0, 0.0, 0.0), "axis_dir": v(0.0, 0.0, 1.0), "count": 4, "angle_deg": 360})
                }
                "mirror" => json!({"child": b, "plane": "YZ"}),
                "update" => json!({"node_id": b, "params": {"size": {"x": 40}}}),
                "set_material" => json!({"part_ids": [b], "material": "aluminum"}),
                "delete" => json!({"part_id": b}),
                other => panic!("manifest tool {other} has no executability check"),
            };
            apply(&mut api, &name, args);
            if matches!(name.as_str(), "union" | "difference" | "intersection") {
                // Booleans consume the operands; start fresh.
                api = DocumentApi::new(ReplicaId(7));
                let fresh = cube(&mut api);
                assert_eq!(fresh, a, "replica-deterministic ids");
            }
            covered.push(name);
        }
        assert_eq!(covered.len(), tools().len());
    }

    #[test]
    fn every_create_kind_lowers() {
        let mut api = DocumentApi::new(ReplicaId(3));
        let base = apply(
            &mut api,
            "create",
            json!({"type": "cube", "params": {"size": {"x": 20, "y": 20, "z": 20}}}),
        )["result"]["createdFeatureId"]
            .as_str()
            .unwrap()
            .to_string();
        let schema_enum: Vec<String> = tools()
            .iter()
            .find(|t| t["function"]["name"] == "create")
            .unwrap()["function"]["parameters"]["properties"]["type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(schema_enum, CREATE_KINDS);
        for kind in CREATE_KINDS {
            let (params, parent) = match kind {
                "cube" => (json!({"size": {"x": 1, "y": 2, "z": 3}}), None),
                "cylinder" => (json!({"radius": 2, "height": 5}), None),
                "sphere" => (json!({"radius": 2}), None),
                "cone" => (
                    json!({"radius_bottom": 3, "radius_top": 1, "height": 4}),
                    None,
                ),
                "torus" => (json!({"major_radius": 5, "minor_radius": 1}), None),
                "wedge" => (json!({"size": {"x": 1, "y": 2, "z": 3}}), None),
                "prism" => (json!({"sides": 6, "radius": 2, "height": 3}), None),
                "tube" => (
                    json!({"start": {"x": 0, "y": 0, "z": 0}, "end": {"x": 0, "y": 0, "z": 10}, "radius": 1}),
                    None,
                ),
                "fillet" => (json!({"radius": 1}), Some(&base)),
                "chamfer" => (json!({"distance": 1}), Some(&base)),
                "shell" => (json!({"thickness": 1}), Some(&base)),
                other => panic!("no check for {other}"),
            };
            let mut args = json!({"type": kind, "params": params});
            if let Some(p) = parent {
                args["parent_part_id"] = json!(p);
            }
            // Lowering is the contract; modifier geometry may still refuse at apply.
            crate::tools::lower("create", &args, &api).unwrap_or_else(|e| panic!("{kind}: {e}"));
        }
    }

    #[test]
    fn vision_tools_are_separate_and_unlowered() {
        assert_eq!(names(&vision_tools()), ["render_view"]);
        assert!(!names(&tools()).contains(&"render_view".to_string()));
        let api = DocumentApi::new(ReplicaId(1));
        assert!(matches!(
            crate::tools::lower("render_view", &json!({}), &api),
            Err(CommandError::Unsupported { .. })
        ));
    }

    #[test]
    fn observed_model_call_shapes_lower() {
        // Verbatim argument shapes a live model emitted for "create a 20mm
        // cube, move it +10mm in Z, inspect it" (create nests type + params
        // one level down).
        let mut api = DocumentApi::new(ReplicaId(1));
        let created = apply(
            &mut api,
            "create",
            json!({"name": "cube", "params": {"type": "cube", "params": {"size": {"x": 20, "y": 20, "z": 20}}}}),
        );
        let id = created["result"]["createdFeatureId"]
            .as_str()
            .unwrap()
            .to_string();
        apply(
            &mut api,
            "translate",
            json!({"child": id, "offset": {"x": 0, "y": 0, "z": 10}}),
        );
        let text = read_tool("inspect", &json!({"part_id": id}), &api).unwrap();
        assert!(text.contains("volume 8000.000"), "{text}");
        let (lo, hi) = api.part_world_bbox(&id).unwrap();
        assert!(
            (lo[2] - 10.0).abs() < 1e-9 && (hi[2] - 30.0).abs() < 1e-9,
            "{lo:?} {hi:?}"
        );
        assert_eq!(api.get_state().parts.len(), 1);
        let name = serde_json::to_value(&api.get_state().parts[0]).unwrap()["name"].clone();
        assert_eq!(name, "cube");
    }

    #[test]
    fn inspect_measures_with_parameters_applied() {
        let mut doc: serde_json::Value =
            serde_json::from_str(include_str!("../../../examples/parametric-plate.vcad")).unwrap();
        doc["parameters"]["width"]["value"] = json!(120.0);
        let api = DocumentApi::from_ir(&doc.to_string(), ReplicaId(1)).unwrap();
        let id = api.get_state().parts[0].id().to_string();
        let text = read_tool("inspect", &json!({"part_id": id}), &api).unwrap();
        assert!(text.contains("size (120.00, 50.00"), "{text}");
    }

    #[test]
    fn inspect_measures_a_pcb_board_document() {
        let ir = include_str!("../../../examples/pcb-motor/stator-v3.vcad");
        let api = DocumentApi::from_ir(ir, ReplicaId(1)).unwrap();
        let id = api.get_state().parts[0].id().to_string();
        let text = read_tool("inspect", &json!({"part_id": id}), &api).unwrap();
        assert!(text.contains("size (70.00, 70.00, 2.60)"), "{text}");
    }

    #[test]
    fn inspect_says_why_a_part_could_not_be_evaluated() {
        // A fillet over a through-hole refuses to rebuild the holed faces.
        let ir = json!({
            "version": "0.1",
            "nodes": {
                "0": {"id": 0, "name": null, "op": {"type": "Cube", "size": {"x": 20.0, "y": 20.0, "z": 5.0}}},
                "1": {"id": 1, "name": null, "op": {"type": "Cylinder", "radius": 3.0, "height": 7.0, "segments": 0}},
                "2": {"id": 2, "name": null, "op": {"type": "Translate", "child": 1, "offset": {"x": 10.0, "y": 10.0, "z": -1.0}}},
                "3": {"id": 3, "name": null, "op": {"type": "Difference", "left": 0, "right": 2}},
                "4": {"id": 4, "name": null, "op": {"type": "Fillet", "child": 3, "radius": 0.5}}
            },
            "materials": {"default": {"name": "default", "color": [0.8, 0.8, 0.8], "metallic": 0.0, "roughness": 0.5}},
            "part_materials": {},
            "roots": [{"root": 4, "material": "default"}]
        });
        let api = DocumentApi::from_ir(&ir.to_string(), ReplicaId(1)).unwrap();
        let state = api.get_state();
        let id = state.parts.last().unwrap().id().to_string();
        let err = read_tool_envelope(&api, "inspect", &json!({"part_id": id}).to_string());
        assert!(err.contains("\"ok\":false"), "{err}");
        assert!(err.contains("inner loops"), "reason hidden: {err}");
    }

    #[test]
    fn describe_scene_lists_what_the_tree_shows() {
        let mut api = DocumentApi::new(ReplicaId(2));
        let mut cube = |x: f64| {
            let r = apply(
                &mut api,
                "create",
                json!({"type": "cube", "params": {"size": {"x": x, "y": 10, "z": 10}}}),
            );
            r["result"]["createdFeatureId"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let (a, b) = (cube(10.0), cube(5.0));
        apply(&mut api, "difference", json!({"left": a, "right": b}));
        let scene = describe_scene(&api);
        assert!(
            scene.starts_with("1 part(s)"),
            "operands listed as parts: {scene}"
        );
        assert!(!scene.contains(&format!("- {a} ")), "{scene}");
        assert!(scene.contains("current as of this call"), "{scene}");
    }

    #[test]
    fn inspect_reports_volume_and_mass() {
        let mut api = DocumentApi::new(ReplicaId(2));
        let id = apply(
            &mut api,
            "create",
            json!({"type": "cube", "params": {"size": {"x": 10, "y": 10, "z": 10}}}),
        )["result"]["createdFeatureId"]
            .as_str()
            .unwrap()
            .to_string();
        apply(
            &mut api,
            "set_material",
            json!({"part_id": id, "material": "aluminum"}),
        );
        let text = read_tool("inspect", &json!({"part_id": id}), &api).unwrap();
        assert!(text.contains("volume 1000.000 mm^3"), "{text}");
        assert!(text.contains("mass 2.700 g"), "{text}");
        let scene = describe_scene(&api);
        assert!(scene.contains("material=aluminum"), "{scene}");
        assert!(scene.contains("max (10.00, 10.00, 10.00)"), "{scene}");
        let err = read_tool_envelope(&api, "inspect", r#"{"part_id":"9:9"}"#);
        assert!(err.contains("\"ok\":false"), "{err}");
    }
}
