//! Batch (animation) rendering: photoreal, ray-traced, and drafting.
//!
//! A jointed assembly posed over time is, geometrically, the *same* parts in
//! different places. The expensive half of a photoreal render — evaluating
//! the parametric DAG into BReps and building a BVH over each one — depends
//! only on the parts, so it belongs outside the frame loop. This module puts
//! it there: evaluate once, build one BLAS per part, then per frame solve
//! forward kinematics, hand each object its new object→world transform, and
//! rebuild only the top-level acceleration structure before tracing.
//!
//! The alternative — which this replaces — is to bake one document per frame
//! and shell out to the renderer N times, paying the full evaluation cost
//! every frame. On a 35-part machine at 640px/16spp that was ~55 s a frame
//! against ~10 s of actual tracing.
//!
//! That argument is about *geometry*, not about shading, so the same
//! front half serves all three styles: the drafting line-art raster
//! ([`render_drafting_animation`], the default), direct BRep ray tracing
//! ([`render_raytrace_animation`]), and the path tracer
//! ([`render_photoreal_animation`]). [`plan_animation`] is that front half —
//! resolve the timeline, evaluate once, sample the sequence, bake one pose
//! vector per frame — and each back end differs only in how it turns a pose
//! into pixels.
//!
//! Every back end frames its camera on the union of all posed bounds, once,
//! and hands that same framing to every frame (see
//! [`crate::FixedFraming`]). Framing per frame would resize the canvas as
//! the subject's projected extent changed — frames that no muxer will accept
//! and, even at a fixed size, a machine that swims about in the canvas.
//!
//! # What a timeline can drive
//!
//! Joint tracks pose the assembly through forward kinematics. Visibility
//! tracks switch instances on and off per frame — a hidden part is simply
//! not drawn, nothing is re-evaluated. The Explode track pushes each
//! instance along its authored `explode` offset (the directions the still
//! `--explode` uses), scaled by the track value. The `camera` shots
//! (Turntable / Orbit / Focus / Static) move the camera; a moving camera
//! keeps one canvas and one scale for the whole shot (see
//! [`moving_framings`]). Parameter tracks would need the geometry
//! re-evaluated every frame, which this path exists to avoid, so they are
//! refused — as is anything else the renderer cannot honour — rather than
//! silently rendering frames that do not move.
//!
//! The pose source is the document's own [`Timeline`] when it has one, or a
//! keyframe file passed to `--animate`. Both are sampled by
//! [`Timeline::sample_sequence`], the same code the MCP `animate` tool and
//! the app's timeline scrubber use, so a rendered frame and a scrubbed frame
//! agree by construction.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vcad_ir::animation::{AnimTarget, Timeline};

use crate::photoreal::{
    build_objects, check_glow, check_raster_opts, dress_scene, frame_view, frame_view_windowed,
    has_media, object_corners, object_corners_with, trace_frame, PhotorealOptions,
};
use crate::raster::encode_png;
use crate::RasterOptions;
use vcad_kernel::Solid;

/// What to render, and where to put it.
#[derive(Debug, Clone)]
pub struct AnimateOptions {
    /// Directory the `frame_NNNN.png` files are written to. Created if absent.
    pub out_dir: PathBuf,
    /// Keyframes to drive the assembly with. `None` uses the document's own
    /// timeline, which is where the MCP `animate` tool leaves one.
    pub timeline: Option<Timeline>,
    /// Render only the first `n` frames of the sampled sequence. `None`
    /// renders the whole timeline.
    pub frames: Option<usize>,
    /// Override the timeline's own frame rate.
    pub fps: Option<f64>,
}

/// What a finished animation cost, frame by frame.
#[derive(Debug, Clone)]
pub struct AnimateReport {
    /// The PNG files written, in order.
    pub frames: Vec<PathBuf>,
    /// Effective frame rate (the timeline's, or the `--fps` override).
    pub fps: f64,
    /// Time spent evaluating the document — paid exactly once.
    pub eval: Duration,
    /// Time spent building per-part BVHs and framing the camera — also once.
    pub setup: Duration,
    /// Per-frame trace + encode + write time.
    pub per_frame: Vec<Duration>,
    /// Number of traceable parts in the scene.
    pub parts: usize,
    /// Number of those parts driven by joints (the rest are static).
    pub articulated: usize,
}

impl AnimateReport {
    /// Total wall time across evaluation, setup, and every frame.
    pub fn total(&self) -> Duration {
        self.eval + self.setup + self.per_frame.iter().sum::<Duration>()
    }

    /// Median per-frame cost — the steady-state number, unpolluted by a
    /// first frame that warms caches.
    pub fn median_frame(&self) -> Duration {
        if self.per_frame.is_empty() {
            return Duration::ZERO;
        }
        let mut v = self.per_frame.clone();
        v.sort_unstable();
        v[v.len() / 2]
    }
}

/// Parse an `--animate` keyframe file.
///
/// Accepts the document's own [`Timeline`] shape verbatim, and additionally a
/// shorthand for the common case of driving joints:
///
/// ```json
/// { "fps": 24, "durationS": 6,
///   "tracks": [ { "joint": "A", "keys": [ { "t": 0, "value": 0 },
///                                         { "t": 6, "value": 1440 } ] } ] }
/// ```
///
/// `"joint"` may name a joint by id (`joint_0`) or by its authored name
/// (`A`); resolution against the document happens later, in
/// [`render_photoreal_animation`], so a typo fails with the list of joints
/// that do exist rather than silently animating nothing.
pub fn parse_timeline_spec(json: &str) -> Result<Timeline, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("--animate: not valid JSON: {e}"))?;

    // Rewrite the shorthand into real `AnimTarget`s before deserializing, so
    // the IR type stays the single definition of a track.
    if let Some(tracks) = value.get_mut("tracks").and_then(|t| t.as_array_mut()) {
        for track in tracks {
            let Some(obj) = track.as_object_mut() else {
                continue;
            };
            if obj.contains_key("target") {
                continue;
            }
            let sugar = ["joint", "parameter", "visibility"]
                .into_iter()
                .find_map(|k| obj.remove(k).map(|v| (k, v)));
            let target = match sugar {
                Some(("joint", v)) => serde_json::json!({ "type": "Joint", "jointId": v }),
                Some(("parameter", v)) => serde_json::json!({ "type": "Parameter", "name": v }),
                Some(("visibility", v)) => {
                    serde_json::json!({ "type": "Visibility", "instanceId": v })
                }
                _ => {
                    return Err("--animate: each track needs a `target`, or the \
                                `joint`/`parameter`/`visibility` shorthand"
                        .to_string())
                }
            };
            obj.insert("target".to_string(), target);
        }
    }

    let timeline: Timeline =
        serde_json::from_value(value).map_err(|e| format!("--animate: not a timeline: {e}"))?;
    if !(timeline.duration_s.is_finite() && timeline.duration_s > 0.0) {
        return Err("--animate: durationS must be positive".to_string());
    }
    if timeline.tracks.is_empty() && timeline.camera.is_empty() {
        return Err(
            "--animate: timeline has no tracks and no camera shots — nothing would move"
                .to_string(),
        );
    }
    Ok(timeline)
}

/// Resolve a track's joint reference against the document: joint id first,
/// then authored joint name. Fails closed, listing what is available.
fn resolve_joint<'a>(
    doc: &'a vcad_ir::Document,
    reference: &str,
) -> Result<&'a vcad_ir::Joint, String> {
    let joints = doc.joints.as_deref().unwrap_or(&[]);
    if let Some(j) = joints.iter().find(|j| j.id == reference) {
        return Ok(j);
    }
    if let Some(j) = joints.iter().find(|j| j.name.as_deref() == Some(reference)) {
        return Ok(j);
    }
    let mut known: Vec<String> = joints
        .iter()
        .map(|j| match &j.name {
            Some(n) => format!("{} ({n})", j.id),
            None => j.id.clone(),
        })
        .collect();
    known.sort();
    Err(format!(
        "--animate: no joint '{reference}' in the document. Available: {}",
        if known.is_empty() {
            "none — this document has no joints".to_string()
        } else {
            known.join(", ")
        }
    ))
}

/// One frame's camera, compiled from the timeline's `camera` shots.
#[derive(Debug, Clone)]
struct FrameCamera {
    /// Orientation for this frame.
    view: crate::View,
    /// A `Focus` shot: which scene entries (statics first, then parts, the
    /// same order as everything else here) to frame, and the dolly factor.
    focus: Option<(Vec<bool>, f64)>,
}

/// A document evaluated once and posed for every frame — everything the
/// three back ends share, and the reason none of them re-evaluates geometry.
struct AnimationPlan {
    /// Static scene roots, already world-placed. Never move.
    statics: Vec<crate::SceneSolid>,
    /// Assembly instances in their *local* frame, in pose-vector order.
    parts: Vec<crate::SceneSolid>,
    /// `poses[frame][part]` — the object→world transform for `parts[part]`,
    /// joint tracks and the explode offset already applied.
    poses: Vec<Vec<KTransform>>,
    /// `visible[frame][part]` — from the timeline's visibility tracks.
    /// Statics have no instance id to key a track on, so they always draw.
    visible: Vec<Vec<bool>>,
    /// Per-frame cameras when the timeline has `camera` shots; `None` keeps
    /// the one `--view` camera for the whole sequence.
    cameras: Option<Vec<FrameCamera>>,
    /// Effective frame rate (the timeline's, or the `--fps` override).
    fps: f64,
    /// What the single evaluation cost.
    eval: Duration,
}

/// Map an authored track reference to an instance id: id first, then the
/// instance's authored name. Fails closed with what is available.
fn resolve_instance(doc: &vcad_ir::Document, reference: &str) -> Result<String, String> {
    let instances = doc.instances.as_deref().unwrap_or(&[]);
    if let Some(i) = instances.iter().find(|i| i.id == reference) {
        return Ok(i.id.clone());
    }
    if let Some(i) = instances
        .iter()
        .find(|i| i.name.as_deref() == Some(reference))
    {
        return Ok(i.id.clone());
    }
    let mut known: Vec<String> = instances
        .iter()
        .map(|i| match &i.name {
            Some(n) => format!("{} ({n})", i.id),
            None => i.id.clone(),
        })
        .collect();
    known.sort();
    Err(format!(
        "--animate: visibility track names no instance '{reference}'. Available: {}",
        if known.is_empty() {
            "none".to_string()
        } else {
            known.join(", ")
        }
    ))
}

/// The orbit view looking back along `dir` (scene → camera).
fn view_from_direction(dir: &vcad_ir::Vec3) -> Result<crate::View, String> {
    let len = (dir.x * dir.x + dir.y * dir.y + dir.z * dir.z).sqrt();
    if !(len.is_finite() && len > 1e-12) {
        return Err(
            "--animate: a Static camera shot's `direction` must be a non-zero vector".to_string(),
        );
    }
    Ok(crate::View::Orbit {
        azimuth: dir.y.atan2(dir.x).to_degrees(),
        elevation: (dir.z / len).clamp(-1.0, 1.0).asin().to_degrees(),
    })
}

/// Compile the timeline's camera shots into one camera per sampled frame.
///
/// Turntable and Orbit take the sampler's azimuth/elevation (the same
/// numbers the MCP preview and the app scrub to) as a [`crate::View::Orbit`].
/// Focus keeps the previous orientation and frames its target, dollied.
/// Static uses its `direction` when given and otherwise `base` (the `--view`
/// flag, isometric by default) — the sampler would report azimuth 0 /
/// elevation 30 for both, which is neither the authored direction nor the
/// isometric the IR documents. Time before the first shot uses `base` too;
/// gaps hold the previous camera, as the sampler does.
fn compile_cameras(
    timeline: &Timeline,
    sequence: &[vcad_ir::animation::SequenceFrame],
    base: crate::View,
    labels: &[&[String]],
) -> Result<Option<Vec<FrameCamera>>, String> {
    use vcad_ir::animation::CameraShotKind;
    if timeline.camera.is_empty() {
        return Ok(None);
    }
    // Validate every shot up front, so a bad shot fails before frame 0 —
    // including one the truncated (`--frames`) sequence never reaches.
    let mut focus_masks: Vec<Option<Vec<bool>>> = Vec::with_capacity(timeline.camera.len());
    for shot in &timeline.camera {
        if !(shot.start_s.is_finite() && shot.end_s.is_finite() && shot.end_s > shot.start_s) {
            return Err(format!(
                "--animate: camera shot [{}, {}) is empty or not finite",
                shot.start_s, shot.end_s
            ));
        }
        match &shot.kind {
            CameraShotKind::Focus { target, dolly } => {
                if !(dolly.is_finite() && *dolly > 0.0) {
                    return Err(format!(
                        "--animate: Focus dolly must be positive, got {dolly}"
                    ));
                }
                let want = target.trim().to_ascii_lowercase();
                let mask: Vec<bool> = labels
                    .iter()
                    .map(|ls| ls.iter().any(|l| l.to_ascii_lowercase() == want))
                    .collect();
                if !mask.iter().any(|&m| m) {
                    let mut available: Vec<&str> = labels
                        .iter()
                        .flat_map(|ls| ls.iter().map(String::as_str))
                        .collect();
                    available.sort_unstable();
                    available.dedup();
                    return Err(format!(
                        "--animate: Focus shot target '{target}' matches no part. Available: {}",
                        available.join(", ")
                    ));
                }
                focus_masks.push(Some(mask));
            }
            CameraShotKind::Static { direction: Some(d) } => {
                view_from_direction(d)?;
                focus_masks.push(None);
            }
            CameraShotKind::Turntable {
                degrees,
                elevation_deg,
            } => {
                if !(degrees.is_finite() && elevation_deg.is_finite()) {
                    return Err("--animate: Turntable angles must be finite".to_string());
                }
                focus_masks.push(None);
            }
            CameraShotKind::Orbit { from, to } => {
                if !from.iter().chain(to).all(|v| v.is_finite()) {
                    return Err("--animate: Orbit angles must be finite".to_string());
                }
                focus_masks.push(None);
            }
            CameraShotKind::Static { direction: None } => focus_masks.push(None),
        }
    }

    let mut prev = FrameCamera {
        view: base,
        focus: None,
    };
    let mut out = Vec::with_capacity(sequence.len());
    for frame in sequence {
        let t = frame.t;
        // Same selection rule as `Timeline::sample_sequence`: the last shot
        // whose [start, end) contains t wins.
        let active = timeline
            .camera
            .iter()
            .enumerate()
            .rfind(|(_, s)| t >= s.start_s && t < s.end_s);
        let cam = match active {
            None => prev.clone(),
            Some((k, shot)) => match &shot.kind {
                CameraShotKind::Turntable { .. } | CameraShotKind::Orbit { .. } => FrameCamera {
                    view: crate::View::Orbit {
                        azimuth: frame.camera.azimuth_deg,
                        elevation: frame.camera.elevation_deg,
                    },
                    focus: None,
                },
                CameraShotKind::Focus { .. } => FrameCamera {
                    view: prev.view,
                    focus: focus_masks[k].clone().map(|m| (m, frame.camera.dolly)),
                },
                CameraShotKind::Static { direction } => FrameCamera {
                    view: match direction {
                        Some(d) => view_from_direction(d)?,
                        None => base,
                    },
                    focus: None,
                },
            },
        };
        prev = cam.clone();
        out.push(cam);
    }
    Ok(Some(out))
}

/// Resolve the timeline, evaluate the document **once**, and bake one pose
/// vector per frame. The style-agnostic half of every animation render.
///
/// Honoured: joint tracks (FK), visibility tracks (per-instance on/off, no
/// re-evaluation), the Explode track (each instance's authored `explode`
/// offset — the same directions the still `--explode` reads — scaled by the
/// track value and added after FK), and the camera shots. Parameter tracks
/// would need the geometry re-evaluated per frame, which this path exists to
/// avoid, so they are refused rather than ignored.
fn plan_animation(
    raw_vcad: &str,
    an: &AnimateOptions,
    base_view: crate::View,
) -> Result<AnimationPlan, String> {
    // ── evaluate, exactly once ───────────────────────────────────────────
    let t_eval = Instant::now();
    let mut ev = crate::evaluate_vcad_document(raw_vcad)?;
    let eval = t_eval.elapsed();

    if ev.parts.is_empty() {
        return Err(
            "--animate: this document has no assembly instances; only a \
                    jointed assembly can be posed over time"
                .to_string(),
        );
    }

    let timeline = match an.timeline.clone().or_else(|| ev.doc.timeline.clone()) {
        Some(t) => t,
        None => {
            return Err(
                "--animate: no timeline. Pass --animate <keyframes.json>, or \
                        set one on the document"
                    .to_string(),
            )
        }
    };
    let timeline = Timeline {
        fps: an.fps.filter(|f| *f > 0.0).unwrap_or(timeline.fps),
        ..timeline
    };

    // ── resolve every track against the document, failing closed ─────────
    // Joint tracks: reference → joint id. Visibility: reference → instance id.
    let mut track_joint_ids: Vec<(String, String)> = Vec::new();
    let mut track_instance_ids: Vec<(String, String)> = Vec::new();
    let mut has_explode = false;
    for track in &timeline.tracks {
        match &track.target {
            AnimTarget::Joint { joint_id } => {
                let j = resolve_joint(&ev.doc, joint_id)?;
                track_joint_ids.push((joint_id.clone(), j.id.clone()));
            }
            AnimTarget::Visibility { instance_id } => {
                let id = resolve_instance(&ev.doc, instance_id)?;
                track_instance_ids.push((instance_id.clone(), id));
            }
            AnimTarget::Explode => has_explode = true,
            AnimTarget::Parameter { name } => {
                return Err(format!(
                    "--animate: parameter track '{name}' is not supported: a \
                     parameter changes the geometry, which this path evaluates \
                     exactly once. Render parameter sweeps one still per value."
                ))
            }
        }
    }
    if track_joint_ids.is_empty()
        && track_instance_ids.is_empty()
        && !has_explode
        && timeline.camera.is_empty()
    {
        return Err(
            "--animate: the timeline has no joint, visibility, explode or \
                    camera entries — nothing would change between frames"
                .to_string(),
        );
    }

    let part_solids: Vec<crate::SceneSolid> = std::mem::take(&mut ev.parts)
        .into_iter()
        .map(crate::part_as_local_scene_solid)
        .collect();
    let instance_ids: Vec<String> = part_solids.iter().map(|s| s.id.clone()).collect();

    // Explode offsets: authored per instance (`Instance::explode`), the same
    // data the still `--explode` and the kernel's `PosedAssembly::exploded`
    // read. Inventing directions here would make a render disagree with
    // every other exploded view of the document.
    let explode_offsets: Vec<[f64; 3]> = {
        let by_id: std::collections::HashMap<&str, [f64; 3]> = ev
            .doc
            .instances
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|i| i.explode.map(|e| (i.id.as_str(), [e.x, e.y, e.z])))
            .collect();
        instance_ids
            .iter()
            .map(|id| by_id.get(id.as_str()).copied().unwrap_or([0.0; 3]))
            .collect()
    };
    if has_explode {
        let moves = timeline
            .tracks
            .iter()
            .filter(|t| matches!(t.target, AnimTarget::Explode))
            .flat_map(|t| &t.keys)
            .any(|k| k.value != 0.0);
        let any_offset = explode_offsets.iter().flatten().any(|v| *v != 0.0);
        if moves && !any_offset {
            return Err("--animate: the timeline has an Explode track, but no \
                        instance declares an `explode` offset (mm, per instance) \
                        — every part would stay put. Add `explode` to the \
                        instances that should come apart."
                .to_string());
        }
    }

    // ── sample the timeline, and pose every frame up front ───────────────
    let mut sequence = timeline.sample_sequence();
    if let Some(n) = an.frames {
        if n == 0 {
            return Err("--frames must be at least 1".to_string());
        }
        sequence.truncate(n);
    }

    let labels: Vec<&[String]> = ev
        .statics
        .iter()
        .chain(part_solids.iter())
        .map(|s| s.labels.as_slice())
        .collect();
    let cameras = compile_cameras(&timeline, &sequence, base_view, &labels)?;

    let fallbacks: std::collections::HashMap<&str, Option<vcad_ir::Transform3D>> = ev
        .doc
        .instances
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|i| (i.id.as_str(), i.transform))
        .collect();

    let mut poses: Vec<Vec<KTransform>> = Vec::with_capacity(sequence.len());
    let mut visible: Vec<Vec<bool>> = Vec::with_capacity(sequence.len());
    let mut base_pose: Option<Vec<KTransform>> = None;
    for frame in &sequence {
        // Drive the document's joints, then let the shared FK solver do the
        // rest — chained fixed joints carry their groups along for free.
        if let Some(joints) = ev.doc.joints.as_mut() {
            for (reference, id) in &track_joint_ids {
                let Some(value) = frame.joints.get(reference) else {
                    continue;
                };
                if let Some(j) = joints.iter_mut().find(|j| &j.id == id) {
                    j.state = *value;
                }
            }
        }
        // Nothing drives FK after frame 0: its placement holds for the
        // whole sequence, so solve it once.
        if base_pose.is_none() || !track_joint_ids.is_empty() {
            let world = crate::assembly_world_transforms(&ev.doc)?;
            base_pose = Some(
                instance_ids
                    .iter()
                    .map(|id| {
                        match world
                            .get(id)
                            .copied()
                            .or_else(|| fallbacks.get(id.as_str()).copied().flatten())
                        {
                            Some(t) => crate::transform3d_to_kernel(&t),
                            None => KTransform::identity(),
                        }
                    })
                    .collect(),
            );
        }
        let base = base_pose.as_ref().expect("solved above");
        // The explode offset goes on last, in world space — after FK, like
        // the still path's `--explode`.
        let factor = frame.explode;
        poses.push(
            base.iter()
                .zip(&explode_offsets)
                .map(|(t, off)| {
                    if factor == 0.0 || off.iter().all(|v| *v == 0.0) {
                        t.clone()
                    } else {
                        KTransform::translation(off[0] * factor, off[1] * factor, off[2] * factor)
                            .then(t)
                    }
                })
                .collect(),
        );
        visible.push(
            instance_ids
                .iter()
                .map(|id| {
                    track_instance_ids
                        .iter()
                        .filter(|(_, iid)| iid == id)
                        .filter_map(|(reference, _)| frame.visibility.get(reference))
                        .all(|v| *v)
                })
                .collect(),
        );
    }

    Ok(AnimationPlan {
        statics: std::mem::take(&mut ev.statics),
        parts: part_solids,
        poses,
        visible,
        cameras,
        fps: timeline.fps,
        eval,
    })
}

/// Axis-aligned world bounds, accumulated point by point.
#[derive(Debug, Clone, Copy)]
struct Aabb {
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Aabb {
    fn empty() -> Self {
        Aabb {
            lo: [f64::INFINITY; 3],
            hi: [f64::NEG_INFINITY; 3],
        }
    }

    fn add(&mut self, p: [f64; 3]) {
        for ((lo, hi), v) in self.lo.iter_mut().zip(&mut self.hi).zip(p) {
            *lo = lo.min(v);
            *hi = hi.max(v);
        }
    }

    fn is_empty(&self) -> bool {
        !(0..3).all(|i| self.lo[i].is_finite() && self.hi[i].is_finite())
    }

    fn corners(&self) -> [[f64; 3]; 8] {
        std::array::from_fn(|k| {
            [
                if k & 1 == 0 { self.lo[0] } else { self.hi[0] },
                if k & 2 == 0 { self.lo[1] } else { self.hi[1] },
                if k & 4 == 0 { self.lo[2] } else { self.hi[2] },
            ]
        })
    }
}

/// One frame of a moving-camera shot: the options to render it with (its
/// view, and a canvas pinned for the whole sequence) and its screen window.
#[cfg(feature = "raster")]
struct MovingFrame {
    opts: RasterOptions,
    framing: crate::FixedFraming,
}

/// Screen window and depth range of `pts` seen from `view`.
#[cfg(feature = "raster")]
fn project_bounds(view: crate::View, pts: &[[f64; 3]]) -> ([f64; 2], [f64; 2], (f64, f64)) {
    let right = crate::normalize(view.right());
    let down = crate::normalize(view.down());
    let cam = view.cam();
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    let mut depth = (f64::INFINITY, f64::NEG_INFINITY);
    for p in pts {
        let s = [crate::dot(*p, right), crate::dot(*p, down)];
        for i in 0..2 {
            min[i] = min[i].min(s[i]);
            max[i] = max[i].max(s[i]);
        }
        let d = crate::dot(*p, cam);
        depth = (depth.0.min(d), depth.1.max(d));
    }
    (min, max, depth)
}

/// Frame a timeline whose camera moves.
///
/// One canvas and one scale for the whole shot, or the frames would differ
/// in size (no muxer accepts that) and the subject would breathe in and out
/// as the camera turned. The world bounds of everything visible in any
/// frame are projected into every frame's view; the largest screen extent
/// over the shot sets the canvas and the scale, and each frame centres that
/// window on the bounds as *it* sees them. A Focus frame instead fits its
/// target's posed bounds into the same canvas, shrunk by the dolly.
///
/// The canvas comes back pinned in each frame's options (`height_px` set),
/// so `--auto-aspect` is resolved once, here, rather than per view.
#[cfg(feature = "raster")]
fn moving_framings(
    opts: &RasterOptions,
    cams: &[FrameCamera],
    poses: &[Vec<KTransform>],
    visible: &[Vec<bool>],
    static_corners: &[Vec<[f64; 3]>],
    part_corners: &dyn Fn(usize, &KTransform) -> Vec<[f64; 3]>,
) -> Result<Vec<MovingFrame>, String> {
    let mut union = Aabb::empty();
    for c in static_corners.iter().flatten() {
        union.add(*c);
    }
    for (pose, vis) in poses.iter().zip(visible) {
        for (k, t) in pose.iter().enumerate() {
            if vis[k] {
                for c in part_corners(k, t) {
                    union.add(c);
                }
            }
        }
    }
    if union.is_empty() {
        return Err("--animate: nothing is visible in any frame".to_string());
    }
    let uc = union.corners();
    let per: Vec<_> = cams.iter().map(|c| project_bounds(c.view, &uc)).collect();
    let (mut ex, mut ey) = (0.0f64, 0.0f64);
    for (mn, mx, _) in &per {
        ex = ex.max(mx[0] - mn[0]);
        ey = ey.max(mx[1] - mn[1]);
    }
    if !(ex.max(ey).is_finite() && ex.max(ey) > 1e-9) {
        return Err("degenerate projection (no extent) across the sequence".to_string());
    }
    // A flat subject seen edge-on has zero extent on one axis; keep the
    // window non-degenerate so the canvas and scale stay finite.
    let floor = 1e-6 * ex.max(ey);
    let (ex, ey) = (ex.max(floor), ey.max(floor));
    let canvas = crate::raster::canvas_for(opts, ex, ey);
    let mut pinned = opts.clone();
    pinned.size_px = canvas.w as u32;
    pinned.height_px = Some(canvas.h as u32);
    let diag = (0..3)
        .map(|i| (union.hi[i] - union.lo[i]).powi(2))
        .sum::<f64>()
        .sqrt();
    let n_static = static_corners.len();

    cams.iter()
        .enumerate()
        .map(|(f, cam)| {
            let (mn, mx, depth) = per[f];
            let (centre, half) = match &cam.focus {
                None => (
                    [(mn[0] + mx[0]) / 2.0, (mn[1] + mx[1]) / 2.0],
                    [ex / 2.0, ey / 2.0],
                ),
                Some((mask, dolly)) => {
                    // The target's bounds in this frame's pose, hidden or not —
                    // a Focus on a part that is switched off still aims there.
                    let mut tb = Aabb::empty();
                    for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
                        if i < n_static {
                            static_corners[i].iter().for_each(|c| tb.add(*c));
                        } else {
                            let k = i - n_static;
                            part_corners(k, &poses[f][k])
                                .into_iter()
                                .for_each(|c| tb.add(c));
                        }
                    }
                    if tb.is_empty() {
                        return Err(format!(
                            "--animate: frame {f}: the Focus target has no geometry to frame"
                        ));
                    }
                    let (tmn, tmx, _) = project_bounds(cam.view, &tb.corners());
                    let fit = ((tmx[0] - tmn[0]) / ex)
                        .max((tmx[1] - tmn[1]) / ey)
                        .max(1e-6)
                        * dolly;
                    (
                        [(tmn[0] + tmx[0]) / 2.0, (tmn[1] + tmx[1]) / 2.0],
                        [ex * fit / 2.0, ey * fit / 2.0],
                    )
                }
            };
            let mut o = pinned.clone();
            o.view = cam.view;
            Ok(MovingFrame {
                opts: o,
                framing: crate::FixedFraming {
                    screen_min: [centre[0] - half[0], centre[1] - half[1]],
                    screen_max: [centre[0] + half[0], centre[1] + half[1]],
                    diag,
                    depth,
                },
            })
        })
        .collect()
}

/// Render a jointed assembly over a timeline, evaluating geometry once.
///
/// Writes `frame_0000.png` … into `an.out_dir` and reports what each stage
/// cost. `progress` is called after every frame with `(index, total,
/// elapsed)` so a CLI can stream timings.
pub fn render_photoreal_animation(
    raw_vcad: &str,
    opts: &RasterOptions,
    pr: &PhotorealOptions,
    an: &AnimateOptions,
    progress: &mut dyn FnMut(usize, usize, Duration),
) -> Result<AnimateReport, String> {
    check_raster_opts(opts)?;
    let evals_before = crate::document_eval_count();
    let plan = plan_animation(raw_vcad, an, opts.view)?;
    let AnimationPlan {
        statics,
        parts: part_solids,
        poses,
        visible,
        cameras,
        fps,
        eval,
    } = plan;

    // Glowing media are not animated yet: a medium is baked into a splat
    // cloud in world space, and nothing re-places it per frame. Refuse rather
    // than draw it as an opaque solid.
    if has_media(&statics) || has_media(&part_solids) {
        return Err("--animate does not support glowing media (volumeEmission) \
                    yet; render frames one at a time with --photoreal"
            .to_string());
    }
    check_glow(pr, true, false)?;

    let t_setup = Instant::now();

    // ── one BLAS per part, built once ────────────────────────────────────
    // Static scene roots first (their transforms never change), then the
    // assembly instances, whose object→world transform is rewritten per
    // frame.
    let mut objects = build_objects(&statics, pr.mesh_segments()).unwrap_or_default();
    let static_count = objects.len();
    objects.extend(build_objects(&part_solids, pr.mesh_segments())?);
    let articulated = objects.len() - static_count;
    // build_objects fails closed on untraceable parts, so counts can only
    // agree here; keep the invariant visible without implying a live branch.
    debug_assert_eq!(
        articulated,
        part_solids.len(),
        "build_objects returned fewer objects than instances without erroring"
    );

    // ── frame the camera on the union of every pose ──────────────────────
    // A per-frame fit would make the machine swim about in the canvas; the
    // union keeps the camera locked and nothing ever leaves the frame. Only
    // visible poses count: a part hidden all along must not shrink the shot.
    let static_corners: Vec<Vec<[f64; 3]>> =
        objects[..static_count].iter().map(object_corners).collect();
    let mut corners: Vec<[f64; 3]> = static_corners.iter().flatten().copied().collect();
    for (pose, vis) in poses.iter().zip(&visible) {
        for ((obj, t), on) in objects[static_count..].iter().zip(pose).zip(vis) {
            // Bounds under this pose, WITHOUT writing the pose onto the
            // object — the trace loop owns those transforms.
            if *on {
                corners.extend(object_corners_with(obj, t));
            }
        }
    }
    // One framing per frame: the same one throughout for a fixed camera,
    // or one per view when the timeline moves the camera.
    let framings: Vec<crate::photoreal::Framing> = match &cameras {
        None => vec![frame_view(&corners, opts, pr)?],
        Some(cams) => {
            let part_corners =
                |k: usize, t: &KTransform| object_corners_with(&objects[static_count + k], t);
            moving_framings(opts, cams, &poses, &visible, &static_corners, &part_corners)?
                .into_iter()
                .map(|m| {
                    frame_view_windowed(
                        &corners,
                        &m.opts,
                        pr,
                        Some((m.framing.screen_min, m.framing.screen_max)),
                    )
                })
                .collect::<Result<_, _>>()?
        }
    };
    let setup = t_setup.elapsed();

    std::fs::create_dir_all(&an.out_dir)
        .map_err(|e| format!("create {}: {e}", an.out_dir.display()))?;

    // ── trace ────────────────────────────────────────────────────────────
    let mut report = AnimateReport {
        frames: Vec::with_capacity(poses.len()),
        fps,
        eval,
        setup,
        per_frame: Vec::with_capacity(poses.len()),
        parts: objects.len(),
        articulated,
    };

    let total = poses.len();
    // `Scene` owns its objects, so hand it a fresh vector each frame built
    // from the same `Arc<Bvh>` handles — an Arc bump per part, not a rebuild.
    let blueprint: Vec<(std::sync::Arc<vcad_kernel_raytrace::Bvh>, _)> = objects
        .iter()
        .map(|o| (std::sync::Arc::clone(&o.bvh), o.material))
        .collect();
    // Back into the kernel's `Transform`, so the static placements and the
    // per-frame poses are one type the loop below can choose between.
    let static_transforms: Vec<KTransform> = objects[..static_count]
        .iter()
        .map(|o| KTransform {
            matrix: o.transform.matrix,
        })
        .collect();

    for (i, (pose, vis)) in poses.iter().zip(&visible).enumerate() {
        let t_frame = Instant::now();
        let framing = &framings[i.min(framings.len() - 1)];
        let frame_objects: Vec<vcad_kernel_raytrace::pathtrace::Object> = blueprint
            .iter()
            .enumerate()
            .filter(|(k, _)| *k < static_count || vis[*k - static_count])
            .map(|(k, (bvh, material))| {
                let transform = if k < static_count {
                    &static_transforms[k]
                } else {
                    &pose[k - static_count]
                };
                vcad_kernel_raytrace::pathtrace::Object::placed(
                    std::sync::Arc::clone(bvh),
                    *material,
                    vcad_kernel_raytrace::tlas::placement(transform),
                )
            })
            .collect();
        let scene = dress_scene(frame_objects, framing, pr)?;
        let png = encode_png(trace_frame(&scene, framing, pr, true), opts)?;
        let path = an.out_dir.join(format!("frame_{i:04}.png"));
        std::fs::write(&path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
        let elapsed = t_frame.elapsed();
        report.frames.push(path);
        report.per_frame.push(elapsed);
        progress(i + 1, total, elapsed);
    }

    // The whole point of this path, asserted rather than asserted-to.
    let evals = crate::document_eval_count() - evals_before;
    debug_assert_eq!(evals, 1, "geometry was evaluated {evals} times, not once");

    Ok(report)
}

/// Options the raster animation tiers cannot honour, refused up front with
/// the flag to drop.
///
/// `--trim` is the interesting one: it crops each frame to the drawn
/// content, which for a moving subject is a *different* crop every frame —
/// exactly the size drift a fixed camera exists to prevent. There is no
/// silently-correct interpretation, so it is refused rather than quietly
/// ignored.
fn check_animation_raster_opts(opts: &RasterOptions, tier: &str) -> Result<(), String> {
    if opts.size_px < 16 {
        return Err("size_px too small".to_string());
    }
    if !(opts.fill_frac > 0.0 && opts.fill_frac <= 1.0) {
        return Err("fill_frac must be in (0, 1]".to_string());
    }
    if opts.trim_margin_px.is_some() {
        return Err(format!(
            "--trim does not compose with --animate: it crops each frame to \
             its own content, so a moving subject would give every frame a \
             different size. Drop --trim (use --auto-aspect, which the {tier} \
             animation resolves once against the whole sequence)."
        ));
    }
    if opts.focus.is_some() {
        return Err(
            "--focus does not compose with --animate: the camera is framed \
             once on the whole posed sequence, so a per-part frame would be \
             overwritten. Drop --focus."
                .to_string(),
        );
    }
    Ok(())
}

/// Freeze a solid at `segments` into a mesh-backed solid.
///
/// The animation draws the same geometry in a new place every frame, so
/// tessellating inside the frame loop would pay the BRep→triangles cost N
/// times over for an identical result. A mesh-backed solid renders
/// identically to the BRep it came from *at the same segment count* (the
/// invariant the root-mesh cache already relies on), and its
/// `apply_transform` is a vertex walk rather than a surface rebuild.
#[cfg(feature = "raster")]
fn freeze(solid: &Solid, segments: u32) -> Solid {
    Solid::from_mesh(solid.to_mesh(segments))
}

/// Fold every vertex of `solid`, transformed by `t`, into `fb`.
#[cfg(feature = "raster")]
fn accumulate_verts(fb: &mut crate::FramingBuilder, solid: &Solid, t: &KTransform) {
    let mesh = solid.to_mesh(0); // mesh-backed: a clone, not a tessellation
    for c in mesh.vertices.as_chunks::<3>().0 {
        let p = t.apply_point(&vcad_kernel::vcad_kernel_math::Point3::new(
            c[0] as f64,
            c[1] as f64,
            c[2] as f64,
        ));
        fb.add([p.x, p.y, p.z]);
    }
}

/// Kernel rigid transform — the pose type the FK solver hands back.
type KTransform = vcad_kernel::vcad_kernel_math::Transform;

/// The eight corners of a solid's axis-aligned bounds, in its own frame.
///
/// A degenerate (empty) solid yields eight copies of the origin, which the
/// framing builder folds in harmlessly; `FramingBuilder::finish` is what
/// fails closed when *nothing* has extent.
#[cfg(feature = "raytrace")]
fn solid_bbox_corners(solid: &Solid) -> [[f64; 3]; 8] {
    let mesh = solid.to_mesh(crate::TESSELLATION_SEGMENTS);
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for c in mesh.vertices.as_chunks::<3>().0 {
        for i in 0..3 {
            lo[i] = lo[i].min(c[i] as f64);
            hi[i] = hi[i].max(c[i] as f64);
        }
    }
    if !lo.iter().all(|v| v.is_finite()) {
        return [[0.0; 3]; 8];
    }
    std::array::from_fn(|k| {
        [
            if k & 1 == 0 { lo[0] } else { hi[0] },
            if k & 2 == 0 { lo[1] } else { hi[1] },
            if k & 4 == 0 { lo[2] } else { hi[2] },
        ]
    })
}

/// Render a jointed assembly over a timeline in the **default drafting
/// line-art style** — the same projection, shading ramp, and hidden-line
/// overlay a single `-o frame.png` render produces, one PNG per frame.
///
/// Geometry is evaluated once and tessellated once; a frame costs only a
/// vertex transform per part plus the projection/rasterization, which is why
/// this tier runs in tens of milliseconds a frame where the path tracer
/// takes seconds. The canvas is pinned across the sequence (see
/// [`crate::FixedFraming`]), so every PNG has identical dimensions; with no
/// camera shots the camera itself is pinned too and the machine stays put
/// while its joints move.
#[cfg(feature = "raster")]
pub fn render_drafting_animation(
    raw_vcad: &str,
    opts: &RasterOptions,
    an: &AnimateOptions,
    progress: &mut dyn FnMut(usize, usize, Duration),
) -> Result<AnimateReport, String> {
    check_animation_raster_opts(opts, "drafting")?;
    if opts.section.is_some() {
        return Err(
            "--section does not compose with --animate: the cut is a boolean \
             against tessellated geometry, which the animation path freezes \
             once and only re-poses. Drop --section."
                .to_string(),
        );
    }

    let evals_before = crate::document_eval_count();
    let AnimationPlan {
        statics,
        parts,
        poses,
        visible,
        cameras,
        fps,
        eval,
    } = plan_animation(raw_vcad, an, opts.view)?;

    let t_setup = Instant::now();
    // Tessellate at exactly the segment count the raster path would have
    // chosen for this canvas, so a frame is what a single still would have
    // drawn.
    let segments = crate::tessellation_segments(Some(opts.size_px));
    let static_solids: Vec<Solid> = statics.iter().map(|s| freeze(&s.solid, segments)).collect();
    let part_solids: Vec<Solid> = parts.iter().map(|s| freeze(&s.solid, segments)).collect();

    // Columns the raster path wants, statics first then instances — the same
    // order `evaluate_vcad` produces for a still.
    let tints: Vec<Option<[f64; 3]>> = statics.iter().chain(parts.iter()).map(|s| s.tint).collect();
    let names: Vec<Option<String>> = statics
        .iter()
        .chain(parts.iter())
        .map(|s| s.name.clone())
        .collect();

    // ── frame the camera on the union of every visible pose ──────────────
    let frames: Vec<(RasterOptions, crate::FixedFraming)> = match &cameras {
        None => {
            let mut fb = crate::FramingBuilder::new(opts.view);
            let identity = KTransform::identity();
            for s in &static_solids {
                accumulate_verts(&mut fb, s, &identity);
            }
            for (pose, vis) in poses.iter().zip(&visible) {
                for ((s, t), on) in part_solids.iter().zip(pose).zip(vis) {
                    if *on {
                        accumulate_verts(&mut fb, s, t);
                    }
                }
            }
            vec![(opts.clone(), fb.finish()?)]
        }
        Some(cams) => {
            let static_corners: Vec<Vec<[f64; 3]>> = static_solids
                .iter()
                .map(|s| {
                    mesh_box(s)
                        .map(|b| b.corners().to_vec())
                        .unwrap_or_default()
                })
                .collect();
            let local: Vec<Option<Aabb>> = part_solids.iter().map(mesh_box).collect();
            let part_corners = |k: usize, t: &KTransform| posed_corners(local[k], t);
            moving_framings(opts, cams, &poses, &visible, &static_corners, &part_corners)?
                .into_iter()
                .map(|m| (m.opts, m.framing))
                .collect()
        }
    };
    let setup = t_setup.elapsed();

    std::fs::create_dir_all(&an.out_dir)
        .map_err(|e| format!("create {}: {e}", an.out_dir.display()))?;

    let mut report = AnimateReport {
        frames: Vec::with_capacity(poses.len()),
        fps,
        eval,
        setup,
        per_frame: Vec::with_capacity(poses.len()),
        parts: static_solids.len() + part_solids.len(),
        articulated: part_solids.len(),
    };
    let total = poses.len();
    let n_static = static_solids.len();
    for (i, (pose, vis)) in poses.iter().zip(&visible).enumerate() {
        let t_frame = Instant::now();
        let (frame_opts, framing) = &frames[i.min(frames.len() - 1)];
        // Hidden parts are skipped outright — their tint and name columns
        // with them, so the three stay index-aligned.
        let mut frame_solids: Vec<Solid> = static_solids.clone();
        let mut frame_tints: Vec<Option<[f64; 3]>> = tints[..n_static].to_vec();
        let mut frame_names: Vec<Option<String>> = names[..n_static].to_vec();
        for (k, (s, t)) in part_solids.iter().zip(pose).enumerate() {
            if vis[k] {
                frame_solids.push(s.apply_transform(t));
                frame_tints.push(tints[n_static + k]);
                frame_names.push(names[n_static + k].clone());
            }
        }
        let png = if frame_solids.is_empty() {
            crate::raster::blank_png(frame_opts, framing)?
        } else {
            crate::raster::render_png_solids_framed(
                &frame_solids,
                &frame_tints,
                &frame_names,
                frame_opts,
                framing,
            )?
        };
        let path = an.out_dir.join(format!("frame_{i:04}.png"));
        std::fs::write(&path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
        let elapsed = t_frame.elapsed();
        report.frames.push(path);
        report.per_frame.push(elapsed);
        progress(i + 1, total, elapsed);
    }

    let evals = crate::document_eval_count() - evals_before;
    debug_assert_eq!(evals, 1, "geometry was evaluated {evals} times, not once");
    Ok(report)
}

/// Bounds of a mesh-backed solid in its own frame, or `None` if empty.
#[cfg(feature = "raster")]
fn mesh_box(solid: &Solid) -> Option<Aabb> {
    let mesh = solid.to_mesh(0); // mesh-backed: a clone, not a tessellation
    let mut b = Aabb::empty();
    for c in mesh.vertices.as_chunks::<3>().0 {
        b.add([c[0] as f64, c[1] as f64, c[2] as f64]);
    }
    (!b.is_empty()).then_some(b)
}

/// The eight corners of `local` under pose `t` (none for an empty part).
fn posed_corners(local: Option<Aabb>, t: &KTransform) -> Vec<[f64; 3]> {
    let Some(b) = local else {
        return Vec::new();
    };
    b.corners()
        .iter()
        .map(|c| {
            let p = t.apply_point(&vcad_kernel::vcad_kernel_math::Point3::new(
                c[0], c[1], c[2],
            ));
            [p.x, p.y, p.z]
        })
        .collect()
}

/// Render a jointed assembly over a timeline with direct BRep ray tracing —
/// the `--raytrace` middle tier between drafting line art and the path
/// tracer.
///
/// Analytic surfaces mean the geometry cannot be frozen to triangles (that
/// is the whole point of the tier), so each frame re-places the BReps and
/// rebuilds their BVHs; evaluation still happens exactly once. Framing is
/// pinned the same way as the drafting tier — from the union of every posed
/// part's *bounds*, which is what the ray-traced still frames on too — so
/// frames come out uniformly sized.
#[cfg(feature = "raytrace")]
pub fn render_raytrace_animation(
    raw_vcad: &str,
    opts: &RasterOptions,
    an: &AnimateOptions,
    progress: &mut dyn FnMut(usize, usize, Duration),
) -> Result<AnimateReport, String> {
    check_animation_raster_opts(opts, "ray-traced")?;
    if opts.section.is_some() {
        return Err(
            "--section does not compose with --raytrace --animate; render the \
             drafting tier for a sectioned sequence"
                .to_string(),
        );
    }

    let evals_before = crate::document_eval_count();
    let AnimationPlan {
        statics,
        parts,
        poses,
        visible,
        cameras,
        fps,
        eval,
    } = plan_animation(raw_vcad, an, opts.view)?;

    let t_setup = Instant::now();
    // Framing from posed bounding boxes: the ray-traced still frames on BVH
    // root AABBs, so matching it here keeps the two paths reading alike.
    // Each part's *local* bbox corners are computed once (a tessellation
    // each, not one per frame) and then merely transformed per pose.
    let static_corners: Vec<Vec<[f64; 3]>> = statics
        .iter()
        .map(|s| solid_bbox_corners(&s.solid).to_vec())
        .collect();
    let local_corners: Vec<[[f64; 3]; 8]> =
        parts.iter().map(|s| solid_bbox_corners(&s.solid)).collect();
    let part_corners = |k: usize, t: &KTransform| -> Vec<[f64; 3]> {
        local_corners[k]
            .iter()
            .map(|c| {
                let p = t.apply_point(&vcad_kernel::vcad_kernel_math::Point3::new(
                    c[0], c[1], c[2],
                ));
                [p.x, p.y, p.z]
            })
            .collect()
    };
    let frames: Vec<(RasterOptions, crate::FixedFraming)> = match &cameras {
        None => {
            let mut fb = crate::FramingBuilder::new(opts.view);
            for c in static_corners.iter().flatten() {
                fb.add(*c);
            }
            for (pose, vis) in poses.iter().zip(&visible) {
                for (k, t) in pose.iter().enumerate() {
                    if vis[k] {
                        part_corners(k, t).into_iter().for_each(|c| fb.add(c));
                    }
                }
            }
            vec![(opts.clone(), fb.finish()?)]
        }
        Some(cams) => {
            moving_framings(opts, cams, &poses, &visible, &static_corners, &part_corners)?
                .into_iter()
                .map(|m| (m.opts, m.framing))
                .collect()
        }
    };
    let setup = t_setup.elapsed();

    std::fs::create_dir_all(&an.out_dir)
        .map_err(|e| format!("create {}: {e}", an.out_dir.display()))?;

    let mut report = AnimateReport {
        frames: Vec::with_capacity(poses.len()),
        fps,
        eval,
        setup,
        per_frame: Vec::with_capacity(poses.len()),
        parts: statics.len() + parts.len(),
        articulated: parts.len(),
    };
    let total = poses.len();
    for (i, (pose, vis)) in poses.iter().zip(&visible).enumerate() {
        let t_frame = Instant::now();
        let (frame_opts, framing) = &frames[i.min(frames.len() - 1)];
        let mut scene: Vec<crate::SceneSolid> = statics.clone();
        scene.extend(
            parts
                .iter()
                .zip(pose)
                .zip(vis)
                .filter(|(_, on)| **on)
                .map(|((s, t), _)| crate::SceneSolid {
                    solid: s.solid.apply_transform(t),
                    tint: s.tint,
                    material: s.material.clone(),
                    name: s.name.clone(),
                    labels: s.labels.clone(),
                    id: s.id.clone(),
                }),
        );
        let png = if scene.is_empty() {
            crate::raster::blank_png(frame_opts, framing)?
        } else {
            crate::render_raytrace_png_solids_framed(&scene, frame_opts, framing)?
        };
        let path = an.out_dir.join(format!("frame_{i:04}.png"));
        std::fs::write(&path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
        let elapsed = t_frame.elapsed();
        report.frames.push(path);
        report.per_frame.push(elapsed);
        progress(i + 1, total, elapsed);
    }

    let evals = crate::document_eval_count() - evals_before;
    debug_assert_eq!(evals, 1, "geometry was evaluated {evals} times, not once");
    Ok(report)
}

/// Assemble `report`'s frames into an H.264 mp4 with ffmpeg.
///
/// Returns the ffmpeg command line when ffmpeg is not on `PATH` (or fails),
/// so a caller can print it rather than losing the frames. Deliberately not
/// load-bearing: the PNGs are the deliverable, the mp4 is a convenience.
pub fn assemble_mp4(frame_dir: &Path, fps: f64, out: &Path) -> Result<(), String> {
    assemble_mp4_with(frame_dir, fps, out, None)
}

/// [`assemble_mp4`], burning in an ASS subtitle track (a HUD) when
/// `subtitles` is given. Needs an ffmpeg built with libass.
pub fn assemble_mp4_with(
    frame_dir: &Path,
    fps: f64,
    out: &Path,
    subtitles: Option<&Path>,
) -> Result<(), String> {
    let mut vf = "pad=ceil(iw/2)*2:ceil(ih/2)*2".to_string();
    if let Some(ass) = subtitles {
        // The filter-graph parser treats `:` `\\` `'` and `,` as syntax.
        let escaped = ass
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace(':', "\\:")
            .replace('\'', "\\'")
            .replace(',', "\\,");
        vf = format!("{vf},subtitles={escaped}");
    }
    let args: Vec<String> = vec![
        "-y".into(),
        "-framerate".into(),
        format!("{fps}"),
        "-i".into(),
        frame_dir.join("frame_%04d.png").display().to_string(),
        "-vf".into(),
        vf,
        "-c:v".into(),
        "libx264".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        out.display().to_string(),
    ];
    let rendered = format!("ffmpeg {}", args.join(" "));
    match std::process::Command::new("ffmpeg")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("ffmpeg exited {s}; run it yourself:\n  {rendered}")),
        Err(_) => Err(format!(
            "ffmpeg not found on PATH; the frames are written — run:\n  {rendered}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_ir::animation::{AnimKey, AnimTrack};

    /// A private, per-test output directory under the system temp dir.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vcad-render-anim-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Decoded pixel dimensions of a written frame.
    fn png_size(path: &PathBuf) -> (u32, u32) {
        let img = image::open(path).expect("decode frame");
        (
            image::GenericImageView::width(&img),
            image::GenericImageView::height(&img),
        )
    }

    /// Two cubes, one bolted to the other by `kind`, driven from a timeline.
    fn jointed_doc(kind: &str, axis: [f64; 3]) -> String {
        format!(
            r#"{{
  "version": "0.1",
  "nodes": {{
    "1": {{ "id": 1, "name": "base", "op": {{ "type": "Cube", "size": {{ "x": 20, "y": 20, "z": 4 }} }} }},
    "2": {{ "id": 2, "name": "arm", "op": {{ "type": "Cube", "size": {{ "x": 40, "y": 6, "z": 6 }} }} }}
  }},
  "materials": {{}},
  "part_materials": {{}},
  "roots": [],
  "partDefs": {{
    "base": {{ "id": "base", "name": "base", "root": 1 }},
    "arm": {{ "id": "arm", "name": "arm", "root": 2 }}
  }},
  "instances": [
    {{ "id": "base", "partDefId": "base" }},
    {{ "id": "arm", "partDefId": "arm" }}
  ],
  "joints": [
    {{ "id": "joint_0", "name": "swing", "parentInstanceId": "base",
       "childInstanceId": "arm",
       "parentAnchor": {{ "x": 0, "y": 0, "z": 0 }},
       "childAnchor": {{ "x": 0, "y": 0, "z": 0 }},
       "kind": {{ "type": "{kind}", "axis": {{ "x": {}, "y": {}, "z": {} }}, "limits": [-1000, 1000] }},
       "state": 0.0 }}
  ],
  "groundInstanceId": "base"
}}"#,
            axis[0], axis[1], axis[2]
        )
    }

    fn joint_timeline(id: &str, from: f64, to: f64) -> Timeline {
        Timeline {
            duration_s: 1.0,
            fps: 2.0,
            tracks: vec![AnimTrack {
                target: AnimTarget::Joint {
                    joint_id: id.to_string(),
                },
                keys: vec![
                    AnimKey {
                        t: 0.0,
                        value: from,
                        ease: Default::default(),
                    },
                    AnimKey {
                        t: 1.0,
                        value: to,
                        ease: Default::default(),
                    },
                ],
            }],
            camera: Vec::new(),
        }
    }

    /// The world pose of a revolute child must actually rotate with the
    /// joint state, about the anchor — the FK contract the animation path
    /// leans on for every frame after the first.
    #[test]
    fn revolute_state_swings_the_child() {
        let doc: vcad_ir::Document =
            serde_json::from_str(&jointed_doc("Revolute", [0.0, 0.0, 1.0])).expect("parse");
        let pose_at = |deg: f64| {
            let mut doc = doc.clone();
            doc.joints.as_mut().unwrap()[0].state = deg;
            crate::assembly_world_transforms(&doc).expect("fk")["arm"]
        };
        assert!(
            pose_at(0.0).rotation.z.abs() < 1e-9,
            "zero state must be identity"
        );
        let quarter = pose_at(90.0);
        assert!(
            (quarter.rotation.z - 90.0).abs() < 1e-6,
            "expected a 90° Z rotation, got {:?}",
            quarter.rotation
        );
        // Anchor at the origin, so the child stays pinned there.
        assert!(quarter.translation.x.abs() < 1e-9);
        assert!(quarter.translation.y.abs() < 1e-9);
    }

    /// And a slider must translate along its axis by exactly the state.
    #[test]
    fn prismatic_state_slides_the_child() {
        let doc: vcad_ir::Document =
            serde_json::from_str(&jointed_doc("Slider", [1.0, 0.0, 0.0])).expect("parse");
        let mut doc = doc.clone();
        doc.joints.as_mut().unwrap()[0].state = 7.5;
        let pose = crate::assembly_world_transforms(&doc).expect("fk")["arm"];
        assert!(
            (pose.translation.x - 7.5).abs() < 1e-9,
            "got {:?}",
            pose.translation
        );
        assert!(pose.translation.y.abs() < 1e-9);
        assert!(pose.rotation.z.abs() < 1e-9, "a slider must not rotate");
    }

    #[test]
    fn shorthand_and_native_tracks_parse_the_same() {
        let sugar = parse_timeline_spec(
            r#"{"fps":24,"durationS":2,"tracks":[{"joint":"A","keys":[{"t":0,"value":0},{"t":2,"value":90}]}]}"#,
        )
        .expect("shorthand");
        let native = parse_timeline_spec(
            r#"{"fps":24,"durationS":2,"tracks":[{"target":{"type":"Joint","jointId":"A"},
                "keys":[{"t":0,"value":0},{"t":2,"value":90}]}]}"#,
        )
        .expect("native");
        assert_eq!(sugar, native);
    }

    #[test]
    fn a_track_naming_no_joint_fails_with_the_list() {
        let doc: vcad_ir::Document =
            serde_json::from_str(&jointed_doc("Revolute", [0.0, 0.0, 1.0])).expect("parse");
        let err = resolve_joint(&doc, "nope").expect_err("should fail");
        assert!(err.contains("joint_0"), "unhelpful error: {err}");
        assert!(
            err.contains("swing"),
            "should offer the authored name: {err}"
        );
    }

    /// A joint track may name its joint by authored name as well as by id —
    /// `.loon` authors write the name, the IR mints the id.
    #[test]
    fn joints_resolve_by_name_or_id() {
        let doc: vcad_ir::Document =
            serde_json::from_str(&jointed_doc("Revolute", [0.0, 0.0, 1.0])).expect("parse");
        assert_eq!(resolve_joint(&doc, "swing").unwrap().id, "joint_0");
        assert_eq!(resolve_joint(&doc, "joint_0").unwrap().id, "joint_0");
    }

    /// The drafting tier renders the whole sequence, and every frame comes
    /// out the same size — the property a per-frame fit (or `--trim`) would
    /// break, and the one an mp4 muxer refuses to work without.
    #[test]
    fn drafting_animation_frames_share_one_size() {
        let dir = scratch_dir("size");
        let an = AnimateOptions {
            out_dir: dir.clone(),
            timeline: Some(joint_timeline("swing", 0.0, 90.0)),
            frames: None,
            fps: None,
        };
        // Small canvas, no supersampling: this is about framing and frame
        // count, not image quality, and it runs on every build.
        let opts = RasterOptions {
            size_px: 128,
            auto_aspect: true,
            aa: Some(1),
            ..Default::default()
        };
        let report = render_drafting_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &opts,
            &an,
            &mut |_, _, _| {},
        )
        .expect("drafting animation");

        assert_eq!(report.frames.len(), 3, "1 s at 2 fps is three samples");
        let sizes: Vec<(u32, u32)> = report.frames.iter().map(png_size).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "frames differ in size: {sizes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// …and the arm actually moves: identical frames would satisfy the size
    /// check above while rendering a still life.
    #[test]
    fn drafting_animation_frames_differ() {
        let dir = scratch_dir("motion");
        let an = AnimateOptions {
            out_dir: dir.clone(),
            timeline: Some(joint_timeline("swing", 0.0, 90.0)),
            frames: None,
            fps: None,
        };
        let opts = RasterOptions {
            size_px: 128,
            aa: Some(1),
            ..Default::default()
        };
        let report = render_drafting_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &opts,
            &an,
            &mut |_, _, _| {},
        )
        .expect("drafting animation");
        let bytes: Vec<Vec<u8>> = report
            .frames
            .iter()
            .map(|p| std::fs::read(p).expect("read frame"))
            .collect();
        assert_ne!(bytes[0], bytes[1], "frames 0 and 1 are identical");
        assert_ne!(bytes[1], bytes[2], "frames 1 and 2 are identical");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--trim` and animation are mutually exclusive by construction, and
    /// the refusal has to name the flag to drop.
    #[test]
    fn trim_is_refused_with_animation() {
        let an = AnimateOptions {
            out_dir: scratch_dir("never"),
            timeline: Some(joint_timeline("swing", 0.0, 90.0)),
            frames: None,
            fps: None,
        };
        let opts = RasterOptions {
            size_px: 128,
            trim_margin_px: Some(0),
            ..Default::default()
        };
        let err = render_drafting_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &opts,
            &an,
            &mut |_, _, _| {},
        )
        .expect_err("should refuse");
        assert!(err.contains("--trim"), "unhelpful error: {err}");
    }

    /// The timeline sampler is shared with the app; check the animation
    /// path's assumptions about it hold (inclusive of both ends, linear).
    #[test]
    fn sequence_covers_both_ends() {
        let tl = joint_timeline("joint_0", 0.0, 100.0);
        let seq = tl.sample_sequence();
        assert_eq!(seq.len(), 3, "1 s at 2 fps is t=0, 0.5, 1.0");
        assert_eq!(seq[0].joints["joint_0"], 0.0);
        assert_eq!(seq[1].joints["joint_0"], 50.0);
        assert_eq!(seq[2].joints["joint_0"], 100.0);
    }

    // ── non-joint tracks and the camera ──────────────────────────────────

    fn key(t: f64, value: f64) -> AnimKey {
        AnimKey {
            t,
            value,
            ease: Default::default(),
        }
    }

    fn track(target: AnimTarget, keys: Vec<AnimKey>) -> AnimTrack {
        AnimTrack { target, keys }
    }

    /// 1 s at 2 fps with the given tracks and shots: frames at t = 0, 0.5, 1.
    fn timeline(tracks: Vec<AnimTrack>, camera: Vec<vcad_ir::animation::CameraShot>) -> Timeline {
        Timeline {
            duration_s: 1.0,
            fps: 2.0,
            tracks,
            camera,
        }
    }

    fn shot(
        start_s: f64,
        end_s: f64,
        kind: vcad_ir::animation::CameraShotKind,
    ) -> vcad_ir::animation::CameraShot {
        vcad_ir::animation::CameraShot {
            start_s,
            end_s,
            kind,
        }
    }

    /// The jointed doc, with an authored explode offset on the arm.
    fn exploding_doc() -> String {
        jointed_doc("Revolute", [0.0, 0.0, 1.0]).replace(
            r#"{ "id": "arm", "partDefId": "arm" }"#,
            r#"{ "id": "arm", "partDefId": "arm", "explode": { "x": 0, "y": 0, "z": 50 } }"#,
        )
    }

    fn options(tl: Timeline, tag: &str) -> AnimateOptions {
        AnimateOptions {
            out_dir: scratch_dir(tag),
            timeline: Some(tl),
            frames: None,
            fps: None,
        }
    }

    fn small() -> RasterOptions {
        RasterOptions {
            size_px: 96,
            aa: Some(1),
            ..Default::default()
        }
    }

    /// Opaque (drawn) pixels in a written frame.
    fn coverage(path: &PathBuf) -> usize {
        image::open(path)
            .expect("decode frame")
            .to_rgba8()
            .pixels()
            .filter(|p| p.0[3] > 0)
            .count()
    }

    fn vis_track(id: &str, keys: Vec<AnimKey>) -> AnimTrack {
        track(
            AnimTarget::Visibility {
                instance_id: id.to_string(),
            },
            keys,
        )
    }

    /// A visibility-only timeline is enough to animate (no joint track
    /// needed), and the hidden part is actually gone from its frames.
    #[test]
    fn visibility_track_hides_the_part() {
        let tl = timeline(
            vec![vis_track("arm", vec![key(0.0, 1.0), key(1.0, 0.0)])],
            vec![],
        );
        let an = options(tl, "vis");
        let plan = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &an,
            crate::View::Isometric,
        )
        .expect("plan");
        let arm = plan.parts.iter().position(|p| p.id == "arm").unwrap();
        let base = plan.parts.iter().position(|p| p.id == "base").unwrap();
        // Linear 1 → 0: visible while > 0.5.
        assert_eq!(
            plan.visible.iter().map(|v| v[arm]).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert!(plan.visible.iter().all(|v| v[base]), "base is never hidden");

        let report = render_drafting_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &small(),
            &an,
            &mut |_, _, _| {},
        )
        .expect("render");
        let sizes: Vec<_> = report.frames.iter().map(png_size).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "sizes differ: {sizes:?}"
        );
        let (with_arm, without) = (coverage(&report.frames[0]), coverage(&report.frames[2]));
        assert!(
            without < with_arm,
            "hiding the arm must remove pixels ({with_arm} → {without})"
        );
        let _ = std::fs::remove_dir_all(&an.out_dir);
    }

    /// A frame with every part hidden is a blank frame of the shared size,
    /// not a "no solids produced" abort mid-sequence.
    #[test]
    fn all_hidden_frame_is_blank_and_same_size() {
        let off = vec![key(0.0, 1.0), key(1.0, 0.0)];
        let tl = timeline(
            vec![vis_track("arm", off.clone()), vis_track("base", off)],
            vec![],
        );
        let an = options(tl, "allhidden");
        let report = render_drafting_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &small(),
            &an,
            &mut |_, _, _| {},
        )
        .expect("render");
        assert_eq!(png_size(&report.frames[0]), png_size(&report.frames[2]));
        assert!(coverage(&report.frames[0]) > 0);
        assert_eq!(coverage(&report.frames[2]), 0, "nothing should be drawn");
        let _ = std::fs::remove_dir_all(&an.out_dir);
    }

    #[test]
    fn visibility_track_on_unknown_instance_fails_with_the_list() {
        let tl = timeline(vec![vis_track("nope", vec![key(0.0, 1.0)])], vec![]);
        let err = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &options(tl, "visnope"),
            crate::View::Isometric,
        )
        .err()
        .expect("should refuse");
        assert!(
            err.contains("nope") && err.contains("arm") && err.contains("base"),
            "{err}"
        );
    }

    /// The explode track moves each instance along its authored offset,
    /// scaled by the track value, on top of FK.
    #[test]
    fn explode_track_offsets_the_pose() {
        let tl = timeline(
            vec![track(
                AnimTarget::Explode,
                vec![key(0.0, 0.0), key(1.0, 1.0)],
            )],
            vec![],
        );
        let an = options(tl, "explode");
        let plan = plan_animation(&exploding_doc(), &an, crate::View::Isometric).expect("plan");
        let arm = plan.parts.iter().position(|p| p.id == "arm").unwrap();
        let base = plan.parts.iter().position(|p| p.id == "base").unwrap();
        let z = |f: usize, k: usize| {
            plan.poses[f][k]
                .apply_point(&vcad_kernel::vcad_kernel_math::Point3::new(0.0, 0.0, 0.0))
                .z
        };
        assert!(
            (z(1, arm) - z(0, arm) - 25.0).abs() < 1e-9,
            "half exploded: +25 mm"
        );
        assert!(
            (z(2, arm) - z(0, arm) - 50.0).abs() < 1e-9,
            "fully exploded: +50 mm"
        );
        assert!(
            (z(2, base) - z(0, base)).abs() < 1e-12,
            "no offset, no motion"
        );

        let report = render_drafting_animation(&exploding_doc(), &small(), &an, &mut |_, _, _| {})
            .expect("render");
        let bytes: Vec<Vec<u8>> = report
            .frames
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert_ne!(bytes[0], bytes[2], "exploded frames must differ");
        let _ = std::fs::remove_dir_all(&an.out_dir);
    }

    /// Explode composes with joints: the offset lands after FK, in world space.
    #[test]
    fn explode_composes_with_a_joint_track() {
        let tl = timeline(
            vec![
                track(AnimTarget::Explode, vec![key(0.0, 1.0)]),
                track(
                    AnimTarget::Joint {
                        joint_id: "swing".into(),
                    },
                    vec![key(0.0, 0.0), key(1.0, 90.0)],
                ),
            ],
            vec![],
        );
        let plan = plan_animation(
            &exploding_doc(),
            &options(tl, "explj"),
            crate::View::Isometric,
        )
        .expect("plan");
        let arm = plan.parts.iter().position(|p| p.id == "arm").unwrap();
        let p = plan.poses[2][arm]
            .apply_point(&vcad_kernel::vcad_kernel_math::Point3::new(10.0, 0.0, 0.0));
        // Rotated 90° about Z (x → y), then lifted 50 mm.
        assert!(
            p.x.abs() < 1e-6 && (p.y - 10.0).abs() < 1e-6 && (p.z - 50.0).abs() < 1e-6,
            "{p:?}"
        );
    }

    /// An explode track over a document that declares no offsets would
    /// render a still life; refuse it and name the missing field.
    #[test]
    fn explode_without_offsets_fails_closed() {
        let tl = timeline(
            vec![track(
                AnimTarget::Explode,
                vec![key(0.0, 0.0), key(1.0, 1.0)],
            )],
            vec![],
        );
        let err = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &options(tl, "explnone"),
            crate::View::Isometric,
        )
        .err()
        .expect("should refuse");
        assert!(err.contains("explode"), "{err}");
    }

    #[test]
    fn parameter_track_fails_closed() {
        let tl = timeline(
            vec![
                track(
                    AnimTarget::Parameter { name: "w".into() },
                    vec![key(0.0, 1.0)],
                ),
                track(
                    AnimTarget::Joint {
                        joint_id: "swing".into(),
                    },
                    vec![key(0.0, 0.0), key(1.0, 90.0)],
                ),
            ],
            vec![],
        );
        let err = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &options(tl, "param"),
            crate::View::Isometric,
        )
        .err()
        .expect("should refuse even next to a joint track");
        assert!(err.contains("parameter track 'w'"), "{err}");
    }

    #[test]
    fn empty_timeline_fails_closed() {
        let err = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &options(timeline(vec![], vec![]), "empty"),
            crate::View::Isometric,
        )
        .err()
        .expect("should refuse");
        assert!(err.contains("nothing would change"), "{err}");
        assert!(parse_timeline_spec(r#"{"durationS":1,"tracks":[]}"#).is_err());
        parse_timeline_spec(
            r#"{"durationS":1,"camera":[{"startS":0,"endS":1,"kind":{"type":"Turntable","degrees":360}}]}"#,
        )
        .expect("a camera-only keyframe file is a valid animation");
    }

    /// A turntable sweeps the azimuth, every frame keeps one size (even
    /// with `--auto-aspect`, whose aspect would otherwise follow each view),
    /// and the frames differ although nothing in the scene moves.
    #[test]
    fn turntable_camera_turns_and_keeps_one_size() {
        use vcad_ir::animation::CameraShotKind;
        let tl = timeline(
            vec![],
            vec![shot(
                0.0,
                2.0,
                CameraShotKind::Turntable {
                    degrees: 180.0,
                    elevation_deg: 20.0,
                },
            )],
        );
        let an = options(tl, "turn");
        let plan = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &an,
            crate::View::Isometric,
        )
        .expect("plan");
        let views: Vec<crate::View> = plan
            .cameras
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.view)
            .collect();
        assert_eq!(
            views,
            [0.0, 45.0, 90.0].map(|az| crate::View::Orbit {
                azimuth: az,
                elevation: 20.0
            })
        );

        let opts = RasterOptions {
            auto_aspect: true,
            ..small()
        };
        let report = render_drafting_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &opts,
            &an,
            &mut |_, _, _| {},
        )
        .expect("render");
        let sizes: Vec<_> = report.frames.iter().map(png_size).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "sizes differ: {sizes:?}"
        );
        let bytes: Vec<Vec<u8>> = report
            .frames
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert_ne!(bytes[0], bytes[1]);
        assert_ne!(bytes[1], bytes[2]);
        let _ = std::fs::remove_dir_all(&an.out_dir);
    }

    /// Orbit interpolates; Static honours its direction (which the sampler
    /// drops) and falls back to `--view`; time before any shot is `--view`.
    #[test]
    fn orbit_static_and_gaps_compile_to_views() {
        use vcad_ir::animation::CameraShotKind;
        let tl = Timeline {
            duration_s: 2.0,
            fps: 2.0,
            tracks: vec![],
            camera: vec![
                shot(
                    0.5,
                    1.5,
                    CameraShotKind::Orbit {
                        from: [10.0, 0.0],
                        to: [30.0, 40.0],
                    },
                ),
                shot(
                    1.5,
                    2.5,
                    CameraShotKind::Static {
                        direction: Some(vcad_ir::Vec3::new(0.0, 0.0, 2.0)),
                    },
                ),
            ],
        };
        let plan = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &options(tl, "orbit"),
            crate::View::Front,
        )
        .expect("plan");
        let views: Vec<crate::View> = plan.cameras.unwrap().iter().map(|c| c.view).collect();
        assert_eq!(
            views[0],
            crate::View::Front,
            "before the first shot: --view"
        );
        assert_eq!(
            views[1],
            crate::View::Orbit {
                azimuth: 10.0,
                elevation: 0.0
            }
        );
        assert_eq!(
            views[2],
            crate::View::Orbit {
                azimuth: 20.0,
                elevation: 20.0
            }
        );
        match views[3] {
            crate::View::Orbit { elevation, .. } => assert!((elevation - 90.0).abs() < 1e-9),
            v => panic!("Static direction +Z should look straight down, got {v:?}"),
        }
    }

    #[test]
    fn static_zero_direction_fails_closed() {
        use vcad_ir::animation::CameraShotKind;
        let tl = timeline(
            vec![],
            vec![shot(
                0.0,
                1.0,
                CameraShotKind::Static {
                    direction: Some(vcad_ir::Vec3::new(0.0, 0.0, 0.0)),
                },
            )],
        );
        let err = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &options(tl, "zero"),
            crate::View::Isometric,
        )
        .err()
        .expect("should refuse");
        assert!(err.contains("direction"), "{err}");
    }

    /// A Focus shot frames its target (here the small arm, dollied in) on
    /// the same canvas; an unknown target fails with the list of parts.
    #[test]
    fn focus_shot_zooms_onto_its_target() {
        use vcad_ir::animation::CameraShotKind;
        let focus = |target: &str| {
            timeline(
                vec![],
                vec![shot(
                    0.25,
                    0.75,
                    CameraShotKind::Focus {
                        target: target.to_string(),
                        dolly: 0.25,
                    },
                )],
            )
        };
        let err = plan_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &options(focus("nope"), "focusnope"),
            crate::View::Isometric,
        )
        .err()
        .expect("should refuse");
        assert!(err.contains("nope") && err.contains("arm"), "{err}");

        let an = options(focus("arm"), "focus");
        let report = render_drafting_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &small(),
            &an,
            &mut |_, _, _| {},
        )
        .expect("render");
        let sizes: Vec<_> = report.frames.iter().map(png_size).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "sizes differ: {sizes:?}"
        );
        // Frame 0 precedes the shot (whole scene); frame 1 is halfway into
        // it (dolly 0.625 on the arm). Same view, closer camera: the drawn
        // area grows.
        let (wide, close) = (coverage(&report.frames[0]), coverage(&report.frames[1]));
        assert!(close > wide, "focus should zoom in ({wide} → {close} px)");
        let _ = std::fs::remove_dir_all(&an.out_dir);
    }

    /// The ray-traced tier honours the same plan: visibility and camera.
    #[cfg(feature = "raytrace")]
    #[test]
    fn raytrace_tier_honours_visibility_and_camera() {
        use vcad_ir::animation::CameraShotKind;
        let tl = timeline(
            vec![vis_track("arm", vec![key(0.0, 1.0), key(1.0, 0.0)])],
            vec![shot(
                0.0,
                2.0,
                CameraShotKind::Turntable {
                    degrees: 90.0,
                    elevation_deg: 30.0,
                },
            )],
        );
        let an = options(tl, "rt");
        let opts = RasterOptions {
            size_px: 48,
            ..Default::default()
        };
        let report = render_raytrace_animation(
            &jointed_doc("Revolute", [0.0, 0.0, 1.0]),
            &opts,
            &an,
            &mut |_, _, _| {},
        )
        .expect("render");
        let sizes: Vec<_> = report.frames.iter().map(png_size).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "sizes differ: {sizes:?}"
        );
        let bytes: Vec<Vec<u8>> = report
            .frames
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert_ne!(bytes[0], bytes[2]);
        let _ = std::fs::remove_dir_all(&an.out_dir);
    }
}
