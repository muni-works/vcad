//! L-BFGS over the reverse-mode seam: many-parameter optimization at one
//! pullback per iterate.
//!
//! [`crate::minimize`] is projected gradient descent driven by forward mode
//! ([`crate::objective_gradient`]): `n` seam passes per iterate, and the
//! zig-zag convergence of steepest descent on an anisotropic objective.
//! `minimize_lbfgs` swaps both halves out: gradients come from
//! [`crate::objective_gradient_reverse`] (one pullback + `n` dot products),
//! and the search direction is the two-loop L-BFGS recursion over a short
//! memory of `(s, y)` curvature pairs, so the optimizer learns the
//! objective's scaling instead of paying for it every step.
//!
//! It keeps the discipline the GD loop established:
//!
//! - **Box projection.** Iterates are clamped into `OptimizeOptions::bounds`;
//!   the Armijo test measures sufficient decrease along the *projected*
//!   displacement `trial − θ`, so a step that projection flattens against a
//!   bound is simply not accepted.
//! - **Frozen errors are subgradient signals.** A trial step whose rebuild
//!   trips a topology change / lost correspondence / failed boundary solve
//!   is a failed step: the line search shrinks, exactly as a non-decreasing
//!   objective would (never silently accepted). Errors at an *accepted*
//!   iterate propagate.
//!
//! ## L-BFGS near the seam's subgradients
//!
//! Quasi-Newton curvature memory assumes a smooth objective; the frozen seam
//! is smooth only inside a topology class, with subgradient walls at the
//! edges. Two guards keep a corrupted pair out of the memory:
//!
//! 1. **Positive curvature only.** A pair `(s, y)` is stored only when
//!    `s·y > εₖ` (with `εₖ` scaled by `‖s‖‖y‖`); a non-positive inner
//!    product means the secant carries no usable convexity and would make
//!    the inverse-Hessian estimate indefinite, so the pair is dropped.
//! 2. **Subgradient-straddling pairs are dropped.** If the line search that
//!    produced an accepted step had to shrink *past a frozen error* to get
//!    there, the accepted `(s, y)` straddles a topology wall and its `y`
//!    mixes two different smooth branches. Such a pair is not stored — the
//!    step is still taken, but the curvature it implies is discarded.
//!
//! If the two-loop direction ever fails to be a descent direction, or the
//! line search cannot find any accepted step, the memory is **restarted**
//! (cleared) and the next iterate falls back to projected steepest descent —
//! the always-correct direction — before rebuilding curvature.

//! The L-BFGS machinery (two-loop recursion, projected Armijo search,
//! guarded curvature memory) is `tang_optim::minimize_lbfgs`; this module
//! wires it to the reverse-mode seam and maps its result onto the GD loop's
//! types. A rebuild error at a trial point is the `Err` tang-optim treats
//! as a failed step.

use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_tessellate::TessellationParams;

use tang_optim::{LbfgsOptions, LbfgsStop};

use crate::optimize::{objective_gradient_reverse, MeshObjective};
use crate::{DiffError, IterateRecord, OptimizeOptions, OptimizeResult, ParamSeeding, StopReason};

/// Minimize a [`MeshObjective`] over θ by projected L-BFGS driven by
/// reverse-mode gradients. See the module docs for the box-projection,
/// subgradient, and curvature-memory policies.
///
/// The `build` / `seeding_for` / `params` triple is identical to
/// [`crate::minimize`]'s, so a problem posed for GD ports to L-BFGS by
/// swapping the objective for its [`MeshObjective`] form. `OptimizeOptions`,
/// [`StopReason`], and [`IterateRecord`] are reused unchanged;
/// `initial_step` seeds only the first (steepest-descent) line search —
/// once curvature exists the natural quasi-Newton trial length is 1.
pub fn minimize_lbfgs(
    build: &impl Fn(&[f64]) -> BRepSolid,
    seeding_for: &impl Fn(&BRepSolid, usize) -> ParamSeeding,
    objective: &impl MeshObjective,
    theta0: &[f64],
    params: &TessellationParams,
    options: &OptimizeOptions,
) -> Result<OptimizeResult, DiffError> {
    let opts = LbfgsOptions {
        max_iters: options.max_iters,
        initial_step: options.initial_step,
        min_step: options.min_step,
        grad_tol: options.grad_tol,
        bounds: options.bounds.clone(),
        ..LbfgsOptions::default()
    };
    let f = |theta: &[f64]| objective_gradient_reverse(build, seeding_for, objective, theta, params);
    let r = tang_optim::minimize_lbfgs(f, theta0, &opts)?;
    Ok(OptimizeResult {
        theta: r.x,
        objective: r.value,
        history: r
            .history
            .into_iter()
            .map(|it| IterateRecord {
                theta: it.x,
                objective: it.value,
                gradient: it.gradient,
            })
            .collect(),
        stop: match r.stop {
            LbfgsStop::GradientConverged => StopReason::GradientConverged,
            LbfgsStop::StepConverged => StopReason::StepConverged,
            LbfgsStop::MaxIters => StopReason::MaxIters,
        },
    })
}
