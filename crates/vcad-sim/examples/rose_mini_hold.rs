//! Drop rose mini into the gym and see what happens: does it stand under PD hold?
//!
//! ```bash
//! vcad export hardware/rose-mini/rose-mini.loon rose-mini.vcad      # rose-streamlined branch
//! python3 docs/videos/robot-from-nothing/poc/float_rose_mini.py rose-mini.vcad rose-float.vcad
//! cargo run --release -p vcad-sim --example rose_mini_hold -- rose-float.vcad [hold|passive] > trace.csv
//! ```
//!
//! The Free base, densities and effort limits come from the patch script, because
//! loon export can't express them yet (crates/vcad-loon/src/convert.rs). No policy:
//! this is the "before" shot — a K1-class biped holding its zero pose with a joint
//! PD and nothing else. 1 kHz physics, 50 Hz control, 3 s.

use vcad_ir::Document;
use vcad_kernel_physics::{Action, EnvConfig, RobotEnv, TerminationConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: rose_mini_hold <doc.vcad> [hold|passive]");
    let mode = std::env::args().nth(2).unwrap_or_else(|| "hold".into());
    let doc: Document = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let cfg = EnvConfig {
        termination: Some(TerminationConfig {
            base_height_below: None,
            base_tilt_above_deg: None,
            terminate_on_joint_limit: false,
        }),
        base_instance_id: Some("cover-chest_i".to_string()),
        ..EnvConfig::default()
    };
    let mut env = RobotEnv::new_with_config(
        doc,
        vec!["sole_ll_i".into(), "sole_rl_i".into()],
        Some(0.001),
        Some(20),
        None,
        cfg,
    )?;
    env.set_max_steps(150);
    let ids: Vec<String> = env.actuated_joint_ids().to_vec();
    eprintln!(
        "floating_base={} action_dim={} obs_dim={} joints={}",
        env.has_floating_base(),
        env.action_dim(),
        env.observation_dim(),
        env.num_joints()
    );
    if mode == "hold" {
        // Gains keyed by joint id; loon export names them joint_<idx>, so every
        // actuated joint gets one leg-class gain here (convert.rs:503).
        for id in &ids {
            env.set_joint_gains(id, 150.0, 4.0);
        }
        // Joints whose kp is past the explicit-PD stability limit get clamped to
        // 80% of it, so the trace measures the robot and not an integrator blowing
        // up. `STABLE=0` keeps the blanket gain to see the difference.
        let clamp = std::env::var("STABLE").map(|v| v != "0").unwrap_or(true);
        for w in env.check_gain_stability() {
            eprintln!("gain warning: {w:?}");
            if clamp {
                env.set_joint_gains(&w.joint_id, 0.8 * w.max_stable_kp, 4.0);
                eprintln!(
                    "  -> clamped {} to kp {:.1}",
                    w.joint_id,
                    0.8 * w.max_stable_kp
                );
            }
        }
        if clamp {
            let left = env.check_gain_stability();
            eprintln!("gain warnings after clamp: {}", left.len());
        }
    }
    let obs = env.reset();
    eprintln!("reset base_pose={:?}", obs.base_pose);
    println!("t_s,base_x_mm,base_y_mm,base_z_mm,tilt_deg,foot_l_z_mm,foot_r_z_mm,done");
    for i in 0..150 {
        let a = if mode == "hold" {
            Action::PositionTarget(vec![0.0; env.action_dim()])
        } else {
            Action::Torque(vec![0.0; env.action_dim()])
        };
        let r = env.step_full(a);
        let o = &r.observation;
        let p = o.base_pose.unwrap_or([f64::NAN; 7]);
        // tilt of base z-axis from world z, from quaternion (w, x, y, z) at p[3..7]
        let (x, y) = (p[4], p[5]);
        let zz = 1.0 - 2.0 * (x * x + y * y);
        let tilt = zz.clamp(-1.0, 1.0).acos().to_degrees();
        let fz = |k: usize| {
            o.end_effector_poses
                .get(k)
                .map_or(f64::NAN, |f| f[2] * 1000.0)
        };
        println!(
            "{:.3},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{}",
            (i + 1) as f64 * 0.02,
            p[0] * 1000.0,
            p[1] * 1000.0,
            p[2] * 1000.0,
            tilt,
            fz(0),
            fz(1),
            r.done
        );
        if r.done {
            break;
        }
    }
    Ok(())
}
