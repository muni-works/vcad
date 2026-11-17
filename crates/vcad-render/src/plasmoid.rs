//! `--sim plasmoid`: render a racing-plasmoid document over its timeline,
//! stepping the reduced plasmoid model every frame.
//!
//! The document (`docs/plasmoid-sim/plasmoid-clip.loon`) carries everything:
//!
//! * the device, with roots named `dish`, `spike` and `magnet`;
//! * the sim's inputs as parameters — `dish_r`, `glow_height`, `orbit_r`,
//!   `magnet_radius`, `magnet_thickness`, `pressure_torr`, `mobility_margin`
//!   and the magnet's default (hold) pose `magnet_r`, `magnet_angle`,
//!   `magnet_gap` (mm and degrees);
//! * the magnet's choreography as the document [`Timeline`]: parameter tracks
//!   on `magnet_r`, `magnet_angle` and `magnet_gap`, sampled with
//!   [`Timeline::sample_track`] — the code every other timeline consumer uses
//!   — and interpolated in (r, angle, gap), so a flick sweeps an arc;
//! * the plasma as glowing media: roots `foot` and `filament` (drawn at the
//!   hold pose; re-placed here every frame) and any other medium (the faint
//!   glow sheet), which stays where the document put it.
//!
//! [`PlasmoidRig`] binds that document to
//! [`vcad_kernel_particle::plasmoid`]: it builds the model parameters, the
//! magnet and the pose-of-time closure, and runs the unchanged
//! [`simulate`] over the whole clip **once**, before any frame is traced —
//! sampled continuously in time (fixed RK4 substeps, the pose evaluated at
//! every substep), never held per frame, so a 0.12 s flick reaches the
//! integrator as the smooth sweep it is. Frames are then independent: a
//! resumed render, or one cut short with `--frames`, sees the same foot.
//!
//! What is simulated and what is drawn: the foot's position and the magnet's
//! pose come from the sim and the timeline. The filament's *shape* — an arch
//! from the spike tip to the foot, thickening toward the foot — is the same
//! stylisation the document's own `filament` root draws (the reduced model
//! has no filament geometry); its *rendering* is physically based emission
//! along every ray. Each frame integrates [`PlasmoidClipOptions::exposure`]
//! foot positions across its 1/fps exposure at `1/exposure` of the intensity
//! each — motion blur from the sim, not a 2-D smear.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vcad_ir::animation::{AnimTarget, AnimTrack, Timeline};
use vcad_ir::Document;
use vcad_kernel_particle::plasmoid::{
    simulate, DiscMagnet, MagnetPose, PlasmaParams, PlasmoidSample,
};
use vcad_kernel_raytrace::medium::{EmissiveMedium, MediumCloud, DEFAULT_BUDGET};
use vcad_kernel_raytrace::pathtrace::Object;

use crate::animate::{AnimateOptions, AnimateReport};
use crate::photoreal::{
    apply_look, build_objects, check_glow, dress_scene, frame_scene, trace_frame, PhotorealOptions,
};
use crate::raster::encode_png;
use crate::RasterOptions;

/// The parameter tracks that pose the magnet, in (r mm, azimuth °, gap mm).
pub const MAGNET_TRACKS: [&str; 3] = ["magnet_r", "magnet_angle", "magnet_gap"];

/// Every parameter the rig reads from the document.
const REQUIRED_PARAMS: [&str; 10] = [
    "dish_r",
    "glow_height",
    "orbit_r",
    "magnet_radius",
    "magnet_thickness",
    "magnet_r",
    "magnet_angle",
    "magnet_gap",
    "pressure_torr",
    "mobility_margin",
];

/// A foot within this distance (m) of the magnet's axis, with the magnet
/// below [`LOCK_MAGNET_Z_M`], counts as locked — the criterion the demo's
/// beats (and the HUD) are defined by.
pub const LOCK_RADIUS_M: f64 = 8e-3;
/// Magnet-centre height (m) below which a near foot counts as locked.
pub const LOCK_MAGNET_Z_M: f64 = 0.05;

/// The racing-plasmoid model bound to a document and its timeline.
#[derive(Debug, Clone)]
pub struct PlasmoidRig {
    /// Model parameters: `argon_dish(pressure_torr)` with the document's dish
    /// radius, orbit and glow height, and the mobility calibrated to
    /// `mobility_margin ×` the depinning threshold at the hold pose.
    pub params: PlasmaParams,
    /// The disc magnet (`magnet_radius × magnet_thickness`, N52).
    pub magnet: DiscMagnet,
    /// The magnet at the parameters' default values — the pose the document
    /// draws it in.
    pub hold: MagnetPose,
    /// Foot start position, m: on the orbit at angle 0.
    pub start: [f64; 2],
    /// Clip length, s (the timeline's duration).
    pub duration_s: f64,
    /// Frame rate (the timeline's, or an override).
    pub fps: f64,
    /// Argon pressure, Torr.
    pub pressure_torr: f64,
    /// `magnet_r`, `magnet_angle`, `magnet_gap`, each `None` when the
    /// timeline leaves that channel at its default.
    tracks: [Option<AnimTrack>; 3],
    /// Default (r mm, angle °, gap mm).
    hold_polar: [f64; 3],
}

impl PlasmoidRig {
    /// Bind `doc` (its parameters) to `timeline` (its magnet tracks). Fails
    /// closed on a missing parameter or a timeline that does not move the
    /// magnet at all.
    pub fn from_document(doc: &Document, timeline: &Timeline) -> Result<Self, String> {
        let env = vcad_ir::resolve_parameters(&doc.parameters)
            .map_err(|e| format!("--sim plasmoid: parameters: {e:?}"))?;
        let missing: Vec<&str> = REQUIRED_PARAMS
            .iter()
            .copied()
            .filter(|n| !env.contains_key(*n))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "--sim plasmoid: the document does not declare {} (defparam); see \
                 docs/plasmoid-sim/plasmoid-clip.loon",
                missing.join(", ")
            ));
        }
        let v = |n: &str| env[n];
        let tracks = MAGNET_TRACKS.map(|name| {
            timeline
                .tracks
                .iter()
                .find(|t| matches!(&t.target, AnimTarget::Parameter { name: n } if n == name))
                .cloned()
        });
        if tracks.iter().all(Option::is_none) {
            return Err(format!(
                "--sim plasmoid: the timeline has no parameter track on {} — nothing \
                 moves the magnet",
                MAGNET_TRACKS.join(" / ")
            ));
        }

        let magnet = DiscMagnet {
            radius_m: v("magnet_radius") * 1e-3,
            thickness_m: v("magnet_thickness") * 1e-3,
            ..DiscMagnet::n52_10x5()
        };
        let hold_polar = [v("magnet_r"), v("magnet_angle"), v("magnet_gap")];
        let hold = polar_pose(&magnet, hold_polar);
        let mut params = PlasmaParams::argon_dish(v("pressure_torr"));
        params.dish_radius_m = v("dish_r") * 1e-3;
        params.orbit_radius_m = v("orbit_r") * 1e-3;
        params.glow_height_m = v("glow_height") * 1e-3;
        params.mobility_m2_s = v("mobility_margin") * params.depinning_mobility(&magnet, &hold);
        let fps = if timeline.fps > 0.0 {
            timeline.fps
        } else {
            24.0
        };
        Ok(Self {
            start: [params.orbit_radius_m, 0.0],
            params,
            magnet,
            hold,
            duration_s: timeline.duration_s,
            fps,
            pressure_torr: v("pressure_torr"),
            tracks,
            hold_polar,
        })
    }

    /// (r mm, angle °, gap mm) at time `t`, from the timeline.
    pub fn polar_at(&self, t: f64) -> [f64; 3] {
        let mut out = self.hold_polar;
        for (o, track) in out.iter_mut().zip(&self.tracks) {
            if let Some(v) = track.as_ref().and_then(|tr| Timeline::sample_track(tr, t)) {
                *o = v;
            }
        }
        out
    }

    /// The magnet's pose at time `t`.
    pub fn pose_at(&self, t: f64) -> MagnetPose {
        polar_pose(&self.magnet, self.polar_at(t))
    }

    /// Run the model over the whole clip with RK4 step `dt`, sampling every
    /// `stride` steps — [`simulate`] with the timeline as the pose source.
    pub fn trajectory(&self, dt: f64, stride: usize) -> Vec<PlasmoidSample> {
        simulate(
            &self.params,
            &self.magnet,
            |t| Some(self.pose_at(t)),
            self.start,
            self.duration_s,
            dt,
            stride,
        )
    }
}

/// Magnet pose from (r mm, azimuth °, air gap under the lower face mm),
/// north pole down.
fn polar_pose(m: &DiscMagnet, [r_mm, ang_deg, gap_mm]: [f64; 3]) -> MagnetPose {
    let (r, ang, gap) = (r_mm * 1e-3, ang_deg.to_radians(), gap_mm * 1e-3);
    MagnetPose {
        center_m: [r * ang.cos(), r * ang.sin(), gap + 0.5 * m.thickness_m],
        axis: [0.0, 0.0, -1.0],
    }
}

/// Whether the sample's foot is locked under the magnet.
pub fn is_locked(s: &PlasmoidSample) -> bool {
    let Some(m) = s.magnet else {
        return false;
    };
    let c = m.center_m;
    let d = ((s.pos_m[0] - c[0]).powi(2) + (s.pos_m[1] - c[1]).powi(2)).sqrt();
    d < LOCK_RADIUS_M && c[2] < LOCK_MAGNET_Z_M
}

/// How the sim is sampled for a clip.
#[derive(Debug, Clone, Copy)]
pub struct PlasmoidClipOptions {
    /// RK4 steps per frame (`dt = 1 / (fps · substeps)`).
    pub substeps: usize,
    /// Foot positions integrated across each frame's exposure. Must divide
    /// `substeps`.
    pub exposure: usize,
    /// Lattice spacing (mm) of the per-frame foot and filament media.
    pub medium_spacing_mm: f64,
    /// Keep frames already on disk (resume an interrupted render).
    pub resume: bool,
}

impl Default for PlasmoidClipOptions {
    /// 80 substeps at 30 fps is the demo's `dt = 1/2400 s`. 16 exposure
    /// samples (480 Hz) keep the blur of a racing foot (~20 mm a frame)
    /// continuous; 4 strobe into separate copies.
    fn default() -> Self {
        Self {
            substeps: 80,
            exposure: 16,
            medium_spacing_mm: 0.25,
            resume: false,
        }
    }
}

/// What a plasmoid clip render produced.
#[derive(Debug, Clone)]
pub struct PlasmoidClipReport {
    /// The rig the document bound to.
    pub rig: PlasmoidRig,
    /// Frames and timings.
    pub animation: AnimateReport,
    /// The trajectory, `exposure` samples per frame from t = 0 to the end.
    pub trajectory: Vec<PlasmoidSample>,
    /// Samples per frame in [`Self::trajectory`].
    pub exposure: usize,
}

/// The filament's centreline at parameter `s ∈ [0, 1]`: the same arch as
/// `plasmoid-clip.loon`'s `filament` root, from the spike tip `tip` to the
/// foot `foot` (both mm), lifted `9·sin(πs)(1−s) + 3·sin(πs)` mm.
fn arch(tip: [f64; 3], foot: [f64; 3], s: f64) -> [f64; 3] {
    let lift =
        9.0 * (std::f64::consts::PI * s).sin() * (1.0 - s) + 3.0 * (std::f64::consts::PI * s).sin();
    [
        tip[0] + (foot[0] - tip[0]) * s,
        tip[1] + (foot[1] - tip[1]) * s,
        tip[2] * (1.0 - s) + foot[2] * s + lift,
    ]
}

/// Filament radius (mm) at parameter `s` — `plasmoid-clip.loon`'s `fil-r`.
fn arch_radius(s: f64) -> f64 {
    0.7 + 1.1 * s
}

/// Add the filament (spike tip → foot) and the foot ball as media to
/// `cloud`, each at `scale` of its material's emission.
#[allow(clippy::too_many_arguments)]
fn add_plasma(
    cloud: &mut MediumCloud,
    tip: [f64; 3],
    foot: [f64; 3],
    foot_radius: f64,
    m_fil: &EmissiveMedium,
    m_foot: &EmissiveMedium,
    scale: f32,
    h: f64,
) {
    let scaled = |m: &EmissiveMedium| EmissiveMedium {
        emission: m.emission.map(|e| e * scale),
        ..*m
    };
    // Foot: a ball, depth = R − |p − c|.
    let r = foot_radius;
    cloud.add_field(
        [foot[0] - r, foot[1] - r, foot[2] - r],
        [foot[0] + r, foot[1] + r, foot[2] + r],
        |p| r - dist(p, foot),
        &scaled(m_foot),
        h,
    );
    // Filament: a tube around a sampled centreline, depth = r(s*) − d where
    // s* is the closest centreline point.
    const N: usize = 48;
    let pts: Vec<[f64; 3]> = (0..=N)
        .map(|i| arch(tip, foot, i as f64 / N as f64))
        .collect();
    let rmax = arch_radius(1.0);
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &pts {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a] - rmax);
            hi[a] = hi[a].max(p[a] + rmax);
        }
    }
    cloud.add_field(
        lo,
        hi,
        |p| {
            let mut best = (f64::INFINITY, 0.0);
            for (i, w) in pts.windows(2).enumerate() {
                let (d, u) = seg_dist(p, w[0], w[1]);
                if d < best.0 {
                    best = (d, (i as f64 + u) / N as f64);
                }
            }
            arch_radius(best.1) - best.0
        },
        &scaled(m_fil),
        h,
    );
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Distance from `p` to segment `ab`, and the segment parameter of the
/// closest point.
fn seg_dist(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> (f64, f64) {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let l2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let u = if l2 > 0.0 {
        ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = [a[0] + ab[0] * u, a[1] + ab[1] * u, a[2] + ab[2] * u];
    (dist(p, q), u)
}

/// The medium a root's material declares, or an error naming the root.
fn medium_of(s: &crate::SceneSolid, root: &str) -> Result<EmissiveMedium, String> {
    s.material
        .as_ref()
        .and_then(EmissiveMedium::from_material_def)
        .ok_or_else(|| {
            format!(
                "--sim plasmoid: root `{root}` must use a material-glow (volumeEmission) material"
            )
        })
}

fn bbox(s: &crate::SceneSolid) -> ([f64; 3], [f64; 3]) {
    let b = s.solid.bounding_box();
    (b.0, b.1)
}

/// Render a plasmoid clip: the document's timeline poses the magnet, the
/// model runs once over the clip, and every frame places the magnet, the
/// foot and the filament and path-traces the scene.
///
/// Writes `frame_0000.png` … into `an.out_dir` (frames tile `[0, duration)`
/// at the timeline's fps).
pub fn render_plasmoid_animation(
    raw_vcad: &str,
    opts: &RasterOptions,
    pr: &PhotorealOptions,
    an: &AnimateOptions,
    sim: &PlasmoidClipOptions,
    progress: &mut dyn FnMut(usize, usize, Duration),
) -> Result<PlasmoidClipReport, String> {
    crate::photoreal::check_raster_opts(opts)?;
    if sim.exposure == 0 || sim.substeps == 0 || !sim.substeps.is_multiple_of(sim.exposure) {
        return Err(format!(
            "--sim plasmoid: exposure samples ({}) must divide the substeps per frame ({})",
            sim.exposure, sim.substeps
        ));
    }
    let t_eval = Instant::now();
    let ev = crate::evaluate_vcad_document(raw_vcad)?;
    let eval = t_eval.elapsed();
    if !ev.parts.is_empty() {
        return Err("--sim plasmoid: expects scene roots, not assembly instances".to_string());
    }
    let timeline = an
        .timeline
        .clone()
        .or_else(|| ev.doc.timeline.clone())
        .ok_or("--sim plasmoid: the document has no timeline (the magnet's choreography)")?;
    let timeline = Timeline {
        fps: an.fps.filter(|f| *f > 0.0).unwrap_or(timeline.fps),
        ..timeline
    };
    let rig = PlasmoidRig::from_document(&ev.doc, &timeline)?;

    let t_setup = Instant::now();
    let solids = ev.statics;
    let find = |name: &str| -> Result<usize, String> {
        let hits: Vec<usize> = solids
            .iter()
            .enumerate()
            .filter(|(_, s)| s.name.as_deref() == Some(name))
            .map(|(i, _)| i)
            .collect();
        match hits.as_slice() {
            [i] => Ok(*i),
            [] => Err(format!("--sim plasmoid: no root named `{name}`")),
            _ => Err(format!("--sim plasmoid: more than one root named `{name}`")),
        }
    };
    let (i_magnet, i_spike, i_foot, i_fil) = (
        find("magnet")?,
        find("spike")?,
        find("foot")?,
        find("filament")?,
    );
    let m_foot = medium_of(&solids[i_foot], "foot")?;
    let m_fil = medium_of(&solids[i_fil], "filament")?;
    let foot_radius = {
        let (lo, hi) = bbox(&solids[i_foot]);
        0.5 * (hi[0] - lo[0]).min(hi[1] - lo[1])
    };
    let tip = {
        let (lo, hi) = bbox(&solids[i_spike]);
        [0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1]), hi[2]]
    };
    let glow_z = rig.params.glow_height_m * 1e3;

    check_glow(pr, false, true)?;
    let mut objects = build_objects(&solids, pr.mesh_segments())?;
    apply_look(&mut objects, &solids, pr);
    // Framed on the document as authored — magnet, foot and filament at the
    // hold pose — and held for the whole clip.
    let framing = frame_scene(&solids, &objects, opts, pr)?;

    // Static media (the glow sheet) go into one cloud, built once and cloned
    // per frame; static surfaces stay as objects; the magnet is re-placed.
    let mut statics: Vec<Object> = Vec::new();
    let mut magnet: Option<Object> = None;
    let mut base_cloud = MediumCloud::new();
    for (i, (o, s)) in objects.into_iter().zip(&solids).enumerate() {
        if i == i_foot || i == i_fil {
            continue;
        }
        if i == i_magnet {
            magnet = Some(o);
            continue;
        }
        match s
            .material
            .as_ref()
            .and_then(EmissiveMedium::from_material_def)
        {
            Some(m) => {
                base_cloud.add_auto(o.bvh.as_ref(), &m, DEFAULT_BUDGET);
            }
            None => statics.push(o),
        }
    }
    let magnet = magnet.expect("found above");

    // ── the sim, once, for the whole clip ────────────────────────────────
    let dt = 1.0 / (rig.fps * sim.substeps as f64);
    let trajectory = rig.trajectory(dt, sim.substeps / sim.exposure);
    let mut total = (rig.duration_s * rig.fps).round() as usize;
    if let Some(n) = an.frames {
        if n == 0 {
            return Err("--frames must be at least 1".to_string());
        }
        total = total.min(n);
    }
    let setup = t_setup.elapsed();

    std::fs::create_dir_all(&an.out_dir)
        .map_err(|e| format!("create {}: {e}", an.out_dir.display()))?;
    let mut report = AnimateReport {
        frames: Vec::with_capacity(total),
        fps: rig.fps,
        eval,
        setup,
        per_frame: Vec::with_capacity(total),
        parts: solids.len(),
        articulated: 1,
    };

    let hold_mm = rig.hold.center_m.map(|c| c * 1e3);
    let scale = 1.0 / sim.exposure as f32;
    for i in 0..total {
        let path = an.out_dir.join(format!("frame_{i:04}.png"));
        if sim.resume && path.exists() {
            report.frames.push(path);
            continue;
        }
        let t_frame = Instant::now();
        let t = i as f64 / rig.fps;
        let c = rig.pose_at(t).center_m.map(|c| c * 1e3);
        let mut frame_objects: Vec<Object> = statics
            .iter()
            .map(|o| Object {
                bvh: Arc::clone(&o.bvh),
                material: o.material,
                transform: o.transform.clone(),
                texture: o.texture.clone(),
            })
            .collect();
        frame_objects.push(Object {
            bvh: Arc::clone(&magnet.bvh),
            material: magnet.material,
            transform: vcad_kernel_raytrace::tlas::placement(
                &vcad_kernel::vcad_kernel_math::Transform::translation(
                    c[0] - hold_mm[0],
                    c[1] - hold_mm[1],
                    c[2] - hold_mm[2],
                ),
            ),
            texture: magnet.texture.clone(),
        });
        let mut cloud = base_cloud.clone();
        for k in 0..sim.exposure {
            let s = &trajectory[(i * sim.exposure + k).min(trajectory.len() - 1)];
            let foot = [s.pos_m[0] * 1e3, s.pos_m[1] * 1e3, glow_z];
            add_plasma(
                &mut cloud,
                tip,
                foot,
                foot_radius,
                &m_fil,
                &m_foot,
                scale,
                sim.medium_spacing_mm,
            );
        }
        let mut scene = dress_scene(frame_objects, &framing, pr)?;
        scene.splats = cloud.build();
        let png = encode_png(trace_frame(&scene, &framing, pr, false), opts)?;
        std::fs::write(&path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
        let elapsed = t_frame.elapsed();
        report.frames.push(path);
        report.per_frame.push(elapsed);
        progress(i + 1, total, elapsed);
    }

    Ok(PlasmoidClipReport {
        rig,
        animation: report,
        trajectory,
        exposure: sim.exposure,
    })
}

/// The trajectory as CSV — `t,x,y,mx,my,mz,beta,locked` (m, s), the format
/// `docs/plasmoid-sim/figures.py` reads.
pub fn trajectory_csv(trajectory: &[PlasmoidSample]) -> String {
    use std::fmt::Write as _;
    let mut csv = String::from("t,x,y,mx,my,mz,beta,locked\n");
    for s in trajectory {
        let c = s
            .magnet
            .map(|m| m.center_m)
            .unwrap_or([0.0, 0.0, f64::INFINITY]);
        let _ = writeln!(
            csv,
            "{:.5},{:.6},{:.6},{:.6},{:.6},{:.6},{:.4},{}",
            s.t_s,
            s.pos_m[0],
            s.pos_m[1],
            c[0],
            c[1],
            c[2],
            s.beta,
            is_locked(s) as u8
        );
    }
    csv
}

/// An ASS subtitle track with the clip's HUD: title, model line, phase
/// (racing / locked / magnet near), β at the foot and t, and a timeline strip
/// marking when the magnet is down (grey) and the foot is locked (orange),
/// with a playhead. Burned in by [`crate::animate::assemble_mp4_with`].
pub fn hud_ass(
    rig: &PlasmoidRig,
    trajectory: &[PlasmoidSample],
    exposure: usize,
    w: u32,
    h: u32,
) -> String {
    use std::fmt::Write as _;
    let ts = |t: f64| {
        // Floor, not round: an event rounded up to the next centisecond
        // starts after its frame's timestamp and shows a frame late.
        let cs = (t.max(0.0) * 100.0 + 1e-6).floor() as u64;
        format!(
            "{}:{:02}:{:02}.{:02}",
            cs / 360_000,
            cs / 6000 % 60,
            cs / 100 % 60,
            cs % 100
        )
    };
    let (w, h) = (w as f64, h as f64);
    let scale = h / 1280.0;
    let px = |v: f64| (v * scale).round();
    let mut s = String::new();
    let _ = write!(
        s,
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 2\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
         BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, \
         Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Big,DejaVu Sans,{big},&H00D7E1EB,&H00000000,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,7,0,0,0,1\n\
         Style: Small,DejaVu Sans,{small},&H00878C96,&H00000000,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,7,0,0,0,1\n\
         Style: Shape,DejaVu Sans,10,&H00FFFFFF,&H00000000,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,7,0,0,0,1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        big = px(34.0),
        small = px(24.0)
    );
    let ev = |s: &mut String, layer: u32, a: f64, b: f64, style: &str, text: &str| {
        let _ = writeln!(
            s,
            "Dialogue: {layer},{},{},{style},,0,0,0,,{text}",
            ts(a),
            ts(b)
        );
    };
    let x0 = px(32.0);
    ev(
        &mut s,
        0,
        0.0,
        rig.duration_s + 1.0,
        "Big",
        &format!("{{\\pos({x0},{})}}vcad · plasmoid sim", px(40.0)),
    );
    let model = format!(
        "reduced model · N52 {:.0}×{:.0} mm · Ar {} Torr",
        2e3 * rig.magnet.radius_m,
        1e3 * rig.magnet.thickness_m,
        trim_num(rig.pressure_torr)
    );
    ev(
        &mut s,
        0,
        0.0,
        rig.duration_s + 1.0,
        "Small",
        &format!("{{\\pos({x0},{})}}{model}", px(84.0)),
    );

    // Timeline strip: background, magnet-down and locked ticks, playhead.
    let (sx0, sx1, sy) = (px(32.0), w - px(32.0), h - px(90.0));
    let bar = |x: f64, y: f64, bw: f64, bh: f64, colour: &str| {
        format!(
            "{{\\an7\\pos(0,0)\\1c&H{colour}&\\bord0\\shad0\\p1}}m {x} {y} l {} {y} {} {} {x} {}",
            x + bw,
            x + bw,
            y + bh,
            y + bh
        )
    };
    ev(
        &mut s,
        0,
        0.0,
        rig.duration_s + 1.0,
        "Shape",
        &bar(sx0, sy, sx1 - sx0, px(18.0), "1A1A1E"),
    );
    let span =
        |pred: &dyn Fn(&PlasmoidSample) -> bool, y: f64, bh: f64, colour: &str, s: &mut String| {
            for (a, b) in intervals(trajectory, pred) {
                let xa = sx0 + (sx1 - sx0) * a / rig.duration_s;
                let xb = sx0 + (sx1 - sx0) * b / rig.duration_s;
                ev(
                    s,
                    1,
                    0.0,
                    rig.duration_s + 1.0,
                    "Shape",
                    &bar(xa, y, (xb - xa).max(1.0), bh, colour),
                );
            }
        };
    span(
        &|q| q.magnet.is_some_and(|m| m.center_m[2] < 0.1),
        sy + px(12.0),
        px(6.0),
        "786E6E",
        &mut s,
    );
    span(&is_locked, sy, px(10.0), "328CFF", &mut s);
    ev(
        &mut s,
        0,
        0.0,
        rig.duration_s + 1.0,
        "Small",
        &format!(
            "{{\\pos({sx0},{})\\1c&H6C7078&}}orange: locked   grey: magnet down",
            sy + px(26.0)
        ),
    );

    // Per-frame lines: phase, β and t, playhead.
    let frames = (rig.duration_s * rig.fps).round() as usize;
    for i in 0..frames {
        let (a, b) = (i as f64 / rig.fps, (i + 1) as f64 / rig.fps);
        let smp = &trajectory[(i * exposure).min(trajectory.len() - 1)];
        let locked = is_locked(smp);
        let mz = smp.magnet.map_or(f64::INFINITY, |m| m.center_m[2]);
        let phase = if mz > 0.1 {
            "racing"
        } else if locked {
            "locked to magnet"
        } else {
            "magnet near — not locked"
        };
        let colour = if locked { "5AAAFF" } else { "C8D2DC" };
        ev(
            &mut s,
            2,
            a,
            b,
            "Big",
            &format!("{{\\pos({x0},{})\\1c&H{colour}&}}{phase}", h - px(190.0)),
        );
        ev(
            &mut s,
            2,
            a,
            b,
            "Small",
            &format!(
                "{{\\pos({x0},{})}}β at foot {:5.2}   t {:5.2} s",
                h - px(146.0),
                smp.beta,
                a
            ),
        );
        let x = sx0 + (sx1 - sx0) * a / rig.duration_s;
        ev(
            &mut s,
            2,
            a,
            b,
            "Shape",
            &bar(x - px(1.0), sy - px(6.0), px(2.0), px(30.0), "FFFFFF"),
        );
    }
    s
}

/// Maximal time intervals `[a, b]` over which `pred` holds.
pub fn intervals(
    trajectory: &[PlasmoidSample],
    pred: &dyn Fn(&PlasmoidSample) -> bool,
) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start: Option<f64> = None;
    for smp in trajectory {
        match (pred(smp), start) {
            (true, None) => start = Some(smp.t_s),
            (false, Some(a)) => {
                out.push((a, smp.t_s));
                start = None;
            }
            _ => {}
        }
    }
    if let (Some(a), Some(last)) = (start, trajectory.last()) {
        out.push((a, last.t_s));
    }
    out
}

fn trim_num(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Write the clip's sidecars next to its frames: `trajectory.csv` and
/// `hud.ass`. Returns the HUD path.
pub fn write_sidecars(
    out_dir: &Path,
    report: &PlasmoidClipReport,
    w: u32,
    h: u32,
) -> Result<std::path::PathBuf, String> {
    let csv = out_dir.join("trajectory.csv");
    std::fs::write(&csv, trajectory_csv(&report.trajectory))
        .map_err(|e| format!("write {}: {e}", csv.display()))?;
    let hud = out_dir.join("hud.ass");
    std::fs::write(
        &hud,
        hud_ass(&report.rig, &report.trajectory, report.exposure, w, h),
    )
    .map_err(|e| format!("write {}: {e}", hud.display()))?;
    Ok(hud)
}
