//! Electrode design by multi-start gradient ascent over a [`DeviceSpec`]'s
//! named parameters — the engine behind the `optimize_electrodes` MCP tool.
//!
//! The objective is the **tracer's** ensemble-mean D-D yield per ion
//! (`fom::EnsembleStats::mean_ddn_sigma_v_m3`): adaptive time step,
//! electrode/wall termination, a pass budget. Every line search, every
//! reported value and the `history` are in that objective, whatever the
//! gradient mode.
//!
//! Gradients are routed per variable by [`DeviceSpec::parameter_roles`]:
//!
//! - [`GradientMode::Hybrid`]: potentials and ampere-turns take the
//!   discrete adjoint ([`crate::adjoint::yield_gradient`], one run for all
//!   of them), geometry takes central differences of the tracer objective.
//!   The adjoint differentiates its own smooth surrogate J (fixed step,
//!   fixed horizon, **no wire termination**), not the tracer objective, so
//!   its block enters as a relative sensitivity, `f · ∂ln J/∂θ`, putting
//!   it on the same scale as the FD block of `f`. Because J is blind to
//!   grid interception it can point the wrong way on shield current; a
//!   failed line search on a hybrid direction retries that iteration on a
//!   pure-FD gradient ([`crate::optimize::maximize_hybrid`]), and the
//!   iteration record says which source each component used.
//! - [`GradientMode::Fd`]: central differences for everything (the
//!   original behavior).
//!
//! Candidates that fail to resolve or converge score 0 instead of aborting
//! the search. No wall-clock timing in here — this runs inside wasm, where
//! `Instant` panics; time it from the caller.

use std::cell::Cell;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::adjoint::{yield_gradient, AdjointConfig};
use crate::field::FieldMap;
use crate::fom;
use crate::optimize::{
    central_difference, maximize_hybrid, FdOptions, GradEstimate, GradSource, Iteration,
};
use crate::poisson::{self, SolveOptions};
use crate::spec::{DeviceSpec, ParamRole};
use crate::trace::{TraceOptions, Tracer, DEUTERON};

/// One optimization variable: a named spec parameter with box bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptVariable {
    /// Parameter name (as used in the spec).
    pub name: String,
    /// Lower bound.
    pub lo: f64,
    /// Upper bound.
    pub hi: f64,
    /// Explicit start for the first seed (all variables must give one).
    #[serde(default)]
    pub start: Option<f64>,
}

/// How gradients are computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradientMode {
    /// Central differences for every variable.
    #[default]
    Fd,
    /// Adjoint for potentials / ampere-turns, FD for geometry, FD fallback
    /// when an adjoint direction fails to climb.
    #[serde(alias = "adjoint")]
    Hybrid,
}

/// The full optimization request (the `optimize_json` of the wasm entry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElectrodeOptProblem {
    /// Variables to optimize.
    pub variables: Vec<OptVariable>,
    /// Radial grid nodes.
    #[serde(default = "d_nr")]
    pub nr: usize,
    /// Axial grid nodes.
    #[serde(default = "d_nz")]
    pub nz: usize,
    /// Tracer ensemble size.
    #[serde(default = "d_particles")]
    pub particles: usize,
    /// Tracer pass budget.
    #[serde(default = "d_passes")]
    pub max_passes: u32,
    /// Ascent iterations per start.
    #[serde(default = "d_iters")]
    pub max_iters: usize,
    /// Three seeds across the box instead of one.
    #[serde(default = "d_multi")]
    pub multi_start: bool,
    /// Gradient routing.
    #[serde(default)]
    pub gradient: GradientMode,
    /// Adjoint ensemble size (hybrid mode).
    #[serde(default = "d_adj_particles")]
    pub adjoint_particles: usize,
    /// Adjoint flight-time budget, in ideal chamber crossings (hybrid mode).
    #[serde(default = "d_adj_crossings")]
    pub adjoint_crossings: f64,
}

fn d_nr() -> usize {
    81
}
fn d_nz() -> usize {
    161
}
fn d_particles() -> usize {
    48
}
fn d_passes() -> u32 {
    20
}
fn d_iters() -> usize {
    8
}
fn d_multi() -> bool {
    true
}
fn d_adj_particles() -> usize {
    AdjointConfig::default().n_particles
}
fn d_adj_crossings() -> f64 {
    AdjointConfig::default().time_budget_crossings
}

impl ElectrodeOptProblem {
    /// A problem over `variables` with every option at its default.
    pub fn new(variables: Vec<OptVariable>) -> Self {
        Self {
            variables,
            nr: d_nr(),
            nz: d_nz(),
            particles: d_particles(),
            max_passes: d_passes(),
            max_iters: d_iters(),
            multi_start: d_multi(),
            gradient: GradientMode::default(),
            adjoint_particles: d_adj_particles(),
            adjoint_crossings: d_adj_crossings(),
        }
    }
}

/// One start of the multi-start search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartReport {
    /// Best parameters this start reached.
    pub params: BTreeMap<String, f64>,
    /// Tracer objective there.
    pub value: f64,
    /// Where it started.
    pub x0: BTreeMap<String, f64>,
    /// Tracer evaluations spent (FD probes + line search).
    pub evals: usize,
    /// Adjoint gradient runs spent.
    pub adjoint_evals: usize,
    /// Objective after each accepted step.
    pub history: Vec<f64>,
    /// Per-iteration objective + gradient source per variable.
    pub iterations: Vec<Iteration>,
}

/// Result of [`optimize_electrodes`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElectrodeOptResult {
    /// Best parameter set over all starts.
    pub best_params: BTreeMap<String, f64>,
    /// Tracer D-D yield per ion there, m³.
    pub best_sigma_v_m3: f64,
    /// Total tracer evaluations over all starts.
    pub evals: usize,
    /// Total adjoint gradient runs over all starts.
    pub adjoint_evals: usize,
    /// Objective after each accepted step of the best start.
    pub history: Vec<f64>,
    /// Iteration records of the best start.
    pub iterations: Vec<Iteration>,
    /// Index into `starts` of the winner.
    pub best_start: usize,
    /// Every start.
    pub starts: Vec<StartReport>,
    /// Gradient mode used.
    pub gradient: GradientMode,
    /// Planned gradient route per variable (iterations record what was
    /// actually used, including FD fallbacks).
    pub variable_gradients: BTreeMap<String, GradSource>,
}

/// Run the multi-start search. Errors only on a malformed request (no
/// variables); unresolvable or unsolvable candidates score 0.
pub fn optimize_electrodes(
    spec: &DeviceSpec,
    base: &BTreeMap<String, f64>,
    problem: &ElectrodeOptProblem,
) -> Result<ElectrodeOptResult, String> {
    if problem.variables.is_empty() {
        return Err("optimize requires at least one variable".into());
    }
    let lo: Vec<f64> = problem.variables.iter().map(|v| v.lo).collect();
    let hi: Vec<f64> = problem.variables.iter().map(|v| v.hi).collect();
    let names: Vec<String> = problem.variables.iter().map(|v| v.name.clone()).collect();
    let roles = spec.parameter_roles();
    let use_adjoint: Vec<bool> = names
        .iter()
        .map(|n| {
            problem.gradient == GradientMode::Hybrid && roles.get(n) == Some(&ParamRole::Adjoint)
        })
        .collect();
    let variable_gradients: BTreeMap<String, GradSource> = names
        .iter()
        .zip(&use_adjoint)
        .map(|(n, a)| {
            let s = if *a {
                GradSource::Adjoint
            } else {
                GradSource::FiniteDifference
            };
            (n.clone(), s)
        })
        .collect();

    let params_at = |x: &[f64]| {
        let mut p = base.clone();
        for (name, value) in names.iter().zip(x) {
            p.insert(name.clone(), *value);
        }
        p
    };
    let sopts = SolveOptions::default();

    let mut objective = |x: &[f64]| -> f64 {
        let Ok(device) = spec.resolve(&params_at(x)) else {
            return 0.0;
        };
        let Ok(sol) = poisson::solve(&device, problem.nr, problem.nz, &sopts) else {
            return 0.0;
        };
        let fields = FieldMap::new(&device, &sol);
        let topts = TraceOptions {
            max_passes: problem.max_passes,
            ..Default::default()
        };
        let tracer = Tracer::new(&device, &fields, &sol, topts);
        let v =
            fom::stats(&tracer.launch_ensemble(DEUTERON, problem.particles)).mean_ddn_sigma_v_m3;
        if v.is_finite() {
            v
        } else {
            0.0
        }
    };

    // Frozen per start: the adjoint's step and horizon scale with the
    // reference drop, so letting it follow a potential variable would make
    // the integration window a hidden, undifferentiated parameter.
    let reference_drop: Cell<Option<f64>> = Cell::new(None);
    let adjoint_runs = Cell::new(0usize);
    let any_adjoint = use_adjoint.iter().any(|a| *a);

    // ∂ ln J / ∂ name for the adjoint variables, or None when unusable.
    let adjoint_log_grad = |x: &[f64]| -> Option<BTreeMap<String, f64>> {
        let device = spec.resolve(&params_at(x)).ok()?;
        adjoint_runs.set(adjoint_runs.get() + 1);
        let cfg = AdjointConfig {
            n_particles: problem.adjoint_particles,
            time_budget_crossings: problem.adjoint_crossings,
            reference_drop_v: reference_drop.get(),
            ..AdjointConfig::default()
        };
        let g = yield_gradient(&device, problem.nr, problem.nz, &sopts, &cfg).ok()?;
        if !(g.value.is_finite() && g.value > 0.0) {
            return None;
        }
        let by_name = spec.adjoint_gradient_by_name(&g);
        if by_name.values().any(|d| !d.is_finite()) {
            return None;
        }
        Some(by_name.into_iter().map(|(k, d)| (k, d / g.value)).collect())
    };

    let mut grad =
        |x: &[f64], fx: f64, force_fd: bool, f: &mut dyn FnMut(&[f64]) -> f64| -> GradEstimate {
            let dim = x.len();
            let mut g = vec![0.0; dim];
            let mut sources = vec![GradSource::FiniteDifference; dim];
            if any_adjoint {
                let adj = if force_fd || fx <= 0.0 {
                    None
                } else {
                    adjoint_log_grad(x)
                };
                for i in 0..dim {
                    if !use_adjoint[i] {
                        continue;
                    }
                    match adj.as_ref() {
                        Some(m) => {
                            g[i] = fx * m.get(&names[i]).copied().unwrap_or(0.0);
                            sources[i] = GradSource::Adjoint;
                        }
                        None => sources[i] = GradSource::FdFallback,
                    }
                }
            }
            let rel = FdOptions::default().rel_step;
            for i in 0..dim {
                if sources[i] != GradSource::Adjoint {
                    g[i] = central_difference(&mut *f, x, i, &lo, &hi, rel);
                }
            }
            GradEstimate { grad: g, sources }
        };

    let seed_fractions: Vec<f64> = if problem.multi_start {
        vec![0.2, 0.5, 0.8]
    } else {
        vec![0.5]
    };
    let explicit: Option<Vec<f64>> = problem.variables.iter().map(|v| v.start).collect();
    let fd = FdOptions {
        max_iters: problem.max_iters,
        ..FdOptions::default()
    };

    let mut starts: Vec<StartReport> = Vec::new();
    for (k, frac) in seed_fractions.iter().enumerate() {
        let x0: Vec<f64> = match (&explicit, k) {
            (Some(e), 0) => e.clone(),
            _ => lo
                .iter()
                .zip(&hi)
                .map(|(a, b)| a + frac * (b - a))
                .collect(),
        };
        reference_drop.set(
            spec.resolve(&params_at(&x0))
                .ok()
                .map(|d| d.max_potential_drop_v()),
        );
        let adj_before = adjoint_runs.get();
        let r = maximize_hybrid(&mut objective, &mut grad, &x0, &lo, &hi, &fd);
        starts.push(StartReport {
            params: names.iter().cloned().zip(r.x.iter().copied()).collect(),
            value: r.value,
            x0: names.iter().cloned().zip(x0.iter().copied()).collect(),
            evals: r.evals,
            adjoint_evals: adjoint_runs.get() - adj_before,
            history: r.history,
            iterations: r.iterations,
        });
    }

    let best_start =
        starts
            .iter()
            .enumerate()
            .fold(0, |b, (i, s)| if s.value > starts[b].value { i } else { b });
    let best = &starts[best_start];
    Ok(ElectrodeOptResult {
        best_params: best.params.clone(),
        best_sigma_v_m3: best.value,
        evals: starts.iter().map(|s| s.evals).sum(),
        adjoint_evals: adjoint_runs.get(),
        history: best.history.clone(),
        iterations: best.iterations.clone(),
        best_start,
        starts,
        gradient: problem.gradient,
        variable_gradients,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{ParamValue, RingSpec};

    fn named(n: &str) -> ParamValue {
        ParamValue::Named(n.into())
    }

    /// Shielded two-ring with a shared bias, a single-ring shield current
    /// and a geometric variable.
    fn spec() -> DeviceSpec {
        let ring = |z: ParamValue, at: ParamValue| RingSpec {
            ring_radius_mm: 40.0.into(),
            z_mm: z,
            wire_radius_mm: 6.0.into(),
            potential_v: named("bias"),
            ampere_turns: at,
        };
        DeviceSpec {
            chamber_radius_mm: 100.0.into(),
            chamber_half_height_mm: 100.0.into(),
            wall_potential_v: 0.0.into(),
            rings: vec![
                ring(named("ring_z"), named("shield")),
                ring(named("ring_z_neg"), (-15_000.0).into()),
            ],
        }
    }

    fn base() -> BTreeMap<String, f64> {
        [
            ("bias", -20_000.0),
            ("shield", 15_000.0),
            ("ring_z", 20.0),
            ("ring_z_neg", -20.0),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect()
    }

    fn small(variables: Vec<OptVariable>, gradient: GradientMode) -> ElectrodeOptProblem {
        ElectrodeOptProblem {
            nr: 41,
            nz: 81,
            particles: 8,
            max_passes: 6,
            max_iters: 3,
            multi_start: false,
            gradient,
            adjoint_particles: 4,
            adjoint_crossings: 0.8,
            ..ElectrodeOptProblem::new(variables)
        }
    }

    fn var(name: &str, lo: f64, hi: f64, start: f64) -> OptVariable {
        OptVariable {
            name: name.into(),
            lo,
            hi,
            start: Some(start),
        }
    }

    /// The name-level chain rule (shared `bias` sums both rings; `shield`
    /// drives ring 0 only) matches central differences of the adjoint's
    /// own J under the validated smooth configuration.
    #[test]
    fn named_adjoint_gradient_matches_fd_of_its_own_objective() {
        let spec = spec();
        let cfg = AdjointConfig {
            n_particles: 6,
            time_budget_crossings: 0.8,
            reference_drop_v: Some(20_000.0),
            ..AdjointConfig::default()
        };
        let sopts = SolveOptions {
            tol: 1e-10,
            ..SolveOptions::default()
        };
        let j = |p: &BTreeMap<String, f64>| {
            let d = spec.resolve(p).unwrap();
            yield_gradient(&d, 61, 121, &sopts, &cfg).unwrap()
        };
        let p0 = base();
        let g = j(&p0);
        assert!(g.value > 0.0);
        let by_name = spec.adjoint_gradient_by_name(&g);
        // Geometry names are FD-only and must not appear.
        assert!(!by_name.contains_key("ring_z"));

        let fd = |name: &str, h: f64| {
            let mut pp = p0.clone();
            *pp.get_mut(name).unwrap() += h;
            let mut pm = p0.clone();
            *pm.get_mut(name).unwrap() -= h;
            (j(&pp).value - j(&pm).value) / (2.0 * h)
        };
        for (name, h, tol) in [("bias", 3.0, 0.03), ("shield", 20.0, 0.02)] {
            let adj = by_name[name];
            let num = fd(name, h);
            let rel = (adj - num).abs() / num.abs().max(1e-300);
            assert!(
                rel < tol,
                "{name}: adjoint {adj:.6e} vs fd {num:.6e} (rel {rel:.3})"
            );
        }
    }

    #[test]
    fn hybrid_routes_by_role_and_never_descends() {
        let spec = spec();
        let problem = small(
            vec![
                var("bias", -30_000.0, -10_000.0, -20_000.0),
                var("ring_z", 10.0, 40.0, 20.0),
            ],
            GradientMode::Hybrid,
        );
        let r = optimize_electrodes(&spec, &base(), &problem).unwrap();
        assert_eq!(r.variable_gradients["bias"], GradSource::Adjoint);
        assert_eq!(r.variable_gradients["ring_z"], GradSource::FiniteDifference);
        assert!(!r.iterations.is_empty());
        for it in &r.iterations {
            // Geometry is never adjoint; bias is adjoint or a labeled
            // FD fallback, never silently FD.
            assert_eq!(it.sources[1], GradSource::FiniteDifference);
            assert_ne!(it.sources[0], GradSource::FiniteDifference);
            assert!(it.value_after >= it.value);
        }
        assert!(r.history.windows(2).all(|w| w[1] > w[0]));
        assert!(r.adjoint_evals >= 1);
        assert!(r.best_sigma_v_m3 > 0.0);
        assert!((-30_000.0..=-10_000.0).contains(&r.best_params["bias"]));
        assert!((10.0..=40.0).contains(&r.best_params["ring_z"]));
        // The base map's other parameters still bind.
        assert_eq!(r.best_sigma_v_m3, r.starts[0].value);
    }

    #[test]
    fn fd_mode_is_all_fd_and_backward_compatible() {
        let spec = spec();
        let problem = small(
            vec![var("shield", 0.0, 30_000.0, 15_000.0)],
            GradientMode::default(),
        );
        let r = optimize_electrodes(&spec, &base(), &problem).unwrap();
        assert_eq!(r.gradient, GradientMode::Fd);
        assert_eq!(r.adjoint_evals, 0);
        assert!(r
            .iterations
            .iter()
            .all(|it| it.sources == vec![GradSource::FiniteDifference]));
        assert!(r.evals >= 3);

        // Old-style JSON (no `gradient`) still parses, as fd.
        let p: ElectrodeOptProblem =
            serde_json::from_str(r#"{"variables":[{"name":"shield","lo":0,"hi":1}]}"#).unwrap();
        assert_eq!(p.gradient, GradientMode::Fd);
        assert_eq!(p.nr, 81);
        let h: ElectrodeOptProblem = serde_json::from_str(
            r#"{"variables":[{"name":"shield","lo":0,"hi":1}],"gradient":"hybrid"}"#,
        )
        .unwrap();
        assert_eq!(h.gradient, GradientMode::Hybrid);
    }

    #[test]
    fn empty_variables_is_an_error() {
        let r = optimize_electrodes(&spec(), &base(), &ElectrodeOptProblem::new(vec![]));
        assert!(r.is_err());
    }
}
