//! Gradient-driven optimization over named document parameters, plus
//! per-face shape sensitivities the viewport can paint as a heatmap.
//!
//! Gradients come from the differentiable seam
//! ([`vcad_eval::diff::document_parameter_qoi_gradient`]): volume / mass /
//! centroid derivatives are exact analytic seam evaluations; bounding-box
//! extents are central finite differences (a bbox is a non-smooth max). The
//! optimizer is a bound-projected, diagonally-preconditioned gradient descent
//! with Armijo backtracking on a quadratic-penalty merit function, wrapped in
//! a penalty continuation loop — deliberately simple, robust on the
//! few-parameter problems agents pose, and cancellable between every
//! evaluation.
//!
//! Every result carries a **central-FD check** of the objective gradient at
//! the final point; a check that disagrees fails the task closed rather than
//! returning an optimum built on a wrong gradient.
//!
//! Stiffness objectives/constraints are refused: they need the FEA shape
//! adjoint (`vcad-kernel-fea`) priced through the seam, which is not wired to
//! document parameters yet. Silently substituting a geometric proxy would be
//! a lie about the part.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{json, Value};
use vcad_eval::diff::{document_parameter_qoi_gradient, PartQoiGradient};
use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::{Document, Expr};
use vcad_kernel::vcad_kernel_primitives::BRepSolid;
use vcad_kernel_diff::{evaluate_with_sensitivity, synthesize_seeding};
use vcad_kernel_tessellate::frozen::{capture_plan, NodeRecipe};
use vcad_kernel_tessellate::TessellationParams;

use crate::task::{JobError, TaskCtx};

/// A quantity of interest the optimizer can read (summed over parts, or
/// for one part when `part` is given).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qoi {
    /// Signed volume, mm³.
    Volume,
    /// Mass (`density · volume`).
    Mass,
    /// Bounding-box extent along an axis (0 = x, 1 = y, 2 = z).
    Extent(usize),
    /// Centroid coordinate along an axis.
    Centroid(usize),
}

impl Qoi {
    fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "volume" => Qoi::Volume,
            "mass" => Qoi::Mass,
            "bbox_x" => Qoi::Extent(0),
            "bbox_y" => Qoi::Extent(1),
            "bbox_z" => Qoi::Extent(2),
            "centroid_x" => Qoi::Centroid(0),
            "centroid_y" => Qoi::Centroid(1),
            "centroid_z" => Qoi::Centroid(2),
            "stiffness" | "compliance" | "deflection" | "stress" => {
                return Err(format!(
                    "`{s}` needs the FEA shape adjoint priced through the seam, which is not \
                     wired to document parameters yet — refusing rather than optimizing a proxy"
                ))
            }
            other => return Err(format!("unknown qoi `{other}`")),
        })
    }

    fn read(self, p: &PartQoiGradient) -> (f64, f64) {
        match self {
            Qoi::Volume => (p.volume, p.d_volume),
            Qoi::Mass => (p.mass, p.d_mass),
            Qoi::Extent(a) => (p.bbox_extents[a], p.d_bbox_extents[a]),
            Qoi::Centroid(a) => (p.centroid[a], p.d_centroid[a]),
        }
    }
}

#[derive(Debug, Clone)]
struct Param {
    name: String,
    lo: f64,
    hi: f64,
}

#[derive(Debug, Clone)]
struct Constraint {
    qoi: Qoi,
    part: Option<usize>,
    /// `true` for `>=`, `false` for `<=`.
    ge: bool,
    value: f64,
    label: String,
}

/// Parsed `optimize_parameters` request.
#[derive(Debug, Clone)]
pub struct OptimizeRequest {
    params: Vec<Param>,
    objective: Qoi,
    objective_part: Option<usize>,
    maximize: bool,
    constraints: Vec<Constraint>,
    density: f64,
    max_iters: usize,
    tol: f64,
    tess: TessellationParams,
    fd_tolerance: f64,
}

impl OptimizeRequest {
    /// Parse the JSON request.
    ///
    /// ```json
    /// { "parameters": [{"name": "r", "min": 3, "max": 15}],
    ///   "objective": {"qoi": "mass", "sense": "min"},
    ///   "constraints": [{"qoi": "bbox_z", "op": ">=", "value": 20}],
    ///   "density": 1.0, "maxIterations": 40, "tolerance": 1e-3 }
    /// ```
    pub fn from_json(req: &Value) -> Result<Self, String> {
        let params = req
            .get("parameters")
            .and_then(Value::as_array)
            .ok_or("`parameters` must be an array of {name, min, max}")?
            .iter()
            .map(|p| {
                let name = p
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("parameter needs `name`")?;
                let lo = p
                    .get("min")
                    .and_then(Value::as_f64)
                    .ok_or("parameter needs `min`")?;
                let hi = p
                    .get("max")
                    .and_then(Value::as_f64)
                    .ok_or("parameter needs `max`")?;
                if !(lo.is_finite() && hi.is_finite() && lo < hi) {
                    return Err(format!("parameter `{name}`: need finite min < max"));
                }
                Ok(Param {
                    name: name.to_string(),
                    lo,
                    hi,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if params.is_empty() || params.len() > 16 {
            return Err("optimize 1–16 parameters".into());
        }
        let obj = req
            .get("objective")
            .cloned()
            .unwrap_or(json!({"qoi": "mass"}));
        let objective = Qoi::parse(obj.get("qoi").and_then(Value::as_str).unwrap_or("mass"))?;
        let maximize = obj.get("sense").and_then(Value::as_str) == Some("max");
        let objective_part = obj.get("part").and_then(Value::as_u64).map(|p| p as usize);
        let constraints = req
            .get("constraints")
            .and_then(Value::as_array)
            .map(|cs| {
                cs.iter()
                    .map(|c| {
                        let q = c
                            .get("qoi")
                            .and_then(Value::as_str)
                            .ok_or("constraint needs `qoi`")?;
                        let op = c.get("op").and_then(Value::as_str).unwrap_or(">=");
                        let ge = match op {
                            ">=" => true,
                            "<=" => false,
                            o => return Err(format!("constraint op `{o}` (>= | <=)")),
                        };
                        let value = c
                            .get("value")
                            .and_then(Value::as_f64)
                            .filter(|v| v.is_finite())
                            .ok_or("constraint needs a finite `value`")?;
                        Ok(Constraint {
                            qoi: Qoi::parse(q)?,
                            part: c.get("part").and_then(Value::as_u64).map(|p| p as usize),
                            ge,
                            value,
                            label: format!("{q} {op} {value}"),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .transpose()?
            .unwrap_or_default();
        let num = |k: &str, d: f64| req.get(k).and_then(Value::as_f64).unwrap_or(d);
        Ok(OptimizeRequest {
            params,
            objective,
            objective_part,
            maximize,
            constraints,
            density: num("density", 1.0),
            max_iters: num("maxIterations", 40.0).clamp(1.0, 500.0) as usize,
            tol: num("tolerance", 1e-3),
            tess: TessellationParams {
                circle_segments: num("circleSegments", 48.0) as u32,
                height_segments: 2,
                ..Default::default()
            },
            fd_tolerance: num("fdTolerance", 2e-2),
        })
    }
}

/// QoI values + gradients at one parameter vector.
#[derive(Debug, Clone)]
struct Sample {
    #[allow(dead_code)]
    theta: Vec<f64>,
    /// `parts[k]` = per-part QoI bundle differentiated w.r.t. parameter k.
    parts: Vec<Vec<PartQoiGradient>>,
}

impl Sample {
    fn qoi(&self, q: Qoi, part: Option<usize>) -> Result<(f64, Vec<f64>), String> {
        let mut val = 0.0;
        let mut grad = vec![0.0; self.parts.len()];
        for (k, per_param) in self.parts.iter().enumerate() {
            let mut v = 0.0;
            let mut g = 0.0;
            let pick: Vec<&PartQoiGradient> = match part {
                Some(i) => vec![per_param
                    .get(i)
                    .ok_or_else(|| format!("part {i} out of range ({} parts)", per_param.len()))?],
                None => per_param.iter().collect(),
            };
            if part.is_none() && matches!(q, Qoi::Extent(_) | Qoi::Centroid(_)) && pick.len() != 1 {
                return Err("bbox/centroid qoi on a multi-part document needs `part`".into());
            }
            for p in pick {
                let (a, b) = q.read(p);
                v += a;
                g += b;
            }
            val = v;
            grad[k] = g;
        }
        Ok((val, grad))
    }
}

fn with_params(doc: &Document, params: &[Param], theta: &[f64]) -> Result<Document, String> {
    let mut d = doc.clone();
    for (p, &t) in params.iter().zip(theta) {
        let slot = d
            .parameters
            .get_mut(&p.name)
            .ok_or_else(|| format!("document has no parameter `{}`", p.name))?;
        slot.value = Expr::Number(t);
    }
    Ok(d)
}

fn sample(doc: &Document, r: &OptimizeRequest, theta: &[f64]) -> Result<Sample, String> {
    let d = with_params(doc, &r.params, theta)?;
    let parts = r
        .params
        .iter()
        .map(|p| {
            // Seeding-synthesis match step, not an FD step (the seam derivative is
            // analytic). Larger steps trip the frozen-plan topology guard.
            let step = 1e-4;
            document_parameter_qoi_gradient(&d, &p.name, r.density, &r.tess, step)
                .map_err(|e| format!("gradient w.r.t. `{}`: {e}", p.name))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Sample {
        theta: theta.to_vec(),
        parts,
    })
}

/// Merit function value + gradient: normalized objective plus quadratic
/// penalty on normalized constraint violations.
fn merit(
    r: &OptimizeRequest,
    s: &Sample,
    j_scale: f64,
    mu: f64,
) -> Result<(f64, Vec<f64>, f64), String> {
    let (j, gj) = s.qoi(r.objective, r.objective_part)?;
    let sign = if r.maximize { -1.0 } else { 1.0 };
    let mut f = sign * j / j_scale;
    let mut g: Vec<f64> = gj.iter().map(|x| sign * x / j_scale).collect();
    let mut worst = 0.0f64;
    for c in &r.constraints {
        let (v, gv) = s.qoi(c.qoi, c.part)?;
        let scale = c.value.abs().max(1.0);
        // viol > 0 means violated.
        let viol = if c.ge { c.value - v } else { v - c.value } / scale;
        if viol > 0.0 {
            worst = worst.max(viol);
            f += mu * viol * viol;
            let dviol_sign = if c.ge { -1.0 } else { 1.0 };
            for (gk, gvk) in g.iter_mut().zip(&gv) {
                *gk += mu * 2.0 * viol * dviol_sign * gvk / scale;
            }
        }
    }
    Ok((f, g, worst))
}

fn project(r: &OptimizeRequest, theta: &mut [f64]) {
    for (t, p) in theta.iter_mut().zip(&r.params) {
        *t = t.clamp(p.lo, p.hi);
    }
}

/// One iteration record (goes to the task's history).
#[derive(Debug, Clone, Serialize)]
pub struct IterRecord {
    /// Iteration index.
    pub iter: usize,
    /// Parameter values.
    pub theta: Vec<f64>,
    /// Raw objective value.
    pub objective: f64,
    /// Largest normalized constraint violation (0 = feasible).
    pub max_violation: f64,
}

/// `optimize_parameters` as a task job. Returns the optimum, iteration
/// history, the FD gradient check, and the per-face sensitivity field at the
/// optimum (see [`face_sensitivities`]).
pub fn optimize_parameters(doc: &Document, req: &Value, ctx: &TaskCtx) -> Result<Value, JobError> {
    let r = OptimizeRequest::from_json(req)?;
    let env = vcad_ir::resolve_parameters(&doc.parameters).map_err(|e| e.to_string())?;
    let mut theta: Vec<f64> = r
        .params
        .iter()
        .map(|p| {
            env.get(&p.name)
                .copied()
                .ok_or_else(|| format!("document has no parameter `{}`", p.name))
        })
        .collect::<Result<_, _>>()?;
    project(&r, &mut theta);
    let ranges: Vec<f64> = r.params.iter().map(|p| p.hi - p.lo).collect();

    ctx.progress(0.0, "evaluating start point")?;
    let mut cur = sample(doc, &r, &theta)?;
    let (j0, _) = cur.qoi(r.objective, r.objective_part)?;
    let j_scale = j0.abs().max(1e-9);

    let mut history = Vec::new();
    let mut mu = 10.0;
    let mut iter = 0usize;
    let mut converged = false;
    let outer_rounds = 5;
    let mut trial_len = 0.25; // max normalized step, adapts.
    'outer: for round in 0..outer_rounds {
        let (mut fcur, mut gcur, mut viol) = merit(&r, &cur, j_scale, mu)?;
        loop {
            if iter >= r.max_iters {
                break 'outer;
            }
            ctx.checkpoint()?;
            // Preconditioned descent direction in normalized coordinates.
            let d: Vec<f64> = gcur.iter().zip(&ranges).map(|(g, rg)| -g * rg).collect();
            let dmax = d.iter().fold(0.0f64, |m, x| m.max(x.abs()));
            // Projected-gradient stationarity: would a small step move θ?
            let moved = {
                let mut probe = theta.clone();
                for k in 0..probe.len() {
                    probe[k] += 1e-3 * d[k] / dmax.max(1e-300) * ranges[k];
                }
                project(&r, &mut probe);
                probe
                    .iter()
                    .zip(&theta)
                    .zip(&ranges)
                    .any(|((a, b), rg)| (a - b).abs() / rg > 1e-7)
            };
            if dmax < 1e-12 || !moved {
                break;
            }
            let mut alpha = trial_len;
            let mut accepted = None;
            while alpha > r.tol * 1e-2 {
                let mut cand = theta.clone();
                for k in 0..cand.len() {
                    cand[k] += alpha * d[k] / dmax * ranges[k];
                }
                project(&r, &mut cand);
                let step: f64 = cand
                    .iter()
                    .zip(&theta)
                    .zip(&ranges)
                    .map(|((a, b), rg)| ((a - b) / rg).abs())
                    .fold(0.0, f64::max);
                if step < r.tol * 1e-2 {
                    break;
                }
                ctx.checkpoint()?;
                let s = sample(doc, &r, &cand)?;
                let (fs, gs, vs) = merit(&r, &s, j_scale, mu)?;
                // Armijo on the projected step.
                let decrease: f64 = gcur
                    .iter()
                    .zip(cand.iter().zip(&theta))
                    .map(|(g, (a, b))| g * (a - b))
                    .sum();
                if fs <= fcur + 1e-4 * decrease {
                    accepted = Some((cand, s, fs, gs, vs, step));
                    break;
                }
                alpha *= 0.5;
            }
            let Some((cand, s, fs, gs, vs, step)) = accepted else {
                break;
            };
            theta = cand;
            cur = s;
            fcur = fs;
            gcur = gs;
            viol = vs;
            trial_len = (alpha * 2.0).min(0.5);
            iter += 1;
            let (j, _) = cur.qoi(r.objective, r.objective_part)?;
            history.push(IterRecord {
                iter,
                theta: theta.clone(),
                objective: j,
                max_violation: viol,
            });
            let frac = (iter as f64 / r.max_iters as f64).min(0.9);
            ctx.progress(
                frac * 0.85,
                format!("round {round} iter {iter}: objective {j:.6}, violation {viol:.2e}"),
            )?;
            if step < r.tol {
                break;
            }
        }
        if viol <= r.tol {
            converged = true;
            break;
        }
        mu *= 10.0;
        trial_len = 0.25;
    }

    // ---- FD gradient check at the optimum (fail closed) ----
    ctx.progress(0.88, "finite-difference gradient check")?;
    let (j_opt, g_opt) = cur.qoi(r.objective, r.objective_part)?;
    let mut fd_report = Vec::new();
    let mut fd_ok = true;
    for k in 0..theta.len() {
        ctx.checkpoint()?;
        let h = 1e-3 * ranges[k];
        let (mut tp, mut tm) = (theta.clone(), theta.clone());
        tp[k] += h;
        tm[k] -= h;
        let jp = objective_value(doc, &r, &tp)?;
        let jm = objective_value(doc, &r, &tm)?;
        let fd = (jp - jm) / (2.0 * h);
        let scale = fd.abs().max(g_opt[k].abs()).max(1e-9 * j_scale);
        let rel = (fd - g_opt[k]).abs() / scale;
        // A gradient that is tiny relative to the objective can't be checked
        // to a relative tolerance; compare absolutely against the objective.
        let ok = rel <= r.fd_tolerance || (fd - g_opt[k]).abs() <= 1e-6 * j_scale;
        fd_ok &= ok;
        fd_report.push(json!({
            "parameter": r.params[k].name, "analytic": g_opt[k], "fd": fd,
            "relError": rel, "pass": ok,
        }));
    }
    let fd_check = json!({ "pass": fd_ok, "tolerance": r.fd_tolerance, "perParameter": fd_report });
    if !fd_ok {
        return Err(JobError::Failed {
            message: "analytic gradient disagrees with finite differences — refusing the result"
                .into(),
            report: Some(json!({ "fdCheck": fd_check, "theta": theta })),
        });
    }

    ctx.progress(0.92, "per-face sensitivities at the optimum")?;
    let opt_doc = with_params(doc, &r.params, &theta)?;
    let mut fields = Vec::new();
    for (k, p) in r.params.iter().enumerate() {
        ctx.checkpoint()?;
        let step = 1e-4;
        let mut field = face_sensitivities(&opt_doc, &p.name, &r.tess, step)?;
        field["dObjective"] = json!(g_opt[k]);
        fields.push(field);
    }

    let constraints: Vec<Value> = r
        .constraints
        .iter()
        .map(|c| {
            let v = cur.qoi(c.qoi, c.part).map(|x| x.0).unwrap_or(f64::NAN);
            let ok = if c.ge {
                v >= c.value - r.tol * c.value.abs().max(1.0)
            } else {
                v <= c.value + r.tol * c.value.abs().max(1.0)
            };
            json!({ "constraint": c.label, "value": v, "satisfied": ok })
        })
        .collect();
    Ok(json!({
        "parameters": r.params.iter().zip(&theta).map(|(p, t)| json!({
            "name": p.name, "value": t, "min": p.lo, "max": p.hi,
            "atBound": (*t - p.lo).abs() < 1e-9 || (*t - p.hi).abs() < 1e-9,
        })).collect::<Vec<_>>(),
        "objective": { "start": j0, "final": j_opt, "gradient": g_opt },
        "constraints": constraints,
        "converged": converged,
        "iterations": iter,
        "history": history,
        "fdCheck": fd_check,
        "sensitivity": fields,
        "basis": "seam-analytic mass/volume/centroid; central-FD bbox extents",
    }))
}

fn objective_value(doc: &Document, r: &OptimizeRequest, theta: &[f64]) -> Result<f64, String> {
    // Independent of the seam: rebuild + mesh integral of the evaluated part
    // for mass/volume, and the gradient bundle's value for the rest.
    match r.objective {
        Qoi::Volume | Qoi::Mass => {
            let d = with_params(doc, &r.params, theta)?;
            let scene =
                evaluate_document(&d, &EvalOptions::default()).map_err(|e| e.to_string())?;
            let mut v = 0.0;
            for (i, part) in scene
                .parts
                .iter()
                .filter(|p| p.solid.as_ref().and_then(|s| s.as_brep()).is_some())
                .enumerate()
            {
                if r.objective_part.is_some_and(|want| want != i) {
                    continue;
                }
                let brep = part
                    .solid
                    .as_ref()
                    .and_then(|s| s.as_brep())
                    .expect("filtered");
                let plan = capture_plan(brep, &r.tess).map_err(|e| format!("{e:?}"))?;
                let seam = evaluate_with_sensitivity(brep, &plan, &Default::default())
                    .map_err(|e| e.to_string())?;
                v += vcad_kernel_diff::mesh_volume(&seam.positions, &seam.triangles);
            }
            Ok(if r.objective == Qoi::Mass {
                v * r.density
            } else {
                v
            })
        }
        _ => Ok(sample(doc, r, theta)?.qoi(r.objective, r.objective_part)?.0),
    }
}

// ---------------------------------------------------------------------------
// Per-face sensitivities
// ---------------------------------------------------------------------------

fn solids(doc: &Document, parameter: &str, value: f64) -> Result<Vec<BRepSolid>, String> {
    let mut d = doc.clone();
    d.parameters
        .get_mut(parameter)
        .ok_or_else(|| format!("document has no parameter `{parameter}`"))?
        .value = Expr::Number(value);
    let opts = EvalOptions {
        skip_clash_detection: true,
        ..EvalOptions::default()
    };
    let scene = evaluate_document(&d, &opts).map_err(|e| e.to_string())?;
    Ok(scene
        .parts
        .iter()
        .filter_map(|p| p.solid.as_ref().and_then(|s| s.as_brep()).cloned())
        .collect())
}

fn node_faces(r: &NodeRecipe) -> Option<BTreeSet<u32>> {
    match r {
        NodeRecipe::SurfaceUv { face, .. } => Some([*face].into()),
        NodeRecipe::Boundary { face_a, face_b, .. } => Some([*face_a, *face_b].into()),
        _ => None,
    }
}

/// Per-face shape sensitivity of every solid part w.r.t. one parameter:
/// the **normal velocity** `v_n = (dx/dθ)·n̂` of the surface, in mm per unit
/// of θ. Positive = the surface moves outward (adds material) as θ grows.
///
/// Output per part, ready for a viewport heatmap:
/// - `positions` (flat xyz, f32-rounded) + `indices` — the frozen seam mesh;
/// - `triNormalVelocity` — `v_n` per triangle (paint this);
/// - `triFace` — canonical face index per triangle, or -1 where the frozen
///   plan does not pin a triangle to one face (all three nodes are topology
///   vertices — common on planar faces);
/// - `faces` — area-weighted mean and max-|v_n| per pinned face;
/// - `range` — `[min, max]` of `v_n`, for a symmetric diverging color map.
pub fn face_sensitivities(
    doc: &Document,
    parameter: &str,
    tess: &TessellationParams,
    probe_step: f64,
) -> Result<Value, String> {
    let env = vcad_ir::resolve_parameters(&doc.parameters).map_err(|e| e.to_string())?;
    let theta0 = *env
        .get(parameter)
        .ok_or_else(|| format!("document has no parameter `{parameter}`"))?;
    let base = solids(doc, parameter, theta0)?;
    let n_parts = base.len();
    for probe in [theta0 - probe_step, theta0 + probe_step] {
        if solids(doc, parameter, probe)?.len() != n_parts {
            return Err("part count changes under the probe step".into());
        }
    }
    let mut parts = Vec::new();
    for (i, brep) in base.iter().enumerate() {
        let build = |t: &[f64]| -> BRepSolid {
            solids(doc, parameter, t[0])
                .expect("probe validated")
                .into_iter()
                .nth(i)
                .expect("part count validated")
        };
        let seeding =
            synthesize_seeding(&build, &[theta0], 0, probe_step).map_err(|e| e.to_string())?;
        let plan = capture_plan(brep, tess).map_err(|e| format!("{e:?}"))?;
        let seam = evaluate_with_sensitivity(brep, &plan, &seeding).map_err(|e| e.to_string())?;

        let p = |k: u32| {
            let q = &seam.positions[k as usize];
            [q.x, q.y, q.z]
        };
        let v = |k: u32| {
            let q = &seam.velocities[k as usize];
            [q.x, q.y, q.z]
        };
        let mut tri_vn = Vec::with_capacity(seam.triangles.len());
        let mut tri_face = Vec::with_capacity(seam.triangles.len());
        let mut face_acc: std::collections::BTreeMap<u32, (f64, f64, f64)> = Default::default();
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for t in &seam.triangles {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let area = 0.5 * len;
            let vn = if len > 0.0 {
                let mut s = 0.0;
                for k in t {
                    let w = v(*k);
                    s += (w[0] * n[0] + w[1] * n[1] + w[2] * n[2]) / len;
                }
                s / 3.0
            } else {
                0.0
            };
            lo = lo.min(vn);
            hi = hi.max(vn);
            tri_vn.push(vn);
            // Face = intersection of the face sets of the nodes that carry one.
            let mut set: Option<BTreeSet<u32>> = None;
            for k in t {
                if let Some(fs) = node_faces(&seam.recipes[*k as usize]) {
                    set = Some(match set {
                        None => fs,
                        Some(s) => s.intersection(&fs).copied().collect(),
                    });
                }
            }
            let face = match set {
                Some(s) if s.len() == 1 => *s.iter().next().expect("len 1") as i64,
                _ => -1,
            };
            if face >= 0 {
                let e = face_acc.entry(face as u32).or_insert((0.0, 0.0, 0.0));
                e.0 += area * vn;
                e.1 += area;
                e.2 = e.2.max(vn.abs());
            }
            tri_face.push(face);
        }
        let positions: Vec<f32> = seam
            .positions
            .iter()
            .flat_map(|q| [q.x as f32, q.y as f32, q.z as f32])
            .collect();
        let indices: Vec<u32> = seam.triangles.iter().flatten().copied().collect();
        let faces: Vec<Value> = face_acc
            .iter()
            .map(|(f, (wa, a, m))| json!({ "face": f, "meanNormalVelocity": wa / a, "maxAbs": m, "area": a }))
            .collect();
        parts.push(json!({
            "part": i,
            "positions": positions,
            "indices": indices,
            "triNormalVelocity": tri_vn,
            "triFace": tri_face,
            "faces": faces,
            "range": [lo, hi],
        }));
    }
    Ok(
        json!({ "parameter": parameter, "value": theta0, "units": "mm per unit parameter", "parts": parts }),
    )
}
