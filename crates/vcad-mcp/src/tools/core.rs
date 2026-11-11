//! Native core tools. Argument names match the node server's schemas
//! (`packages/mcp/src/tools/*.ts`) so a client can't tell which side served a
//! call; anything the native port does not cover returns
//! [`ToolError::Delegate`] and the node server answers instead.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use vcad_ir::Document;
use vcad_tool_derive::{OutputSchema, ToolSchema};

use super::{ToolCtx, ToolDef, ToolError, ToolOutput, ToolSchemaEntry};
use crate::store::{CommitMeta, DocumentId, StoreError};

/// Native core tools (arguments derive `inputSchema`).
#[allow(dead_code)]
#[derive(ToolSchema)]
#[tool(category = "core")]
enum CoreTool {
    /// Report this server's identity, uptime, boot token and store durability. Call FIRST on any preview-shaped complaint: a low uptime_s or changed instance_id means the server restarted.
    ServerInfo,
    /// Open an editing session for a CAD document. Returns a `document_id` for subsequent calls. Pass `initial` (Document IR) to edit an existing document, or `path` to open a .vcad / .loon file (in vcad.app it opens in the shared window; .loon files hot-reload when regenerated); omit both for an empty one.
    OpenDocument {
        /// Document IR to start from.
        initial: Option<Value>,
        /// A .vcad or .loon file to open.
        path: Option<String>,
    },
    /// Return the full IR Document JSON for an open session (omit document_id for the current document — the open vcad.app window; large documents are then summarized unless full is set).
    GetDocument {
        /// Session id from open_document / create_cad_loon; omit for the current document.
        document_id: Option<String>,
        /// Never summarize.
        full: Option<bool>,
    },
    /// Close a document session. Idempotent — closing an unknown id reports `closed: false`.
    CloseDocument {
        /// Session id.
        document_id: String,
    },
    /// Create (or replace) a CAD document from loon source — the preferred authoring tool for whole parts. Returns a `document_id`.
    CreateCadLoon {
        /// Loon source.
        source: String,
        /// Replace this session's document instead of opening a new one.
        document_id: Option<String>,
        /// "replace" (default) or "add".
        mode: Option<String>,
        /// Directory `[use …]` modules and relative imports resolve against (local server).
        base_dir: Option<String>,
        /// Short name for the undo step (vcad.app).
        name: Option<String>,
    },
    /// Open a `.loon` source file (evaluated on load) or a `.vcad` document from disk into a new session.
    LoadDocument {
        /// Path to a .loon or .vcad file.
        path: Option<String>,
        /// Saved name (served by the node server).
        name: Option<String>,
    },
    /// Inspect a session document: volume, surface area, bounding box, center of mass, triangle count.
    InspectCad {
        /// Session id.
        document_id: Option<String>,
        /// Inline Document IR (instead of document_id).
        document: Option<Value>,
    },
    /// Render a session document to a PNG (drafting line art, Z-up) so you can SEE the geometry.
    RenderView {
        /// Session id.
        document_id: Option<String>,
        /// Inline Document IR (instead of document_id).
        document: Option<Value>,
        /// iso | isometric | top | front | side.
        view: Option<String>,
        /// Orbit azimuth in degrees (with elevation).
        azimuth: Option<f64>,
        /// Orbit elevation in degrees.
        elevation: Option<f64>,
        /// Root/part name to frame.
        focus: Option<String>,
        /// Image width in pixels (default 768).
        width_px: Option<u32>,
    },
    /// Export a session document to a file (.stl or .vcad natively; other formats are served by the node exporter). Relative paths resolve against VCAD_MCP_EXPORT_DIR or the working directory.
    ExportCad {
        /// Session id.
        document_id: Option<String>,
        /// Inline Document IR.
        document: Option<Value>,
        /// Inline Document IR (alias).
        ir: Option<Value>,
        /// Output filename with extension.
        filename: String,
    },
}

/// `server_info` result.
#[derive(Serialize, OutputSchema)]
pub struct ServerInfoOut {
    /// Server name.
    pub name: String,
    /// Server version.
    pub version: String,
    /// Seconds since process start.
    pub uptime_s: f64,
    /// Stable for the process life.
    pub instance_id: String,
    /// Embedded in every minted document_id.
    pub boot_token: String,
    /// "headless" or "app".
    pub store: String,
    /// Sessions survive a restart.
    pub durable: bool,
    /// Where sessions persist, if on disk.
    pub session_dir: Option<String>,
    /// Always "in-memory".
    pub artifact_store: String,
    /// Protocol revisions served.
    pub protocol_versions: Vec<String>,
}

/// A session handle result.
#[derive(Serialize, OutputSchema)]
pub struct DocumentHandle {
    /// Session id.
    pub document_id: String,
    /// Scene roots.
    pub roots: u64,
    /// IR nodes.
    pub nodes: u64,
    /// Whether sessions survive a restart.
    pub durable: bool,
}

/// `close_document` result.
#[derive(Serialize, OutputSchema)]
pub struct CloseOut {
    /// True when a session was closed.
    pub closed: bool,
}

/// An xyz triple.
#[derive(Serialize, Clone, Copy, Default)]
pub struct Xyz {
    /// x.
    pub x: f64,
    /// y.
    pub y: f64,
    /// z.
    pub z: f64,
}

/// Axis-aligned box.
#[derive(Serialize)]
pub struct BBox {
    /// Minimum corner.
    pub min: Xyz,
    /// Maximum corner.
    pub max: Xyz,
}

fn xyz_schema() -> Value {
    json!({"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"z":{"type":"number"}},"required":["x","y","z"]})
}

impl crate::SubToolSchema for Xyz {
    fn sub_schema() -> Value {
        xyz_schema()
    }
}

impl crate::SubToolSchema for BBox {
    fn sub_schema() -> Value {
        json!({"type":"object","properties":{"min":xyz_schema(),"max":xyz_schema()},"required":["min","max"]})
    }
}

/// `inspect_cad` result.
#[derive(Serialize, OutputSchema)]
pub struct InspectOut {
    /// Volume, mm³.
    pub volume: f64,
    /// Surface area, mm².
    pub surface_area: f64,
    /// Bounding box, mm.
    #[tool(expand)]
    pub bounding_box: BBox,
    /// Center of mass (uniform density), mm.
    #[tool(expand)]
    pub center_of_mass: Xyz,
    /// Triangle count.
    pub triangles: u64,
    /// Evaluated parts.
    pub parts: u64,
    /// Roots that failed to evaluate.
    pub failures: Vec<String>,
}

/// `export_cad` result.
#[derive(Serialize, OutputSchema)]
pub struct ExportOut {
    /// Absolute path written.
    pub path: String,
    /// Format written.
    pub format: String,
    /// Bytes written.
    pub bytes: u64,
}

pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub(crate) fn store_err(e: StoreError) -> ToolError {
    ToolError::InvalidArgs(e.to_string())
}

pub(crate) fn parse_doc(v: &Value) -> Result<Document, ToolError> {
    crate::store::unwrap_document(v.clone())
        .ok_or_else(|| ToolError::InvalidArgs("not a vcad Document (needs nodes + roots)".into()))
}

/// Resolve `document_id` | `document` | `ir` to a Document.
pub(crate) fn resolve_doc(ctx: &ToolCtx, args: &Value) -> Result<Document, ToolError> {
    if let Some(id) = arg_str(args, "document_id") {
        return ctx.store.load(&DocumentId::from(id)).map_err(store_err);
    }
    for key in ["document", "ir"] {
        if let Some(v) = args.get(key).filter(|v| v.is_object()) {
            return parse_doc(v);
        }
    }
    let id = resolve_id(ctx, args)?;
    ctx.store.load(&id).map_err(store_err)
}

/// `document_id`, else the store's current document (vcad.app: the front
/// window; headless: the last one created or edited in this process).
pub(crate) fn resolve_id(ctx: &ToolCtx, args: &Value) -> Result<DocumentId, ToolError> {
    if let Some(id) = arg_str(args, "document_id") {
        return Ok(DocumentId::from(id));
    }
    ctx.store.current().ok_or_else(|| {
        ToolError::InvalidArgs(
            "pass document_id (from open_document / create_cad_loon) — no document is current".into(),
        )
    })
}

/// Read a `.vcad` (JSON) or `.loon` (evaluated) file.
pub(crate) fn read_doc_file(path: &str) -> Result<Document, ToolError> {
    let p = Path::new(path);
    let raw = std::fs::read_to_string(p)
        .map_err(|e| ToolError::InvalidArgs(format!("{path}: {e}")))?;
    if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("loon")) {
        eval_loon(&raw, p.parent().and_then(Path::to_str))
    } else {
        let v: Value = serde_json::from_str(&raw)
            .map_err(|e| ToolError::InvalidArgs(format!("{path}: {e}")))?;
        parse_doc(&v)
    }
}

pub(crate) fn handle(ctx: &ToolCtx, id: DocumentId) -> Result<ToolOutput, ToolError> {
    let doc = ctx.store.load(&id).map_err(store_err)?;
    let out = DocumentHandle {
        document_id: id.0,
        roots: doc.roots.len() as u64,
        nodes: doc.nodes.len() as u64,
        durable: ctx.store.durable(),
    };
    Ok(ToolOutput::structured(
        serde_json::to_value(out).unwrap_or_default(),
    ))
}

pub(crate) fn evaluate(doc: &Document) -> Result<vcad_eval::EvaluatedScene, ToolError> {
    let opts = vcad_eval::EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    };
    vcad_eval::evaluate_document(doc, &opts)
        .map_err(|e| ToolError::InvalidArgs(format!("evaluation failed: {e}")))
}

/// All evaluated triangle soups (root parts + placed instances).
pub(crate) fn meshes(doc: &Document, scene: &vcad_eval::EvaluatedScene) -> Vec<(Vec<f32>, Vec<u32>)> {
    let placed = vcad_eval::placed_instance_meshes(doc, scene);
    scene
        .parts
        .iter()
        .map(|p| (p.mesh.positions.clone(), p.mesh.indices.clone()))
        .chain(placed.into_iter().map(|(_, m)| (m.positions, m.indices)))
        .collect()
}

/// Mass properties from closed triangle meshes (divergence theorem).
pub fn mesh_properties(meshes: &[(Vec<f32>, Vec<u32>)]) -> (f64, f64, BBox, Xyz, u64) {
    let mut vol = 0.0;
    let mut area = 0.0;
    let mut c = [0.0f64; 3];
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut tris = 0u64;
    for (pos, idx) in meshes {
        let p = |i: u32| {
            let i = i as usize * 3;
            [pos[i] as f64, pos[i + 1] as f64, pos[i + 2] as f64]
        };
        for v in pos.as_chunks::<3>().0 {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k] as f64);
                hi[k] = hi[k].max(v[k] as f64);
            }
        }
        for t in idx.as_chunks::<3>().0 {
            let (a, b, d) = (p(t[0]), p(t[1]), p(t[2]));
            let cross = [
                b[1] * d[2] - b[2] * d[1],
                b[2] * d[0] - b[0] * d[2],
                b[0] * d[1] - b[1] * d[0],
            ];
            let v6 = a[0] * cross[0] + a[1] * cross[1] + a[2] * cross[2];
            vol += v6 / 6.0;
            for k in 0..3 {
                c[k] += v6 / 6.0 * (a[k] + b[k] + d[k]) / 4.0;
            }
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [d[0] - a[0], d[1] - a[1], d[2] - a[2]];
            let n = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            area += 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            tris += 1;
        }
    }
    let com = if vol.abs() > 1e-12 {
        Xyz {
            x: c[0] / vol,
            y: c[1] / vol,
            z: c[2] / vol,
        }
    } else {
        Xyz::default()
    };
    let bbox = if lo[0].is_finite() {
        BBox {
            min: Xyz {
                x: lo[0],
                y: lo[1],
                z: lo[2],
            },
            max: Xyz {
                x: hi[0],
                y: hi[1],
                z: hi[2],
            },
        }
    } else {
        BBox {
            min: Xyz::default(),
            max: Xyz::default(),
        }
    };
    (vol, area, bbox, com, tris)
}

fn stl_bytes(meshes: &[(Vec<f32>, Vec<u32>)]) -> Vec<u8> {
    let tris: usize = meshes.iter().map(|(_, i)| i.len() / 3).sum();
    let mut out = vec![0u8; 80];
    out[..15].copy_from_slice(b"vcad-mcp export");
    out.extend_from_slice(&(tris as u32).to_le_bytes());
    for (pos, idx) in meshes {
        for t in idx.as_chunks::<3>().0 {
            let v = |i: u32| {
                let i = i as usize * 3;
                [pos[i], pos[i + 1], pos[i + 2]]
            };
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let mut n = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len > 0.0 {
                n.iter_mut().for_each(|x| *x /= len);
            }
            for f in n.iter().chain(&a).chain(&b).chain(&c) {
                out.extend_from_slice(&f.to_le_bytes());
            }
            out.extend_from_slice(&[0, 0]);
        }
    }
    out
}

fn export_path(filename: &str) -> PathBuf {
    let p = Path::new(filename);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let base = std::env::var_os("VCAD_MCP_EXPORT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(p)
}

pub(crate) fn eval_loon(source: &str, base_dir: Option<&str>) -> Result<Document, ToolError> {
    vcad_loon::eval_vcad(source, base_dir.map(Path::new))
        .map_err(|e| ToolError::InvalidArgs(format!("loon error: {e}")))
}

fn expand_tilde(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// A big document's shape without its bulk (`get_document` on the open window).
fn summarize_doc(v: &Value, bytes: usize) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(o) = v.as_object() {
        for (k, x) in o {
            let s = match x {
                Value::Object(m) => json!(format!("{} entries", m.len())),
                Value::Array(a) => json!(format!("{} items", a.len())),
                other => other.clone(),
            };
            out.insert(k.clone(), s);
        }
        if let Some(r) = o.get("roots") {
            out.insert("roots".into(), r.clone());
        }
    }
    out.insert(
        "note".into(),
        json!(format!("Document is {bytes} bytes; pass full:true for all of it.")),
    );
    Value::Object(out)
}

/// Static server identity used by `server_info`.
#[derive(Clone)]
pub struct InfoSource {
    /// Server name.
    pub name: String,
    /// Server version.
    pub version: String,
    /// Process start.
    pub started: std::time::Instant,
    /// Instance id.
    pub instance_id: String,
}

fn entries() -> std::collections::HashMap<String, ToolSchemaEntry> {
    CoreTool::tool_schemas()
        .into_iter()
        .map(|e| (e.name.clone(), e))
        .collect()
}

fn def(
    entries: &mut std::collections::HashMap<String, ToolSchemaEntry>,
    name: &str,
    output: Option<Value>,
    f: impl Fn(&ToolCtx, Value) -> Result<ToolOutput, ToolError> + Send + Sync + 'static,
) -> ToolDef {
    let entry = entries
        .remove(name)
        .unwrap_or_else(|| panic!("no schema for {name}"));
    ToolDef::from_entry(entry, output, Arc::new(f))
}

/// The native tool set.
pub fn core_tools(info: InfoSource) -> Vec<ToolDef> {
    let mut e = entries();
    vec![
        def(
            &mut e,
            "server_info",
            Some(ServerInfoOut::output_schema()),
            move |ctx, _| {
                let out = ServerInfoOut {
                    name: info.name.clone(),
                    version: info.version.clone(),
                    uptime_s: info.started.elapsed().as_secs_f64(),
                    instance_id: info.instance_id.clone(),
                    boot_token: crate::store::boot_token().to_string(),
                    store: ctx.store.kind().as_str().to_string(),
                    durable: ctx.store.durable(),
                    session_dir: ctx.store.location().map(|p| p.display().to_string()),
                    artifact_store: "in-memory".into(),
                    protocol_versions: crate::protocol::SUPPORTED_PROTOCOL_VERSIONS
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                };
                Ok(ToolOutput::structured(
                    serde_json::to_value(out).unwrap_or_default(),
                ))
            },
        ),
        def(
            &mut e,
            "open_document",
            Some(DocumentHandle::output_schema()),
            |ctx, args| {
                if let Some(path) = arg_str(&args, "path") {
                    let expanded = expand_tilde(path);
                    if !expanded.exists() {
                        return Err(ToolError::InvalidArgs(format!("no file at {}", expanded.display())));
                    }
                    if let Some(id) = ctx.store.open_path(&expanded).map_err(store_err)? {
                        return handle(ctx, id);
                    }
                    let doc = read_doc_file(&expanded.display().to_string())?;
                    let name = expanded.file_stem().and_then(|s| s.to_str());
                    let id = ctx.store.create(doc, name).map_err(store_err)?;
                    return handle(ctx, id);
                }
                let doc = match args.get("initial").filter(|v| v.is_object()) {
                    Some(v) => parse_doc(v)?,
                    None => Document::default(),
                };
                let id = ctx.store.create(doc, None).map_err(store_err)?;
                handle(ctx, id)
            },
        ),
        def(&mut e, "get_document", None, |ctx, args| {
            let explicit = arg_str(&args, "document_id").is_some();
            let id = resolve_id(ctx, &args)?;
            let doc = ctx.store.load(&id).map_err(store_err)?;
            let mut v =
                serde_json::to_value(&doc).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            let full = args.get("full").and_then(Value::as_bool) == Some(true);
            let bytes = v.to_string().len();
            if !explicit && !full && bytes >= 60_000 {
                v = summarize_doc(&v, bytes);
            }
            Ok(ToolOutput {
                content: vec![json!({"type":"text","text": v.to_string()})],
                structured: None,
                is_error: false,
            })
        }),
        def(
            &mut e,
            "close_document",
            Some(CloseOut::output_schema()),
            |ctx, args| {
                let id: DocumentId = arg_str(&args, "document_id")
                    .ok_or_else(|| ToolError::InvalidArgs("document_id required".into()))?
                    .into();
                let existed = ctx.store.load(&id).is_ok();
                if existed {
                    ctx.store.close(&id).map_err(store_err)?;
                }
                Ok(ToolOutput::structured(json!({ "closed": existed })))
            },
        ),
        def(
            &mut e,
            "create_cad_loon",
            Some(DocumentHandle::output_schema()),
            |ctx, args| {
                // Features the native port doesn't have yet go to node verbatim.
                for k in ["loons", "use_loons", "files", "modules", "joint_state"] {
                    if args.get(k).is_some() {
                        return Err(ToolError::Delegate(format!("`{k}` not supported natively")));
                    }
                }
                if arg_str(&args, "mode") == Some("add") {
                    return Err(ToolError::Delegate(
                        "mode=add not supported natively".into(),
                    ));
                }
                let source = arg_str(&args, "source")
                    .ok_or_else(|| ToolError::InvalidArgs("source required".into()))?;
                let doc = eval_loon(source, arg_str(&args, "base_dir"))?;
                let label = arg_str(&args, "name")
                    .filter(|n| !n.is_empty())
                    .map_or_else(|| "create_cad_loon".to_string(), str::to_string);
                // vcad.app: no id means "the open window" (the user watches it
                // land as one undo step); headless mints a new session.
                let target = arg_str(&args, "document_id").map(DocumentId::from).or_else(|| {
                    (ctx.store.kind() == crate::store::StoreKind::App)
                        .then(|| ctx.store.current())
                        .flatten()
                });
                // Replacing a document keeps its board / schematic.
                let mut note = None;
                let doc = match target.as_ref().and_then(|id| ctx.store.load(id).ok()) {
                    Some(session) => {
                        let sv = serde_json::to_value(&session).unwrap_or_default();
                        let mut dv = serde_json::to_value(&doc).unwrap_or_default();
                        match super::loon_carry::carry_electronics(&sv, &mut dv) {
                            Ok(Some(n)) => {
                                note = Some(n);
                                parse_doc(&dv)?
                            }
                            Ok(None) => doc,
                            Err(e) => return Ok(ToolOutput::error(e)),
                        }
                    }
                    None => doc,
                };
                // The live window fails closed: a part the kernel refused
                // never lands as a silent "no geometry" part.
                if ctx.store.kind() == crate::store::StoreKind::App {
                    let scene = evaluate(&doc)?;
                    if !scene.failures.is_empty() {
                        let why: Vec<String> = scene
                            .failures
                            .iter()
                            .map(|f| format!("{} (node {}): {}", f.scope, f.node_id, f.error))
                            .collect();
                        return Ok(ToolOutput::error(format!(
                            "loon program loaded but some parts failed to build; document left unchanged:\n{}",
                            why.join("\n")
                        )));
                    }
                }
                let id = match target {
                    Some(id) => {
                        ctx.store
                            .commit(&id, doc, CommitMeta::agent(label))
                            .map_err(store_err)?;
                        id
                    }
                    None => ctx.store.create(doc, None).map_err(store_err)?,
                };
                let mut out = handle(ctx, id)?;
                if let Some(n) = note {
                    out.content.push(json!({"type": "text", "text": n}));
                }
                Ok(out)
            },
        ),
        def(
            &mut e,
            "load_document",
            Some(DocumentHandle::output_schema()),
            |ctx, args| {
                let Some(path) = arg_str(&args, "path") else {
                    return Err(ToolError::Delegate(
                        "named saves are served by the node server".into(),
                    ));
                };
                let p = Path::new(path);
                let doc = read_doc_file(path)?;
                let name = p.file_stem().and_then(|s| s.to_str());
                let id = ctx.store.create(doc, name).map_err(store_err)?;
                handle(ctx, id)
            },
        ),
        def(
            &mut e,
            "inspect_cad",
            Some(InspectOut::output_schema()),
            |ctx, args| {
                let doc = resolve_doc(ctx, &args)?;
                let scene = evaluate(&doc)?;
                let m = meshes(&doc, &scene);
                let (volume, surface_area, bounding_box, center_of_mass, triangles) =
                    mesh_properties(&m);
                let out = InspectOut {
                    volume,
                    surface_area,
                    bounding_box,
                    center_of_mass,
                    triangles,
                    parts: m.len() as u64,
                    failures: scene
                        .failures
                        .iter()
                        .map(|f| format!("{} (node {}): {}", f.scope, f.node_id, f.error))
                        .collect(),
                };
                Ok(ToolOutput::structured(
                    serde_json::to_value(out).unwrap_or_default(),
                ))
            },
        ),
        def(&mut e, "render_view", None, |ctx, args| {
            if args.get("section").is_some() || args.get("joint_state").is_some() {
                return Err(ToolError::Delegate(
                    "section/joint_state not supported natively".into(),
                ));
            }
            let doc = resolve_doc(ctx, &args)?;
            let view = match (
                args.get("azimuth").and_then(Value::as_f64),
                args.get("elevation").and_then(Value::as_f64),
            ) {
                (Some(azimuth), Some(elevation)) => vcad_render::View::Orbit { azimuth, elevation },
                _ => match arg_str(&args, "view").unwrap_or("iso") {
                    "top" => vcad_render::View::Top,
                    "front" => vcad_render::View::Front,
                    "side" => vcad_render::View::Side,
                    _ => vcad_render::View::Isometric,
                },
            };
            let opts = vcad_render::RasterOptions {
                view,
                size_px: args
                    .get("width_px")
                    .and_then(Value::as_u64)
                    .unwrap_or(768)
                    .clamp(64, 4096) as u32,
                focus: arg_str(&args, "focus").map(str::to_string),
                ..Default::default()
            };
            let raw =
                serde_json::to_string(&doc).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            let png = vcad_render::render_png_str(&raw, &opts)
                .map_err(|e| ToolError::InvalidArgs(format!("render failed: {e}")))?;
            use base64::Engine;
            Ok(ToolOutput {
                content: vec![json!({
                    "type": "image",
                    "mimeType": "image/png",
                    "data": base64::engine::general_purpose::STANDARD.encode(&png),
                })],
                structured: None,
                is_error: false,
            })
        }),
        def(
            &mut e,
            "export_cad",
            Some(ExportOut::output_schema()),
            |ctx, args| {
                let filename = arg_str(&args, "filename")
                    .ok_or_else(|| ToolError::InvalidArgs("filename required".into()))?;
                let ext = Path::new(filename)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ext != "stl" && ext != "vcad" {
                    return Err(ToolError::Delegate(format!(
                        ".{ext} export is served by the node exporter"
                    )));
                }
                let doc = resolve_doc(ctx, &args)?;
                let bytes = if ext == "vcad" {
                    serde_json::to_vec(&doc).map_err(|e| ToolError::InvalidArgs(e.to_string()))?
                } else {
                    let scene = evaluate(&doc)?;
                    stl_bytes(&meshes(&doc, &scene))
                };
                let path = export_path(filename);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&path, &bytes)
                    .map_err(|e| ToolError::InvalidArgs(format!("{}: {e}", path.display())))?;
                let out = ExportOut {
                    path: path.display().to_string(),
                    format: ext,
                    bytes: bytes.len() as u64,
                };
                Ok(ToolOutput::structured(
                    serde_json::to_value(out).unwrap_or_default(),
                ))
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_cube_properties() {
        // Outward-wound unit cube.
        let pos: Vec<f32> = vec![
            0., 0., 0., 1., 0., 0., 1., 1., 0., 0., 1., 0., 0., 0., 1., 1., 0., 1., 1., 1., 1., 0.,
            1., 1.,
        ];
        let idx = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        let (v, a, bb, com, t) = mesh_properties(&[(pos, idx)]);
        assert!((v - 1.0).abs() < 1e-9, "{v}");
        assert!((a - 6.0).abs() < 1e-9);
        assert_eq!(t, 12);
        assert!((com.x - 0.5).abs() < 1e-9 && (com.z - 0.5).abs() < 1e-9);
        assert_eq!(bb.max.y, 1.0);
    }

    #[test]
    fn every_core_tool_has_schema() {
        let tools = core_tools(InfoSource {
            name: "t".into(),
            version: "0".into(),
            started: std::time::Instant::now(),
            instance_id: "i".into(),
        });
        assert_eq!(tools.len(), 9);
        let inspect = tools.iter().find(|t| t.name == "inspect_cad").unwrap();
        let out = inspect.output_schema.as_ref().unwrap();
        assert_eq!(
            out["properties"]["bounding_box"]["properties"]["min"]["required"],
            json!(["x", "y", "z"])
        );
    }
}
