//! Position-target tracking on a simple 2-link leg (acceptance run 8).
//!
//! Field report: targets `0.6·sin(t)` (hip) and `−0.8 + 0.5·sin(t)` (knee)
//! ended at hip 2.0°, knee −4.1° — the agent sent radians to an API that
//! takes degrees, so the commanded swing was under a degree. With the units
//! right, the servo itself must follow the swing: this measures the tracking
//! error of the default (inertia-derived) gains, ground on and off.

use vcad_ir::Document;
use vcad_kernel_physics::{Action, GroundConfig, RobotEnv};

/// Base block (grounded) with a thigh and shin hanging below it on Y-axis
/// revolutes. Links are 20×20×200 mm steel-ish cubes (default density).
fn leg_doc(base_z: f64) -> Document {
    serde_json::from_value(serde_json::json!({
        "version": "0.1",
        "nodes": {
            "0": {"id": 0, "op": {"type": "Cube", "size": {"x": 100.0, "y": 100.0, "z": 20.0}}},
            "1": {"id": 1, "op": {"type": "Cube", "size": {"x": 20.0, "y": 20.0, "z": 200.0}}},
            "2": {"id": 2, "op": {"type": "Cube", "size": {"x": 20.0, "y": 20.0, "z": 200.0}}}
        },
        "roots": [],
        "materials": {},
        "part_materials": {},
        "partDefs": {
            "base": {"id": "base", "name": "base", "root": 0},
            "thigh": {"id": "thigh", "name": "thigh", "root": 1},
            "shin": {"id": "shin", "name": "shin", "root": 2}
        },
        "instances": [
            {"id": "base", "partDefId": "base",
             "transform": {"translation": {"x": -50.0, "y": -50.0, "z": base_z},
                           "rotation": {"x": 0.0, "y": 0.0, "z": 0.0},
                           "scale": {"x": 1.0, "y": 1.0, "z": 1.0}}},
            {"id": "thigh", "partDefId": "thigh"},
            {"id": "shin", "partDefId": "shin"}
        ],
        "joints": [
            {"id": "hip", "parentInstanceId": "base", "childInstanceId": "thigh",
             "parentAnchor": {"x": 50.0, "y": 50.0, "z": 0.0},
             "childAnchor": {"x": 10.0, "y": 10.0, "z": 200.0},
             "kind": {"type": "Revolute", "axis": {"x": 0.0, "y": 1.0, "z": 0.0},
                       "limits": [-90.0, 90.0]},
             "state": 0.0},
            {"id": "knee", "parentInstanceId": "thigh", "childInstanceId": "shin",
             "parentAnchor": {"x": 10.0, "y": 10.0, "z": 0.0},
             "childAnchor": {"x": 10.0, "y": 10.0, "z": 200.0},
             "kind": {"type": "Revolute", "axis": {"x": 0.0, "y": 1.0, "z": 0.0},
                       "limits": [-120.0, 10.0]},
             "state": 0.0}
        ],
        "groundInstanceId": "base"
    }))
    .expect("valid doc")
}

struct Tracking {
    rms_deg: [f64; 2],
    max_deg: [f64; 2],
    final_deg: [f64; 2],
}

/// Drive the field report's targets (radians, converted to the API's degrees)
/// for `secs` and measure error after a 1 s settle.
fn track(ground_on: bool, secs: f64) -> Tracking {
    let dt = 1.0 / 240.0;
    let ground = GroundConfig {
        enabled: ground_on,
        ..GroundConfig::default()
    };
    // Base 500 mm up: the 400 mm leg swings clear of the floor.
    let mut env =
        RobotEnv::new(leg_doc(500.0), vec![], Some(dt as f32), None, Some(ground)).expect("env");
    env.reset();
    let (mut sq, mut max, mut n) = ([0.0; 2], [0.0f64; 2], 0usize);
    let mut last = [0.0; 2];
    let steps = (secs / dt) as usize;
    for k in 0..steps {
        let t = (k + 1) as f64 * dt;
        let target = [
            (0.6 * t.sin()).to_degrees(),
            (-0.8 + 0.5 * t.sin()).to_degrees(),
        ];
        let (obs, _, _) = env.step(Action::PositionTarget(target.to_vec()));
        for j in 0..2 {
            let e = obs.joint_positions[j] - target[j];
            last[j] = e;
            if t > 1.0 {
                sq[j] += e * e;
                max[j] = max[j].max(e.abs());
            }
        }
        if t > 1.0 {
            n += 1;
        }
    }
    Tracking {
        rms_deg: [(sq[0] / n as f64).sqrt(), (sq[1] / n as f64).sqrt()],
        max_deg: max,
        final_deg: last,
    }
}

#[test]
fn two_link_leg_tracks_sinusoidal_position_targets() {
    for ground_on in [true, false] {
        let r = track(ground_on, 8.0);
        eprintln!(
            "ground {}: hip rms {:.2}° max {:.2}° final {:.2}° | knee rms {:.2}° max {:.2}° final {:.2}°",
            if ground_on { "on " } else { "off" },
            r.rms_deg[0], r.max_deg[0], r.final_deg[0], r.rms_deg[1], r.max_deg[1], r.final_deg[1],
        );
        for j in 0..2 {
            assert!(
                r.rms_deg[j] < 2.0,
                "ground {ground_on}: joint {j} rms {:.2}°",
                r.rms_deg[j]
            );
            assert!(
                r.max_deg[j] < 4.0,
                "ground {ground_on}: joint {j} max {:.2}°",
                r.max_deg[j]
            );
        }
    }
}
