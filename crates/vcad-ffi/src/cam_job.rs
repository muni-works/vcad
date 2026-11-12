//! Multi-tool CAM jobs for the native CNC workspace (`vcad_cam_job`,
//! `vcad_cam_tool_library`): a tool library, an ordered list of operations
//! each bound to a tool, stock-sim verification of the whole job, and a
//! G-code post with tool changes.
//!
//! The single-tool `vcad_cam_generate` in [`crate::cam`] stays as-is for the
//! existing per-operation flow; this is the job-level layer the web app's
//! `ToolLibrary` / `OperationList` / `CamPanel` provide.
//!
//! # Frame
//!
//! Stock top is Z = 0 with material below (`[0, w] × [0, h] × [-depth, 0]`),
//! G54 origin at the stock's lower-left top corner — the convention the
//! native machine bar zeroes to.
//!
//! # Post dialects
//!
//! - `linuxcnc`: `T<n> M6` + `G43 H<n>` at every tool change.
//! - `grbl` (default): stock Grbl has no tool changer and no `M6`, so the
//!   combined program stops the spindle, retracts and pauses with `M0` and a
//!   comment naming the next tool; the operator must re-touch Z before
//!   resuming. Because a Grbl `M0` hold does not allow jogging on every
//!   firmware, `programs` also carries one self-contained file per tool — the
//!   recommended way to run a multi-tool job on Grbl.
//!
//! # Verification
//!
//! Every operation's toolpath (rapids included — a rapid through material is
//! a crash) is subtracted from an octree stock with that operation's cutter
//! envelope. With a `target` the result is graded against the part
//! (`vcad-kernel-stocksim`: no gouge, no excess beyond the allowance); without
//! one the report still carries removed volume and a floor check (nothing
//! cut below the deepest programmed depth, nothing outside the stock XY plus
//! the cutter radius).

use std::ffi::c_char;
use std::ptr;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use vcad_kernel::vcad_kernel_stocksim::{verify_stock_against_mesh, Stock, VerifyOptions};
use vcad_kernel_cam::{
    ArcDir, CamSettings, Contour, Contour2D, Face, Pocket2D, Point2D, Tool, ToolLibrary, Toolpath,
    ToolpathSegment,
};

use crate::analysis::{evaluate, parse_request};
use crate::err;
use crate::json::{guard_ptr, into_c};

/// One tool in the job's library.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolSpec {
    /// Tool number (T-word).
    pub number: u32,
    #[serde(default)]
    pub name: String,
    /// flat_endmill | ball_endmill | bull_endmill | vbit | drill | face_mill
    #[serde(rename = "type", default = "default_kind")]
    pub kind: String,
    pub diameter: f64,
    #[serde(default = "default_flute")]
    pub flute_length: f64,
    #[serde(default = "default_flutes")]
    pub flutes: u8,
    #[serde(default)]
    pub corner_radius: f64,
    #[serde(default = "default_angle")]
    pub angle: f64,
    #[serde(default)]
    pub rpm: f64,
    #[serde(default)]
    pub feed: f64,
    #[serde(default)]
    pub plunge: f64,
}
fn default_kind() -> String {
    "flat_endmill".into()
}
fn default_flute() -> f64 {
    20.0
}
fn default_flutes() -> u8 {
    2
}
fn default_angle() -> f64 {
    90.0
}

impl ToolSpec {
    fn tool(&self) -> Tool {
        match self.kind.as_str() {
            "ball_endmill" => Tool::BallEndMill {
                diameter: self.diameter,
                flute_length: self.flute_length,
                flutes: self.flutes,
            },
            "bull_endmill" => Tool::BullEndMill {
                diameter: self.diameter,
                corner_radius: self.corner_radius.max(0.01),
                flute_length: self.flute_length,
                flutes: self.flutes,
            },
            "vbit" => Tool::VBit {
                diameter: self.diameter,
                angle: self.angle,
            },
            "drill" => Tool::Drill {
                diameter: self.diameter,
                point_angle: if self.angle > 0.0 { self.angle } else { 118.0 },
            },
            "face_mill" => Tool::FaceMill {
                diameter: self.diameter,
                inserts: self.flutes.max(1),
            },
            _ => Tool::FlatEndMill {
                diameter: self.diameter,
                flute_length: self.flute_length,
                flutes: self.flutes,
            },
        }
    }
    fn label(&self) -> String {
        if self.name.is_empty() {
            format!("{:.3} mm {}", self.diameter, self.kind.replace('_', " "))
        } else {
            self.name.clone()
        }
    }
}

/// One machining operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpSpec {
    /// face | pocket | circle_pocket | profile | contour_outside | contour_inside
    pub kind: String,
    /// Tool number from the library.
    pub tool: u32,
    #[serde(default)]
    pub name: String,
    pub depth: f64,
    pub stepdown: f64,
    pub stepover: f64,
    /// Overrides; 0 = the tool's default.
    #[serde(default)]
    pub feed: f64,
    #[serde(default)]
    pub plunge: f64,
    #[serde(default)]
    pub rpm: f64,
    /// Rectangle (pocket / profile / face region; face defaults to the stock).
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub width: f64,
    #[serde(default)]
    pub height: f64,
    /// Circle pocket.
    #[serde(default)]
    pub cx: f64,
    #[serde(default)]
    pub cy: f64,
    #[serde(default)]
    pub radius: f64,
    /// Contour polyline (closed), stock frame.
    #[serde(default)]
    pub contour: Vec<[f64; 2]>,
    #[serde(default)]
    pub tabs: u32,
    #[serde(default)]
    pub tab_width: f64,
    #[serde(default)]
    pub tab_height: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StockSpec {
    width: f64,
    height: f64,
    depth: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetSpec {
    /// IR document holding the part to produce.
    doc: Value,
    #[serde(default)]
    part: usize,
    /// `"auto"` (default): XY min → stock origin, bottom → stock bottom. Or an explicit
    /// `[dx, dy, dz]` translation into the stock frame.
    #[serde(default)]
    offset: Option<[f64; 3]>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobRequest {
    #[serde(default = "default_name")]
    name: String,
    stock: StockSpec,
    tools: Vec<ToolSpec>,
    operations: Vec<OpSpec>,
    #[serde(default = "default_clearance")]
    clearance: f64,
    #[serde(default = "default_post")]
    post: String,
    #[serde(default = "default_true")]
    verify: bool,
    #[serde(default)]
    target: Option<TargetSpec>,
    #[serde(default)]
    resolution: Option<f64>,
    #[serde(default)]
    tolerance: Option<f64>,
    #[serde(default)]
    allowance: Option<f64>,
}
fn default_name() -> String {
    "vcad job".into()
}
fn default_clearance() -> f64 {
    5.0
}
fn default_post() -> String {
    "grbl".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
struct Move {
    to: [f64; 3],
    rapid: bool,
    feed: f64,
    /// Index into `operations`.
    op: usize,
    /// Tool number cutting this move.
    tool: u32,
}

fn finite_pos(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

fn polyline(points: &[[f64; 2]]) -> Result<Contour, String> {
    if points.len() < 3 {
        return Err("a contour needs at least three points".into());
    }
    let mut c = Contour::new(Point2D::new(points[0][0], points[0][1]));
    for p in &points[1..] {
        c.line_to(Point2D::new(p[0], p[1]));
    }
    let (a, b) = (points[0], points[points.len() - 1]);
    if (a[0] - b[0]).hypot(a[1] - b[1]) > 1e-6 {
        c.line_to(Point2D::new(a[0], a[1]));
    }
    Ok(c)
}

/// Generate the kernel toolpath for one operation.
fn toolpath(
    op: &OpSpec,
    tool: &ToolSpec,
    stock: &StockSpec,
    settings: &CamSettings,
) -> Result<Toolpath, String> {
    let t = tool.tool();
    let (w, h) = if op.width > 0.0 && op.height > 0.0 {
        (op.width, op.height)
    } else {
        (stock.width, stock.height)
    };
    let path = match op.kind.as_str() {
        "face" => Face::new(op.x, op.y, op.x + w, op.y + h, op.depth).generate(&t, settings),
        "pocket" => Pocket2D::rectangle(op.x, op.y, w, h, op.depth).generate(&t, settings),
        "circle_pocket" => {
            if !finite_pos(op.radius) {
                return Err("circle pocket needs a positive radius".into());
            }
            Pocket2D::circle(op.cx, op.cy, op.radius, op.depth).generate(&t, settings)
        }
        "profile" => Contour2D::outside(Contour::rectangle(op.x, op.y, w, h), op.depth)
            .generate(&t, settings),
        "contour_outside" | "contour_inside" => {
            let c = polyline(&op.contour)?;
            let mut o = if op.kind == "contour_outside" {
                Contour2D::outside(c, op.depth)
            } else {
                Contour2D::inside(c, op.depth)
            };
            if op.tabs > 0 {
                if !finite_pos(op.tab_width)
                    || !finite_pos(op.tab_height)
                    || op.tab_height > op.depth
                    || op.tabs > 64
                {
                    return Err(
                        "tabs need a positive width and a height no deeper than the cut".into(),
                    );
                }
                o = o.with_tabs(op.tabs as usize, op.tab_width, op.tab_height);
            }
            o.generate(&t, settings)
        }
        other => return Err(format!("unknown operation `{other}`")),
    };
    path.map_err(|e| e.to_string())
}

/// Linearize an XY arc (absolute centre) in ~5° steps.
fn arc_points(from: [f64; 3], to: [f64; 3], center: [f64; 3], dir: ArcDir) -> Vec<[f64; 3]> {
    use std::f64::consts::TAU;
    let r = (from[0] - center[0]).hypot(from[1] - center[1]);
    let a0 = (from[1] - center[1]).atan2(from[0] - center[0]);
    let a1 = (to[1] - center[1]).atan2(to[0] - center[0]);
    let delta = match dir {
        ArcDir::Ccw => {
            let d = a1 - a0;
            if d <= 0.0 {
                d + TAU
            } else {
                d
            }
        }
        ArcDir::Cw => {
            let d = a0 - a1;
            -(if d <= 0.0 { d + TAU } else { d })
        }
    };
    let n = ((delta.abs() / 5f64.to_radians()).ceil() as usize).max(1);
    (1..=n)
        .map(|i| {
            let f = i as f64 / n as f64;
            if i == n {
                return to;
            }
            let a = a0 + delta * f;
            [
                center[0] + r * a.cos(),
                center[1] + r * a.sin(),
                from[2] + (to[2] - from[2]) * f,
            ]
        })
        .collect()
}

/// Run a job given as a JSON request value (in-crate callers, e.g. the
/// instrument mill path).
pub(crate) fn run_job_value(v: &Value) -> Result<Value, String> {
    let req: JobRequest = serde_json::from_value(v.clone()).map_err(|e| format!("job: {e}"))?;
    job(&req)
}

fn job(req: &JobRequest) -> Result<Value, String> {
    let s = &req.stock;
    if ![s.width, s.height, s.depth, req.clearance]
        .iter()
        .all(|v| finite_pos(*v))
        || s.width > 2000.0
        || s.height > 2000.0
        || s.depth > 300.0
        || req.clearance > 100.0
    {
        return Err("stock and clearance must be finite, positive and bounded".into());
    }
    if req.tools.is_empty() || req.operations.is_empty() {
        return Err("a job needs at least one tool and one operation".into());
    }
    let dialect = match req.post.as_str() {
        "grbl" | "linuxcnc" => req.post.as_str(),
        other => return Err(format!("unknown post `{other}` (grbl | linuxcnc)")),
    };
    for t in &req.tools {
        if !finite_pos(t.diameter) || t.diameter > 100.0 || t.number == 0 || t.number > 999 {
            return Err(format!(
                "tool T{}: diameter must be in (0, 100] mm and number 1–999",
                t.number
            ));
        }
    }
    let mut numbers: Vec<u32> = req.tools.iter().map(|t| t.number).collect();
    numbers.sort_unstable();
    if numbers.windows(2).any(|w| w[0] == w[1]) {
        return Err("tool numbers must be unique".into());
    }

    let mut moves: Vec<Move> = Vec::new();
    let mut passes: Vec<(Tool, Toolpath)> = Vec::new();
    let mut op_reports = Vec::new();
    // (tool number, code) per contiguous same-tool block, for per-tool files.
    let mut blocks: Vec<(u32, f64, String)> = Vec::new();
    let mut deepest = 0.0f64;
    let clear = req.clearance;

    for (i, op) in req.operations.iter().enumerate() {
        let tool = req
            .tools
            .iter()
            .find(|t| t.number == op.tool)
            .ok_or_else(|| {
                format!(
                    "operation {}: tool T{} is not in the library",
                    i + 1,
                    op.tool
                )
            })?;
        let feed = if op.feed > 0.0 {
            op.feed
        } else if tool.feed > 0.0 {
            tool.feed
        } else {
            400.0
        };
        let plunge = if op.plunge > 0.0 {
            op.plunge
        } else if tool.plunge > 0.0 {
            tool.plunge
        } else {
            100.0
        };
        let rpm = if op.rpm > 0.0 {
            op.rpm
        } else if tool.rpm > 0.0 {
            tool.rpm
        } else {
            10000.0
        };
        if ![op.depth, op.stepdown, op.stepover, feed, plunge, rpm]
            .iter()
            .all(|v| finite_pos(*v))
            || op.depth > s.depth + 1e-9
            || op.stepover > tool.diameter + 1e-9
            || op.stepdown < 0.001
            || op.stepover < 0.001
            || feed > 30000.0
            || plunge > 30000.0
            || rpm > 100000.0
        {
            return Err(format!(
                "operation {} ({}): depth must be within the stock, stepover ≤ tool diameter, feeds/RPM positive",
                i + 1,
                op.kind
            ));
        }
        let extent = if op.width > 0.0 {
            op.width.max(op.height)
        } else {
            s.width.max(s.height)
        };
        if (op.depth / op.stepdown).ceil() * ((extent + tool.diameter) / op.stepover).ceil()
            > 20000.0
        {
            return Err(format!(
                "operation {}: too many passes; raise stepdown or stepover",
                i + 1
            ));
        }
        let settings = CamSettings {
            stepover: op.stepover,
            stepdown: op.stepdown,
            feed_rate: feed,
            plunge_rate: plunge,
            spindle_rpm: rpm,
            safe_z: clear,
            retract_z: clear,
        };
        let path = toolpath(op, tool, s, &settings)
            .map_err(|e| format!("operation {} ({}): {e}", i + 1, op.kind))?;
        deepest = deepest.max(op.depth);

        // Emit motion with explicit coordinates and vertical retracts.
        let mut code = format!(
            "({}: {} with T{} {})\n",
            i + 1,
            if op.name.is_empty() {
                op.kind.as_str()
            } else {
                op.name.as_str()
            },
            tool.number,
            tool.label()
        );
        let new_tool = blocks.last().map(|b| b.0) != Some(tool.number);
        if new_tool {
            blocks.push((tool.number, rpm, String::new()));
        }
        code.push_str(&format!("M3 S{rpm:.0}\n"));
        let margin = tool.diameter / 2.0 + 0.002;
        let mut prev: Option<[f64; 3]> = None;
        let mut emitted = 0usize;
        let mut push = |to: [f64; 3],
                        rapid: bool,
                        f: f64,
                        code: &mut String,
                        prev: &mut Option<[f64; 3]>|
         -> Result<(), String> {
            if to.iter().any(|v| !v.is_finite()) {
                return Err("non-finite generated motion".into());
            }
            if to[0] < -margin
                || to[0] > s.width + margin
                || to[1] < -margin
                || to[1] > s.height + margin
                || to[2] < -op.depth - 0.001
                || to[2] > clear + 0.001
            {
                return Err(format!(
                    "operation {}: generated motion leaves the stock region",
                    i + 1
                ));
            }
            if rapid {
                if let Some(from) = *prev {
                    if from[2] < clear - 0.001 {
                        code.push_str(&format!("G0 Z{clear:.3}\n"));
                        moves.push(Move {
                            to: [from[0], from[1], clear],
                            rapid: true,
                            feed: 0.0,
                            op: i,
                            tool: tool.number,
                        });
                    }
                }
            }
            code.push_str(&format!(
                "G{} X{:.3} Y{:.3} Z{:.3}",
                if rapid { 0 } else { 1 },
                to[0],
                to[1],
                to[2]
            ));
            if !rapid {
                code.push_str(&format!(" F{f:.1}"));
            }
            code.push('\n');
            moves.push(Move {
                to,
                rapid,
                feed: f,
                op: i,
                tool: tool.number,
            });
            *prev = Some(to);
            Ok(())
        };
        for seg in &path.segments {
            match seg {
                ToolpathSegment::Rapid { to } => push(*to, true, 0.0, &mut code, &mut prev)?,
                ToolpathSegment::Linear { to, feed } => {
                    push(*to, false, *feed, &mut code, &mut prev)?
                }
                ToolpathSegment::Arc {
                    to,
                    center,
                    dir,
                    feed,
                    ..
                } => {
                    let from = prev.unwrap_or([to[0], to[1], clear]);
                    for p in arc_points(from, *to, *center, *dir) {
                        push(p, false, *feed, &mut code, &mut prev)?;
                    }
                }
                _ => continue,
            }
            emitted += 1;
        }
        if emitted == 0 {
            return Err(format!(
                "operation {}: the tool does not fit the region",
                i + 1
            ));
        }
        if let Some(p) = prev {
            if p[2] < clear - 0.001 {
                code.push_str(&format!("G0 Z{clear:.3}\n"));
                moves.push(Move {
                    to: [p[0], p[1], clear],
                    rapid: true,
                    feed: 0.0,
                    op: i,
                    tool: tool.number,
                });
            }
        }
        blocks.last_mut().unwrap().2.push_str(&code);
        op_reports.push(json!({
            "index": i,
            "kind": op.kind,
            "tool": tool.number,
            "cuttingLengthMm": path.cutting_length(),
            "estimatedSeconds": path.estimated_time(),
            "segments": path.len(),
        }));
        passes.push((tool.tool(), path));
    }

    // ── post ──
    let header = |title: &str| {
        format!("({title} - G54 at stock top lower-left, Z0 = stock top)\nG21\nG90\nG94\nG17\nG40\nG49\nG54\nM5\nG0 Z{clear:.3}\n")
    };
    let mut combined = header(&format!(
        "{} - {} tools, {} ops",
        req.name,
        blocks
            .iter()
            .map(|b| b.0)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        req.operations.len()
    ));
    let mut programs = Vec::new();
    for (k, (num, _rpm, body)) in blocks.iter().enumerate() {
        let tool = req.tools.iter().find(|t| t.number == *num).unwrap();
        let change = match dialect {
            "linuxcnc" => format!("M5\nG0 Z{clear:.3}\n(Tool {num}: {})\nT{num} M6\nG43 H{num}\n", tool.label()),
            _ if k == 0 => format!("(Load T{num}: {} and touch off Z on the stock top)\n", tool.label()),
            _ => format!(
                "M5\nG0 Z{clear:.3}\n(TOOL CHANGE: load T{num} {} - re-touch Z0 on the stock top before resuming)\nM0\n",
                tool.label()
            ),
        };
        combined.push_str(&change);
        combined.push_str(body);
        let mut single = header(&format!(
            "{} - part {} of {}: T{num} {}",
            req.name,
            k + 1,
            blocks.len(),
            tool.label()
        ));
        single.push_str(&format!(
            "(Load T{num}: {} and touch off Z on the stock top)\n",
            tool.label()
        ));
        single.push_str(body);
        single.push_str(&format!("G0 Z{clear:.3}\nM5\nM2\n"));
        programs.push(json!({"tool": num, "name": tool.label(), "gcode": single}));
    }
    combined.push_str(&format!("G0 Z{clear:.3}\nM5\nM2\n"));

    // ── stock sim ──
    let verification = if req.verify {
        Some(simulate(req, &passes, deepest)?)
    } else {
        None
    };
    let tool_changes = blocks.len().saturating_sub(1);
    Ok(json!({
        "gcode": combined,
        "post": dialect,
        "programs": programs,
        "toolChanges": tool_changes,
        "moves": moves,
        "operations": op_reports,
        "verification": verification,
    }))
}

/// Remaining material volume by sampling the stock SDF on a grid (≤ ~1M
/// samples) — the marching-cubes surface is not closed at the stock walls,
/// so its signed volume is not trustworthy.
fn sampled_volume(stock: &Stock) -> f64 {
    let b = stock.bounds();
    let ext = [b[3] - b[0], b[4] - b[1], b[5] - b[2]];
    let cell = (ext[0] * ext[1] * ext[2] / 1.0e6)
        .cbrt()
        .max(stock.resolution() * 0.5);
    let n = ext.map(|e| ((e / cell).ceil() as usize).max(1));
    let mut inside = 0usize;
    for i in 0..n[0] {
        let x = b[0] + ext[0] * (i as f64 + 0.5) / n[0] as f64;
        for j in 0..n[1] {
            let y = b[1] + ext[1] * (j as f64 + 0.5) / n[1] as f64;
            for k in 0..n[2] {
                let z = b[2] + ext[2] * (k as f64 + 0.5) / n[2] as f64;
                if stock.sdf_at(x, y, z) <= 0.0 {
                    inside += 1;
                }
            }
        }
    }
    ext[0] * ext[1] * ext[2] * inside as f64 / (n[0] * n[1] * n[2]) as f64
}

fn simulate(req: &JobRequest, passes: &[(Tool, Toolpath)], deepest: f64) -> Result<Value, String> {
    let s = &req.stock;
    let bounds = [0.0, 0.0, -s.depth, s.width, s.height, 0.0];
    let auto_res = (s.width.max(s.height).max(s.depth) / 160.0).max(0.1);
    let res = req.resolution.unwrap_or(auto_res);
    if !finite_pos(res) {
        return Err("resolution must be positive".into());
    }
    let mut stock = Stock::from_box(bounds, res);
    let stock_volume = s.width * s.height * s.depth;
    let mut per_tool = Vec::new();
    let mut prev_left = stock_volume;
    for ((tool, path), op) in passes.iter().zip(&req.operations) {
        stock.subtract_toolpath(tool, path);
        let left = sampled_volume(&stock);
        per_tool.push(
            json!({"tool": op.tool, "kind": op.kind, "removedMm3": (prev_left - left).max(0.0)}),
        );
        prev_left = left;
    }
    // Floor check: no material removed below the deepest programmed cut.
    let probe = res * 1.5;
    let floor_z = -deepest - probe;
    let mut floor_breaches = 0usize;
    if floor_z > -s.depth {
        let n = 24;
        for ix in 0..n {
            for iy in 0..n {
                let x = s.width * (ix as f64 + 0.5) / n as f64;
                let y = s.height * (iy as f64 + 0.5) / n as f64;
                if stock.sdf_at(x, y, floor_z) > 0.0 {
                    floor_breaches += 1;
                }
            }
        }
    }
    let mut out = json!({
        "resolution": res,
        "stockVolumeMm3": stock_volume,
        "remainingVolumeMm3": prev_left,
        "removedVolumeMm3": (stock_volume - prev_left).max(0.0),
        "perOperation": per_tool,
        "floorCheck": {"z": floor_z, "breaches": floor_breaches, "pass": floor_breaches == 0},
    });
    let Some(target) = &req.target else {
        out["pass"] = json!(floor_breaches == 0);
        out["graded"] = json!(false);
        return Ok(out);
    };
    let doc = vcad_ir::Document::from_json(&target.doc.to_string())
        .map_err(|e| format!("target doc: {e}"))?;
    let scene = evaluate(&doc)?;
    let part = scene
        .parts
        .get(target.part)
        .ok_or_else(|| format!("target part {} out of range", target.part))?;
    let mut verts = part.mesh.positions.clone();
    let offset = match target.offset {
        Some(o) => o,
        None => {
            let mut lo = [f64::INFINITY; 3];
            for c in verts.as_chunks::<3>().0.iter() {
                for k in 0..3 {
                    lo[k] = lo[k].min(c[k] as f64);
                }
            }
            // The part sits on the stock's bottom face, corner at the origin.
            [-lo[0], -lo[1], -s.depth - lo[2]]
        }
    };
    for c in verts.as_chunks_mut::<3>().0.iter_mut() {
        for k in 0..3 {
            c[k] += offset[k] as f32;
        }
    }
    let opts = VerifyOptions {
        tolerance: req.tolerance.unwrap_or(0.1),
        allowance: req.allowance.unwrap_or(0.1),
        max_examples: 16,
    };
    let v = verify_stock_against_mesh(&stock, &verts, &part.mesh.indices, &opts);
    out["graded"] = json!(true);
    out["targetOffset"] = json!(offset);
    out["pass"] = json!(v.pass && floor_breaches == 0);
    out["stock"] = serde_json::to_value(&v).map_err(|e| e.to_string())?;
    Ok(out)
}

fn tool_type(t: &Tool) -> &'static str {
    match t {
        Tool::FlatEndMill { .. } => "flat_endmill",
        Tool::BallEndMill { .. } => "ball_endmill",
        Tool::BullEndMill { .. } => "bull_endmill",
        Tool::VBit { .. } => "vbit",
        Tool::Drill { .. } => "drill",
        Tool::FaceMill { .. } => "face_mill",
    }
}

/// The default tool library in [`ToolSpec`] wire form.
pub(crate) fn default_library() -> Value {
    let lib = ToolLibrary::default_library();
    let tools: Vec<ToolSpec> = lib
        .tools
        .iter()
        .map(|e| ToolSpec {
            number: e.number,
            name: e.name.clone(),
            kind: tool_type(&e.tool).into(),
            diameter: e.tool.diameter(),
            flute_length: e.tool.max_depth().unwrap_or(20.0),
            flutes: e.tool.flutes(),
            corner_radius: e.tool.corner_radius(),
            angle: match e.tool {
                Tool::VBit { angle, .. } => angle,
                Tool::Drill { point_angle, .. } => point_angle,
                _ => 90.0,
            },
            rpm: e.default_rpm,
            feed: e.default_feed,
            plunge: e.default_plunge,
        })
        .collect();
    json!({ "tools": tools })
}

fn finish(r: Result<String, String>) -> *mut c_char {
    match r {
        Ok(s) => into_c(s),
        Err(e) => {
            err::set_error(e);
            ptr::null_mut()
        }
    }
}

/// Multi-tool job request → `{gcode, programs, moves, operations,
/// verification}`. Free with `vcad_string_free`; null + `vcad_last_error`
/// on a refused job.
#[no_mangle]
pub extern "C" fn vcad_cam_job(request_json: *const c_char) -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_cam_job", || {
        finish((|| {
            let v = parse_request(request_json)?;
            let req: JobRequest = serde_json::from_value(v).map_err(|e| format!("job: {e}"))?;
            Ok(job(&req)?.to_string())
        })())
    })
}

/// The kernel's default tool library (`{"tools": [...]}`).
#[no_mangle]
pub extern "C" fn vcad_cam_tool_library() -> *mut c_char {
    err::clear_error();
    guard_ptr("vcad_cam_tool_library", || {
        finish(Ok(default_library().to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 40 × 30 × 10 stock; face 1 mm with a 6 mm end mill (T1), then a
    /// Ø14 × 3 mm circular pocket with a 3.175 mm end mill (T2).
    fn two_tool_job(target: bool) -> Value {
        let mut j = json!({
            "name": "gate",
            "stock": {"width": 40, "height": 30, "depth": 10},
            "tools": [
                {"number": 1, "type": "flat_endmill", "diameter": 6, "rpm": 12000, "feed": 800, "plunge": 200},
                {"number": 2, "type": "flat_endmill", "diameter": 3.175, "rpm": 16000, "feed": 500, "plunge": 120}
            ],
            "operations": [
                {"kind": "face", "tool": 1, "depth": 1, "stepdown": 1, "stepover": 4},
                {"kind": "circle_pocket", "tool": 2, "cx": 20, "cy": 15, "radius": 7, "depth": 4, "stepdown": 1, "stepover": 1.2}
            ],
            "clearance": 5,
            "resolution": 0.25
        });
        if target {
            // The part: stock minus the faced 1 mm, minus a Ø14 pocket 3 mm
            // deep below the new top. Modelled at the origin; the "auto" offset
            // sets it on the stock bottom.
            j["target"] = json!({"doc": {
                "version": "0.1",
                "nodes": {
                    "1": {"id":1,"op":{"type":"Cube","size":{"x":40,"y":30,"z":9}}},
                    "2": {"id":2,"op":{"type":"Cylinder","radius":7,"height":4,"segments":96}},
                    "3": {"id":3,"op":{"type":"Translate","child":2,"offset":{"x":20,"y":15,"z":6}}},
                    "4": {"id":4,"name":"part","op":{"type":"Difference","left":1,"right":3}}
                },
                "materials": {}, "part_materials": {},
                "roots": [{"root":4,"material":"default"}]
            }});
        }
        j
    }

    fn run(v: Value) -> Result<Value, String> {
        let req: JobRequest = serde_json::from_value(v).map_err(|e| e.to_string())?;
        job(&req)
    }

    /// Gate: two-tool job, verified by the stock sim against the target part,
    /// and posted with a tool change.
    #[test]
    fn two_tool_job_is_verified_and_posted() {
        let out = run(two_tool_job(true)).unwrap();
        let v = &out["verification"];
        assert_eq!(v["graded"], true);
        assert_eq!(v["pass"], true, "stock sim: {v:#}");
        let removed = v["removedVolumeMm3"].as_f64().unwrap();
        let expected = 40.0 * 30.0 + std::f64::consts::PI * 49.0 * 3.0;
        assert!(
            (removed - expected).abs() / expected < 0.08,
            "removed {removed} vs {expected}"
        );
        assert_eq!(out["toolChanges"], 1);
        let code = out["gcode"].as_str().unwrap();
        assert!(code.contains("TOOL CHANGE: load T2") && code.contains("M0\n"));
        assert!(code.ends_with("M5\nM2\n"));
        let programs = out["programs"].as_array().unwrap();
        assert_eq!(programs.len(), 2);
        for p in programs {
            assert!(!p["gcode"].as_str().unwrap().contains("M0\n"));
        }
        let mut lc = two_tool_job(false);
        lc["post"] = json!("linuxcnc");
        let out = run(lc).unwrap();
        let code = out["gcode"].as_str().unwrap();
        assert!(code.contains("T1 M6") && code.contains("T2 M6") && code.contains("G43 H2"));
        assert_eq!(out["verification"]["pass"], true);
    }

    /// The stock sim catches a pocket cut deeper than the part allows.
    #[test]
    fn gouge_is_caught() {
        let mut j = two_tool_job(true);
        j["operations"][1]["depth"] = json!(6);
        let out = run(j).unwrap();
        let v = &out["verification"];
        assert_eq!(v["pass"], false);
        assert_eq!(v["stock"]["gouge"]["pass"], false, "{v:#}");
    }

    #[test]
    fn refuses_unknown_tool_and_duplicate_numbers() {
        let mut j = two_tool_job(false);
        j["operations"][1]["tool"] = json!(7);
        assert!(run(j).unwrap_err().contains("T7"));
        let mut j = two_tool_job(false);
        j["tools"][1]["number"] = json!(1);
        assert!(run(j).is_err());
    }

    #[test]
    fn default_library_round_trips() {
        let lib = default_library();
        let tools: Vec<ToolSpec> = serde_json::from_value(lib["tools"].clone()).unwrap();
        assert!(tools.len() >= 3);
    }
}
