//! The plasmoid clip's timeline-driven run reproduces the KEYS-driven one.
//!
//! The 20 s magnet choreography used to live in a `KEYS` table in
//! `crates/vcad-kernel-particle/examples/plasmoid_magnet.rs`; it now lives in
//! `docs/plasmoid-sim/plasmoid-clip.loon` as the document timeline, and
//! `vcad-render --sim plasmoid` reads it (with the sim's inputs, as
//! parameters) from the document. This pins the move: the old table is kept
//! here verbatim as the golden fixture, run through the unchanged
//! `simulate()` with the old constants, and the rig bound to the committed
//! `.vcad` must give the same foot to a micrometre and the same LOCK/release
//! beats.

use std::path::PathBuf;

use vcad_kernel_particle::plasmoid::{simulate, DiscMagnet, MagnetPose, PlasmaParams};
use vcad_render::plasmoid::{intervals, is_locked, PlasmoidRig};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn clip_doc() -> vcad_ir::Document {
    let path = repo().join("docs/plasmoid-sim/plasmoid-clip.vcad");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).expect("plasmoid-clip.vcad parses as a Document")
}

// ── the golden fixture: examples/plasmoid_magnet.rs before the move ──────

const GAP: f64 = 10e-3;
const MARGIN: f64 = 1.8;
const KEYS: &[(f64, f64, f64, f64)] = &[
    (0.0, 0.055, 120.0, 0.25),
    (4.0, 0.055, 120.0, 0.25),
    (6.0, 0.055, 120.0, GAP),
    (8.0, 0.055, 120.0, GAP),
    (11.0, 0.055, 20.0, GAP),
    (13.0, 0.030, -40.0, GAP),
    (14.0, 0.050, -70.0, GAP),
    (14.12, 0.050, -150.0, GAP),
    (16.0, 0.050, -150.0, GAP),
    (16.5, 0.050, -150.0, GAP),
    (18.0, 0.050, -150.0, GAP),
    (19.0, 0.050, -150.0, 0.25),
    (21.0, 0.050, -150.0, 0.25),
];

fn smooth(u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

fn keys_pose(m: &DiscMagnet, t: f64) -> MagnetPose {
    let i = KEYS
        .windows(2)
        .position(|w| t >= w[0].0 && t < w[1].0)
        .unwrap_or(KEYS.len() - 2);
    let (a, b) = (KEYS[i], KEYS[i + 1]);
    let u = smooth((t - a.0) / (b.0 - a.0));
    let lerp = |p: f64, q: f64| p + (q - p) * u;
    let (r, ang, gap) = (lerp(a.1, b.1), lerp(a.2, b.2).to_radians(), lerp(a.3, b.3));
    MagnetPose {
        center_m: [r * ang.cos(), r * ang.sin(), gap + 0.5 * m.thickness_m],
        axis: [0.0, 0.0, -1.0],
    }
}

#[test]
fn timeline_reproduces_the_keys_trajectory() {
    let doc = clip_doc();
    let timeline = doc
        .timeline
        .clone()
        .expect("the clip document has a timeline");
    let rig = PlasmoidRig::from_document(&doc, &timeline).expect("rig binds");

    // The magnet path itself, sampled finely across the flick and everywhere.
    let m = DiscMagnet::n52_10x5();
    for k in 0..=20_000 {
        let t = k as f64 * 1e-3;
        let (a, b) = (keys_pose(&m, t).center_m, rig.pose_at(t).center_m);
        for c in 0..3 {
            assert!(
                (a[c] - b[c]).abs() < 1e-9,
                "magnet at t={t}: {a:?} vs {b:?}"
            );
        }
    }

    // The old run: argon at 1 Torr, mobility 1.8× the depinning threshold at
    // the t = 7 s hold, RK4 at 1/2400 s, a sample every 20 steps (120 Hz).
    let mut p = PlasmaParams::argon_dish(1.0);
    p.mobility_m2_s = MARGIN * p.depinning_mobility(&m, &keys_pose(&m, 7.0));
    let dt = 1.0 / 2400.0;
    let golden = simulate(
        &p,
        &m,
        |t| Some(keys_pose(&m, t)),
        [p.orbit_radius_m, 0.0],
        20.0,
        dt,
        20,
    );

    // The new run: 30 fps × 80 substeps = the same dt, 4 samples a frame.
    assert_eq!(rig.fps, 30.0);
    assert_eq!(rig.duration_s, 20.0);
    let ours = rig.trajectory(1.0 / (rig.fps * 80.0), 20);
    assert_eq!(golden.len(), ours.len());
    let mut worst: f64 = 0.0;
    for (g, o) in golden.iter().zip(&ours) {
        assert!((g.t_s - o.t_s).abs() < 1e-9);
        let d = ((g.pos_m[0] - o.pos_m[0]).powi(2) + (g.pos_m[1] - o.pos_m[1]).powi(2)).sqrt();
        worst = worst.max(d);
        assert!(
            d < 1e-6,
            "foot diverged at t={:.3}: {:?} vs {:?}",
            g.t_s,
            g.pos_m,
            o.pos_m
        );
    }
    eprintln!("worst foot difference over 20 s: {worst:.3e} m");

    // Same beats: lock → drag → flick break-away → recapture → release.
    let lg = intervals(&golden, &is_locked);
    let lo = intervals(&ours, &is_locked);
    assert_eq!(lg, lo, "LOCK/release times differ");
    assert!(
        lg.len() >= 2,
        "expected a lock, a break-away and a recapture: {lg:?}"
    );
}

/// The committed `.vcad` is `plasmoid-clip.loon` through loon2vcad: the
/// parameters and timeline the renderer reads are the ones the source says.
#[test]
fn committed_clip_matches_its_loon_source() {
    let path = repo().join("docs/plasmoid-sim/plasmoid-clip.loon");
    let fresh = vcad_loon::eval_vcad_file(&path).expect("plasmoid-clip.loon evaluates");
    let committed = clip_doc();
    assert_eq!(
        fresh.timeline, committed.timeline,
        "regenerate plasmoid-clip.vcad"
    );
    let resolve = |d: &vcad_ir::Document| {
        let mut v: Vec<(String, f64)> = vcad_ir::resolve_parameters(&d.parameters)
            .unwrap()
            .into_iter()
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    assert_eq!(
        resolve(&fresh),
        resolve(&committed),
        "regenerate plasmoid-clip.vcad"
    );
}
