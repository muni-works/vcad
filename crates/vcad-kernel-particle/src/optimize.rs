//! Finite-difference maximization over electrode parameters.
//!
//! M0 stand-in for the discrete adjoint (see the milestone ladder in
//! `docs/particle-optics-m0.md`): central-difference gradient + projected
//! gradient ascent with backtracking. Deterministic, dependency-free, and
//! good enough to close the design loop on a handful of parameters. The
//! adjoint replaces the gradient estimate later without changing callers.
//! [`maximize_hybrid`] is that seam: per-component gradient sources
//! (adjoint where available, FD otherwise) with an FD fallback.

use serde::{Deserialize, Serialize};

/// Options for [`maximize`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FdOptions {
    /// Central-difference step, as a fraction of each parameter's range.
    pub rel_step: f64,
    /// Maximum ascent iterations.
    pub max_iters: usize,
    /// Initial step length, as a fraction of the parameter range.
    pub initial_step: f64,
    /// Stop when the step length falls below this fraction of the range.
    pub min_step: f64,
}

impl Default for FdOptions {
    fn default() -> Self {
        Self {
            rel_step: 1e-2,
            max_iters: 20,
            initial_step: 0.1,
            min_step: 1e-3,
        }
    }
}

/// Result of [`maximize`].
#[derive(Debug, Clone, PartialEq)]
pub struct FdResult {
    /// Best parameters found.
    pub x: Vec<f64>,
    /// Objective at `x`.
    pub value: f64,
    /// Total objective evaluations.
    pub evals: usize,
    /// Objective value after each accepted step (starts with f(x₀)).
    pub history: Vec<f64>,
}

/// Maximize `f` over the box `[lo, hi]` starting at `x0`.
///
/// The objective may be noisy-deterministic (e.g. a traced ensemble with
/// fixed seeds); backtracking only accepts strict improvements.
pub fn maximize(
    f: &mut dyn FnMut(&[f64]) -> f64,
    x0: &[f64],
    lo: &[f64],
    hi: &[f64],
    opts: &FdOptions,
) -> FdResult {
    assert_eq!(x0.len(), lo.len());
    assert_eq!(x0.len(), hi.len());
    let dim = x0.len();
    let range: Vec<f64> = lo
        .iter()
        .zip(hi)
        .map(|(a, b)| (b - a).abs().max(1e-12))
        .collect();
    let clamp = |x: &mut [f64]| {
        for i in 0..dim {
            x[i] = x[i].clamp(lo[i].min(hi[i]), hi[i].max(lo[i]));
        }
    };

    let mut x: Vec<f64> = x0.to_vec();
    clamp(&mut x);
    let mut value = f(&x);
    let mut evals = 1;
    let mut history = vec![value];
    let mut step = opts.initial_step;

    for _ in 0..opts.max_iters {
        // Central-difference gradient.
        let mut grad = vec![0.0; dim];
        let mut gnorm = 0.0;
        for i in 0..dim {
            let h = opts.rel_step * range[i];
            let mut xp = x.clone();
            xp[i] += h;
            clamp(&mut xp);
            let mut xm = x.clone();
            xm[i] -= h;
            clamp(&mut xm);
            let denom = xp[i] - xm[i];
            let g = if denom.abs() < 1e-15 {
                0.0
            } else {
                (f(&xp) - f(&xm)) / denom
            };
            evals += 2;
            grad[i] = g * range[i]; // scale-free direction
            gnorm += grad[i] * grad[i];
        }
        gnorm = gnorm.sqrt();
        // Scale-invariant stop: the gradient (per unit of scaled parameter)
        // is negligible relative to the objective's own magnitude. An
        // absolute epsilon here silently kills objectives that live at
        // 1e-32 (e.g. cross-section integrals in SI units).
        if gnorm <= 1e-12 * value.abs() || gnorm == 0.0 {
            break;
        }

        // Backtracking line search along the normalized gradient.
        let mut improved = false;
        let mut s = step;
        for _ in 0..8 {
            let mut xt = x.clone();
            for i in 0..dim {
                xt[i] += s * range[i] * grad[i] / gnorm;
            }
            clamp(&mut xt);
            let vt = f(&xt);
            evals += 1;
            if vt > value {
                x = xt;
                value = vt;
                history.push(value);
                step = s * 1.5;
                improved = true;
                break;
            }
            s *= 0.5;
        }
        if !improved {
            step *= 0.25;
            if step < opts.min_step {
                break;
            }
        }
    }

    FdResult {
        x,
        value,
        evals,
        history,
    }
}

/// An objective that returns its value and gradient together.
pub type ValueAndGradFn = dyn FnMut(&[f64]) -> (f64, Vec<f64>);

/// Maximize using a caller-supplied gradient (e.g. the discrete adjoint
/// from [`crate::adjoint::yield_gradient`]): projected ascent with the
/// same backtracking line search as [`maximize`], but no FD probes —
/// one gradient evaluation replaces `2·dim` objective calls per step.
pub fn maximize_with_gradient(
    fg: &mut ValueAndGradFn,
    x0: &[f64],
    lo: &[f64],
    hi: &[f64],
    opts: &FdOptions,
) -> FdResult {
    assert_eq!(x0.len(), lo.len());
    assert_eq!(x0.len(), hi.len());
    let dim = x0.len();
    let range: Vec<f64> = lo
        .iter()
        .zip(hi)
        .map(|(a, b)| (b - a).abs().max(1e-12))
        .collect();
    let clamp = |x: &mut [f64]| {
        for i in 0..dim {
            x[i] = x[i].clamp(lo[i].min(hi[i]), hi[i].max(lo[i]));
        }
    };

    let mut x: Vec<f64> = x0.to_vec();
    clamp(&mut x);
    let (mut value, mut grad) = fg(&x);
    let mut evals = 1;
    let mut history = vec![value];
    let mut step = opts.initial_step;

    for _ in 0..opts.max_iters {
        let scaled: Vec<f64> = grad.iter().zip(&range).map(|(g, r)| g * r).collect();
        let gnorm = scaled.iter().map(|g| g * g).sum::<f64>().sqrt();
        if gnorm <= 1e-12 * value.abs() || gnorm == 0.0 {
            break;
        }
        let mut improved = false;
        let mut s = step;
        for _ in 0..8 {
            let mut xt = x.clone();
            for i in 0..dim {
                xt[i] += s * range[i] * scaled[i] / gnorm;
            }
            clamp(&mut xt);
            let (vt, gt) = fg(&xt);
            evals += 1;
            if vt > value {
                x = xt;
                value = vt;
                grad = gt;
                history.push(value);
                step = s * 1.5;
                improved = true;
                break;
            }
            s *= 0.5;
        }
        if !improved {
            step *= 0.25;
            if step < opts.min_step {
                break;
            }
        }
    }

    FdResult {
        x,
        value,
        evals,
        history,
    }
}

/// Where one gradient component came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradSource {
    /// The discrete adjoint ([`crate::adjoint::yield_gradient`]).
    Adjoint,
    /// Central differences of the objective.
    FiniteDifference,
    /// The adjoint was requested but unusable at this point (solve
    /// failure, zero or non-finite surrogate yield), so this component
    /// fell back to central differences.
    FdFallback,
}

/// A gradient estimate at one point, with per-component provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct GradEstimate {
    /// d objective / d x, in the caller's (unscaled) units.
    pub grad: Vec<f64>,
    /// Source of each component.
    pub sources: Vec<GradSource>,
}

/// One ascent iteration of [`maximize_hybrid`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Iteration {
    /// 0-based iteration index.
    pub iter: usize,
    /// Objective at the iterate the gradient was taken at.
    pub value: f64,
    /// Objective after this iteration's line search (equal to `value`
    /// when no step was accepted).
    pub value_after: f64,
    /// ‖∇f ⊙ range‖ — the scale-free gradient norm the step follows.
    pub scaled_grad_norm: f64,
    /// Gradient source per variable.
    pub sources: Vec<GradSource>,
    /// Whether the line search accepted a step.
    pub accepted: bool,
    /// Cumulative objective evaluations at the end of the iteration
    /// (gradient probes + line-search trials; adjoint runs excluded).
    pub evals: usize,
}

/// Result of [`maximize_hybrid`].
#[derive(Debug, Clone, PartialEq)]
pub struct HybridResult {
    /// Best parameters found.
    pub x: Vec<f64>,
    /// Objective at `x`.
    pub value: f64,
    /// Total objective evaluations (gradient probes + line search).
    pub evals: usize,
    /// Objective value after each accepted step (starts with f(x₀)).
    pub history: Vec<f64>,
    /// Per-iteration record.
    pub iterations: Vec<Iteration>,
}

/// Central difference of `f` along coordinate `i` at `x`, with step
/// `rel_step · (hi − lo)`, clamped to the box (the same probe
/// [`maximize`] uses). Costs two evaluations of `f`.
pub fn central_difference(
    f: &mut dyn FnMut(&[f64]) -> f64,
    x: &[f64],
    i: usize,
    lo: &[f64],
    hi: &[f64],
    rel_step: f64,
) -> f64 {
    let (a, b) = (lo[i].min(hi[i]), hi[i].max(lo[i]));
    let h = rel_step * (b - a).max(1e-12);
    let mut xp = x.to_vec();
    xp[i] = (xp[i] + h).clamp(a, b);
    let mut xm = x.to_vec();
    xm[i] = (xm[i] - h).clamp(a, b);
    let denom = xp[i] - xm[i];
    if denom.abs() < 1e-15 {
        return 0.0;
    }
    (f(&xp) - f(&xm)) / denom
}

/// Gradient hook for [`maximize_hybrid`]: `(x, f(x), force_fd, f)`, where
/// `f` is the objective for any FD probes the hook needs.
pub type GradientHook<'a> =
    dyn FnMut(&[f64], f64, bool, &mut dyn FnMut(&[f64]) -> f64) -> GradEstimate + 'a;

/// Projected gradient ascent where the gradient comes from a caller hook
/// that may mix sources per component (adjoint for some variables, finite
/// differences for others).
///
/// Unlike [`maximize_with_gradient`], the line search evaluates only the
/// objective `f`; `grad` runs once per iterate. The hook receives the
/// point, `f` there, a `force_fd` flag, and `f` itself for FD probes
/// (which count toward `evals`).
///
/// **Fallback:** an adjoint gradient may differentiate a smooth surrogate
/// of `f` rather than `f` itself (the particle adjoint has no wire
/// termination, so it is blind to grid interception). When a line search
/// along a direction with any non-FD component finds no improvement, the
/// gradient is re-taken with `force_fd = true` at the same point and the
/// line search retried once, before the step shrinks. So the hybrid never
/// stalls where pure FD would still climb.
pub fn maximize_hybrid(
    f: &mut dyn FnMut(&[f64]) -> f64,
    grad: &mut GradientHook<'_>,
    x0: &[f64],
    lo: &[f64],
    hi: &[f64],
    opts: &FdOptions,
) -> HybridResult {
    assert_eq!(x0.len(), lo.len());
    assert_eq!(x0.len(), hi.len());
    let dim = x0.len();
    let range: Vec<f64> = lo
        .iter()
        .zip(hi)
        .map(|(a, b)| (b - a).abs().max(1e-12))
        .collect();
    let clamp = |x: &mut [f64]| {
        for i in 0..dim {
            x[i] = x[i].clamp(lo[i].min(hi[i]), hi[i].max(lo[i]));
        }
    };

    let evals = std::cell::Cell::new(0usize);
    let mut counted = |x: &[f64]| {
        evals.set(evals.get() + 1);
        f(x)
    };

    let mut x: Vec<f64> = x0.to_vec();
    clamp(&mut x);
    let mut value = counted(&x);
    let mut history = vec![value];
    let mut iterations = Vec::new();
    let mut step = opts.initial_step;
    // Gradient at the current point, kept across a rejected line search
    // (the point hasn't moved). `true` = it is already all-FD.
    let mut cached: Option<(GradEstimate, bool)> = None;

    'outer: for iter in 0..opts.max_iters {
        let (mut est, mut is_fd) = match cached.take() {
            Some(c) => c,
            None => {
                let e = grad(&x, value, false, &mut counted);
                let fd = e.sources.iter().all(|s| *s != GradSource::Adjoint);
                (e, fd)
            }
        };
        let value_before = value;
        loop {
            let scaled: Vec<f64> = est.grad.iter().zip(&range).map(|(g, r)| g * r).collect();
            let gnorm = scaled.iter().map(|g| g * g).sum::<f64>().sqrt();
            let flat = !gnorm.is_finite() || gnorm <= 1e-12 * value.abs() || gnorm == 0.0;
            let mut improved = false;
            if !flat {
                let mut s = step;
                for _ in 0..8 {
                    let mut xt = x.clone();
                    for i in 0..dim {
                        xt[i] += s * range[i] * scaled[i] / gnorm;
                    }
                    clamp(&mut xt);
                    let vt = counted(&xt);
                    if vt > value {
                        x = xt;
                        value = vt;
                        history.push(value);
                        step = s * 1.5;
                        improved = true;
                        break;
                    }
                    s *= 0.5;
                }
            }
            if !improved && !is_fd {
                // The adjoint direction failed (or was flat): retry this
                // iteration on a pure-FD gradient at the same point.
                est = grad(&x, value, true, &mut counted);
                is_fd = true;
                continue;
            }
            iterations.push(Iteration {
                iter,
                value: value_before,
                value_after: value,
                scaled_grad_norm: gnorm,
                sources: est.sources.clone(),
                accepted: improved,
                evals: evals.get(),
            });
            if flat {
                break 'outer;
            }
            if !improved {
                cached = Some((est, is_fd));
                step *= 0.25;
                if step < opts.min_step {
                    break 'outer;
                }
            }
            break;
        }
    }

    HybridResult {
        x,
        value,
        evals: evals.get(),
        history,
        iterations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_driven_ascent_climbs() {
        // Same bowl as the FD test, but with supplied gradients.
        let mut fg = |x: &[f64]| {
            let v = -(x[0] - 2.0).powi(2) - 3.0 * (x[1] + 1.0).powi(2);
            let g = vec![-2.0 * (x[0] - 2.0), -6.0 * (x[1] + 1.0)];
            (v, g)
        };
        let r = maximize_with_gradient(
            &mut fg,
            &[-4.0, 4.0],
            &[-5.0, -5.0],
            &[5.0, 5.0],
            &FdOptions {
                max_iters: 60,
                ..FdOptions::default()
            },
        );
        assert!((r.x[0] - 2.0).abs() < 0.05, "x = {:?}", r.x);
        assert!((r.x[1] + 1.0).abs() < 0.05, "x = {:?}", r.x);
        // Far fewer evals than the FD path needs for the same bowl
        // (FD at 2 params spends ~5 evals per accepted step; this spends
        // 1 + line-search retries).
        assert!(r.evals < 200, "evals = {}", r.evals);
    }

    #[test]
    fn hybrid_falls_back_to_fd_when_the_adjoint_lies() {
        // f = -(x-2)², but the "adjoint" hook reports the wrong sign —
        // the surrogate-objective failure mode. The fallback must still
        // climb, and label the components it re-took.
        let mut f = |x: &[f64]| -(x[0] - 2.0).powi(2);
        let mut grad = |x: &[f64], _fx: f64, force_fd: bool, f: &mut dyn FnMut(&[f64]) -> f64| {
            if force_fd {
                GradEstimate {
                    grad: vec![central_difference(f, x, 0, &[-5.0], &[5.0], 1e-2)],
                    sources: vec![GradSource::FdFallback],
                }
            } else {
                GradEstimate {
                    grad: vec![2.0 * (x[0] - 2.0)],
                    sources: vec![GradSource::Adjoint],
                }
            }
        };
        let r = maximize_hybrid(
            &mut f,
            &mut grad,
            &[-3.0],
            &[-5.0],
            &[5.0],
            &FdOptions {
                max_iters: 40,
                ..FdOptions::default()
            },
        );
        assert!((r.x[0] - 2.0).abs() < 0.05, "x = {:?}", r.x);
        assert!(r.history.windows(2).all(|w| w[1] > w[0]));
        assert!(r
            .iterations
            .iter()
            .filter(|it| it.accepted)
            .all(|it| it.sources == vec![GradSource::FdFallback]));
        // Cumulative eval counter matches the result.
        assert_eq!(r.iterations.last().unwrap().evals, r.evals);
    }

    #[test]
    fn hybrid_with_a_true_gradient_needs_no_fd() {
        let mut f = |x: &[f64]| -(x[0] - 2.0).powi(2) - 3.0 * (x[1] + 1.0).powi(2);
        let mut grad = |x: &[f64], _fx: f64, _force_fd: bool, _f: &mut dyn FnMut(&[f64]) -> f64| {
            GradEstimate {
                grad: vec![-2.0 * (x[0] - 2.0), -6.0 * (x[1] + 1.0)],
                sources: vec![GradSource::Adjoint; 2],
            }
        };
        let r = maximize_hybrid(
            &mut f,
            &mut grad,
            &[-4.0, 4.0],
            &[-5.0, -5.0],
            &[5.0, 5.0],
            &FdOptions {
                max_iters: 60,
                ..FdOptions::default()
            },
        );
        assert!((r.x[0] - 2.0).abs() < 0.05 && (r.x[1] + 1.0).abs() < 0.05);
        assert!(r.history.windows(2).all(|w| w[1] > w[0]));
        // A true gradient climbs without FD: every accepted step rode it.
        assert!(r
            .iterations
            .iter()
            .filter(|it| it.accepted)
            .all(|it| it.sources == vec![GradSource::Adjoint; 2]));
    }

    #[test]
    fn climbs_a_smooth_bowl() {
        // f(x, y) = -(x-2)² - 3(y+1)², max at (2, -1) inside the box.
        let mut f = |x: &[f64]| -(x[0] - 2.0).powi(2) - 3.0 * (x[1] + 1.0).powi(2);
        let r = maximize(
            &mut f,
            &[-4.0, 4.0],
            &[-5.0, -5.0],
            &[5.0, 5.0],
            &FdOptions {
                max_iters: 60,
                ..FdOptions::default()
            },
        );
        assert!((r.x[0] - 2.0).abs() < 0.05, "x = {:?}", r.x);
        assert!((r.x[1] + 1.0).abs() < 0.05, "x = {:?}", r.x);
        assert!(r.value > -0.01);
        assert!(r.history.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn survives_astronomically_small_objectives() {
        // Cross-section-integral scale: values around 1e-30. The optimizer
        // must not mistake "tiny units" for "converged".
        let mut f = |x: &[f64]| 1.0e-30 * (4.0 - (x[0] - 2.0).powi(2));
        let r = maximize(
            &mut f,
            &[-3.0],
            &[-5.0],
            &[5.0],
            &FdOptions {
                max_iters: 60,
                ..FdOptions::default()
            },
        );
        assert!((r.x[0] - 2.0).abs() < 0.05, "x = {:?}", r.x);
        assert!(
            r.evals > 10,
            "stopped suspiciously early: {} evals",
            r.evals
        );
    }

    #[test]
    fn respects_bounds() {
        // Max of x+y is at the box corner.
        let mut f = |x: &[f64]| x[0] + x[1];
        let r = maximize(
            &mut f,
            &[0.0, 0.0],
            &[-1.0, -1.0],
            &[1.0, 2.0],
            &FdOptions::default(),
        );
        assert!(r.x[0] <= 1.0 + 1e-12 && r.x[1] <= 2.0 + 1e-12);
        assert!(r.value > 2.5, "should approach the corner: {}", r.value);
    }
}
