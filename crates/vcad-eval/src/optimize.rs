//! Document-level optimization: minimize a weighted sum of quantities of
//! interest over named document parameters, with every gradient component
//! labeled by the route that produced it.
//!
//! [`crate::diff`] prices `d(mass properties)/dθ` for one parameter at a
//! time. This module turns that into an optimizer over a whole `.vcad`
//! document:
//!
//! - **Parameters** are named document parameters with a box `[lo, hi]`.
//! - **Objective** is `Σ wᵢ · shape(qᵢ)`, where each `qᵢ` is a QoI and the
//!   shape is linear, a squared deviation from a target, or a one-sided
//!   squared hinge (`max(0, t − q)²` / `max(0, q − t)²`).
//! - **QoIs** come from two places. Mass properties (volume, mass,
//!   centroid) are built in and differentiated by the seam — exact
//!   frozen-plan derivatives, provenance [`Provenance::Seam`]. Anything else
//!   is a [`ScalarQoi`] plug-in: a closure over the evaluated scene (a modal
//!   frequency, a cavity resonance, a peak temperature). A plug-in that can
//!   price its own gradient returns it ([`Provenance::Adjoint`]); otherwise
//!   the optimizer differences it centrally over the named parameters,
//!   re-evaluating the document at each probe ([`Provenance::Fd`]).
//! - **Constraints** are either parameter expressions in the document's own
//!   formula language (`"port_len <= body_h - 2*wall - 15"`) or bounds on a
//!   QoI (`modal_f1 >= 400`). Parameter-expression gradients are computed
//!   from the expression alone, without touching geometry, and are labeled
//!   [`Provenance::Linear`] when the expression is linear in the parameters.
//!
//! # Algorithm
//!
//! Augmented Lagrangian (Powell–Hestenes–Rockafellar) for the inequality
//! constraints, with each subproblem solved by `tang_optim`'s box-projected
//! L-BFGS in normalized coordinates `x = (θ − lo)/(hi − lo) ∈ [0, 1]ⁿ`. The
//! final multipliers are the subproblem's exact KKT multipliers, so the run
//! ends with a first-order optimality report ([`KktReport`]): feasibility,
//! projected stationarity of the Lagrangian, and complementarity.
//!
//! # The trajectory
//!
//! Every accepted iterate is recorded with its parameters, objective, each
//! term's value and contribution, each constraint's value and multiplier,
//! and the gradient of every QoI broken out per parameter with its
//! provenance — enough to replay the run on camera without re-solving.
//!
//! # Cost
//!
//! One iterate costs one base document evaluation, plus `2·n` probe
//! evaluations for seam surface matching (only when a seam QoI is used),
//! plus `2·n` FD probe evaluations (only when an FD QoI is used). Seam
//! evaluations share an [`IncrementalCache`] (nodes that don't depend on
//! the probed parameter are reused); FD probes, which need only meshes, go
//! through a content-addressed root-mesh cache so a revisited parameter
//! vector is free. Every sample is memoized by its exact parameter vector,
//! so line-search re-visits and outer-loop restarts cost nothing. Each
//! trajectory step carries its wall time and evaluation counts.
//!
//! **Honesty.** A seam derivative is the derivative of the frozen
//! tessellation (see [`crate::diff`]); an FD derivative is only as smooth as
//! the solver behind it. Both are labeled; neither is passed off as the
//! other. A parameter vector that crosses a topology boundary inside a seam
//! probe is a failed evaluation (the line search shrinks), never a fabricated
//! gradient.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use vcad_ir::{Document, Expr, Parameter};
use vcad_kernel::vcad_kernel_primitives::BRepSolid;
use vcad_kernel_diff::{
    evaluate_with_sensitivity, mass_properties, mass_properties_with_derivative,
    synthesize_seeding, DiffError, MassProperties, ParamSeeding,
};
use vcad_kernel_tessellate::frozen::capture_plan;
use vcad_kernel_tessellate::TessellationParams;
use web_time::Instant;

use crate::cache::MemoryMeshCache;
use crate::evaluate::evaluate_document_incremental;
use crate::incremental::IncrementalCache;
use crate::{EvalOptions, EvaluatedScene};

// ─────────────────────────────────────────────────────────────────────────────
// Public request types
// ─────────────────────────────────────────────────────────────────────────────

/// One optimized parameter: a named document parameter and its box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Name in `doc.parameters`.
    pub name: String,
    /// Lower bound.
    pub lo: f64,
    /// Upper bound.
    pub hi: f64,
}

/// A reference to a quantity of interest: its name and, for per-part
/// quantities, which solid part (in evaluated solid-part order). Built-in
/// names: `volume`, `mass` (summed over solid parts when `part` is absent),
/// `centroid_x`, `centroid_y`, `centroid_z` (need `part` on a multi-part
/// document). Any other name is looked up in the plug-in registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QoiRef {
    /// QoI name.
    pub qoi: String,
    /// Solid-part index, when the QoI is per part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<usize>,
}

impl QoiRef {
    /// A whole-document QoI.
    pub fn new(qoi: impl Into<String>) -> Self {
        Self {
            qoi: qoi.into(),
            part: None,
        }
    }

    /// A per-part QoI.
    pub fn part(qoi: impl Into<String>, part: usize) -> Self {
        Self {
            qoi: qoi.into(),
            part: Some(part),
        }
    }

    /// Display label, e.g. `mass[0]`.
    pub fn label(&self) -> String {
        match self.part {
            Some(p) => format!("{}[{p}]", self.qoi),
            None => self.qoi.clone(),
        }
    }
}

/// How a QoI enters the objective.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermShape {
    /// `q`.
    Linear,
    /// `(q − target)²`.
    Target(f64),
    /// `max(0, threshold − q)²` — penalize falling below.
    HingeBelow(f64),
    /// `max(0, q − threshold)²` — penalize rising above.
    HingeAbove(f64),
}

impl TermShape {
    /// `(shape(q), d shape / dq)`.
    fn apply(self, q: f64) -> (f64, f64) {
        match self {
            TermShape::Linear => (q, 1.0),
            TermShape::Target(t) => ((q - t).powi(2), 2.0 * (q - t)),
            TermShape::HingeBelow(t) => {
                let v = (t - q).max(0.0);
                (v * v, -2.0 * v)
            }
            TermShape::HingeAbove(t) => {
                let v = (q - t).max(0.0);
                (v * v, 2.0 * v)
            }
        }
    }
}

/// One objective term: `weight · shape(qoi)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectiveTerm {
    /// The quantity.
    #[serde(flatten)]
    pub qoi: QoiRef,
    /// Weight (negative to maximize a linear term).
    #[serde(default = "one")]
    pub weight: f64,
    /// How the quantity enters.
    #[serde(default = "linear")]
    pub shape: TermShape,
}

fn one() -> f64 {
    1.0
}
fn linear() -> TermShape {
    TermShape::Linear
}

/// Comparison sense of a constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintOp {
    /// `≤`
    #[serde(rename = "<=")]
    Le,
    /// `≥`
    #[serde(rename = ">=")]
    Ge,
}

/// An inequality constraint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Constraint {
    /// `lhs <= rhs` / `lhs >= rhs` over parameter expressions, in the
    /// document's formula language. Other (non-optimized) parameters may
    /// appear and are held at their document values.
    Expr {
        /// e.g. `"port_len <= body_h - 2*wall - 15"`.
        expr: String,
        /// Normalization scale for the violation (default 1, i.e. the
        /// expression's own units).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scale: Option<f64>,
    },
    /// `qoi op value`.
    Qoi {
        /// The quantity.
        #[serde(flatten)]
        qoi: QoiRef,
        /// Comparison.
        op: ConstraintOp,
        /// Bound.
        value: f64,
    },
}

impl Constraint {
    /// Display label.
    pub fn label(&self) -> String {
        match self {
            Constraint::Expr { expr, .. } => expr.clone(),
            Constraint::Qoi { qoi, op, value } => format!(
                "{} {} {value}",
                qoi.label(),
                match op {
                    ConstraintOp::Le => "<=",
                    ConstraintOp::Ge => ">=",
                }
            ),
        }
    }
}

/// Optimizer options. Every field has a default.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OptimizeDocOptions {
    /// Density fed to the mass integrals (geometry is mm; pass `kg/m³·1e-9`
    /// for kg, or `1.0` for raw volume).
    pub density: f64,
    /// Augmented-Lagrangian outer rounds.
    pub max_outer: usize,
    /// L-BFGS iterations per round.
    pub max_inner: usize,
    /// Constraint-violation tolerance (normalized) for convergence.
    pub feas_tol: f64,
    /// Projected-gradient ∞-norm tolerance (normalized coordinates, scaled
    /// objective) for L-BFGS and the KKT report.
    pub grad_tol: f64,
    /// Initial penalty.
    pub mu0: f64,
    /// Seam surface-matching probe step (absolute, parameter units). Not an
    /// FD step: seam derivatives are analytic.
    pub seam_step: f64,
    /// FD probe step for plug-in QoIs, as a fraction of each parameter's
    /// range (a plug-in may override with [`ScalarQoi::fd_step`]).
    pub fd_rel_step: f64,
    /// Circle segments of the frozen tessellation the seam differentiates.
    pub circle_segments: u32,
    /// First L-BFGS trial step (normalized coordinates).
    pub initial_step: f64,
    /// After the run, central-difference every seam QoI at the optimum and
    /// report the agreement ([`SeamFdCheck`]).
    pub check_seam_fd: bool,
}

impl Default for OptimizeDocOptions {
    fn default() -> Self {
        Self {
            density: 1.0,
            max_outer: 8,
            max_inner: 25,
            feas_tol: 1e-3,
            grad_tol: 1e-4,
            mu0: 10.0,
            seam_step: 1e-4,
            fd_rel_step: 1e-2,
            circle_segments: 48,
            initial_step: 0.1,
            check_seam_fd: false,
        }
    }
}

/// A full optimization request (the JSON surface shared by MCP / WASM / FFI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizeDocRequest {
    /// Parameters and boxes.
    pub parameters: Vec<ParamSpec>,
    /// Objective terms (summed).
    pub objective: Vec<ObjectiveTerm>,
    /// Inequality constraints.
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    /// Options.
    #[serde(default)]
    pub options: OptimizeDocOptions,
}

// ─────────────────────────────────────────────────────────────────────────────
// Plug-in QoIs
// ─────────────────────────────────────────────────────────────────────────────

/// What a [`ScalarQoi`] sees at one parameter vector.
pub struct QoiInput<'a> {
    /// The document with the parameter vector applied.
    pub doc: &'a Document,
    /// Its evaluated scene. On FD probes a part may carry `solid: None`
    /// (a root-mesh cache hit) unless [`ScalarQoi::needs_brep`] is true.
    pub scene: &'a EvaluatedScene,
    /// The optimized parameters, in order.
    pub params: &'a [ParamSpec],
    /// Their current values.
    pub theta: &'a [f64],
}

/// A scalar solver output the optimizer can read. Register one per solver
/// coupling (modal, acoustics, thermal, …); the optimizer differences it
/// over the named parameters unless it prices its own gradient.
pub trait ScalarQoi {
    /// Name used in requests (`"modal_f1"`).
    fn name(&self) -> &str;

    /// Value at `input` for `part` (the request's part index, if any; the
    /// plug-in decides what it indexes).
    fn evaluate(&self, input: &QoiInput, part: Option<usize>) -> Result<f64, String>;

    /// `d value / dθ` from the solver itself (an adjoint), if it has one.
    /// `None` means "difference me". The default is `None`.
    fn gradient(
        &self,
        _input: &QoiInput,
        _part: Option<usize>,
    ) -> Option<Result<Vec<f64>, String>> {
        None
    }

    /// FD step for parameter `k`, overriding the request's relative step.
    fn fd_step(&self, _param: &ParamSpec) -> Option<f64> {
        None
    }

    /// Whether [`evaluate`](Self::evaluate) needs BRep solids (disables the
    /// root-mesh cache for this QoI's probes). Default `false`.
    fn needs_brep(&self) -> bool {
        false
    }
}

/// Lookup of plug-in QoIs by name.
#[derive(Default)]
pub struct QoiRegistry {
    entries: Vec<Box<dyn ScalarQoi>>,
}

impl QoiRegistry {
    /// An empty registry (built-in seam QoIs only).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plug-in. A later registration of the same name wins.
    pub fn register(&mut self, q: Box<dyn ScalarQoi>) -> &mut Self {
        self.entries.retain(|e| e.name() != q.name());
        self.entries.push(q);
        self
    }

    /// Builder form of [`register`](Self::register).
    pub fn with(mut self, q: Box<dyn ScalarQoi>) -> Self {
        self.register(q);
        self
    }

    fn get(&self, name: &str) -> Option<&dyn ScalarQoi> {
        self.entries
            .iter()
            .find(|e| e.name() == name)
            .map(|b| b.as_ref())
    }

    /// Registered names.
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name().to_string()).collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Provenance + trajectory
// ─────────────────────────────────────────────────────────────────────────────

/// Finite-difference scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FdScheme {
    /// `(f(θ+h) − f(θ−h)) / 2h`.
    Central,
    /// `(f(θ+h) − f(θ)) / h` (θ at its lower bound).
    Forward,
    /// `(f(θ) − f(θ−h)) / h` (θ at its upper bound).
    Backward,
}

/// The route that produced one gradient component.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum Provenance {
    /// Exact frozen-plan seam derivative.
    Seam,
    /// Finite difference of a re-evaluated document.
    Fd {
        /// Step, parameter units.
        step: f64,
        /// Scheme.
        scheme: FdScheme,
    },
    /// The solver's own adjoint.
    Adjoint,
    /// A parameter expression linear in the parameters (exact, no geometry).
    Linear,
}

impl Provenance {
    /// Short on-screen tag: `seam`, `fd(0.4)`, `adjoint`, `linear`.
    pub fn tag(&self) -> String {
        match self {
            Provenance::Seam => "seam".into(),
            Provenance::Fd { step, .. } => format!("fd({step:.3e})"),
            Provenance::Adjoint => "adjoint".into(),
            Provenance::Linear => "linear".into(),
        }
    }
}

/// One component `∂q/∂θ_k`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradComponent {
    /// Parameter name.
    pub param: String,
    /// Derivative, quantity units per parameter unit.
    pub value: f64,
    /// Route.
    pub provenance: Provenance,
}

/// A QoI's value and per-parameter gradient at one iterate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QoiSample {
    /// Which QoI.
    #[serde(flatten)]
    pub qoi: QoiRef,
    /// Value.
    pub value: f64,
    /// `∂q/∂θ`, one component per parameter.
    pub gradient: Vec<GradComponent>,
}

/// One objective term at one iterate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermSample {
    /// Label, e.g. `mass[0]`.
    pub label: String,
    /// The QoI value `q`.
    pub qoi_value: f64,
    /// `weight · shape(q)`.
    pub contribution: f64,
    /// `∂ contribution / ∂θ`, one component per parameter, carrying the
    /// QoI's provenance.
    pub gradient: Vec<GradComponent>,
}

/// One constraint at one iterate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintSample {
    /// Label.
    pub label: String,
    /// `g ≤ 0` form, normalized (positive = violated).
    pub g: f64,
    /// Multiplier estimate at this iterate's round.
    pub multiplier: f64,
    /// `∂g/∂θ` (normalized), one component per parameter.
    pub gradient: Vec<GradComponent>,
}

/// Evaluation cost of one sample.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvalCost {
    /// Wall time of the whole sample (all evaluations + seam + solvers), ms.
    pub wall_ms: f64,
    /// Wall time of the base document evaluation, ms.
    pub base_eval_ms: f64,
    /// Document evaluations performed (base + probes).
    pub document_evals: usize,
    /// Node solids reused by the incremental cache across this sample.
    pub reused_solids: usize,
}

/// One accepted iterate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStep {
    /// Global accepted-iterate index (0 = start point).
    pub iter: usize,
    /// Augmented-Lagrangian round.
    pub round: usize,
    /// Parameter values, in request order.
    pub theta: Vec<f64>,
    /// Objective `Σ wᵢ·shape(qᵢ)` (unscaled).
    pub objective: f64,
    /// Augmented-Lagrangian merit (scaled) the inner solver minimized.
    pub merit: f64,
    /// Largest normalized constraint violation (0 = feasible).
    pub max_violation: f64,
    /// `∂ objective / ∂θ`, summed over terms. A component's provenance is
    /// the weakest route among the terms that contribute to it
    /// (fd < adjoint < seam/linear).
    pub objective_gradient: Vec<GradComponent>,
    /// Per-term breakdown.
    pub terms: Vec<TermSample>,
    /// Per-constraint breakdown.
    pub constraints: Vec<ConstraintSample>,
    /// Every QoI read at this iterate.
    pub qois: Vec<QoiSample>,
    /// What this iterate cost.
    pub cost: EvalCost,
}

/// Why the run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizeStop {
    /// Feasible and first-order stationary.
    Converged,
    /// Outer-round budget exhausted.
    MaxRounds,
    /// The inner solver could not make progress (a wall of failed
    /// evaluations, or a minimum it could not certify).
    Stalled,
}

/// First-order optimality at the final iterate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KktReport {
    /// Largest normalized violation.
    pub max_violation: f64,
    /// ∞-norm of the box-projected Lagrangian gradient in normalized
    /// coordinates, objective scaled by its start value.
    pub stationarity: f64,
    /// `max |λⱼ · gⱼ|`.
    pub complementarity: f64,
    /// Final multipliers, one per constraint.
    pub multipliers: Vec<f64>,
    /// Parameters at a bound: `(name, "lo" | "hi")`.
    pub active_bounds: Vec<(String, String)>,
    /// Feasible within `feas_tol`.
    pub feasible: bool,
    /// Feasible, stationary within `kkt_tol`, complementary within it.
    pub satisfied: bool,
    /// Tolerance used for stationarity and complementarity.
    pub kkt_tol: f64,
}

/// Seam-vs-FD agreement for one (QoI, parameter) at the optimum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeamFdCheck {
    /// QoI label.
    pub qoi: String,
    /// Parameter.
    pub param: String,
    /// Seam derivative.
    pub seam: f64,
    /// Central FD of the frozen tessellation (re-evaluated document).
    pub fd: f64,
    /// FD step.
    pub step: f64,
    /// `|seam − fd| / max(|seam|, |fd|)`.
    pub rel_error: f64,
}

/// Result of [`optimize_document`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizeDocResult {
    /// Parameter names, in order.
    pub params: Vec<String>,
    /// Final parameter values.
    pub theta: Vec<f64>,
    /// Final objective.
    pub objective: f64,
    /// Every accepted iterate, start point first.
    pub trajectory: Vec<TrajectoryStep>,
    /// Why it stopped.
    pub stop: OptimizeStop,
    /// First-order optimality at the end.
    pub kkt: KktReport,
    /// Seam-vs-FD checks at the optimum (when requested).
    pub seam_fd_check: Vec<SeamFdCheck>,
    /// Total distinct parameter vectors sampled (incl. line-search trials).
    pub samples: usize,
    /// Total document evaluations.
    pub document_evals: usize,
    /// Total wall time, ms.
    pub wall_ms: f64,
}

/// Errors from [`optimize_document`].
#[derive(Debug, thiserror::Error)]
pub enum OptimizeDocError {
    /// The request is malformed.
    #[error("bad request: {0}")]
    Request(String),
    /// The start point could not be evaluated.
    #[error("start point: {0}")]
    Start(String),
    /// Cancelled by the caller.
    #[error("cancelled")]
    Cancelled,
}

// ─────────────────────────────────────────────────────────────────────────────
// Sampling
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Builtin {
    Volume,
    Mass,
    Centroid(usize),
}

impl Builtin {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "volume" => Builtin::Volume,
            "mass" => Builtin::Mass,
            "centroid_x" => Builtin::Centroid(0),
            "centroid_y" => Builtin::Centroid(1),
            "centroid_z" => Builtin::Centroid(2),
            _ => return None,
        })
    }

    fn read(self, p: &MassProperties<f64>) -> f64 {
        match self {
            Builtin::Volume => p.volume,
            Builtin::Mass => p.mass,
            Builtin::Centroid(0) => p.centroid.x,
            Builtin::Centroid(1) => p.centroid.y,
            Builtin::Centroid(_) => p.centroid.z,
        }
    }
}

/// Parameter-expression constraint, compiled.
struct ExprConstraint {
    /// `lhs − rhs` (≤ 0 form after `sign`).
    diff: String,
    sign: f64,
    scale: f64,
}

/// A value, its gradient, and each component's provenance.
type Valued = (f64, Vec<f64>, Vec<Provenance>);

/// Everything known at one parameter vector.
#[derive(Clone)]
struct Sample {
    qois: HashMap<QoiRef, Valued>,
    cost: EvalCost,
}

type ThetaKey = Vec<u64>;

fn key(theta: &[f64]) -> ThetaKey {
    theta.iter().map(|t| t.to_bits()).collect()
}

struct Problem<'a> {
    doc: &'a Document,
    req: &'a OptimizeDocRequest,
    registry: &'a QoiRegistry,
    tess: TessellationParams,
    /// Every QoI read by the objective or a constraint, deduplicated.
    qois: Vec<QoiRef>,
    exprs: Vec<Option<ExprConstraint>>,
    seam_inc: RefCell<IncrementalCache>,
    probe_inc: RefCell<IncrementalCache>,
    mesh_cache: Rc<MemoryMeshCache>,
    memo: RefCell<HashMap<ThetaKey, Sample>>,
    /// Plug-in values at probe θs (key = θ, QoI).
    fd_memo: RefCell<HashMap<(ThetaKey, QoiRef), f64>>,
    document_evals: Cell<usize>,
    cancel: Option<&'a dyn Fn() -> bool>,
}

fn options_plain() -> EvalOptions {
    EvalOptions {
        skip_clash_detection: true,
        ..Default::default()
    }
}

fn solid_parts(scene: &EvaluatedScene) -> Vec<BRepSolid> {
    scene
        .parts
        .iter()
        .filter_map(|p| p.solid.as_ref().and_then(|s| s.as_brep()).cloned())
        .collect()
}

impl<'a> Problem<'a> {
    fn new(
        doc: &'a Document,
        req: &'a OptimizeDocRequest,
        registry: &'a QoiRegistry,
        cancel: Option<&'a dyn Fn() -> bool>,
    ) -> Result<Self, OptimizeDocError> {
        let bad = |s: String| OptimizeDocError::Request(s);
        if req.parameters.is_empty() {
            return Err(bad("no parameters".into()));
        }
        if req.objective.is_empty() {
            return Err(bad("empty objective".into()));
        }
        for p in &req.parameters {
            if !doc.parameters.contains_key(&p.name) {
                return Err(bad(format!("document has no parameter `{}`", p.name)));
            }
            if !(p.lo.is_finite() && p.hi.is_finite() && p.lo < p.hi) {
                return Err(bad(format!("parameter `{}`: need finite lo < hi", p.name)));
            }
        }
        let mut qois: Vec<QoiRef> = Vec::new();
        let mut push = |q: &QoiRef| -> Result<(), OptimizeDocError> {
            if Builtin::parse(&q.qoi).is_none() && registry.get(&q.qoi).is_none() {
                return Err(bad(format!(
                    "unknown qoi `{}` (built-in: volume, mass, centroid_x|y|z; registered: {})",
                    q.qoi,
                    registry.names().join(", ")
                )));
            }
            if !qois.contains(q) {
                qois.push(q.clone());
            }
            Ok(())
        };
        for t in &req.objective {
            push(&t.qoi)?;
        }
        let mut exprs = Vec::new();
        for c in &req.constraints {
            match c {
                Constraint::Qoi { qoi, .. } => {
                    push(qoi)?;
                    exprs.push(None);
                }
                Constraint::Expr { expr, scale } => {
                    let (lhs, rhs, sign) = if let Some((l, r)) = expr.split_once("<=") {
                        (l, r, 1.0)
                    } else if let Some((l, r)) = expr.split_once(">=") {
                        (l, r, -1.0)
                    } else {
                        return Err(bad(format!("constraint `{expr}`: need `<=` or `>=`")));
                    };
                    exprs.push(Some(ExprConstraint {
                        diff: format!("({}) - ({})", lhs.trim(), rhs.trim()),
                        sign,
                        scale: scale.unwrap_or(1.0).abs().max(1e-300),
                    }));
                }
            }
        }
        let tess = TessellationParams {
            circle_segments: req.options.circle_segments.max(8),
            height_segments: 2,
            ..Default::default()
        };
        let p = Problem {
            doc,
            req,
            registry,
            tess,
            qois,
            exprs,
            seam_inc: RefCell::new(IncrementalCache::new()),
            probe_inc: RefCell::new(IncrementalCache::new()),
            mesh_cache: Rc::new(MemoryMeshCache::new()),
            memo: RefCell::new(HashMap::new()),
            fd_memo: RefCell::new(HashMap::new()),
            document_evals: Cell::new(0),
            cancel,
        };
        // Validate expression constraints parse against the document.
        let theta0 = p.start_theta()?;
        for (i, e) in p.exprs.iter().enumerate() {
            if e.is_some() {
                p.expr_value(i, &theta0).map_err(|m| {
                    bad(format!("constraint `{}`: {m}", req.constraints[i].label()))
                })?;
            }
        }
        Ok(p)
    }

    fn params(&self) -> &[ParamSpec] {
        &self.req.parameters
    }

    fn start_theta(&self) -> Result<Vec<f64>, OptimizeDocError> {
        let env = vcad_ir::resolve_parameters(&self.doc.parameters)
            .map_err(|e| OptimizeDocError::Request(e.to_string()))?;
        Ok(self
            .params()
            .iter()
            .map(|p| env.get(&p.name).copied().unwrap_or(p.lo).clamp(p.lo, p.hi))
            .collect())
    }

    fn with_params(&self, theta: &[f64]) -> Document {
        let mut d = self.doc.clone();
        for (p, &t) in self.params().iter().zip(theta) {
            if let Some(slot) = d.parameters.get_mut(&p.name) {
                slot.value = Expr::Number(t);
            }
        }
        d
    }

    fn check_cancel(&self) -> Result<(), String> {
        match self.cancel {
            Some(c) if c() => Err("cancelled".into()),
            _ => Ok(()),
        }
    }

    /// Evaluate the document at θ. `brep` selects the seam path (solids
    /// kept, incremental cache) vs the probe path (root-mesh cache).
    fn eval(&self, theta: &[f64], brep: bool) -> Result<(Document, EvaluatedScene, f64), String> {
        self.check_cancel()?;
        let d = self.with_params(theta);
        let t0 = Instant::now();
        let scene = if brep {
            evaluate_document_incremental(&d, &options_plain(), &mut self.seam_inc.borrow_mut())
        } else {
            let opts = EvalOptions {
                root_cache: Some(self.mesh_cache.clone()),
                ..options_plain()
            };
            evaluate_document_incremental(&d, &opts, &mut self.probe_inc.borrow_mut())
        }
        .map_err(|e| format!("evaluation failed: {e}"))?;
        self.document_evals.set(self.document_evals.get() + 1);
        if let Some(f) = scene.failures.first() {
            return Err(format!("root failed to evaluate: {f:?}"));
        }
        Ok((d, scene, t0.elapsed().as_secs_f64() * 1e3))
    }

    fn expr_value(&self, i: usize, theta: &[f64]) -> Result<f64, String> {
        let e = self.exprs[i].as_ref().expect("expr constraint");
        let mut params = self.with_params(theta).parameters;
        const SLOT: &str = "__vcad_optimize_constraint";
        params.insert(SLOT.into(), Parameter::derived(e.diff.clone()));
        let env = vcad_ir::resolve_parameters(&params).map_err(|err| err.to_string())?;
        env.get(SLOT)
            .copied()
            .map(|v| e.sign * v / e.scale)
            .ok_or_else(|| "constraint did not resolve".into())
    }

    /// `(g, ∇g, provenance)` of an expression constraint. Central difference
    /// of the expression (no geometry); labeled `linear` when the second
    /// difference vanishes to round-off.
    fn expr_sample(&self, i: usize, theta: &[f64]) -> Result<Valued, String> {
        let g = self.expr_value(i, theta)?;
        let mut grad = Vec::new();
        let mut prov = Vec::new();
        for (k, p) in self.params().iter().enumerate() {
            let h = 1e-3 * (p.hi - p.lo);
            let at = |dt: f64| {
                let mut t = theta.to_vec();
                t[k] += dt;
                self.expr_value(i, &t)
            };
            let (gp, gm) = (at(h)?, at(-h)?);
            let d = (gp - gm) / (2.0 * h);
            let second = (gp - 2.0 * g + gm).abs();
            let linear = second <= 1e-9 * (g.abs() + gp.abs() + gm.abs() + 1.0);
            grad.push(d);
            prov.push(if linear {
                Provenance::Linear
            } else {
                Provenance::Fd {
                    step: h,
                    scheme: FdScheme::Central,
                }
            });
        }
        Ok((g, grad, prov))
    }

    /// Sample every QoI at θ (memoized).
    fn sample(&self, theta: &[f64]) -> Result<Sample, String> {
        if let Some(s) = self.memo.borrow().get(&key(theta)) {
            return Ok(s.clone());
        }
        let t0 = Instant::now();
        let evals0 = self.document_evals.get();
        let n = self.params().len();
        let mut cost = EvalCost::default();
        let mut qois = HashMap::new();

        let needs_seam = self.qois.iter().any(|q| Builtin::parse(&q.qoi).is_some());
        let (doc_at, base_scene, base_ms) = self.eval(theta, true)?;
        cost.base_eval_ms = base_ms;
        cost.reused_solids += self.seam_inc.borrow().stats().reused_solids;

        // ---- Seam QoIs: one base, 2n probes, one plan per part ----
        if needs_seam {
            let base = solid_parts(&base_scene);
            if base.is_empty() {
                return Err("document has no solid parts".into());
            }
            let h = self.req.options.seam_step;
            let memo: RefCell<HashMap<ThetaKey, Vec<BRepSolid>>> = RefCell::new(HashMap::new());
            memo.borrow_mut().insert(key(theta), base.clone());
            for k in 0..n {
                for s in [-1.0, 1.0] {
                    let mut t = theta.to_vec();
                    t[k] += s * h;
                    let (_, sc, _) = self.eval(&t, true)?;
                    cost.reused_solids += self.seam_inc.borrow().stats().reused_solids;
                    let parts = solid_parts(&sc);
                    if parts.len() != base.len() {
                        return Err(format!(
                            "solid part count changed under the `{}` seam probe ({} → {})",
                            self.params()[k].name,
                            base.len(),
                            parts.len()
                        ));
                    }
                    memo.borrow_mut().insert(key(&t), parts);
                }
            }
            // Per-part, per-parameter mass-property derivatives.
            let failed: Cell<bool> = Cell::new(false);
            let mut per_part: Vec<(MassProperties<f64>, Vec<MassProperties<f64>>)> = Vec::new();
            for (i, brep) in base.iter().enumerate() {
                let build = |t: &[f64]| -> BRepSolid {
                    if let Some(v) = memo.borrow().get(&key(t)) {
                        return v[i].clone();
                    }
                    // Not a probe we pre-evaluated (should not happen):
                    // evaluate, and flag a failure rather than guess.
                    match self.eval(t, true).map(|(_, sc, _)| solid_parts(&sc)) {
                        Ok(v) if v.len() == base.len() => v[i].clone(),
                        _ => {
                            failed.set(true);
                            brep.clone()
                        }
                    }
                };
                let plan = capture_plan(brep, &self.tess).map_err(|e| format!("capture: {e}"))?;
                let mut props = None;
                let mut derivs = Vec::with_capacity(n);
                for k in 0..n {
                    let seeding: ParamSeeding =
                        synthesize_seeding(&build, theta, k, h).map_err(|e: DiffError| {
                            format!("seam w.r.t. `{}`: {e}", self.params()[k].name)
                        })?;
                    let seam = evaluate_with_sensitivity(brep, &plan, &seeding)
                        .map_err(|e| format!("seam: {e}"))?;
                    let (p, d) = mass_properties_with_derivative(&seam, self.req.options.density);
                    props.get_or_insert(p);
                    derivs.push(d);
                }
                if failed.get() {
                    return Err("seam probe re-evaluation failed".into());
                }
                per_part.push((props.expect("n ≥ 1"), derivs));
            }
            for q in &self.qois {
                let Some(b) = Builtin::parse(&q.qoi) else {
                    continue;
                };
                let picked: Vec<usize> = match (q.part, b) {
                    (Some(i), _) if i < per_part.len() => vec![i],
                    (Some(i), _) => {
                        return Err(format!(
                            "part {i} out of range ({} solid parts)",
                            per_part.len()
                        ))
                    }
                    (None, Builtin::Centroid(_)) if per_part.len() != 1 => {
                        return Err(format!("`{}` on a multi-part document needs `part`", q.qoi))
                    }
                    (None, _) => (0..per_part.len()).collect(),
                };
                let val: f64 = picked.iter().map(|&i| b.read(&per_part[i].0)).sum();
                let grad: Vec<f64> = (0..n)
                    .map(|k| picked.iter().map(|&i| b.read(&per_part[i].1[k])).sum())
                    .collect();
                qois.insert(q.clone(), (val, grad, vec![Provenance::Seam; n]));
            }
        }

        // ---- Plug-in QoIs ----
        let plugins: Vec<&QoiRef> = self
            .qois
            .iter()
            .filter(|q| Builtin::parse(&q.qoi).is_none())
            .collect();
        if !plugins.is_empty() {
            let input = QoiInput {
                doc: &doc_at,
                scene: &base_scene,
                params: self.params(),
                theta,
            };
            for q in plugins {
                let plugin = self.registry.get(&q.qoi).expect("validated");
                let v = plugin.evaluate(&input, q.part)?;
                if !v.is_finite() {
                    return Err(format!("`{}` is not finite at this point", q.label()));
                }
                if let Some(g) = plugin.gradient(&input, q.part) {
                    let g = g?;
                    if g.len() != n {
                        return Err(format!(
                            "`{}` adjoint returned {} components",
                            q.label(),
                            g.len()
                        ));
                    }
                    qois.insert(q.clone(), (v, g, vec![Provenance::Adjoint; n]));
                    continue;
                }
                self.fd_memo.borrow_mut().insert((key(theta), q.clone()), v);
                let mut grad = Vec::with_capacity(n);
                let mut prov = Vec::with_capacity(n);
                for (k, p) in self.params().iter().enumerate() {
                    let h = plugin
                        .fd_step(p)
                        .unwrap_or(self.req.options.fd_rel_step * (p.hi - p.lo));
                    let can_up = theta[k] + h <= p.hi + 1e-12;
                    let can_dn = theta[k] - h >= p.lo - 1e-12;
                    let scheme = match (can_dn, can_up) {
                        (true, true) => FdScheme::Central,
                        (false, true) => FdScheme::Forward,
                        (true, false) => FdScheme::Backward,
                        (false, false) => {
                            return Err(format!("FD step {h} wider than `{}`'s box", p.name))
                        }
                    };
                    let probe = |s: f64| -> Result<f64, String> {
                        let mut t = theta.to_vec();
                        t[k] += s * h;
                        self.plugin_at(&t, q, plugin)
                    };
                    let d = match scheme {
                        FdScheme::Central => (probe(1.0)? - probe(-1.0)?) / (2.0 * h),
                        FdScheme::Forward => (probe(1.0)? - v) / h,
                        FdScheme::Backward => (v - probe(-1.0)?) / h,
                    };
                    grad.push(d);
                    prov.push(Provenance::Fd { step: h, scheme });
                }
                qois.insert(q.clone(), (v, grad, prov));
            }
        }

        cost.document_evals = self.document_evals.get() - evals0;
        cost.wall_ms = t0.elapsed().as_secs_f64() * 1e3;
        let s = Sample { qois, cost };
        self.memo.borrow_mut().insert(key(theta), s.clone());
        Ok(s)
    }

    /// A plug-in's value at a probe θ (memoized; probe evaluations share the
    /// root-mesh cache unless the plug-in needs BReps).
    fn plugin_at(&self, theta: &[f64], q: &QoiRef, plugin: &dyn ScalarQoi) -> Result<f64, String> {
        let k = (key(theta), q.clone());
        if let Some(v) = self.fd_memo.borrow().get(&k) {
            return Ok(*v);
        }
        let (d, scene, _) = self.eval(theta, plugin.needs_brep())?;
        let input = QoiInput {
            doc: &d,
            scene: &scene,
            params: self.params(),
            theta,
        };
        let v = plugin.evaluate(&input, q.part)?;
        if !v.is_finite() {
            return Err(format!("`{}` is not finite at a probe", q.label()));
        }
        self.fd_memo.borrow_mut().insert(k, v);
        Ok(v)
    }

    /// Objective value + gradient (unscaled) + term breakdown.
    fn objective(&self, s: &Sample) -> (f64, Vec<f64>, Vec<TermSample>) {
        let n = self.params().len();
        let mut f = 0.0;
        let mut g = vec![0.0; n];
        let mut terms = Vec::new();
        for t in &self.req.objective {
            let (q, gq, prov) = &s.qois[&t.qoi];
            let (v, dv) = t.shape.apply(*q);
            f += t.weight * v;
            let comps: Vec<GradComponent> = (0..n)
                .map(|k| {
                    let d = t.weight * dv * gq[k];
                    g[k] += d;
                    GradComponent {
                        param: self.params()[k].name.clone(),
                        value: d,
                        provenance: prov[k],
                    }
                })
                .collect();
            terms.push(TermSample {
                label: t.qoi.label(),
                qoi_value: *q,
                contribution: t.weight * v,
                gradient: comps,
            });
        }
        (f, g, terms)
    }

    /// Constraints in normalized `g ≤ 0` form.
    fn constraints(&self, theta: &[f64], s: &Sample) -> Result<Vec<Valued>, String> {
        self.req
            .constraints
            .iter()
            .enumerate()
            .map(|(i, c)| match c {
                Constraint::Expr { .. } => self.expr_sample(i, theta),
                Constraint::Qoi { qoi, op, value } => {
                    let (q, gq, prov) = &s.qois[qoi];
                    let scale = value.abs().max(1.0);
                    let sign = match op {
                        ConstraintOp::Le => 1.0,
                        ConstraintOp::Ge => -1.0,
                    };
                    Ok((
                        sign * (q - value) / scale,
                        gq.iter().map(|d| sign * d / scale).collect(),
                        prov.clone(),
                    ))
                }
            })
            .collect()
    }
}

/// Weakest route among contributions: fd < adjoint < seam / linear.
fn weakest(a: Provenance, b: Provenance) -> Provenance {
    let rank = |p: &Provenance| match p {
        Provenance::Fd { .. } => 0,
        Provenance::Adjoint => 1,
        Provenance::Seam | Provenance::Linear => 2,
    };
    if rank(&b) < rank(&a) {
        b
    } else {
        a
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The optimizer
// ─────────────────────────────────────────────────────────────────────────────

/// Hooks for long runs.
#[derive(Default)]
pub struct OptimizeHooks<'a> {
    /// Polled before every document evaluation; `true` cancels.
    pub cancel: Option<&'a dyn Fn() -> bool>,
    /// Called with each accepted iterate as it lands.
    pub on_step: Option<&'a dyn Fn(&TrajectoryStep)>,
}

/// Minimize `req.objective` over `req.parameters` subject to
/// `req.constraints`. See the module docs.
pub fn optimize_document(
    doc: &Document,
    req: &OptimizeDocRequest,
    registry: &QoiRegistry,
    hooks: &OptimizeHooks,
) -> Result<OptimizeDocResult, OptimizeDocError> {
    let run_t0 = Instant::now();
    let prob = Problem::new(doc, req, registry, hooks.cancel)?;
    let n = req.parameters.len();
    let lo: Vec<f64> = req.parameters.iter().map(|p| p.lo).collect();
    let range: Vec<f64> = req.parameters.iter().map(|p| p.hi - p.lo).collect();
    let to_theta = |x: &[f64]| -> Vec<f64> { (0..n).map(|k| lo[k] + x[k] * range[k]).collect() };
    let theta0 = prob.start_theta()?;
    let mut x: Vec<f64> = (0..n).map(|k| (theta0[k] - lo[k]) / range[k]).collect();

    let s0 = prob.sample(&theta0).map_err(|e| match e.as_str() {
        "cancelled" => OptimizeDocError::Cancelled,
        _ => OptimizeDocError::Start(e),
    })?;
    let (f0, _, _) = prob.objective(&s0);
    let f_scale = f0.abs().max(1e-12);
    let m = req.constraints.len();
    let mut lambda = vec![0.0; m];
    let mut mu = req.options.mu0;
    // Conditioning: inside the merit each constraint is weighted so its
    // normalized-coordinate gradient has unit ∞-norm at the start point.
    // Without it a constraint read in large units (`h >= 2*r`, ∂g/∂x = 36)
    // sits next to a relative one (`f >= 6` scaled by 1/6, ∂g/∂x = 2) and
    // the penalty is mis-tuned for at least one of them. Reported `g`,
    // feasibility and multipliers stay in the constraint's own units
    // (multiplier = internal λ · weight).
    let cweight: Vec<f64> = prob
        .constraints(&theta0, &s0)
        .map_err(OptimizeDocError::Start)?
        .iter()
        .map(|(_, gg, _)| {
            let nrm = (0..n).map(|k| (gg[k] * range[k]).abs()).fold(0.0, f64::max);
            if nrm > 1e-12 {
                1.0 / nrm
            } else {
                1.0
            }
        })
        .collect();
    let effective =
        |lambda: &[f64]| -> Vec<f64> { lambda.iter().zip(&cweight).map(|(l, w)| l * w).collect() };

    // AL merit in normalized coordinates.
    let merit = |theta: &[f64], lambda: &[f64], mu: f64| -> Result<(f64, Vec<f64>), String> {
        let s = prob.sample(theta)?;
        let (f, gf, _) = prob.objective(&s);
        let mut val = f / f_scale;
        let mut grad: Vec<f64> = (0..n).map(|k| gf[k] / f_scale * range[k]).collect();
        for (j, (g, gg, _)) in prob.constraints(theta, &s)?.into_iter().enumerate() {
            let w = cweight[j];
            let t = w * g + lambda[j] / mu;
            if t > 0.0 {
                val += 0.5 * mu * t * t - lambda[j] * lambda[j] / (2.0 * mu);
                for k in 0..n {
                    grad[k] += mu * t * w * gg[k] * range[k];
                }
            } else {
                val -= lambda[j] * lambda[j] / (2.0 * mu);
            }
        }
        Ok((val, grad))
    };

    let make_step = |iter: usize,
                     round: usize,
                     theta: &[f64],
                     merit_v: f64,
                     lambda: &[f64]|
     -> Result<TrajectoryStep, String> {
        let s = prob.sample(theta)?;
        let (f, gf, terms) = prob.objective(&s);
        let cons = prob.constraints(theta, &s)?;
        let names = |k: usize| req.parameters[k].name.clone();
        let mut obj_prov = vec![Provenance::Seam; n];
        let mut seen = vec![false; n];
        for t in &req.objective {
            let prov = &s.qois[&t.qoi].2;
            for k in 0..n {
                obj_prov[k] = if seen[k] {
                    weakest(obj_prov[k], prov[k])
                } else {
                    prov[k]
                };
                seen[k] = true;
            }
        }
        Ok(TrajectoryStep {
            iter,
            round,
            theta: theta.to_vec(),
            objective: f,
            merit: merit_v,
            max_violation: cons.iter().map(|c| c.0.max(0.0)).fold(0.0, f64::max),
            objective_gradient: (0..n)
                .map(|k| GradComponent {
                    param: names(k),
                    value: gf[k],
                    provenance: obj_prov[k],
                })
                .collect(),
            terms,
            constraints: cons
                .iter()
                .enumerate()
                .map(|(j, (g, gg, prov))| ConstraintSample {
                    label: req.constraints[j].label(),
                    g: *g,
                    multiplier: lambda[j],
                    gradient: (0..n)
                        .map(|k| GradComponent {
                            param: names(k),
                            value: gg[k],
                            provenance: prov[k],
                        })
                        .collect(),
                })
                .collect(),
            qois: prob
                .qois
                .iter()
                .map(|q| {
                    let (v, g, prov) = &s.qois[q];
                    QoiSample {
                        qoi: q.clone(),
                        value: *v,
                        gradient: (0..n)
                            .map(|k| GradComponent {
                                param: names(k),
                                value: g[k],
                                provenance: prov[k],
                            })
                            .collect(),
                    }
                })
                .collect(),
            cost: s.cost.clone(),
        })
    };
    let err_map = |e: String| match e.as_str() {
        "cancelled" => OptimizeDocError::Cancelled,
        _ => OptimizeDocError::Start(e),
    };

    let mut trajectory = Vec::new();
    let push_step = |trajectory: &mut Vec<TrajectoryStep>, step: TrajectoryStep| {
        if let Some(cb) = hooks.on_step {
            cb(&step);
        }
        trajectory.push(step);
    };
    let (m0, _) = merit(&theta0, &lambda, mu).map_err(err_map)?;
    push_step(
        &mut trajectory,
        make_step(0, 0, &theta0, m0, &effective(&lambda)).map_err(err_map)?,
    );

    let mut stop = OptimizeStop::MaxRounds;
    let mut prev_viol = f64::INFINITY;
    for round in 0..req.options.max_outer.max(1) {
        let opts = tang_optim::LbfgsOptions {
            max_iters: req.options.max_inner.max(1),
            initial_step: req.options.initial_step,
            min_step: 1e-7,
            grad_tol: req.options.grad_tol,
            bounds: vec![(0.0, 1.0); n],
            ..Default::default()
        };
        let lam_r = lambda.clone();
        let cancelled = Cell::new(false);
        let r = tang_optim::minimize_lbfgs(
            |xx: &[f64]| {
                let th = to_theta(xx);
                merit(&th, &lam_r, mu).inspect_err(|e| {
                    if e == "cancelled" {
                        cancelled.set(true);
                    }
                })
            },
            &x,
            &opts,
        );
        if cancelled.get() {
            return Err(OptimizeDocError::Cancelled);
        }
        // An error here means the round's own start point failed, which
        // was accepted in the previous round — only possible via cancel.
        let r = r.map_err(err_map)?;
        for it in r.history.iter().skip(1) {
            let th = to_theta(&it.x);
            let iter = trajectory.len();
            push_step(
                &mut trajectory,
                make_step(iter, round, &th, it.value, &effective(&lam_r)).map_err(err_map)?,
            );
        }
        x = r.x.clone();
        let theta = to_theta(&x);
        let s = prob.sample(&theta).map_err(err_map)?;
        let cons = prob.constraints(&theta, &s).map_err(err_map)?;
        let viol = cons.iter().map(|c| c.0.max(0.0)).fold(0.0, f64::max);
        for (j, c) in cons.iter().enumerate() {
            lambda[j] = (lambda[j] + mu * cweight[j] * c.0).max(0.0);
        }
        let inner_ok = matches!(
            r.stop,
            tang_optim::LbfgsStop::GradientConverged | tang_optim::LbfgsStop::StepConverged
        );
        if viol <= req.options.feas_tol && inner_ok {
            // Converged when the multipliers have settled (the next round
            // would solve the same subproblem).
            let stat =
                kkt(&prob, &theta, &s, &effective(&lambda), f_scale, &range, &lo).stationarity;
            if stat <= kkt_tol(req) || m == 0 {
                stop = OptimizeStop::Converged;
                break;
            }
        }
        if m == 0 {
            stop = if inner_ok {
                OptimizeStop::Converged
            } else {
                OptimizeStop::Stalled
            };
            break;
        }
        if viol > 0.25 * prev_viol {
            mu *= 10.0;
        }
        prev_viol = viol;
    }

    let theta = to_theta(&x);
    let s = prob.sample(&theta).map_err(err_map)?;
    let (f, _, _) = prob.objective(&s);
    let report = kkt(&prob, &theta, &s, &effective(&lambda), f_scale, &range, &lo);

    let mut seam_fd_check = Vec::new();
    if req.options.check_seam_fd {
        seam_fd_check = seam_fd(&prob, &theta, &s).map_err(err_map)?;
    }

    let samples = prob.memo.borrow().len();
    let document_evals = prob.document_evals.get();
    Ok(OptimizeDocResult {
        params: req.parameters.iter().map(|p| p.name.clone()).collect(),
        theta,
        objective: f,
        trajectory,
        stop,
        kkt: report,
        seam_fd_check,
        samples,
        document_evals,
        wall_ms: run_t0.elapsed().as_secs_f64() * 1e3,
    })
}

fn kkt_tol(req: &OptimizeDocRequest) -> f64 {
    // FD-priced gradients are only as good as their step; give them room.
    (req.options.grad_tol * 100.0).max(1e-2)
}

fn kkt(
    prob: &Problem,
    theta: &[f64],
    s: &Sample,
    lambda: &[f64],
    f_scale: f64,
    range: &[f64],
    lo: &[f64],
) -> KktReport {
    let n = theta.len();
    let (_, gf, _) = prob.objective(s);
    let cons = prob.constraints(theta, s).unwrap_or_default();
    let mut gl: Vec<f64> = (0..n).map(|k| gf[k] / f_scale * range[k]).collect();
    for (j, (_, gg, _)) in cons.iter().enumerate() {
        for k in 0..n {
            gl[k] += lambda[j] * gg[k] * range[k];
        }
    }
    let mut active = Vec::new();
    let mut stat = 0.0f64;
    for k in 0..n {
        let x = (theta[k] - lo[k]) / range[k];
        let at_lo = x <= 1e-9;
        let at_hi = x >= 1.0 - 1e-9;
        // Box-projected: at a lower bound only a negative gradient (wanting
        // to go lower) is admissible, and vice versa.
        let r = if at_lo {
            active.push((prob.params()[k].name.clone(), "lo".to_string()));
            (-gl[k]).max(0.0)
        } else if at_hi {
            active.push((prob.params()[k].name.clone(), "hi".to_string()));
            gl[k].max(0.0)
        } else {
            gl[k].abs()
        };
        stat = stat.max(r);
    }
    let max_violation = cons.iter().map(|c| c.0.max(0.0)).fold(0.0, f64::max);
    let complementarity = cons
        .iter()
        .zip(lambda)
        .map(|(c, l)| (c.0 * l).abs())
        .fold(0.0, f64::max);
    let tol = kkt_tol(prob.req);
    let feasible = max_violation <= prob.req.options.feas_tol;
    KktReport {
        max_violation,
        stationarity: stat,
        complementarity,
        multipliers: lambda.to_vec(),
        active_bounds: active,
        feasible,
        satisfied: feasible && stat <= tol && complementarity <= tol,
        kkt_tol: tol,
    }
}

/// Central FD of every seam QoI over every parameter, on the same frozen
/// tessellation params the seam uses.
fn seam_fd(prob: &Problem, theta: &[f64], s: &Sample) -> Result<Vec<SeamFdCheck>, String> {
    let mut out = Vec::new();
    let seam_qois: Vec<&QoiRef> = prob
        .qois
        .iter()
        .filter(|q| Builtin::parse(&q.qoi).is_some())
        .collect();
    if seam_qois.is_empty() {
        return Ok(out);
    }
    let props_at = |t: &[f64]| -> Result<Vec<MassProperties<f64>>, String> {
        let (_, sc, _) = prob.eval(t, true)?;
        solid_parts(&sc)
            .iter()
            .map(|b| {
                let plan = capture_plan(b, &prob.tess).map_err(|e| e.to_string())?;
                let seam = evaluate_with_sensitivity(b, &plan, &ParamSeeding::new())
                    .map_err(|e| e.to_string())?;
                Ok(mass_properties(
                    &seam.positions,
                    &seam.triangles,
                    prob.req.options.density,
                ))
            })
            .collect()
    };
    for (k, p) in prob.params().iter().enumerate() {
        let h = 1e-3 * (p.hi - p.lo);
        let mut tp = theta.to_vec();
        let mut tm = theta.to_vec();
        tp[k] += h;
        tm[k] -= h;
        let (pp, pm) = (props_at(&tp)?, props_at(&tm)?);
        for q in &seam_qois {
            let b = Builtin::parse(&q.qoi).expect("seam qoi");
            let read = |ps: &[MassProperties<f64>]| -> f64 {
                match q.part {
                    Some(i) => ps.get(i).map(|x| b.read(x)).unwrap_or(f64::NAN),
                    None => ps.iter().map(|x| b.read(x)).sum(),
                }
            };
            let fd = (read(&pp) - read(&pm)) / (2.0 * h);
            let seam = s.qois[*q].1[k];
            let denom = seam.abs().max(fd.abs()).max(1e-300);
            out.push(SeamFdCheck {
                qoi: q.label(),
                param: p.name.clone(),
                seam,
                fd,
                step: h,
                rel_error: (seam - fd).abs() / denom,
            });
        }
    }
    Ok(out)
}

/// Parse a JSON request (the MCP / WASM / FFI surface).
pub fn parse_request(json: &serde_json::Value) -> Result<OptimizeDocRequest, OptimizeDocError> {
    serde_json::from_value(json.clone()).map_err(|e| OptimizeDocError::Request(e.to_string()))
}
