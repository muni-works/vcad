//! Document optimizer on a puck-like bass-reflex enclosure: a closed can
//! with an internal port tube hanging from the top wall (the geometry of
//! `docs/videos/every-physics/puck.loon`, minus the driver cutout, air and
//! board roots).
//!
//! Minimize the enclosure's mass over `wall` and `body_h` subject to
//!
//! - the port fitting inside the cavity: `port_len <= body_h - 2*wall - 15`
//!   (a parameter expression — `linear` provenance), and
//! - the first elastic mode staying above a target: `modal_f1 >= target`
//!   (the vcad-sound plug-in — `fd(step)` provenance).
//!
//! Gates: the optimum is feasible, satisfies first-order KKT, the mass
//! gradient is `seam` and agrees with a document-rebuild FD, and the modal
//! gradient is labeled `fd`.

use vcad_eval::optimize::{
    optimize_document, Constraint, ConstraintOp, ObjectiveTerm, OptimizeDocOptions,
    OptimizeDocRequest, OptimizeHooks, ParamSpec, Provenance, QoiInput, QoiRef, QoiRegistry,
    ScalarQoi, TermShape, TrajectoryStep,
};
use vcad_ir::{Document, Expr};
use vcad_sound::optimize::ModalFrequencyQoi;

const PUCK: &str = r#"
[defparam body_r 45.0 :unit "mm" :min 30 :max 70]
[defparam body_h 90.0 :unit "mm" :min 40 :max 160]
[defparam wall 3.0 :unit "mm" :min 1.5 :max 6]
[defparam port_r 8.0 :unit "mm" :min 5 :max 15]
[defparam port_len 35.0 :unit "mm" :min 10 :max 60]
[defparam port_wall 2.0 :unit "mm" :min 1.2 :max 4]

[let ri [- body_r wall]]
[let hi [- body_h [* 2.0 wall]]]
[let outer [cylinder body_r body_h]]
[let cavity [translate 0.0 0.0 wall [cylinder ri hi]]]
[let port-tube [translate 0.0 0.0 [- body_h port_len] [cylinder [+ port_r port_wall] port_len]]]
[let port-bore [translate 0.0 0.0 [- body_h port_len 1.0] [cylinder port_r [+ port_len 2.0]]]]
[let s1 [difference cavity outer]]
[let s2 [union port-tube s1]]
[let enclosure [difference port-bore s2]]
[root enclosure "pla"]
"#;

/// PLA, kg/mm³.
const PLA_DENSITY: f64 = 1.24e-6;

fn puck() -> Document {
    vcad_loon::eval_vcad(PUCK, None).expect("puck evaluates")
}

fn with(doc: &Document, vals: &[(&str, f64)]) -> Document {
    let mut d = doc.clone();
    for (k, v) in vals {
        d.parameters.get_mut(*k).expect("param").value = Expr::Number(*v);
    }
    d
}

fn f1_at(doc: &Document, q: &ModalFrequencyQoi) -> f64 {
    let scene = vcad_sound::evaluate(doc).expect("evaluate");
    q.evaluate(
        &QoiInput {
            doc,
            scene: &scene,
            params: &[],
            theta: &[],
        },
        Some(0),
    )
    .expect("modal")
}

// Every sample re-evaluates the puck's booleans 7+ times (base, 2n seam
// probes, 2n modal FD probes) plus 2n+1 voxel modal solves — tens of seconds
// per sample, too slow for PR CI. Run on demand:
// `cargo test --release -p vcad-sound --test optimize_puck -- --ignored --nocapture`
#[test]
#[ignore = "slow (tens of minutes): document optimizer end-to-end on the puck"]
fn puck_min_mass_subject_to_first_mode() {
    let doc = puck();
    let params = vec![
        ParamSpec {
            name: "wall".into(),
            lo: 1.5,
            hi: 6.0,
        },
        ParamSpec {
            name: "body_h".into(),
            lo: 40.0,
            hi: 160.0,
        },
    ];

    // Pin the modal discretization at the start geometry (the plug-in would
    // do this on its first call; doing it here lets the target probe below
    // share it).
    let probe = ModalFrequencyQoi::first();
    let f1_start = f1_at(&doc, &probe);
    // The mass-only optimum is the thinnest wall at the shortest body the
    // port allows (body_h = port_len + 2·wall + 15 = 53 mm). Ask for more
    // stiffness than that design has, so the modal constraint binds.
    let f1_thin = f1_at(&with(&doc, &[("wall", 1.5), ("body_h", 53.0)]), &probe);
    let target = 1.15 * f1_thin;
    eprintln!(
        "f1 start {f1_start:.1} Hz, thin-wall short body {f1_thin:.1} Hz → target {target:.1} Hz"
    );

    let req = OptimizeDocRequest {
        parameters: params,
        objective: vec![ObjectiveTerm {
            qoi: QoiRef::part("mass", 0),
            weight: 1.0,
            shape: TermShape::Linear,
        }],
        constraints: vec![
            Constraint::Expr {
                expr: "port_len <= body_h - 2*wall - 15".into(),
                scale: None,
            },
            Constraint::Qoi {
                qoi: QoiRef::part("modal_f1", 0),
                op: ConstraintOp::Ge,
                value: target,
            },
        ],
        options: OptimizeDocOptions {
            density: PLA_DENSITY,
            max_outer: 5,
            max_inner: 8,
            grad_tol: 1e-3,
            feas_tol: 2e-3,
            circle_segments: 48,
            check_seam_fd: true,
            ..Default::default()
        },
    };
    let reg = QoiRegistry::new().with(Box::new(probe));
    let on_step = |s: &TrajectoryStep| {
        eprintln!(
            "  it {:>2} r{} θ=[{:.3}, {:.3}] mass={:.5} kg  viol={:.2e}  {:.0} ms / {} evals",
            s.iter,
            s.round,
            s.theta[0],
            s.theta[1],
            s.objective,
            s.max_violation,
            s.cost.wall_ms,
            s.cost.document_evals
        )
    };
    let res = optimize_document(
        &doc,
        &req,
        &reg,
        &OptimizeHooks {
            cancel: None,
            on_step: Some(&on_step),
        },
    )
    .expect("optimize");
    eprintln!(
        "θ = {:?}  mass = {:.5} kg  stop = {:?}\nkkt = {:?}\nseam/fd = {:?}\n{} samples, {} document evals, {:.1} s",
        res.theta,
        res.objective,
        res.stop,
        res.kkt,
        res.seam_fd_check,
        res.samples,
        res.document_evals,
        res.wall_ms / 1e3
    );

    // Feasible, first-order optimal.
    assert!(res.kkt.feasible, "{:?}", res.kkt);
    assert!(res.kkt.satisfied, "{:?}", res.kkt);
    // Lighter than the start, and the modal constraint is what stopped it.
    assert!(res.objective < res.trajectory[0].objective);
    let last = res.trajectory.last().unwrap();
    let f1 = last.qois.iter().find(|q| q.qoi.qoi == "modal_f1").unwrap();
    assert!(
        f1.value >= target * (1.0 - 2e-3),
        "f1 {} < target {target}",
        f1.value
    );
    assert!(
        res.kkt.multipliers[1] > 0.0,
        "modal constraint active: {:?}",
        res.kkt
    );

    // Provenance on camera: mass = seam, f1 = fd, port fit = linear.
    let mass = last.qois.iter().find(|q| q.qoi.qoi == "mass").unwrap();
    assert!(mass
        .gradient
        .iter()
        .all(|g| g.provenance == Provenance::Seam));
    assert!(f1
        .gradient
        .iter()
        .all(|g| matches!(g.provenance, Provenance::Fd { .. })));
    assert!(last.constraints[0]
        .gradient
        .iter()
        .all(|g| g.provenance == Provenance::Linear));

    // One FD gradient against the seam gradient for mass.
    assert!(!res.seam_fd_check.is_empty());
    for c in &res.seam_fd_check {
        assert!(c.rel_error < 1e-3, "seam vs FD: {c:?}");
    }
}
