//! Document-level optimizer on a parametric cylinder: closed-form optima, so
//! the gates are exact.
//!
//! 1. Minimize mass subject to two parameter-expression constraints
//!    (`r >= 5`, `h >= 2*r`). Optimum `(r, h) = (5, 10)`, both constraints
//!    active. Checks feasibility, KKT, `linear` provenance on the
//!    constraints, `seam` provenance on mass, and the seam-vs-FD agreement.
//! 2. The same with `r >= 6` priced by a plug-in QoI read off the evaluated
//!    mesh, so its gradient is `fd(step)` over re-evaluated documents — the
//!    fallback route for a solver that reads more than one part's mesh.
//! 3. The same plug-in declared mesh-only, so it is differenced along the
//!    seam's mesh velocity (`seam_fd(step)`, no document re-evaluation per
//!    probe) — and agrees with route 2.

use std::cell::Cell;

use vcad_eval::optimize::OptimizeDocResult;
use vcad_eval::optimize::{
    optimize_document, Constraint, ConstraintOp, ObjectiveTerm, OptimizeDocOptions,
    OptimizeDocRequest, OptimizeHooks, OptimizeStop, ParamSpec, Provenance, QoiInput, QoiRef,
    QoiRegistry, ScalarQoi, TermShape,
};
use vcad_ir::{BindingKey, CsgOp, Document, Expr, MaterialDef, Node, Parameter, SceneEntry};

fn cylinder_doc(r: f64, h: f64) -> Document {
    let mut doc = Document::new();
    doc.nodes.insert(
        1,
        Node {
            id: 1,
            name: Some("can".into()),
            op: CsgOp::Cylinder {
                radius: 1.0,
                height: 1.0,
                segments: 64,
            },
        },
    );
    doc.roots.push(SceneEntry {
        root: 1,
        material: "default".into(),
        visible: None,
    });
    doc.materials.insert(
        "default".into(),
        MaterialDef {
            name: "default".into(),
            ..Default::default()
        },
    );
    doc.parameters.insert("r".into(), Parameter::literal(r));
    doc.parameters.insert("h".into(), Parameter::literal(h));
    for (field, p) in [("radius", "r"), ("height", "h")] {
        doc.bindings.bind(
            BindingKey {
                node_id: 1,
                field_path: field.into(),
            },
            Expr::formula(p),
        );
    }
    doc
}

fn params() -> Vec<ParamSpec> {
    vec![
        ParamSpec {
            name: "r".into(),
            lo: 3.0,
            hi: 15.0,
        },
        ParamSpec {
            name: "h".into(),
            lo: 4.0,
            hi: 40.0,
        },
    ]
}

fn mass_objective() -> Vec<ObjectiveTerm> {
    vec![ObjectiveTerm {
        qoi: QoiRef::new("mass"),
        weight: 1.0,
        shape: TermShape::Linear,
    }]
}

#[test]
fn min_mass_under_linear_constraints() {
    let doc = cylinder_doc(10.0, 20.0);
    let req = OptimizeDocRequest {
        parameters: params(),
        objective: mass_objective(),
        constraints: vec![
            Constraint::Expr {
                expr: "r >= 5".into(),
                scale: None,
            },
            Constraint::Expr {
                expr: "h >= 2*r".into(),
                scale: None,
            },
        ],
        options: OptimizeDocOptions {
            circle_segments: 64,
            check_seam_fd: true,
            ..Default::default()
        },
    };
    let steps = Cell::new(0usize);
    let on_step = |_: &vcad_eval::optimize::TrajectoryStep| steps.set(steps.get() + 1);
    let hooks = OptimizeHooks {
        cancel: None,
        on_step: Some(&on_step),
    };
    let res = optimize_document(&doc, &req, &QoiRegistry::new(), &hooks).expect("optimize");
    eprintln!(
        "θ = {:?}, f = {}, stop = {:?}, kkt = {:?}, evals = {}, wall = {:.0} ms",
        res.theta, res.objective, res.stop, res.kkt, res.document_evals, res.wall_ms
    );

    assert!((res.theta[0] - 5.0).abs() < 0.02, "r → 5: {:?}", res.theta);
    assert!(
        (res.theta[1] - 10.0).abs() < 0.04,
        "h → 10: {:?}",
        res.theta
    );
    assert!(res.kkt.feasible, "{:?}", res.kkt);
    assert!(res.kkt.satisfied, "{:?}", res.kkt);
    assert!(
        res.kkt.multipliers.iter().all(|&l| l > 0.0),
        "both constraints active: {:?}",
        res.kkt.multipliers
    );
    assert_eq!(res.stop, OptimizeStop::Converged);
    assert_eq!(steps.get(), res.trajectory.len());

    // Provenance: mass is seam, expression constraints are linear.
    let last = res.trajectory.last().unwrap();
    assert!(last.qois[0]
        .gradient
        .iter()
        .all(|g| g.provenance == Provenance::Seam));
    for c in &last.constraints {
        assert!(c
            .gradient
            .iter()
            .all(|g| g.provenance == Provenance::Linear));
    }
    // Objective decreased monotonically in the trajectory's first round.
    assert!(res.trajectory.first().unwrap().objective > res.objective);
    // Every step costs something measurable.
    assert!(res.trajectory.iter().all(|s| s.cost.document_evals > 0));

    // Seam mass gradient agrees with a rebuild FD.
    assert_eq!(res.seam_fd_check.len(), 2);
    for c in &res.seam_fd_check {
        assert!(c.rel_error < 1e-4, "seam vs FD: {c:?}");
    }
}

fn max_abs_x(positions: &[f32]) -> f64 {
    positions
        .chunks(3)
        .map(|v| v[0].abs() as f64)
        .fold(0.0, f64::max)
}

/// Radius read off the evaluated mesh: max |x| over the part's vertices.
/// Reads the scene, so the optimizer differences it over re-evaluated
/// documents.
struct MeshRadius;

impl ScalarQoi for MeshRadius {
    fn name(&self) -> &str {
        "mesh_radius"
    }
    fn evaluate(&self, input: &QoiInput, part: Option<usize>) -> Result<f64, String> {
        let p = input.scene.parts.get(part.unwrap_or(0)).ok_or("no part")?;
        Ok(max_abs_x(&p.mesh.positions))
    }
}

/// The same quantity declared mesh-only.
struct MeshRadiusMeshOnly;

impl ScalarQoi for MeshRadiusMeshOnly {
    fn name(&self) -> &str {
        "mesh_radius"
    }
    fn evaluate(&self, input: &QoiInput, part: Option<usize>) -> Result<f64, String> {
        MeshRadius.evaluate(input, part)
    }
    fn mesh_only(&self) -> bool {
        true
    }
    fn evaluate_mesh(
        &self,
        _doc: &Document,
        _part: Option<usize>,
        positions: &[f32],
        _indices: &[u32],
    ) -> Result<f64, String> {
        Ok(max_abs_x(positions))
    }
}

fn radius_request() -> OptimizeDocRequest {
    OptimizeDocRequest {
        parameters: params(),
        objective: mass_objective(),
        constraints: vec![
            Constraint::Qoi {
                qoi: QoiRef::new("mesh_radius"),
                op: ConstraintOp::Ge,
                value: 6.0,
            },
            Constraint::Expr {
                expr: "h >= 2*r".into(),
                scale: None,
            },
        ],
        options: OptimizeDocOptions {
            circle_segments: 64,
            ..Default::default()
        },
    }
}

fn report(res: &OptimizeDocResult) {
    eprintln!(
        "θ = {:?}, stop = {:?}, kkt = {:?}\n  {} samples + {} value-only, {} document evals, {} plug-in evals, {} steps, {:.0} ms",
        res.theta,
        res.stop,
        res.kkt,
        res.samples,
        res.value_samples,
        res.document_evals,
        res.plugin_evals,
        res.trajectory.len(),
        res.wall_ms
    );
}

fn check_radius_optimum(res: &OptimizeDocResult) {
    assert!((res.theta[0] - 6.0).abs() < 0.03, "r → 6: {:?}", res.theta);
    assert!(
        (res.theta[1] - 12.0).abs() < 0.06,
        "h → 12: {:?}",
        res.theta
    );
    assert!(res.kkt.feasible && res.kkt.satisfied, "{:?}", res.kkt);
    let c = &res.trajectory.last().unwrap().constraints[0];
    // d(mesh_radius)/dr = 1 (scaled by 1/6), d/dh = 0.
    assert!((c.gradient[0].value + 1.0 / 6.0).abs() < 1e-3, "{c:?}");
    assert!(c.gradient[1].value.abs() < 1e-6, "{c:?}");
}

#[test]
fn plugin_qoi_constraint_is_finite_differenced() {
    let doc = cylinder_doc(10.0, 20.0);
    let reg = QoiRegistry::new().with(Box::new(MeshRadius));
    let res = optimize_document(&doc, &radius_request(), &reg, &OptimizeHooks::default())
        .expect("optimize");
    report(&res);
    check_radius_optimum(&res);
    let c = &res.trajectory.last().unwrap().constraints[0];
    assert!(
        matches!(c.gradient[0].provenance, Provenance::Fd { .. }),
        "{c:?}"
    );
}

#[test]
fn mesh_only_plugin_is_differenced_along_the_seam() {
    let doc = cylinder_doc(10.0, 20.0);
    let reg = QoiRegistry::new().with(Box::new(MeshRadiusMeshOnly));
    let res = optimize_document(&doc, &radius_request(), &reg, &OptimizeHooks::default())
        .expect("optimize");
    report(&res);
    check_radius_optimum(&res);
    let c = &res.trajectory.last().unwrap().constraints[0];
    assert!(
        matches!(c.gradient[0].provenance, Provenance::SeamFd { .. }),
        "{c:?}"
    );
    // No document re-evaluation per plug-in probe: every document
    // evaluation is a base or a seam probe (≤ 1 + 2n per full sample, 1 per
    // value-only sample, + the start's), never a plug-in FD probe.
    assert!(
        res.document_evals <= res.samples * 5 + res.value_samples + 1,
        "{} evals for {} samples + {} value-only",
        res.document_evals,
        res.samples,
        res.value_samples
    );
}

#[test]
fn unknown_qoi_and_parameter_are_refused() {
    let doc = cylinder_doc(10.0, 20.0);
    let mut req = OptimizeDocRequest {
        parameters: params(),
        objective: vec![ObjectiveTerm {
            qoi: QoiRef::new("stiffness"),
            weight: 1.0,
            shape: TermShape::Linear,
        }],
        constraints: vec![],
        options: Default::default(),
    };
    let reg = QoiRegistry::new();
    assert!(optimize_document(&doc, &req, &reg, &OptimizeHooks::default()).is_err());
    req.objective = mass_objective();
    req.parameters[0].name = "nope".into();
    assert!(optimize_document(&doc, &req, &reg, &OptimizeHooks::default()).is_err());
}

#[test]
fn request_round_trips_json() {
    let json = serde_json::json!({
        "parameters": [{"name": "r", "lo": 3, "hi": 15}],
        "objective": [
            {"qoi": "mass", "part": 0},
            {"qoi": "modal_f1", "weight": 0.01, "shape": {"hinge_below": 400}},
            {"qoi": "helmholtz_fb", "shape": {"target": 90}}
        ],
        "constraints": [
            {"expr": "port_len <= body_h - 2*wall - 15"},
            {"qoi": "modal_f1", "op": ">=", "value": 380}
        ],
        "options": {"density": 1.24e-6}
    });
    let req = vcad_eval::optimize::parse_request(&json).expect("parse");
    assert_eq!(req.objective.len(), 3);
    assert_eq!(req.objective[1].shape, TermShape::HingeBelow(400.0));
    assert_eq!(req.objective[0].qoi.part, Some(0));
    assert!(matches!(req.constraints[0], Constraint::Expr { .. }));
    assert!(matches!(
        req.constraints[1],
        Constraint::Qoi {
            op: ConstraintOp::Ge,
            ..
        }
    ));
    assert_eq!(req.options.density, 1.24e-6);
    assert_eq!(
        req.options.max_outer,
        OptimizeDocOptions::default().max_outer
    );
}
