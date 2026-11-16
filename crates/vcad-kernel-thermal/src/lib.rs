#![warn(missing_docs)]

//! Heat-conduction FEA for the vcad kernel (M0).
//!
//! Answers the question every enclosure, motor, PSU, and PCB design asks —
//! *how hot does it get?* — with a defensible number instead of a hand rule:
//! steady-state conduction through the part, a temperature field, `T_max`
//! and its location, and a per-source thermal resistance
//! θ = (T_source,max − T_ref) / P.
//!
//! The pipeline:
//!
//! 1. [`model::ThermalModel`] — a bounding box divided into a uniform voxel
//!    grid; materials, power sources, and fixed-temperature reservoirs are
//!    axis-aligned box/cylinder regions painted onto it (later regions win);
//!    boundary conditions on the six domain faces plus a rule for exposed
//!    solid↔void faces.
//! 2. [`solve::solve_steady`] — finite-volume discretization with
//!    **harmonic-mean face conductances** (the series-resistance treatment
//!    that is exact at material interfaces; see [`solve`] for why the
//!    arithmetic mean is wrong there), solved by hand-rolled Jacobi-
//!    preconditioned conjugate gradients, matrix-free. The stopping
//!    criterion is relative to the right-hand-side norm — scale-invariant,
//!    so a milliwatt problem and a kilowatt problem converge to the same
//!    relative quality.
//! 3. [`solve::Solution`] — the temperature field plus the figures of
//!    merit: `T_max` and where it is, per-source θ, and an **energy
//!    balance** (power in vs boundary heat out) whose residual is reported,
//!    never assumed.
//! 4. [`transient::solve_transient`] — backward-Euler time stepping with
//!    per-voxel thermal mass ρc_p·V (M1): step responses, time to
//!    temperature, and a stored-vs-injected energy audit that is an
//!    *identity* of the discretization, reported per run.
//!
//! Conductivity is a per-axis diagonal tensor (M1) — the case a real PCB
//! is (copper planes: ~15–20 W/m·K in-plane vs ~0.3–0.5 through-plane).
//! On the hot_chip benchmark the isotropic idealization under-reads θ_ja
//! by 43%; model the split.
//!
//! 5. **Radiation (M7)** — [`model::Boundary::Radiation`]: gray-diffuse
//!    εσ(T_s⁴ − T_∞⁴) loss (plus an optional parallel film) on any
//!    domain face or the exposed rule, half-cell conduction in series,
//!    optionally inside a two-surface gray enclosure (a grid in a vacuum
//!    chamber). The steady and transient solvers switch to Newton when a
//!    face radiates; the adjoint linearizes at the converged state.
//!
//! **Scope and honesty:** conduction plus surface-to-ambient radiation.
//! No fluid flow — convective cooling enters only as a supplied film
//! coefficient `h` on a boundary, and that `h` is the biggest uncertainty
//! in any prediction this crate makes (natural convection correlations
//! carry ±20–30% on a good day). Radiation is gray-diffuse with a
//! *supplied* emissivity (oxidation can move it 5×), each radiating face
//! sees only its ambient or its enclosure wall — no surface-to-surface
//! exchange between parts of the model, no participating media — and the
//! radiating area is the voxel staircase times a supplied `area_scale`.
//! Off-diagonal conductivity tensors (rotated laminates) are out of
//! scope. See `docs/thermal-m0.md` … `docs/thermal-m7.md` for the
//! milestone ladder.
//!
//! Units: public geometry is **millimeters** (vcad convention); material
//! and boundary coefficients are SI (W/m·K, W/m²·K, watts); temperatures
//! are °C throughout. Conduction is linear, so for conduction-only
//! models any consistent affine temperature unit works; radiation needs
//! absolute temperature, so radiating models **must** use °C (converted
//! internally with T_K = T_C + 273.15).

pub mod adjoint;
// wasm32 solves without the graph (no clock for its solve timing).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub mod graph;
pub mod model;
pub mod receipt;
pub mod solve;
pub mod spec;
pub mod transient;
