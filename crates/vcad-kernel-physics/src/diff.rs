//! Physics-rollout gradients: differentiating a phyz simulation objective
//! with respect to CAD parameters θ (milestone M8 of the differentiable
//! seam).
//!
//! # What this computes
//!
//! For a **contact-free articulated rigid-body rollout**, CAD geometry enters
//! the dynamics through exactly one channel: each body's **mass properties** —
//! mass, center of mass, and inertia tensor about the COM (10 scalars per
//! body). Everything downstream (Featherstone ABA, the integrator, the
//! objective) is a smooth function of those 10 scalars per body. That gives an
//! exact chain rule
//!
//! ```text
//! dJ/dθ = Σ_bodies  ∂J/∂p_body · dp_body/dθ
//! ```
//!
//! where `p_body` is the 10-vector `[m, c_x, c_y, c_z, I_xx, I_yy, I_zz,
//! I_xy, I_xz, I_yz]` (COM-frame inertia). The two factors come from two
//! different places, each computed the correct way for its side of the seam:
//!
//! - **`dp_body/dθ`** — *exact*, from the differentiable seam. The frozen
//!   plan plus a per-parameter `ParamSeeding` feeds
//!   `mass_properties_with_derivative`, which carries dual numbers through
//!   the polynomial mass-property integrals: every field's θ-derivative in
//!   one pass, to machine precision. No remeshing, no topology risk.
//!
//! - **`∂J/∂p_body`** — two implementations of the same factor:
//!   - **Adjoint (exact)** — [`rollout_gradient_adjoint`] hands a structured
//!     rollout (`AdjointRolloutSpec`) to the **phyz trajectory adjoint**
//!     (`phyz::diff`), which prices all `10·n_bodies` sensitivities in one
//!     backward pass (dual-number lanes through a scalar-generic ABA — no
//!     finite differences). Requires the rollout in structured form:
//!     single-DOF joints, open-loop control, final-state objective with an
//!     analytic gradient.
//!   - **Central FD (fallback)** — [`rollout_gradient`] takes an *opaque*
//!     rollout closure and perturbs each mass-property scalar ±h
//!     (≈20 rollouts per body). Still no CAD rebuild, no re-tessellation —
//!     only the integrator re-runs. This remains the path for rollouts the
//!     spec cannot express (arbitrary code, state feedback, multi-DOF
//!     joints) or for phyz versions without the adjoint.
//!
//!   Either way the factorization is unchanged; the M11 gates
//!   (`m11_adjoint_rollout.rs`) hold the two implementations to each other
//!   at 1e-5 and to a rebuild-and-resimulate FD at 1e-4.
//!
//! # Contract and boundaries
//!
//! - **Contact-free for the mass-property-only entry points.** The
//!   factorization is exact *because* geometry reaches the dynamics solely
//!   through mass properties. The rollout closure passed to
//!   [`rollout_gradient`] / [`rollout_gradient_adjoint`] **must** build a
//!   contact-free model; if it does not, the returned gradient is silently
//!   incomplete. When contact forces *do* act during the rollout, use
//!   [`contact_rollout_gradient`]: the phyz contact adjoint produces
//!   `∂J_dyn/∂x` on the body's collision skin (the frozen-plan seam mesh)
//!   under the differentiable per-vertex penalty contact model, and that
//!   cotangent goes through the same M5 pullback as [`surface_gradient`].
//!   Objective terms that merely *read* the surface (no contact dynamics)
//!   still go through [`rollout_gradient_with_surface`].
//! - **Anchor channel.** If θ also moves a **joint anchor / mount frame** (a
//!   pivot hole whose position scales with the part), use
//!   [`rollout_gradient_with_anchors`]: the anchor coordinates enter the
//!   factorization as additional scalars — `∂J/∂anchor` by the same
//!   central-FD-rollout pattern, `d(anchor)/dθ` by central FD on the caller's
//!   anchor map (a pure function of θ, no geometry rebuild) — and the two
//!   channels sum. [`rollout_gradient`] is the anchor-free special case.
//! - **Determinism.** phyz's ABA + semi-implicit Euler are deterministic;
//!   the FD estimate of `∂J/∂p` is only meaningful if the rollout closure is a
//!   pure function of its mass-property input (fixed initial state, fixed
//!   control law, fixed step count). Keep it so.

use std::sync::Arc;

use phyz::math::{Mat3, Vec3 as PVec3};
use phyz::SpatialInertia;
use vcad_kernel_diff::{
    evaluate_with_pullback, evaluate_with_sensitivity, mass_properties,
    mass_properties_with_derivative, DiffError, MassProperties, ParamSeeding,
};
use vcad_kernel_math::{Point3 as CadPoint3, Vec3 as CadVec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_tessellate::frozen::capture_plan;
use vcad_kernel_tessellate::TessellationParams;

/// Millimetres to metres.
const MM_TO_M: f64 = 1e-3;
/// mm² to m² (COM-frame inertia carries a squared length).
const MM2_TO_M2: f64 = 1e-6;
/// kg·m⁻³ to kg·mm⁻³ (1 m³ = 1e9 mm³), so seam integrals in mm come out in kg.
const KGM3_TO_KGMM3: f64 = 1e-9;

/// The mass properties of one rigid body, in **SI units** (kg, m, kg·m²),
/// expressed in the body's own CAD frame.
///
/// The inertia is about the **center of mass** (COM), matching
/// [`phyz::SpatialInertia`]. Field order for the packed 10-vector used by the
/// finite-difference loop is `[m, c_x, c_y, c_z, I_xx, I_yy, I_zz, I_xy,
/// I_xz, I_yz]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyMassProps {
    /// Mass (kg).
    pub mass: f64,
    /// Center of mass in the body frame (m).
    pub com: [f64; 3],
    /// Inertia about the COM, `[I_xx, I_yy, I_zz, I_xy, I_xz, I_yz]` (kg·m²).
    pub inertia: [f64; 6],
}

impl BodyMassProps {
    /// Pack into the canonical 10-vector `[m, c_x, c_y, c_z, I_xx, I_yy,
    /// I_zz, I_xy, I_xz, I_yz]`.
    pub fn scalars(&self) -> [f64; 10] {
        [
            self.mass,
            self.com[0],
            self.com[1],
            self.com[2],
            self.inertia[0],
            self.inertia[1],
            self.inertia[2],
            self.inertia[3],
            self.inertia[4],
            self.inertia[5],
        ]
    }

    /// Inverse of [`Self::scalars`].
    pub fn from_scalars(s: [f64; 10]) -> Self {
        Self {
            mass: s[0],
            com: [s[1], s[2], s[3]],
            inertia: [s[4], s[5], s[6], s[7], s[8], s[9]],
        }
    }

    /// Build a [`phyz::SpatialInertia`] (mass, COM offset, COM-frame inertia
    /// matrix) from these properties.
    pub fn to_spatial_inertia(&self) -> SpatialInertia {
        let [ixx, iyy, izz, ixy, ixz, iyz] = self.inertia;
        let m = Mat3::new(ixx, ixy, ixz, ixy, iyy, iyz, ixz, iyz, izz);
        SpatialInertia::new(
            self.mass,
            PVec3::new(self.com[0], self.com[1], self.com[2]),
            m,
        )
    }

    /// Convert seam mass properties (mm, seam density) to SI, given the body's
    /// physical density in kg/m³. `props.mass` is already in kg (the seam was
    /// fed `density · 1e-9`); the centroid and inertia carry length factors.
    fn from_seam(props: &MassProperties<f64>) -> Self {
        let c = props.centroid;
        let i = &props.inertia_centroid;
        Self {
            mass: props.mass,
            com: [c.x * MM_TO_M, c.y * MM_TO_M, c.z * MM_TO_M],
            inertia: [
                i[0][0] * MM2_TO_M2,
                i[1][1] * MM2_TO_M2,
                i[2][2] * MM2_TO_M2,
                i[0][1] * MM2_TO_M2,
                i[0][2] * MM2_TO_M2,
                i[1][2] * MM2_TO_M2,
            ],
        }
    }

    /// Convert a seam **derivative** of mass properties into the SI 10-vector
    /// `dp/dθ`. Same linear unit factors as [`Self::from_seam`] (θ is a length
    /// in mm, so the factors carry through unchanged).
    fn deriv_scalars(dprops: &MassProperties<f64>) -> [f64; 10] {
        let dc = dprops.centroid;
        let di = &dprops.inertia_centroid;
        [
            dprops.mass,
            dc.x * MM_TO_M,
            dc.y * MM_TO_M,
            dc.z * MM_TO_M,
            di[0][0] * MM2_TO_M2,
            di[1][1] * MM2_TO_M2,
            di[2][2] * MM2_TO_M2,
            di[0][1] * MM2_TO_M2,
            di[0][2] * MM2_TO_M2,
            di[1][2] * MM2_TO_M2,
        ]
    }
}

/// Finite-difference step sizes for the `∂J/∂p` estimate, one policy for each
/// class of mass-property scalar.
///
/// Central differences trade truncation error `O(h²)` against roundoff
/// `O(ε·|J|/h)`; the defaults sit near the `~1e-5` relative sweet spot for a
/// smooth `f64` rollout. Mass and inertia use a **relative** step (scaled to
/// the body's own magnitude); the COM uses an **absolute** step (a centered
/// body's COM is ~0, where a relative step would collapse to nothing).
#[derive(Debug, Clone, Copy)]
pub struct MassPropFdSteps {
    /// Relative step for the mass scalar.
    pub mass_rel: f64,
    /// Floor for the mass step (kg).
    pub mass_min: f64,
    /// Absolute step for each COM component (m).
    pub com_abs: f64,
    /// Relative step for inertia components, scaled to the body's reference
    /// inertia (max |diagonal|).
    pub inertia_rel: f64,
    /// Floor for the inertia step (kg·m²).
    pub inertia_min: f64,
}

impl Default for MassPropFdSteps {
    fn default() -> Self {
        Self {
            mass_rel: 1e-5,
            mass_min: 1e-12,
            com_abs: 1e-7,
            inertia_rel: 1e-5,
            inertia_min: 1e-18,
        }
    }
}

impl MassPropFdSteps {
    /// The 10 per-scalar step sizes for one body's nominal properties.
    fn steps_for(&self, p: &BodyMassProps) -> [f64; 10] {
        let mass_h = (self.mass_rel * p.mass.abs()).max(self.mass_min);
        let ref_i = p.inertia[0]
            .abs()
            .max(p.inertia[1].abs())
            .max(p.inertia[2].abs());
        let inertia_h = (self.inertia_rel * ref_i).max(self.inertia_min);
        [
            mass_h,
            self.com_abs,
            self.com_abs,
            self.com_abs,
            inertia_h,
            inertia_h,
            inertia_h,
            inertia_h,
            inertia_h,
            inertia_h,
        ]
    }
}

/// A rigid body whose mass properties depend on CAD parameters θ through the
/// differentiable seam.
///
/// The three fields are the CAD side of the M8 factorization: a build
/// function over θ, a per-parameter seeding source (hand-written or
/// [`synthesize_seeding`](vcad_kernel_diff::synthesize_seeding)), and the
/// physical density.
#[allow(clippy::type_complexity)]
pub struct DiffBody<'a> {
    /// Build this body's B-rep at θ (geometry in millimetres).
    pub build: Box<dyn Fn(&[f64]) -> BRepSolid + 'a>,
    /// Surface seeding for parameter `k`, given the freshly built B-rep and the
    /// current θ. Returns a `Result` so
    /// [`synthesize_seeding`](vcad_kernel_diff::synthesize_seeding) composes
    /// directly — `|_brep, theta, k| synthesize_seeding(&build, theta, k, h)` —
    /// while a hand-written seeding just reads the B-rep and wraps in `Ok`.
    ///
    /// The seeding must key into the **passed** `BRepSolid` (the one the
    /// adapter captured its plan from). A synthesized seeding satisfies this
    /// only when `build` is deterministic in its surface ordering; the
    /// hand-written path always does.
    #[allow(clippy::type_complexity)]
    pub seeding_for: Box<dyn Fn(&BRepSolid, &[f64], usize) -> Result<ParamSeeding, DiffError> + 'a>,
    /// Physical density (kg/m³).
    pub density_kg_m3: f64,
    /// Tessellation parameters for the frozen plan.
    pub tess: TessellationParams,
}

impl DiffBody<'_> {
    /// Seam density (kg/mm³): fed to the mm-frame integrals so mass comes out
    /// in kg.
    fn seam_density(&self) -> f64 {
        self.density_kg_m3 * KGM3_TO_KGMM3
    }
}

/// Objective value and full CAD-parameter gradient of a physics rollout,
/// `(J, dJ/dθ)`, via the mass-property factorization.
///
/// This is shaped exactly like
/// [`objective_gradient`](vcad_kernel_diff::objective_gradient) — a value plus
/// a `Vec<f64>` gradient, `Result` on the seam errors — so a projected
/// gradient-descent / L-BFGS driver can call it as its oracle.
///
/// # Arguments
///
/// - `bodies` — the differentiable rigid bodies. Body `i`'s mass properties
///   are `bodies[i]` evaluated at θ; the rollout receives them in the same
///   order.
/// - `rollout` — builds a **contact-free** phyz model from the per-body mass
///   properties, simulates a fixed trajectory, and returns the scalar
///   objective `J`. Must be deterministic and a pure function of its input
///   (see the module contract).
/// - `theta` — the CAD parameters.
/// - `fd` — finite-difference step policy for `∂J/∂p`.
///
/// # What runs
///
/// One frozen-plan capture per body, one positions-only seam pass per body
/// for the nominal properties, `n·10·2` rollouts for the per-body `∂J/∂p`
/// (`n` = number of bodies), and one seam pass per (body, parameter) for the
/// exact `dp/dθ`. The rollout count is independent of the θ dimension; adding
/// a CAD parameter costs one extra seam pass per body, nothing more.
///
/// # Errors
///
/// Any seam error — capture failure, a topology change under the frozen plan,
/// an inconsistent implicit vertex system, a synthesis ambiguity — surfaces as
/// [`DiffError`], exactly as the seam optimizer's does.
pub fn rollout_gradient(
    bodies: &[DiffBody],
    rollout: &impl Fn(&[BodyMassProps]) -> f64,
    theta: &[f64],
    fd: &MassPropFdSteps,
) -> Result<(f64, Vec<f64>), DiffError> {
    rollout_gradient_with_anchors(
        bodies,
        &|_theta: &[f64]| Vec::new(),
        &|props: &[BodyMassProps], _anchors: &[f64]| rollout(props),
        theta,
        fd,
        &AnchorFdSteps::default(),
    )
}

/// Finite-difference step policy for the **anchor channel** of
/// [`rollout_gradient_with_anchors`].
///
/// Two independent steps, because the two derivatives probe different maps:
/// `value_abs` perturbs an anchor scalar inside the *rollout* (`∂J/∂a`,
/// meters in the phyz frame), while `theta_rel`/`theta_min` step θ inside the
/// caller's *anchor map* (`da/dθ`, a pure function — usually linear — so the
/// step only needs to clear roundoff).
#[derive(Debug, Clone, Copy)]
pub struct AnchorFdSteps {
    /// Absolute step (m) for each anchor scalar in the `∂J/∂a` rollouts.
    pub value_abs: f64,
    /// Relative step (of `|θ_k|`) for the `da/dθ_k` central difference.
    pub theta_rel: f64,
    /// Floor for the θ step.
    pub theta_min: f64,
}

impl Default for AnchorFdSteps {
    fn default() -> Self {
        Self {
            value_abs: 1e-7,
            theta_rel: 1e-6,
            theta_min: 1e-9,
        }
    }
}

/// [`rollout_gradient`] with the **anchor channel**: `(J, dJ/dθ)` where θ
/// moves both the bodies' mass properties *and* joint anchor / mount-frame
/// coordinates.
///
/// `anchor_map` maps θ to a flat vector of anchor scalars (layout is the
/// caller's contract with its own `rollout` — e.g. `[pivot_x_m, pivot_y_m]`).
/// The chain gains one term per anchor scalar:
///
/// ```text
/// dJ/dθ = Σ_bodies ∂J/∂p·dp/dθ  +  Σ_anchors ∂J/∂a·da/dθ
/// ```
///
/// with `∂J/∂a` from central-FD rollouts (like `∂J/∂p` — no CAD rebuild) and
/// `da/dθ` from a central difference of `anchor_map` itself, which is a pure
/// function of θ (no geometry, no remeshing; for the common linear anchor —
/// a hole position proportional to a dimension — the FD is exact to
/// roundoff). Everything in the [`rollout_gradient`] contract carries over;
/// the rollout closure must be pure in *both* inputs.
pub fn rollout_gradient_with_anchors(
    bodies: &[DiffBody],
    anchor_map: &impl Fn(&[f64]) -> Vec<f64>,
    rollout: &impl Fn(&[BodyMassProps], &[f64]) -> f64,
    theta: &[f64],
    fd: &MassPropFdSteps,
    anchor_fd: &AnchorFdSteps,
) -> Result<(f64, Vec<f64>), DiffError> {
    let n = theta.len();

    // 1. Nominal mass properties per body (positions-only seam), and the
    //    frozen plan / B-rep kept for the exact dp/dθ below; nominal anchors.
    let mut breps = Vec::with_capacity(bodies.len());
    let mut plans = Vec::with_capacity(bodies.len());
    let mut nominal = Vec::with_capacity(bodies.len());
    for body in bodies {
        let brep = (body.build)(theta);
        let plan = capture_plan(&brep, &body.tess)?;
        let seam0 = evaluate_with_sensitivity(&brep, &plan, &ParamSeeding::new())?;
        let props = mass_properties(&seam0.positions, &seam0.triangles, body.seam_density());
        nominal.push(BodyMassProps::from_seam(&props));
        breps.push(brep);
        plans.push(plan);
    }
    let anchors0 = anchor_map(theta);

    // Primal objective at the nominal properties and anchors.
    let j0 = rollout(&nominal, &anchors0);

    // 2. ∂J/∂p per body by central FD on the 10 mass-property scalars.
    //    Each body is perturbed independently; the others hold their nominal
    //    values, so the estimate is the true partial.
    let mut djdp: Vec<[f64; 10]> = Vec::with_capacity(bodies.len());
    for (i, nom) in nominal.iter().enumerate() {
        let steps = fd.steps_for(nom);
        let mut grad_p = [0.0f64; 10];
        let mut work = nominal.clone();
        for j in 0..10 {
            let h = steps[j];
            let mut sp = nom.scalars();
            sp[j] += h;
            work[i] = BodyMassProps::from_scalars(sp);
            let jp = rollout(&work, &anchors0);

            let mut sm = nom.scalars();
            sm[j] -= h;
            work[i] = BodyMassProps::from_scalars(sm);
            let jm = rollout(&work, &anchors0);

            grad_p[j] = (jp - jm) / (2.0 * h);
        }
        // Restore body i for the next body's perturbations.
        work[i] = *nom;
        djdp.push(grad_p);
    }

    // 2b. ∂J/∂a per anchor scalar, same central-FD-rollout pattern.
    let mut djda = vec![0.0f64; anchors0.len()];
    for (a, g) in djda.iter_mut().enumerate() {
        let h = anchor_fd.value_abs;
        let mut work = anchors0.clone();
        work[a] = anchors0[a] + h;
        let jp = rollout(&nominal, &work);
        work[a] = anchors0[a] - h;
        let jm = rollout(&nominal, &work);
        *g = (jp - jm) / (2.0 * h);
    }

    // 3 + 4. Exact dp/dθ from the seam, contracted with ∂J/∂p and summed
    //         over bodies; then the anchor channel — da/dθ by central FD on
    //         the (geometry-free) anchor map, contracted with ∂J/∂a.
    let mut gradient = vec![0.0f64; n];
    for (i, body) in bodies.iter().enumerate() {
        for (k, g) in gradient.iter_mut().enumerate() {
            let seeding = (body.seeding_for)(&breps[i], theta, k)?;
            let seam_k = evaluate_with_sensitivity(&breps[i], &plans[i], &seeding)?;
            let (_props, dprops) = mass_properties_with_derivative(&seam_k, body.seam_density());
            let dp = BodyMassProps::deriv_scalars(&dprops);
            let mut acc = 0.0;
            for j in 0..10 {
                acc += djdp[i][j] * dp[j];
            }
            *g += acc;
        }
    }
    if !anchors0.is_empty() {
        for (k, g) in gradient.iter_mut().enumerate() {
            let h = (anchor_fd.theta_rel * theta[k].abs()).max(anchor_fd.theta_min);
            let mut tp = theta.to_vec();
            tp[k] += h;
            let ap = anchor_map(&tp);
            let mut tm = theta.to_vec();
            tm[k] -= h;
            let am = anchor_map(&tm);
            debug_assert_eq!(
                ap.len(),
                anchors0.len(),
                "anchor map length must be θ-invariant"
            );
            for (a, dj) in djda.iter().enumerate() {
                *g += dj * (ap[a] - am[a]) / (2.0 * h);
            }
        }
    }

    Ok((j0, gradient))
}

/// A surface-dependent objective term on one body's frozen-plan mesh.
///
/// Given the seam node positions (mm, CAD frame) and the frozen triangles,
/// return the term's value and its analytic node gradient `∂J_surf/∂x`
/// (J-units per mm, one vector per node). This is the **contact skin** of the
/// M8 factorization: any objective contribution that reads the *surface*
/// rather than the mass properties — a ground-clearance penalty or a
/// penetration term. (Contact *dynamics* have their own entry point:
/// [`contact_rollout_gradient`].)
pub type SurfaceTerm<'a> = Box<dyn Fn(&[CadPoint3], &[[u32; 3]]) -> (f64, Vec<CadVec3>) + 'a>;

/// Price a mesh cotangent through the CAD seam: given `∂J/∂x` on `body`'s
/// frozen-plan nodes (mm frame), return the per-parameter `dJ/dθ`
/// contribution.
///
/// One M5 pullback ([`evaluate_with_pullback`]) prices the whole cotangent;
/// each parameter then costs only a seeding synthesis and a contraction —
/// the reverse-mode economics the skin was designed around. This is the raw
/// entry point a **contact adjoint** plugs into: whatever produces `∂J/∂x`
/// on the collision surface (an analytic penalty, or the phyz contact
/// adjoint that [`contact_rollout_gradient`] drives), this function turns it
/// into CAD-parameter sensitivities.
pub fn surface_gradient(
    body: &DiffBody,
    theta: &[f64],
    djdx: &[CadVec3],
) -> Result<Vec<f64>, DiffError> {
    let brep = (body.build)(theta);
    let plan = capture_plan(&brep, &body.tess)?;
    let cots = evaluate_with_pullback(&brep, &plan, djdx)?;
    let mut out = Vec::with_capacity(theta.len());
    for k in 0..theta.len() {
        let seeding = (body.seeding_for)(&brep, theta, k)?;
        out.push(cots.contract(&seeding));
    }
    Ok(out)
}

/// [`rollout_gradient`] with the **surface skin**: `(J, dJ/dθ)` for a
/// composite objective
///
/// ```text
/// J = J_dyn(mass props)  +  Σ_bodies J_surf(surface nodes)
/// ```
///
/// where `J_dyn` is the contact-free rollout (mass-property channel, exactly
/// as [`rollout_gradient`]) and each `J_surf` reads the body's tessellated
/// surface directly. The gradient sums both channels:
///
/// ```text
/// dJ/dθ = Σ ∂J/∂p·dp/dθ  +  Σ pullback(∂J_surf/∂x)·seeding
/// ```
///
/// The surface channel is **exact** (analytic node gradient through the M5
/// pullback — no FD anywhere in it) and costs one pullback per body
/// regardless of the θ dimension.
///
/// `surface_terms` is parallel to `bodies`; `None` for bodies without a
/// surface term. What this does **not** do: surface-dependent *dynamics*.
/// If contact forces act during the rollout, use
/// [`contact_rollout_gradient`], which obtains `∂J_dyn/∂x` from the phyz
/// contact adjoint and feeds it through the same pullback (documented in
/// `docs/differentiable-seam-m8.md` and `-m11.md`).
pub fn rollout_gradient_with_surface(
    bodies: &[DiffBody],
    surface_terms: &[Option<SurfaceTerm>],
    rollout: &impl Fn(&[BodyMassProps]) -> f64,
    theta: &[f64],
    fd: &MassPropFdSteps,
) -> Result<(f64, Vec<f64>), DiffError> {
    assert_eq!(
        surface_terms.len(),
        bodies.len(),
        "one surface-term slot per body"
    );
    let n = theta.len();

    // 1. Nominal seam per body; keep positions for the surface terms and the
    //    B-rep/plan for both channels' seam passes.
    let mut breps = Vec::with_capacity(bodies.len());
    let mut plans = Vec::with_capacity(bodies.len());
    let mut nominal = Vec::with_capacity(bodies.len());
    let mut j_surf = 0.0;
    let mut cotangents = Vec::with_capacity(bodies.len());
    for (body, term) in bodies.iter().zip(surface_terms) {
        let brep = (body.build)(theta);
        let plan = capture_plan(&brep, &body.tess)?;
        let seam0 = evaluate_with_sensitivity(&brep, &plan, &ParamSeeding::new())?;
        let props = mass_properties(&seam0.positions, &seam0.triangles, body.seam_density());
        nominal.push(BodyMassProps::from_seam(&props));
        // Surface term: value into J, node gradient priced by one pullback.
        cotangents.push(match term {
            Some(t) => {
                let (value, djdx) = t(&seam0.positions, &seam0.triangles);
                j_surf += value;
                Some(evaluate_with_pullback(&brep, &plan, &djdx)?)
            }
            None => None,
        });
        breps.push(brep);
        plans.push(plan);
    }

    let j0 = rollout(&nominal) + j_surf;

    // 2. ∂J_dyn/∂p per body by central FD (J_surf does not depend on p, so
    //    perturbing the scalars probes exactly the dynamic term).
    let mut djdp: Vec<[f64; 10]> = Vec::with_capacity(bodies.len());
    for (i, nom) in nominal.iter().enumerate() {
        let steps = fd.steps_for(nom);
        let mut grad_p = [0.0f64; 10];
        let mut work = nominal.clone();
        for j in 0..10 {
            let h = steps[j];
            let mut sp = nom.scalars();
            sp[j] += h;
            work[i] = BodyMassProps::from_scalars(sp);
            let jp = rollout(&work);

            let mut sm = nom.scalars();
            sm[j] -= h;
            work[i] = BodyMassProps::from_scalars(sm);
            let jm = rollout(&work);

            grad_p[j] = (jp - jm) / (2.0 * h);
        }
        work[i] = *nom;
        djdp.push(grad_p);
    }

    // 3. Per (body, parameter): the mass-property channel (forward seam) and
    //    the surface channel (contraction of the body's one pullback).
    let mut gradient = vec![0.0f64; n];
    for (i, body) in bodies.iter().enumerate() {
        for (k, g) in gradient.iter_mut().enumerate() {
            let seeding = (body.seeding_for)(&breps[i], theta, k)?;
            let seam_k = evaluate_with_sensitivity(&breps[i], &plans[i], &seeding)?;
            let (_props, dprops) = mass_properties_with_derivative(&seam_k, body.seam_density());
            let dp = BodyMassProps::deriv_scalars(&dprops);
            let mut acc = 0.0;
            for j in 0..10 {
                acc += djdp[i][j] * dp[j];
            }
            if let Some(cots) = &cotangents[i] {
                acc += cots.contract(&seeding);
            }
            *g += acc;
        }
    }

    Ok((j0, gradient))
}

/// [`rollout_gradient`] with the **density channel**: `(J, dJ/dθ)` where θ
/// reaches the rollout through each body's *material density* rather than
/// its geometry.
///
/// This is the part-scale half of the atoms → continuum bridge: an upstream
/// material model — in vcad, `vcad-kernel-atoms::homogenize` — maps θ (a
/// lattice constant, a composition fraction) to each body's mass density
/// `ρᵢ(θ)` and its derivative `dρᵢ/dθ`; the geometry is **fixed**. For a
/// homogeneous body every mass-property scalar is linear in ρ (mass ∝ ρ,
/// COM independent of ρ, inertia ∝ ρ), so the material-side factor is exact:
///
/// ```text
/// dp/dρ = [m/ρ, 0, 0, 0, I_xx/ρ, …]        (exact, no FD)
/// dJ/dθ_k = Σ_bodies (∂J/∂p_i · dp_i/dρ_i) · dρᵢ/dθ_k
/// ```
///
/// `∂J/∂p` comes from the same central-FD pattern as [`rollout_gradient`],
/// minus the three COM scalars whose `dp/dρ` is exactly zero — 7 scalars ×
/// 2 = 14 rollouts per body; no CAD rebuild, seam pass, or re-tessellation
/// happens here at all, which is why this channel is infallible where the
/// geometry channels return [`DiffError`].
///
/// `nominal[i]` must be body `i`'s mass properties evaluated **at**
/// `rho0[i]` (e.g. from [`nominal_mass_props`] with
/// `density_kg_m3 = rho0[i]`), and `drho_dtheta[i]` is `dρᵢ/dθ` with one
/// entry per θ component. The rollout closure contract (pure, deterministic,
/// contact-free) is identical to [`rollout_gradient`].
pub fn rollout_gradient_via_density(
    nominal: &[BodyMassProps],
    rho0: &[f64],
    drho_dtheta: &[Vec<f64>],
    rollout: &impl Fn(&[BodyMassProps]) -> f64,
    fd: &MassPropFdSteps,
) -> (f64, Vec<f64>) {
    assert_eq!(nominal.len(), rho0.len(), "one density per body");
    assert_eq!(
        nominal.len(),
        drho_dtheta.len(),
        "one density-derivative row per body"
    );
    // Validate the full shape up front — before any rollout runs or the
    // gradient is allocated — so a ragged input fails with a per-body
    // diagnostic instead of mid-accumulation. With zero bodies the θ
    // dimension is unknowable and the gradient is legitimately empty.
    let n_theta = drho_dtheta.first().map(|r| r.len()).unwrap_or(0);
    for (i, row) in drho_dtheta.iter().enumerate() {
        assert_eq!(
            row.len(),
            n_theta,
            "density-derivative row {i} must share the θ dimension {n_theta}"
        );
    }

    let j0 = rollout(nominal);

    // ∂J/∂p per body by central FD on the density-coupled scalars, exactly
    // as in the geometry channels.
    let mut gradient = vec![0.0f64; n_theta];
    for (i, nom) in nominal.iter().enumerate() {
        assert!(rho0[i] > 0.0, "density must be positive");
        let steps = fd.steps_for(nom);
        let mut work = nominal.to_vec();
        // dJ/dρ_i = Σ_j ∂J/∂p_j · dp_j/dρ_i, with the exact linear factor
        // dp/dρ = p/ρ for mass and inertia and 0 for the COM.
        let p = nom.scalars();
        let mut dj_drho = 0.0;
        for j in 0..10 {
            if (1..=3).contains(&j) {
                // COM components are density-independent: dp/dρ = 0, so
                // their rollouts are skipped entirely.
                continue;
            }
            let dp_drho = p[j] / rho0[i];
            let h = steps[j];
            let mut sp = p;
            sp[j] += h;
            work[i] = BodyMassProps::from_scalars(sp);
            let jp = rollout(&work);

            let mut sm = p;
            sm[j] -= h;
            work[i] = BodyMassProps::from_scalars(sm);
            let jm = rollout(&work);

            dj_drho += (jp - jm) / (2.0 * h) * dp_drho;
        }
        work[i] = *nom;
        for (k, g) in gradient.iter_mut().enumerate() {
            *g += dj_drho * drho_dtheta[i][k];
        }
    }

    (j0, gradient)
}

// ---------------------------------------------------------------------------
// Adjoint-backed rollout gradients (phyz trajectory adjoint for ∂J/∂p)
// ---------------------------------------------------------------------------

/// Low-level phyz interop — the one place `phyz` types cross the crate's
/// public surface.
///
/// vcad-kernel-physics otherwise wraps phyz completely (no phyz types in
/// public APIs). The adjoint-backed gradients are the exception: callers
/// describe the rollout by *building a phyz model themselves*, so
/// [`interop::AdjointRolloutSpec`] necessarily carries
/// `phyz::Model` / `phyz::math::DVec`. Anything imported from here couples
/// your code to phyz's API; the wrapped entry points
/// ([`rollout_gradient_adjoint`], [`contact_rollout_gradient`]) are the
/// supported surface.
pub mod interop {
    use std::sync::Arc;

    use super::BodyMassProps;

    /// A structured, adjoint-differentiable rollout description.
    ///
    /// [`rollout_gradient`](super::rollout_gradient) takes an opaque closure
    /// and therefore has to probe
    /// `∂J/∂p` by finite differences (~20 re-simulations per body). This spec
    /// exposes the rollout's structure — model builder, initial state, open-loop
    /// control schedule, final-state objective with its analytic gradient — so
    /// the phyz **trajectory adjoint** ([`phyz::diff`]) can compute `∂J/∂p`
    /// exactly in one backward pass.
    ///
    /// # Contract
    ///
    /// - `build_model` must install body `i`'s inertia **exactly as**
    ///   `props[i].to_spatial_inertia()` — i.e. in the CAD body frame. Mounting
    ///   (rotating/offsetting the body relative to its joint) belongs in the
    ///   joint's `parent_to_joint` and `axis`, *not* in a transformed inertia;
    ///   a transformed inertia would silently decouple `∂J/∂p` from the seam's
    ///   `dp/dθ`. Violations are detected and panic.
    /// - Joints: single-DOF (revolute/prismatic) + fixed, like phyz's adjoint.
    /// - `ctrl` is open-loop: it must not read the state.
    /// - The rollout the adjoint differentiates is phyz's semi-implicit Euler
    ///   (`v' = v + dt·qdd`, `q' = q + dt·v'`) — the same integrator the m8
    ///   test rollouts hand-roll, so the FD path and the adjoint path price the
    ///   same trajectory.
    #[allow(clippy::type_complexity)]
    pub struct AdjointRolloutSpec {
        /// Build the (contact-free unless used via
        /// [`super::contact_rollout_gradient`]) phyz model at the given mass
        /// properties. See the type-level contract.
        pub build_model: Arc<dyn Fn(&[BodyMassProps]) -> phyz::Model + Send + Sync>,
        /// Initial joint positions (length `nq`).
        pub q0: Vec<f64>,
        /// Initial joint velocities (length `nv`).
        pub v0: Vec<f64>,
        /// Number of integration steps.
        pub steps: usize,
        /// Open-loop control at step `t` (length `nv`).
        pub ctrl: Arc<dyn Fn(usize) -> phyz::math::DVec + Send + Sync>,
        /// Final-state objective `J = g(q_T, v_T)`.
        pub objective_value: Arc<dyn Fn(&[f64], &[f64]) -> f64 + Send + Sync>,
        /// Analytic objective gradient `(∂g/∂q_T, ∂g/∂v_T)`.
        pub objective_gradient: Arc<dyn Fn(&[f64], &[f64]) -> (Vec<f64>, Vec<f64>) + Send + Sync>,
    }
}

use interop::AdjointRolloutSpec;

/// Check the [`AdjointRolloutSpec::build_model`] contract: the model's body
/// inertias must be the props verbatim (CAD body frame, no mount transform
/// baked in).
fn assert_model_matches_props(model: &phyz::Model, props: &[BodyMassProps]) {
    assert_eq!(
        model.nbodies(),
        props.len(),
        "build_model must create exactly one phyz body per DiffBody"
    );
    for (i, (body, p)) in model.bodies.iter().zip(props).enumerate() {
        let si = p.to_spatial_inertia();
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-12);
        let com_ok = close(body.inertia.com.x, si.com.x)
            && close(body.inertia.com.y, si.com.y)
            && close(body.inertia.com.z, si.com.z);
        let mut inertia_ok = close(body.inertia.mass, si.mass);
        for r in 0..3 {
            for c in 0..3 {
                inertia_ok &= close(body.inertia.inertia.get(r, c), si.inertia.get(r, c));
            }
        }
        assert!(
            com_ok && inertia_ok,
            "build_model contract violation on body {i}: the phyz body's \
             spatial inertia differs from props[{i}].to_spatial_inertia(). \
             Mount transforms belong in the joint (parent_to_joint / axis), \
             not in a transformed inertia."
        );
    }
}

/// Shared core of the adjoint-backed gradients: a three-node tang graph
///
/// ```text
/// θ ─┬─ MassProps ──────┐
///    └─ SeamEval (skin) ─┴─ Rollout ── J
/// ```
///
/// whose reverse pass `TensorGraph::backward` derives. Each node is an
/// operator with an exact adjoint: `MassProps` pulls `∂J/∂p` back through
/// the seam's dual-number `dp/dθ`, `SeamEval` pulls the vertex cotangent
/// back through the frozen-plan pullback, and `Rollout` is the phyz
/// trajectory adjoint. The two θ channels sum where the graph joins them;
/// nothing here is a hand chain rule. `SeamEval` appears only with contact
/// skins.
fn adjoint_gradient_core(
    bodies: &[DiffBody],
    spec: &AdjointRolloutSpec,
    contact: Option<(&ContactConfig, &[usize])>,
    theta: &[f64],
) -> Result<(f64, Vec<f64>), DiffError> {
    use tang_compile::tensor::TensorGraph;

    let seam = Arc::new(nodes::FrozenSeam::capture(
        bodies,
        theta,
        contact.map_or(&[][..], |(_, s)| s),
    )?);
    let nb = bodies.len();
    let n = theta.len();

    let mut g = TensorGraph::new();
    let th = g.input(&[n]);
    let props = g.operator(Arc::new(nodes::MassProps(seam.clone())), &[th], &[10 * nb]);
    let mut rollout_in = vec![props];
    if contact.is_some() {
        let skin = g.operator(
            Arc::new(nodes::SeamEval(seam.clone())),
            &[th],
            &[3 * seam.skin_len()],
        );
        rollout_in.push(skin);
    }
    let rollout = Arc::new(nodes::Rollout::new(
        spec,
        contact.map(|(c, _)| *c),
        seam.clone(),
    ));
    let j = g.operator(rollout, &rollout_in, &[1]);
    let dth = g
        .backward(j, &[th])
        .expect("adjoint graph shapes")
        .remove(0);
    let mut out = g.eval(&[theta.to_vec()], &[j, dth]);
    if let Some(e) = seam.take_error() {
        return Err(e);
    }
    let gradient = out.pop().expect("gradient");
    Ok((out[0][0], gradient))
}

/// The operator nodes of [`adjoint_gradient_core`]'s graph.
///
/// `SeamEval` and `MassProps` are nodes of the **frozen plan** captured at
/// θ: their forward evaluates that plan, and they refuse any other θ (a
/// different θ needs a new plan — kernel-diff's contract). Fallible seam
/// work inside a node parks its error in the shared [`FrozenSeam`] and
/// returns NaN; the core surfaces it after evaluation.
mod nodes {
    use std::sync::{Arc, Mutex};

    use tang_compile::tensor::{Operator, ShapeError, TensorGraph, TensorId};
    use vcad_kernel_diff::{
        evaluate_with_pullback, evaluate_with_sensitivity, mass_properties,
        mass_properties_with_derivative, DiffError, ParamSeeding,
    };
    use vcad_kernel_math::{Point3 as CadPoint3, Vec3 as CadVec3};
    use vcad_kernel_primitives::BRepSolid;
    use vcad_kernel_tessellate::frozen::{capture_plan, FrozenPlan};

    use super::interop::AdjointRolloutSpec;
    use super::{assert_model_matches_props, BodyMassProps, ContactConfig, DiffBody, MM_TO_M};

    /// Every body's B-rep, frozen plan, seam and parameter seedings at θ.
    pub(super) struct FrozenSeam {
        theta: Vec<f64>,
        breps: Vec<BRepSolid>,
        plans: Vec<FrozenPlan>,
        /// `seedings[body][k]` for parameter `k`.
        seedings: Vec<Vec<ParamSeeding>>,
        densities: Vec<f64>,
        positions: Vec<Vec<CadPoint3>>,
        nominal: Vec<BodyMassProps>,
        /// Bodies with a collision skin, in skin order.
        skinned: Vec<usize>,
        error: Mutex<Option<DiffError>>,
    }

    // The graph's operators must be `RefUnwindSafe`. B-reps hold
    // `dyn Surface` / `dyn Curve` (no interior mutability in practice) and
    // the rollout holds the caller's closures; a panic inside a node
    // abandons the whole graph, so no broken invariant is ever observed.
    impl std::panic::RefUnwindSafe for FrozenSeam {}
    impl std::panic::RefUnwindSafe for Rollout {}

    impl FrozenSeam {
        pub(super) fn capture(
            bodies: &[DiffBody],
            theta: &[f64],
            skinned: &[usize],
        ) -> Result<Self, DiffError> {
            let mut s = Self {
                theta: theta.to_vec(),
                breps: Vec::with_capacity(bodies.len()),
                plans: Vec::with_capacity(bodies.len()),
                seedings: Vec::with_capacity(bodies.len()),
                densities: Vec::with_capacity(bodies.len()),
                positions: Vec::with_capacity(bodies.len()),
                nominal: Vec::with_capacity(bodies.len()),
                skinned: skinned.to_vec(),
                error: Mutex::new(None),
            };
            for body in bodies {
                let brep = (body.build)(theta);
                let plan = capture_plan(&brep, &body.tess)?;
                let seam0 = evaluate_with_sensitivity(&brep, &plan, &ParamSeeding::new())?;
                let props =
                    mass_properties(&seam0.positions, &seam0.triangles, body.seam_density());
                let seedings = (0..theta.len())
                    .map(|k| (body.seeding_for)(&brep, theta, k))
                    .collect::<Result<Vec<_>, _>>()?;
                s.nominal.push(BodyMassProps::from_seam(&props));
                s.positions.push(seam0.positions);
                s.densities.push(body.seam_density());
                s.seedings.push(seedings);
                s.breps.push(brep);
                s.plans.push(plan);
            }
            Ok(s)
        }

        /// Total skin vertex count.
        pub(super) fn skin_len(&self) -> usize {
            self.skinned.iter().map(|&i| self.positions[i].len()).sum()
        }

        pub(super) fn take_error(&self) -> Option<DiffError> {
            self.error.lock().expect("seam error lock").take()
        }

        fn fail(&self, e: DiffError, len: usize) -> Vec<f64> {
            self.error.lock().expect("seam error lock").get_or_insert(e);
            vec![f64::NAN; len]
        }

        fn check_theta(&self, theta: &[f64]) {
            assert!(
                theta == self.theta.as_slice(),
                "frozen-plan node evaluated away from the θ its plan was captured at"
            );
        }
    }

    /// θ → per-body SI mass properties `[m, c, I]` (10 per body).
    pub(super) struct MassProps(pub(super) Arc<FrozenSeam>);

    impl Operator for MassProps {
        fn name(&self) -> &str {
            "MassProps"
        }

        fn forward(&self, inputs: &[&[f64]]) -> Vec<f64> {
            self.0.check_theta(inputs[0]);
            self.0.nominal.iter().flat_map(|p| p.scalars()).collect()
        }

        fn backward(
            &self,
            g: &mut TensorGraph,
            inputs: &[TensorId],
            _output: TensorId,
            cot: TensorId,
        ) -> Result<Vec<Option<TensorId>>, ShapeError> {
            let n = g.shape(inputs[0])[0];
            let op = Arc::new(MassPropsVjp(self.0.clone()));
            Ok(vec![Some(g.operator(op, &[cot], &[n]))])
        }
    }

    /// `cot_p ↦ Σ_body cot_p · dp/dθ_k`, with `dp/dθ_k` from dual numbers
    /// through the seam (exact).
    struct MassPropsVjp(Arc<FrozenSeam>);

    impl Operator for MassPropsVjp {
        fn name(&self) -> &str {
            "MassPropsVjp"
        }

        fn forward(&self, inputs: &[&[f64]]) -> Vec<f64> {
            let s = &self.0;
            let cot = inputs[0];
            let n = s.theta.len();
            let mut gradient = vec![0.0f64; n];
            for i in 0..s.breps.len() {
                for (k, g) in gradient.iter_mut().enumerate() {
                    let seam_k = match evaluate_with_sensitivity(
                        &s.breps[i],
                        &s.plans[i],
                        &s.seedings[i][k],
                    ) {
                        Ok(m) => m,
                        Err(e) => return s.fail(e, n),
                    };
                    let (_props, dprops) = mass_properties_with_derivative(&seam_k, s.densities[i]);
                    let dp = BodyMassProps::deriv_scalars(&dprops);
                    let acc: f64 = cot[10 * i..10 * i + 10]
                        .iter()
                        .zip(&dp)
                        .map(|(djdp, dpj)| djdp * dpj)
                        .sum();
                    *g += acc;
                }
            }
            gradient
        }

        fn backward(
            &self,
            _g: &mut TensorGraph,
            _inputs: &[TensorId],
            _output: TensorId,
            _cot: TensorId,
        ) -> Result<Vec<Option<TensorId>>, ShapeError> {
            Ok(vec![None]) // first-order only
        }
    }

    /// θ → the skinned bodies' frozen-plan node positions (mm), flattened.
    pub(super) struct SeamEval(pub(super) Arc<FrozenSeam>);

    impl Operator for SeamEval {
        fn name(&self) -> &str {
            "SeamEval"
        }

        fn forward(&self, inputs: &[&[f64]]) -> Vec<f64> {
            self.0.check_theta(inputs[0]);
            self.0
                .skinned
                .iter()
                .flat_map(|&i| self.0.positions[i].iter().flat_map(|p| [p.x, p.y, p.z]))
                .collect()
        }

        fn backward(
            &self,
            g: &mut TensorGraph,
            inputs: &[TensorId],
            _output: TensorId,
            cot: TensorId,
        ) -> Result<Vec<Option<TensorId>>, ShapeError> {
            let n = g.shape(inputs[0])[0];
            let op = Arc::new(SeamPullback(self.0.clone()));
            Ok(vec![Some(g.operator(op, &[cot], &[n]))])
        }
    }

    /// Per-mm vertex cotangent ↦ `dθ` through one frozen-plan pullback per
    /// skinned body.
    struct SeamPullback(Arc<FrozenSeam>);

    impl Operator for SeamPullback {
        fn name(&self) -> &str {
            "SeamPullback"
        }

        fn forward(&self, inputs: &[&[f64]]) -> Vec<f64> {
            let s = &self.0;
            let n = s.theta.len();
            let mut gradient = vec![0.0f64; n];
            let mut at = 0;
            for &i in &s.skinned {
                let nv = s.positions[i].len();
                let djdx_mm: Vec<CadVec3> = inputs[0][3 * at..3 * (at + nv)]
                    .chunks(3)
                    .map(|c| CadVec3::new(c[0], c[1], c[2]))
                    .collect();
                at += nv;
                let cots = match evaluate_with_pullback(&s.breps[i], &s.plans[i], &djdx_mm) {
                    Ok(c) => c,
                    Err(e) => return s.fail(e, n),
                };
                for (k, g) in gradient.iter_mut().enumerate() {
                    *g += cots.contract(&s.seedings[i][k]);
                }
            }
            gradient
        }

        fn backward(
            &self,
            _g: &mut TensorGraph,
            _inputs: &[TensorId],
            _output: TensorId,
            _cot: TensorId,
        ) -> Result<Vec<Option<TensorId>>, ShapeError> {
            Ok(vec![None])
        }
    }

    /// The phyz rollout `(props[, skin mm]) → J`. Its forward runs phyz's
    /// trajectory adjoint (one forward + one backward sweep) and keeps
    /// `∂J/∂p` and `∂J/∂x` for the backward nodes.
    #[allow(clippy::type_complexity)]
    pub(super) struct Rollout {
        build_model: Arc<dyn Fn(&[BodyMassProps]) -> phyz::Model + Send + Sync>,
        ctrl: Arc<dyn Fn(usize) -> phyz::math::DVec + Send + Sync>,
        objective_value: Arc<dyn Fn(&[f64], &[f64]) -> f64 + Send + Sync>,
        objective_gradient: Arc<dyn Fn(&[f64], &[f64]) -> (Vec<f64>, Vec<f64>) + Send + Sync>,
        q0: Vec<f64>,
        v0: Vec<f64>,
        steps: usize,
        contact: Option<ContactConfig>,
        seam: Arc<FrozenSeam>,
        /// `[∂J/∂p flat, ∂J/∂x per mm flat]` from the last forward.
        grads: Arc<Mutex<[Vec<f64>; 2]>>,
    }

    impl Rollout {
        pub(super) fn new(
            spec: &AdjointRolloutSpec,
            contact: Option<ContactConfig>,
            seam: Arc<FrozenSeam>,
        ) -> Self {
            Self {
                build_model: spec.build_model.clone(),
                ctrl: spec.ctrl.clone(),
                objective_value: spec.objective_value.clone(),
                objective_gradient: spec.objective_gradient.clone(),
                q0: spec.q0.clone(),
                v0: spec.v0.clone(),
                steps: spec.steps,
                contact,
                seam,
                grads: Arc::new(Mutex::new([Vec::new(), Vec::new()])),
            }
        }
    }

    impl Operator for Rollout {
        fn name(&self) -> &str {
            "Rollout"
        }

        fn forward(&self, inputs: &[&[f64]]) -> Vec<f64> {
            // Build the model and hold it to the body-frame contract.
            let props: Vec<BodyMassProps> = inputs[0]
                .chunks(10)
                .map(|c| BodyMassProps::from_scalars(c.try_into().expect("10 scalars")))
                .collect();
            let model = (self.build_model)(&props);
            assert_model_matches_props(&model, &props);

            // Collision skins: frozen-plan nodes, mm → m.
            let mut meshes = Vec::new();
            if let Some(skin) = inputs.get(1) {
                let mut at = 0;
                for &i in &self.seam.skinned {
                    let nv = self.seam.positions[i].len();
                    meshes.push(phyz::diff::CollisionMesh {
                        body: i,
                        vertices: skin[3 * at..3 * (at + nv)]
                            .chunks(3)
                            .map(|p| {
                                phyz::math::Vec3::new(
                                    p[0] * MM_TO_M,
                                    p[1] * MM_TO_M,
                                    p[2] * MM_TO_M,
                                )
                            })
                            .collect(),
                    });
                    at += nv;
                }
            }
            let ground = self.contact.map(|cfg| phyz::diff::GroundContact {
                height: cfg.ground_height_m,
                stiffness: cfg.stiffness,
                damping: cfg.damping,
            });
            let objective = phyz::diff::FinalStateObjective {
                value: &*self.objective_value,
                gradient: &*self.objective_gradient,
            };
            let rollout = phyz::diff::AdjointRollout {
                model: &model,
                contact: ground.map(|g| phyz::diff::ContactSetup {
                    ground: g,
                    meshes: &meshes,
                }),
                q0: self.q0.clone(),
                v0: self.v0.clone(),
                steps: self.steps,
                ctrl: &*self.ctrl,
            };
            let adj = phyz::diff::adjoint_rollout_gradient(&rollout, &objective);
            // ∂J/∂x per metre (body frame) → per mm
            let dx: Vec<f64> = adj
                .d_vertices
                .iter()
                .flatten()
                .flat_map(|g| [g.x * MM_TO_M, g.y * MM_TO_M, g.z * MM_TO_M])
                .collect();
            let dp: Vec<f64> = adj.d_inertia.iter().flatten().copied().collect();
            *self.grads.lock().expect("rollout cache") = [dp, dx];
            vec![adj.objective]
        }

        fn backward(
            &self,
            g: &mut TensorGraph,
            inputs: &[TensorId],
            output: TensorId,
            cot: TensorId,
        ) -> Result<Vec<Option<TensorId>>, ShapeError> {
            // `output` is an input of the gradient nodes so they evaluate
            // after this node's forward has filled the cache.
            let mut out = Vec::with_capacity(inputs.len());
            for (which, &x) in inputs.iter().enumerate() {
                let shape = g.shape(x).to_vec();
                let op = Arc::new(RolloutGrad {
                    which,
                    grads: self.grads.clone(),
                });
                out.push(Some(g.operator(op, &[output, cot], &shape)));
            }
            Ok(out)
        }
    }

    /// `(J, cot_J) ↦ cot_J · ∂J/∂(input which)`, read from the rollout's
    /// cache.
    struct RolloutGrad {
        which: usize,
        grads: Arc<Mutex<[Vec<f64>; 2]>>,
    }

    impl Operator for RolloutGrad {
        fn name(&self) -> &str {
            "RolloutGrad"
        }

        fn forward(&self, inputs: &[&[f64]]) -> Vec<f64> {
            let c = inputs[1][0];
            let grads = self.grads.lock().expect("rollout cache");
            grads[self.which].iter().map(|&d| c * d).collect()
        }

        fn backward(
            &self,
            _g: &mut TensorGraph,
            _inputs: &[TensorId],
            _output: TensorId,
            _cot: TensorId,
        ) -> Result<Vec<Option<TensorId>>, ShapeError> {
            Ok(vec![None, None])
        }
    }
}

/// [`rollout_gradient`] with the finite-difference `∂J/∂p` factor replaced
/// by the **phyz trajectory adjoint**: `(J, dJ/dθ)` where both factors of
/// the M8 chain are exact —
///
/// ```text
/// dJ/dθ = Σ_bodies  ∂J/∂p_body (adjoint, exact) · dp_body/dθ (seam, exact)
/// ```
///
/// The FD-based [`rollout_gradient`] remains available as the fallback for
/// rollouts that cannot be expressed as an [`AdjointRolloutSpec`] (closed
/// over arbitrary code, state-feedback control, multi-DOF joints, or a phyz
/// version without the adjoint).
pub fn rollout_gradient_adjoint(
    bodies: &[DiffBody],
    spec: &AdjointRolloutSpec,
    theta: &[f64],
) -> Result<(f64, Vec<f64>), DiffError> {
    adjoint_gradient_core(bodies, spec, None, theta)
}

/// Ground-plane contact configuration for [`contact_rollout_gradient`], in
/// phyz (SI) units.
#[derive(Debug, Clone, Copy)]
pub struct ContactConfig {
    /// World z of the ground plane (m).
    pub ground_height_m: f64,
    /// Penalty stiffness per vertex (N/m).
    pub stiffness: f64,
    /// Penalty damping per vertex (N·s/m).
    pub damping: f64,
}

/// `(J, dJ/dθ)` for a rollout in which **contact forces act during the
/// dynamics** — the boundary the M8 contract left open.
///
/// Each body listed in `skinned` collides with the ground plane through its
/// own frozen-plan seam mesh (mm → m), under phyz's differentiable
/// per-vertex penalty model. The phyz trajectory adjoint returns both
/// `∂J/∂p` and the vertex cotangent `∂J/∂x`; the gradient sums the two
/// channels the seam already knows how to price:
///
/// ```text
/// dJ/dθ = Σ ∂J/∂p·dp/dθ  +  Σ pullback(∂J/∂x)·seeding
/// ```
///
/// The surface channel goes through [`evaluate_with_pullback`] exactly as
/// [`surface_gradient`] — this function *is* the contact adjoint plugged
/// into that seam. Both factors of both channels are analytic; there is no
/// finite difference anywhere in the chain.
///
/// The forward model being differentiated is phyz's diff rollout (vertex
/// penalty contact, no friction, single-DOF joints, open-loop control) —
/// see `phyz::diff` for the contract. It is **not** the GJK/EPA production
/// contact pipeline.
pub fn contact_rollout_gradient(
    bodies: &[DiffBody],
    spec: &AdjointRolloutSpec,
    contact: &ContactConfig,
    skinned: &[usize],
    theta: &[f64],
) -> Result<(f64, Vec<f64>), DiffError> {
    adjoint_gradient_core(bodies, spec, Some((contact, skinned)), theta)
}

/// The nominal SI mass properties of each body at θ (positions-only seam).
///
/// A convenience for callers that want the primal properties — to build the
/// rollout model at θ, or to check a target QoI — without the gradient.
pub fn nominal_mass_props(
    bodies: &[DiffBody],
    theta: &[f64],
) -> Result<Vec<BodyMassProps>, DiffError> {
    let mut out = Vec::with_capacity(bodies.len());
    for body in bodies {
        let brep = (body.build)(theta);
        let plan = capture_plan(&brep, &body.tess)?;
        let seam = evaluate_with_sensitivity(&brep, &plan, &ParamSeeding::new())?;
        let props = mass_properties(&seam.positions, &seam.triangles, body.seam_density());
        out.push(BodyMassProps::from_seam(&props));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spatial_inertia_roundtrip() {
        let p = BodyMassProps {
            mass: 2.0,
            com: [0.1, -0.2, 0.3],
            inertia: [1.0, 2.0, 3.0, 0.1, 0.2, 0.3],
        };
        let si = p.to_spatial_inertia();
        assert!((si.mass - 2.0).abs() < 1e-15);
        assert!((si.com.x - 0.1).abs() < 1e-15);
        // Symmetric off-diagonals mirror into the matrix.
        assert!((si.inertia[(0, 1)] - 0.1).abs() < 1e-15);
        assert!((si.inertia[(1, 0)] - 0.1).abs() < 1e-15);
        assert!((si.inertia[(2, 2)] - 3.0).abs() < 1e-15);
        // Pack / unpack.
        assert_eq!(BodyMassProps::from_scalars(p.scalars()), p);
    }
}
