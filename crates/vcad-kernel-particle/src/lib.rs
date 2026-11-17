#![warn(missing_docs)]

//! Charged-particle optics for the vcad kernel (M0).
//!
//! Models axisymmetric electrostatic devices — fusors, shielded-grid IEC
//! machines, ion traps, electron guns — as **vacuum-field single-particle
//! optics**: solve the electrode potential, trace charged particles through
//! it, and score the electrode geometry with figures of merit.
//!
//! The pipeline:
//!
//! 1. [`device::Device`] — electrodes as wire rings + a grounded chamber,
//!    geometry in millimeters (vcad convention), potentials in volts,
//!    optional ampere-turns per ring for magnetic self-shielding.
//! 2. [`poisson::solve`] — axisymmetric Laplace solver (finite difference,
//!    SOR) for the potential φ(r, z) and field E = −∇φ.
//! 3. [`field::FieldMap`] — E from the grid + analytic B from every
//!    current-carrying ring (complete elliptic integrals, exact off-axis).
//! 4. [`trace`] — Boris pusher with adaptive substepping; particles
//!    terminate on electrode or wall impact, core passes are counted.
//! 5. [`fom`] — ensemble statistics: interception fraction, mean
//!    recirculation count, effective grid transparency.
//! 6. **Non-axisymmetric electrodes** — [`wire3d::Device3d`] (wires along
//!    circles, arcs and polylines: great-circle and latitude/longitude
//!    globes, stalks) in a grounded sphere, solved by the thin-wire
//!    boundary-element method in [`bem`] (analytic, conservative field;
//!    fail-closed surface-residual gates) and traced by the same Boris
//!    tracer through [`field::FieldSampler`] / [`trace::TraceGeometry`].
//!    [`spec3d::DeviceSpec3d`] is its serde seam.
//! 7. [`optimize`] — finite-difference / adjoint / hybrid maximization of
//!    any figure of merit over electrode parameters; [`electrode_opt`]
//!    drives it over a [`spec::DeviceSpec`]'s named parameters (the
//!    `optimize_electrodes` MCP tool; see `docs/particle-optics-m0.md`).
//!
//! Beyond the axisymmetric pipeline, [`field3d`] superposes coils and
//! permanent magnets (loops, solenoids, disc/ring and block magnets) at
//! arbitrary 3D poses, with a mm/degree JSON seam in [`field3d_spec`].
//!
//! **Scope and honesty.** The core is vacuum-field single-particle optics
//! — the regime where electrode geometry dominates and where every classic
//! electrode-design tool (SIMION lineage) operates. Later milestones add,
//! each as a separate opt-in layer: a space-charge gauge and PIC-lite
//! self-consistency ([`space_charge`]), a discrete adjoint ([`adjoint`]),
//! and, for gas-fed fusors, the charge-exchange chain with neutral-gas
//! collisions, an ionization source, the supply-current partition and a
//! two-sided yield band ([`chain`], M7). There is no kinetic plasma model
//! and no electron space charge; see the milestone ladder in
//! `docs/particle-optics-m0.md`.
//!
//! Units: public geometry is **millimeters** (vcad convention); everything
//! internal is SI (meters, volts, tesla, seconds); particle energies are
//! reported in electron-volts.

pub mod adjoint;
pub mod beam_target;
pub mod bem;
pub mod chain;
pub mod confinement;
pub mod device;
pub mod electrode_opt;
pub mod elliptic;
pub mod field;
pub mod field3d;
pub mod field3d_spec;
pub mod fom;
pub mod optimize;
pub mod plasmoid;
pub mod poisson;
pub mod pops;
pub mod power;
pub mod receipt;
pub mod space_charge;
pub mod spec;
pub mod spec3d;
pub mod trace;
pub mod wire3d;
pub mod xsection;

/// Physical constants (SI).
pub mod constants {
    /// Elementary charge, C.
    pub const ELEMENTARY_CHARGE: f64 = 1.602_176_634e-19;
    /// Deuteron mass, kg.
    pub const DEUTERON_MASS: f64 = 3.343_583_772e-27;
    /// Electron mass, kg.
    pub const ELECTRON_MASS: f64 = 9.109_383_701_5e-31;
    /// Vacuum permeability, T·m/A.
    pub const MU_0: f64 = 1.256_637_062_12e-6;
    /// Vacuum permittivity, F/m.
    pub const EPSILON_0: f64 = 8.854_187_812_8e-12;
}
