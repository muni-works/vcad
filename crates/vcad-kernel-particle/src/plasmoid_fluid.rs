//! Deriving the plasmoid lock: a 2D magnetized fluid model of the glow
//! under the filament's foot (gap #4 of `docs/plasmoid-sim.md`).
//!
//! The reduced model in [`crate::plasmoid`] *fits* the pull that pins the
//! racing foot under a magnet: `μ ∇Φ` with `Φ = ln(1+β²)`, plus a swirl
//! `v_spin`. This module derives both from transport physics instead.
//!
//! # Model
//!
//! A quasi-neutral argon glow sheet (thickness `L_z`) on the dish plane,
//! solved on a Cartesian grid centred on the foot. The magnet enters through
//! the full posed field `B(x, y)` ([`crate::field3d::FieldScene`]) sampled
//! at the glow height.
//!
//! - **Electrons** are magnetized: the 3D drift solves
//!   `v + μ_e v×B = −μ_e(E + ∇p_e/(e n))`, i.e. the mobility tensor
//!   `M_e = μ_e (I + β² b̂b̂ᵀ + β[b̂]×) / (1 + β²)` with `β = μ_e|B|`
//!   ([`electron_mobility_tensor`]). ν is the reduced model's
//!   [`crate::plasmoid::collision_frequency`] (σ = 1e-19 m², T_e = 3 eV),
//!   so β is identical in both models.
//! - **Ions** (Ar⁺ in Ar, reduced mobility 1.53 cm²/V/s) are unmagnetized.
//! - **Particle transport** is two-species ambipolar (local zero current,
//!   in 3D): `Γ = −D_a ∇n − W n ∇T_e` with
//!   `D_a = D_i (I − P) + T_e Q`, `P = μ_i A⁻¹`, `Q = μ_i A⁻¹ M_e`,
//!   `A = μ_i I + M_e` ([`AmbipolarTensors`]). Out-of-plane (dish/gas) loss
//!   is `ν_z = D_a,zz / Λ_z²` with `Λ_z = L_z/π`, taken from the *same*
//!   tensor — directly under the magnet B ∥ ẑ, so that loss is not
//!   magnetically reduced.
//! - **Electron energy** (steady; it relaxes in µs, far faster than the
//!   ms density response):
//!   `∇·(κ ∇T_e) + n [H(x) − n_g K(T_e) − ν_z ε_w(T_e)] = 0`, with the
//!   magnetized in-plane conduction tensor `κ = (5/2) n T_e M_e`,
//!   argon collisional loss `K` and ionization `k_iz` from the Lieberman &
//!   Lichtenberg fits ([`argon`]), and sheath energy per lost pair
//!   `ε_w = 2T_e + T_e/2 + V_s`.
//! - **Heating.** The filament deposits RF power per electron
//!   `H(x) = H₀ [f_bg + (1 − f_bg) exp(−|x − x_f|²/2σ_h²)]` (ohmic heating
//!   by a vertical RF field is B-independent for E ∥ B). `H₀` is fixed by
//!   the Schottky condition: with no magnet the foot is exactly
//!   self-sustaining (net growth 0) — [`FootModel::calibrate`].
//!
//! # What is derived
//!
//! The foot is a localized density spot `n ∝ exp(−|x − x_f|²/2σ²)` (the
//! filament's constriction is *not* modelled — a linear fluid cannot make a
//! filament — so σ is assumed). Given the magnet, the energy equation fixes
//! `T_e(x)`; continuity `∂n/∂t = −∇·Γ + g n`, `g = n_g k_iz(T_e) − ν_z`,
//! then fixes the foot's centroid velocity exactly:
//!
//! ```text
//! dx̄/dt = ∫ (x − x̄) g n dA / N   −   ∫ (x − x̄) ∇·Γ dA / N
//!         (ionization skew)           (transport drift, incl. Hall)
//! ```
//!
//! ([`FootModel::drift`]). That velocity *is* the pull the reduced model
//! writes as `μ ∇Φ` (radial part) and `v_spin` (azimuthal part, from the
//! Hall terms) — tabulated per magnet gap and offset in [`LockMap`] and fed
//! back into [`crate::plasmoid::simulate_scene_with`]. It scales as σ² (a
//! wider spot straddles more of the gain gradient), which is why the fit
//! needed a free μ.
//!
//! # Two channels, and why one barely works
//!
//! Magnetizing electrons barely changes *particle* loss: ions limit it
//! until `μ_e/(1+β²) ~ μ_i`, i.e. `β ≳ β_c = √(μ_e/μ_i) ≈ 19` for argon at
//! any pressure ([`FluidParams::critical_beta`]); the magnet's β ≈ 3 at
//! 1 Torr changes `D_a` by a few percent. Electron *heat* conduction is not
//! ambipolar-limited: `κ⊥ ∝ 1/(1+β²)` directly. So the magnet confines the
//! foot's energy, `T_e` rises on the magnet side, ionization (∝
//! `exp(−15.8/T_e)`) rises with it, and the foot grows toward the magnet.
//! [`FluidParams::magnetize_energy`] / [`FluidParams::magnetize_particles`]
//! switch each channel off to show the split.
//!
//! # The electrostatic alternative
//!
//! [`ConductorState`] / [`es_field_ratio`] estimate how a Ni-plated
//! (conducting) magnet perturbs the RF field at the glow — floating
//! (induced dipole + image) or RF-grounded through the hand (image charge).
//! The local heating multiplier `(E/E₀)²` goes through the *same* foot
//! drift, so the two pulls are compared in the same currency
//! ([`FootModel::drift_with_heating`]).
//!
//! # Still assumed (not derived)
//!
//! Foot width σ and heated-core width σ_h, background heating fraction,
//! sheet thickness L_z, frozen ν (T_e = 3 eV for momentum transfer), the
//! local-ambipolar closure, neglect of the electron convective energy flux
//! and Γ·E work (both O(μ_i/μ_e⊥)), and the race speed itself.

use crate::constants::{ELECTRON_MASS, ELEMENTARY_CHARGE};
use crate::field3d::{FieldScene, FieldSource};
use crate::plasmoid::{collision_frequency, DiscMagnet};

/// Boltzmann constant, J/K.
const K_B: f64 = 1.380_649e-23;
/// One torr in pascal.
pub const TORR_PA: f64 = 133.322;
/// Atomic mass unit, kg.
const AMU: f64 = 1.660_539_066_60e-27;

/// Argon atomic data for the glow (SI rates, energies in volts).
///
/// Rate coefficients are the Maxwellian fits of Lieberman & Lichtenberg,
/// *Principles of Plasma Discharges and Materials Processing*, 2nd ed.,
/// Table 3.3 (valid ≈ 1–7 eV). Treat them as ±[`RATE_REL_UNCERTAINTY`].
pub mod argon {
    use super::{AMU, ELECTRON_MASS};

    /// Argon atom mass, kg.
    pub const MASS_KG: f64 = 39.948 * AMU;
    /// Ionization threshold, V.
    pub const E_IZ_V: f64 = 15.76;
    /// Lumped excitation threshold, V.
    pub const E_EX_V: f64 = 12.14;
    /// Relative uncertainty assigned to the fitted rate coefficients.
    pub const RATE_REL_UNCERTAINTY: f64 = 0.3;
    /// Reduced mobility of Ar⁺ in Ar at low E/N, m²/(V·s) at
    /// [`LOSCHMIDT_M3`] (≈ 1.53 cm²/V/s; Ellis et al. tables).
    pub const ION_REDUCED_MOBILITY: f64 = 1.53e-4;
    /// Loschmidt number (273.15 K, 1 atm), m⁻³.
    pub const LOSCHMIDT_M3: f64 = 2.686_780_111e25;
    /// Lower edge of the rate fits' validity, V.
    pub const FIT_TE_MIN_V: f64 = 1.0;
    /// Upper edge of the rate fits' validity, V.
    pub const FIT_TE_MAX_V: f64 = 7.0;

    /// Ionization rate coefficient `k_iz(T_e)`, m³/s.
    pub fn k_iz(te_v: f64) -> f64 {
        2.34e-14 * te_v.powf(0.59) * (-17.44 / te_v).exp()
    }

    /// Lumped excitation rate coefficient, m³/s.
    pub fn k_ex(te_v: f64) -> f64 {
        2.48e-14 * te_v.powf(0.33) * (-12.78 / te_v).exp()
    }

    /// Elastic momentum-transfer rate coefficient, m³/s.
    pub fn k_el(te_v: f64) -> f64 {
        let l = te_v.ln();
        2.336e-14 * te_v.powf(1.609) * (0.0618 * l * l - 0.1171 * l * l * l).exp()
    }

    /// Electron energy lost to collisions per electron per unit gas
    /// density, V·m³/s: `k_iz E_iz + k_ex E_ex + k_el (3 m_e/M) T_e`.
    pub fn collisional_loss(te_v: f64) -> f64 {
        k_iz(te_v) * E_IZ_V
            + k_ex(te_v) * E_EX_V
            + k_el(te_v) * 3.0 * ELECTRON_MASS / MASS_KG * te_v
    }

    /// Energy carried to a wall per lost electron–ion pair, V:
    /// `2T_e` (electron) `+ T_e/2 + V_s` (ion; Bohm presheath + floating
    /// sheath, `V_s = (T_e/2) ln(M / 2π m_e)`).
    pub fn wall_energy_per_pair(te_v: f64) -> f64 {
        let vs = 0.5 * (MASS_KG / (2.0 * std::f64::consts::PI * ELECTRON_MASS)).ln();
        te_v * (2.5 + vs)
    }
}

/// Why a fluid solve refused to produce a number.
#[derive(Debug, Clone, PartialEq)]
pub enum FluidError {
    /// A linear solve did not converge.
    LinearSolve {
        /// Which system.
        what: &'static str,
        /// Iterations spent.
        iterations: usize,
        /// Final relative residual.
        residual: f64,
    },
    /// The nonlinear energy iteration did not converge.
    EnergyIteration {
        /// Final max |ΔT_e|, V.
        last_change_v: f64,
    },
    /// The Schottky (self-sustainment) calibration found no root.
    Calibration(String),
    /// Inputs out of range.
    BadInput(String),
}

impl std::fmt::Display for FluidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LinearSolve {
                what,
                iterations,
                residual,
            } => write!(
                f,
                "{what}: linear solve did not converge ({iterations} iterations, residual {residual:.2e})"
            ),
            Self::EnergyIteration { last_change_v } => write!(
                f,
                "electron energy iteration did not converge (last ΔT_e = {last_change_v:.2e} V)"
            ),
            Self::Calibration(s) => write!(f, "heating calibration failed: {s}"),
            Self::BadInput(s) => write!(f, "bad input: {s}"),
        }
    }
}

impl std::error::Error for FluidError {}

/// How the RF power reaches the electrons under the foot. The RF field is
/// vertical (dish → filament), so the relevant conductivity is
/// `σ_zz ∝ M_e,zz = μ_e (1 + β² b̂_z²)/(1 + β²)`: unity directly under the
/// magnet (E ∥ B) but reduced on its flanks, where B tilts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatingDrive {
    /// B-independent heating per electron (the E ∥ B limit everywhere).
    Uniform,
    /// Fixed RF field: heating per electron `∝ σ_zz ∝ (1+β²b̂_z²)/(1+β²)`.
    FieldDriven,
    /// Fixed RF current density (sheath/circuit-limited): heating per
    /// electron `∝ J²/σ_zz ∝ (1+β²)/(1+β²b̂_z²)`.
    CurrentDriven,
}

impl HeatingDrive {
    /// Heating multiplier at Hall parameter `beta` with field direction
    /// cosine `bz_unit = B_z/|B|` (1 at B = 0).
    pub fn factor(self, beta: f64, bz_unit: f64) -> f64 {
        let f = (1.0 + beta * beta * bz_unit * bz_unit) / (1.0 + beta * beta);
        match self {
            Self::Uniform => 1.0,
            Self::FieldDriven => f,
            Self::CurrentDriven => 1.0 / f,
        }
    }
}

/// Inputs of the glow fluid model (SI unless noted).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidParams {
    /// Argon pressure, Torr.
    pub pressure_torr: f64,
    /// Gas temperature, K.
    pub gas_temp_k: f64,
    /// Electron momentum-transfer cross section for ν, m².
    pub sigma_m_m2: f64,
    /// Electron temperature used for ν (frozen, matching the reduced
    /// model), eV.
    pub te_collision_ev: f64,
    /// Ion temperature, eV.
    pub ti_ev: f64,
    /// Glow sheet thickness `L_z` (out-of-plane loss length `L_z/π`), m.
    /// Assumed.
    pub sheet_thickness_m: f64,
    /// Height above the dish where the glow sits and B is sampled, m.
    pub glow_height_m: f64,
    /// RMS radius σ of the foot's density spot, m. Assumed (clip).
    pub foot_sigma_m: f64,
    /// RMS radius σ_h of the RF-heated core, m. Assumed.
    pub heat_sigma_m: f64,
    /// Background fraction `f_bg` of the per-electron heating. Assumed.
    pub background_heating: f64,
    /// Magnetize electron heat conduction (the energy channel).
    pub magnetize_energy: bool,
    /// Magnetize the particle-transport tensors (the particle channel).
    pub magnetize_particles: bool,
    /// How the RF heating responds to B (the heating channel).
    pub heating_drive: HeatingDrive,
    /// Grid cells per side.
    pub cells: usize,
    /// Grid half-width in units of `foot_sigma_m`.
    pub half_width_sigmas: f64,
}

impl FluidParams {
    /// Argon at `pressure_torr` with the clip-informed defaults: 3 mm foot,
    /// 3 mm heated core, 25 % background heating, 5 mm sheet, glow 2 mm up.
    pub fn argon(pressure_torr: f64) -> Self {
        Self {
            pressure_torr,
            gas_temp_k: 300.0,
            sigma_m_m2: 1e-19,
            te_collision_ev: 3.0,
            ti_ev: 0.026,
            sheet_thickness_m: 5e-3,
            glow_height_m: 2e-3,
            foot_sigma_m: 3e-3,
            heat_sigma_m: 3e-3,
            background_heating: 0.25,
            magnetize_energy: true,
            magnetize_particles: true,
            heating_drive: HeatingDrive::Uniform,
            cells: 72,
            half_width_sigmas: 5.0,
        }
    }

    /// Neutral density, m⁻³.
    pub fn n_gas(&self) -> f64 {
        self.pressure_torr * TORR_PA / (K_B * self.gas_temp_k)
    }

    /// Electron momentum-transfer collision frequency, s⁻¹ (same closure as
    /// the reduced model).
    pub fn nu(&self) -> f64 {
        collision_frequency(
            self.pressure_torr * TORR_PA,
            self.gas_temp_k,
            self.sigma_m_m2,
            self.te_collision_ev,
        )
    }

    /// Electron mobility `e/(m_e ν)`, m²/(V·s).
    pub fn mu_e(&self) -> f64 {
        ELEMENTARY_CHARGE / (ELECTRON_MASS * self.nu())
    }

    /// Ion mobility, m²/(V·s).
    pub fn mu_i(&self) -> f64 {
        argon::ION_REDUCED_MOBILITY * argon::LOSCHMIDT_M3 / self.n_gas()
    }

    /// Ion diffusion coefficient, m²/s.
    pub fn d_i(&self) -> f64 {
        self.mu_i() * self.ti_ev
    }

    /// Out-of-plane diffusion length `Λ_z = L_z/π`, m.
    pub fn lambda_z(&self) -> f64 {
        self.sheet_thickness_m / std::f64::consts::PI
    }

    /// Hall parameter at field magnitude `b_t`.
    pub fn beta(&self, b_t: f64) -> f64 {
        self.mu_e() * b_t.abs()
    }

    /// `β_c = √(μ_e/μ_i)`: below it ions limit cross-field particle loss and
    /// magnetizing electrons barely changes it. Pressure-independent.
    pub fn critical_beta(&self) -> f64 {
        (self.mu_e() / self.mu_i()).sqrt()
    }

    fn grid(&self, center_m: [f64; 2]) -> Grid {
        Grid {
            cells: self.cells,
            center_m,
            half_width_m: self.half_width_sigmas * self.foot_sigma_m,
        }
    }

    fn validate(&self) -> Result<(), FluidError> {
        let ok = self.pressure_torr > 0.0
            && self.foot_sigma_m > 0.0
            && self.heat_sigma_m > 0.0
            && self.sheet_thickness_m > 0.0
            && (0.0..=1.0).contains(&self.background_heating)
            && self.cells >= 8
            && self.half_width_sigmas >= 3.0;
        if ok {
            Ok(())
        } else {
            Err(FluidError::BadInput(format!("{self:?}")))
        }
    }
}

type M3 = [[f64; 3]; 3];
type M2 = [[f64; 2]; 2];

/// Electron mobility tensor `M_e` (m²/(V·s)) in field `b` (tesla): the
/// solution of `v + μ_e v×B = c` is `v = M_e c / μ_e`, so the electron
/// drift is `v_e = −M_e (E + ∇p_e/(e n))`. For `β → ∞` its antisymmetric
/// part gives the E×B drift `E×B/B²`.
pub fn electron_mobility_tensor(mu_e: f64, b: [f64; 3]) -> [[f64; 3]; 3] {
    let bm = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
    let beta = mu_e * bm;
    let u = if bm > 0.0 {
        [b[0] / bm, b[1] / bm, b[2] / bm]
    } else {
        [0.0; 3]
    };
    let s = mu_e / (1.0 + beta * beta);
    // [b̂]× c = b̂ × c.
    let cross = [[0.0, -u[2], u[1]], [u[2], 0.0, -u[0]], [-u[1], u[0], 0.0]];
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let id = if i == j { 1.0 } else { 0.0 };
            m[i][j] = s * (id + beta * beta * u[i] * u[j] + beta * cross[i][j]);
        }
    }
    m
}

fn mat3_mul(a: &M3, b: &M3) -> M3 {
    let mut c = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            c[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    c
}

fn mat3_inv(m: &M3) -> Option<M3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-300 || !det.is_finite() {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

fn block2(m: &M3) -> M2 {
    [[m[0][0], m[0][1]], [m[1][0], m[1][1]]]
}

/// The two-species local-ambipolar tensors at one point (3D):
/// particle flux `Γ = −D_a ∇n − W n ∇T_e` with
/// `D_a = D_i (I − P) + T_e Q` and `W = Q`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbipolarTensors {
    /// `μ_i (μ_i I + M_e)⁻¹`.
    pub p: [[f64; 3]; 3],
    /// `μ_i (μ_i I + M_e)⁻¹ M_e`.
    pub q: [[f64; 3]; 3],
    /// Ion diffusion coefficient, m²/s.
    pub d_i: f64,
}

impl AmbipolarTensors {
    /// Tensors for ion mobility `mu_i`, ion diffusion `d_i` and electron
    /// mobility tensor `m_e`.
    pub fn new(mu_i: f64, d_i: f64, m_e: &[[f64; 3]; 3]) -> Self {
        let mut a = *m_e;
        for (i, row) in a.iter_mut().enumerate() {
            row[i] += mu_i;
        }
        let ainv = mat3_inv(&a).expect("μ_i I + M_e is positive definite");
        let mut p = ainv;
        for row in p.iter_mut() {
            for v in row.iter_mut() {
                *v *= mu_i;
            }
        }
        let q = mat3_mul(&p, m_e);
        Self { p, q, d_i }
    }

    /// Ambipolar diffusion tensor `D_a(T_e)`, m²/s.
    pub fn d_a(&self, te_v: f64) -> [[f64; 3]; 3] {
        std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                let id = if i == j { 1.0 } else { 0.0 };
                self.d_i * (id - self.p[i][j]) + te_v * self.q[i][j]
            })
        })
    }
}

/// Uniform cell-centred square grid.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grid {
    cells: usize,
    center_m: [f64; 2],
    half_width_m: f64,
}

impl Grid {
    fn dx(&self) -> f64 {
        2.0 * self.half_width_m / self.cells as f64
    }
    /// Cell-centre offset from the grid centre along one axis.
    fn off(&self, i: usize) -> f64 {
        -self.half_width_m + (i as f64 + 0.5) * self.dx()
    }
    fn idx(&self, i: usize, j: usize) -> usize {
        j * self.cells + i
    }
    fn len(&self) -> usize {
        self.cells * self.cells
    }
}

/// Per-cell transport, precomputed from the sampled field.
#[derive(Debug, Clone, Copy)]
struct CellTransport {
    /// In-plane electron mobility block used for heat conduction.
    m_heat: M2,
    /// Ambipolar tensors used for particle transport.
    amb: AmbipolarTensors,
    /// Electron Hall parameter.
    beta: f64,
    /// Field, T.
    b: [f64; 3],
    /// Full electron mobility tensor (for E diagnostics).
    m_e: M3,
}

fn sample_transport(
    fp: &FluidParams,
    grid: &Grid,
    b_at: &dyn Fn([f64; 3]) -> [f64; 3],
) -> Vec<CellTransport> {
    let (mu_e, mu_i, d_i) = (fp.mu_e(), fp.mu_i(), fp.d_i());
    let unmag = electron_mobility_tensor(mu_e, [0.0; 3]);
    let amb_unmag = AmbipolarTensors::new(mu_i, d_i, &unmag);
    let n = grid.cells;
    let mut out = Vec::with_capacity(grid.len());
    for j in 0..n {
        for i in 0..n {
            let p = [
                grid.center_m[0] + grid.off(i),
                grid.center_m[1] + grid.off(j),
                fp.glow_height_m,
            ];
            let b = b_at(p);
            let m_e = electron_mobility_tensor(mu_e, b);
            let bm = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
            out.push(CellTransport {
                m_heat: block2(if fp.magnetize_energy { &m_e } else { &unmag }),
                amb: if fp.magnetize_particles {
                    AmbipolarTensors::new(mu_i, d_i, &m_e)
                } else {
                    amb_unmag
                },
                beta: mu_e * bm,
                b,
                m_e,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Linear algebra: 9-point stencil + Jacobi-preconditioned BiCGSTAB
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Stencil9 {
    n: usize,
    c: Vec<[f64; 9]>,
}

fn slot(di: isize, dj: isize) -> usize {
    ((dj + 1) * 3 + (di + 1)) as usize
}

impl Stencil9 {
    fn zeros(n: usize) -> Self {
        Self {
            n,
            c: vec![[0.0; 9]; n * n],
        }
    }

    /// Add `v` to the coefficient of row cell `(i, j)` on column `(ti, tj)`.
    fn add(&mut self, i: usize, j: usize, ti: usize, tj: usize, v: f64) {
        let di = ti as isize - i as isize;
        let dj = tj as isize - j as isize;
        debug_assert!(di.abs() <= 1 && dj.abs() <= 1);
        self.c[j * self.n + i][slot(di, dj)] += v;
    }

    fn apply(&self, u: &[f64], out: &mut [f64]) {
        let n = self.n as isize;
        for j in 0..n {
            for i in 0..n {
                let row = &self.c[(j * n + i) as usize];
                let mut s = 0.0;
                for dj in -1..=1 {
                    let jj = j + dj;
                    if jj < 0 || jj >= n {
                        continue;
                    }
                    for di in -1..=1 {
                        let ii = i + di;
                        if ii < 0 || ii >= n {
                            continue;
                        }
                        let c = row[slot(di, dj)];
                        if c != 0.0 {
                            s += c * u[(jj * n + ii) as usize];
                        }
                    }
                }
                out[(j * n + i) as usize] = s;
            }
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn bicgstab(
    a: &Stencil9,
    b: &[f64],
    x: &mut [f64],
    tol: f64,
    max_it: usize,
    what: &'static str,
) -> Result<usize, FluidError> {
    let len = b.len();
    let dinv: Vec<f64> =
        a.c.iter()
            .map(|r| {
                let d = r[slot(0, 0)];
                if d.abs() > 0.0 {
                    1.0 / d
                } else {
                    1.0
                }
            })
            .collect();
    let bn = dot(b, b).sqrt().max(1e-300);
    let mut r = vec![0.0; len];
    a.apply(x, &mut r);
    for k in 0..len {
        r[k] = b[k] - r[k];
    }
    if dot(&r, &r).sqrt() <= tol * bn {
        return Ok(0);
    }
    let rhat = r.clone();
    let (mut rho, mut alpha, mut omega) = (1.0f64, 1.0f64, 1.0f64);
    let mut v = vec![0.0; len];
    let mut p = vec![0.0; len];
    let mut y = vec![0.0; len];
    let mut s = vec![0.0; len];
    let mut z = vec![0.0; len];
    let mut t = vec![0.0; len];
    let mut res = 1.0;
    for it in 1..=max_it {
        let rho_new = dot(&rhat, &r);
        if rho_new.abs() < 1e-300 {
            break;
        }
        let beta = (rho_new / rho) * (alpha / omega);
        for k in 0..len {
            p[k] = r[k] + beta * (p[k] - omega * v[k]);
            y[k] = dinv[k] * p[k];
        }
        a.apply(&y, &mut v);
        let rv = dot(&rhat, &v);
        if rv.abs() < 1e-300 {
            break;
        }
        alpha = rho_new / rv;
        for k in 0..len {
            s[k] = r[k] - alpha * v[k];
        }
        if dot(&s, &s).sqrt() <= tol * bn {
            for k in 0..len {
                x[k] += alpha * y[k];
            }
            return Ok(it);
        }
        for k in 0..len {
            z[k] = dinv[k] * s[k];
        }
        a.apply(&z, &mut t);
        let tt = dot(&t, &t);
        if tt < 1e-300 {
            break;
        }
        omega = dot(&t, &s) / tt;
        for k in 0..len {
            x[k] += alpha * y[k] + omega * z[k];
            r[k] = s[k] - omega * t[k];
        }
        res = dot(&r, &r).sqrt() / bn;
        if res <= tol {
            return Ok(it);
        }
        rho = rho_new;
    }
    Err(FluidError::LinearSolve {
        what,
        iterations: max_it,
        residual: res,
    })
}

/// `div(−K ∇u)` per unit area, zero-flux (Neumann) outer boundary. `kx[f]`
/// is the tensor on x-face `(i+½, j)` (index `j*(n−1)+i`), `ky` on y-face
/// `(i, j+½)` (index `j*n+i`). Tangential derivatives use the 4-cell
/// average, clamped at the boundary rows.
fn assemble_divflux(n: usize, dx: f64, kx: &[M2], ky: &[M2]) -> Stencil9 {
    let mut st = Stencil9::zeros(n);
    let h2 = dx * dx;
    for j in 0..n {
        let (jm, jp) = (j.saturating_sub(1), (j + 1).min(n - 1));
        for i in 0..n - 1 {
            let k = kx[j * (n - 1) + i];
            // Outflux from a=(i,j) through its +x face, per unit area of a:
            // F/dx = −[Kxx (u_b − u_a) + Kxy (u(i+1,jp)+u(i,jp)−u(i+1,jm)−u(i,jm))/4]/dx².
            let terms = [
                (i + 1, j, -k[0][0] / h2),
                (i, j, k[0][0] / h2),
                (i + 1, jp, -k[0][1] / (4.0 * h2)),
                (i, jp, -k[0][1] / (4.0 * h2)),
                (i + 1, jm, k[0][1] / (4.0 * h2)),
                (i, jm, k[0][1] / (4.0 * h2)),
            ];
            for (ti, tj, v) in terms {
                st.add(i, j, ti, tj, v);
                st.add(i + 1, j, ti, tj, -v);
            }
        }
    }
    for j in 0..n - 1 {
        for i in 0..n {
            let (im, ip) = (i.saturating_sub(1), (i + 1).min(n - 1));
            let k = ky[j * n + i];
            let terms = [
                (i, j + 1, -k[1][1] / h2),
                (i, j, k[1][1] / h2),
                (ip, j + 1, -k[1][0] / (4.0 * h2)),
                (ip, j, -k[1][0] / (4.0 * h2)),
                (im, j + 1, k[1][0] / (4.0 * h2)),
                (im, j, k[1][0] / (4.0 * h2)),
            ];
            for (ti, tj, v) in terms {
                st.add(i, j, ti, tj, v);
                st.add(i, j + 1, ti, tj, -v);
            }
        }
    }
    st
}

fn avg2(a: &M2, b: &M2, wa: f64, wb: f64) -> M2 {
    [
        [
            0.5 * (wa * a[0][0] + wb * b[0][0]),
            0.5 * (wa * a[0][1] + wb * b[0][1]),
        ],
        [
            0.5 * (wa * a[1][0] + wb * b[1][0]),
            0.5 * (wa * a[1][1] + wb * b[1][1]),
        ],
    ]
}

// ---------------------------------------------------------------------------
// Electron energy
// ---------------------------------------------------------------------------

/// Per-electron energy loss rate at `te` for a cell, V/s.
fn energy_loss(fp: &FluidParams, n_gas: f64, cell: &CellTransport, te: f64) -> f64 {
    let nu_z = cell.amb.d_a(te)[2][2] / (fp.lambda_z() * fp.lambda_z());
    n_gas * argon::collisional_loss(te) + nu_z * argon::wall_energy_per_pair(te)
}

const TE_MIN: f64 = 0.2;
const TE_MAX: f64 = 20.0;

/// Solve the steady electron energy equation for `T_e` (in place; `t`
/// holds the initial guess). Returns the outer iterations used.
fn solve_energy(
    fp: &FluidParams,
    grid: &Grid,
    cells: &[CellTransport],
    n: &[f64],
    heat: &[f64],
    t: &mut [f64],
) -> Result<usize, FluidError> {
    let nn = grid.cells;
    let dx = grid.dx();
    let n_gas = fp.n_gas();
    let mut last = f64::INFINITY;
    let mut kx = vec![[[0.0; 2]; 2]; (nn - 1) * nn];
    let mut ky = vec![[[0.0; 2]; 2]; (nn - 1) * nn];
    let mut rhs = vec![0.0; grid.len()];
    for it in 1..=80 {
        // Conduction tensor κ = (5/2) n T_e M_heat at faces (Picard in T_e).
        for j in 0..nn {
            for i in 0..nn - 1 {
                let (a, b) = (grid.idx(i, j), grid.idx(i + 1, j));
                kx[j * (nn - 1) + i] = avg2(
                    &cells[a].m_heat,
                    &cells[b].m_heat,
                    2.5 * n[a] * t[a],
                    2.5 * n[b] * t[b],
                );
            }
        }
        for j in 0..nn - 1 {
            for i in 0..nn {
                let (a, b) = (grid.idx(i, j), grid.idx(i, j + 1));
                ky[j * nn + i] = avg2(
                    &cells[a].m_heat,
                    &cells[b].m_heat,
                    2.5 * n[a] * t[a],
                    2.5 * n[b] * t[b],
                );
            }
        }
        let mut st = assemble_divflux(nn, dx, &kx, &ky);
        // Newton on the local loss: n L(T) ≈ n [L(T0) + L'(T0)(T − T0)].
        for k in 0..grid.len() {
            let t0 = t[k];
            let h = 1e-5 * t0;
            let l0 = energy_loss(fp, n_gas, &cells[k], t0);
            let lp = (energy_loss(fp, n_gas, &cells[k], t0 + h)
                - energy_loss(fp, n_gas, &cells[k], t0 - h))
                / (2.0 * h);
            // Rows divided by n (every term carries one factor of n), so the
            // dilute wings are as well conditioned as the core.
            let inv = 1.0 / n[k];
            for c in st.c[k].iter_mut() {
                *c *= inv;
            }
            st.c[k][slot(0, 0)] += lp;
            rhs[k] = heat[k] - l0 + lp * t0;
        }
        let mut tn = t.to_vec();
        bicgstab(&st, &rhs, &mut tn, 1e-11, 20_000, "electron energy")?;
        let mut change = 0.0f64;
        for k in 0..grid.len() {
            let lo = (0.6 * t[k]).max(TE_MIN);
            let hi = (1.6 * t[k]).min(TE_MAX);
            let v = tn[k].clamp(lo, hi);
            change = change.max((v - t[k]).abs());
            t[k] = v;
        }
        last = change;
        if change < 1e-9 {
            return Ok(it);
        }
    }
    Err(FluidError::EnergyIteration {
        last_change_v: last,
    })
}

// ---------------------------------------------------------------------------
// Foot moments
// ---------------------------------------------------------------------------

/// The foot's response at one site: centroid velocity and its split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FootDrift {
    /// Foot centre (world dish frame), m.
    pub foot_m: [f64; 2],
    /// Centroid velocity `dx̄/dt`, m/s (world frame).
    pub velocity_m_s: [f64; 2],
    /// Part of it from the ionization skew `∫(x − x̄) g n / N`.
    pub growth_velocity_m_s: [f64; 2],
    /// Part of it from transport `−∫(x − x̄) ∇·Γ / N` (incl. Hall).
    pub transport_velocity_m_s: [f64; 2],
    /// Net growth rate `(dN/dt)/N`, s⁻¹ (0 at the calibration point).
    pub growth_rate_per_s: f64,
    /// Peak electron temperature, V.
    pub te_peak_v: f64,
    /// Density-weighted mean electron temperature, V.
    pub te_mean_v: f64,
    /// `T_e` along the x axis through the foot at `−2σ, −σ, 0, σ, 2σ`, V
    /// (for a foot at `(ρ, 0)` with the magnet at the origin, negative
    /// offsets are the magnet side).
    pub te_profile_x_v: [f64; 5],
    /// Electron Hall parameter at the foot centre.
    pub beta_at_foot: f64,
    /// Largest electron E×B speed within σ of the foot, m/s (diagnostic:
    /// a fluid speed, *not* a pattern speed).
    pub max_exb_m_s: f64,
    /// `|Σ ∇·Γ dA − boundary outflux| / boundary scale` (discrete
    /// conservation check).
    pub conservation_residual: f64,
    /// Outer energy iterations.
    pub energy_iterations: usize,
}

fn gaussian_foot(fp: &FluidParams, grid: &Grid) -> Vec<f64> {
    let s2 = fp.foot_sigma_m * fp.foot_sigma_m;
    let nn = grid.cells;
    let mut n = vec![0.0; grid.len()];
    for j in 0..nn {
        for i in 0..nn {
            let (x, y) = (grid.off(i), grid.off(j));
            n[grid.idx(i, j)] = (-(x * x + y * y) / (2.0 * s2)).exp().max(1e-12);
        }
    }
    n
}

fn heating_profile(
    fp: &FluidParams,
    grid: &Grid,
    cells: &[CellTransport],
    h0: f64,
    multiplier: &dyn Fn([f64; 2]) -> f64,
) -> Vec<f64> {
    let s2 = fp.heat_sigma_m * fp.heat_sigma_m;
    let nn = grid.cells;
    let f = fp.background_heating;
    let mut h = vec![0.0; grid.len()];
    for j in 0..nn {
        for i in 0..nn {
            let (x, y) = (grid.off(i), grid.off(j));
            let core = (-(x * x + y * y) / (2.0 * s2)).exp();
            let w = [grid.center_m[0] + x, grid.center_m[1] + y];
            let k = grid.idx(i, j);
            let c = &cells[k];
            let bm = (c.b[0] * c.b[0] + c.b[1] * c.b[1] + c.b[2] * c.b[2]).sqrt();
            let bz = if bm > 0.0 { c.b[2] / bm } else { 1.0 };
            let drive = fp.heating_drive.factor(c.beta, bz);
            h[k] = h0 * (f + (1.0 - f) * core) * multiplier(w) * drive;
        }
    }
    h
}

fn foot_moments(
    fp: &FluidParams,
    grid: &Grid,
    cells: &[CellTransport],
    n: &[f64],
    t: &[f64],
) -> FootDrift {
    let nn = grid.cells;
    let dx = grid.dx();
    let da_all: Vec<M3> = (0..grid.len()).map(|k| cells[k].amb.d_a(t[k])).collect();
    let da_cell = |k: usize| da_all[k];
    let n_gas = fp.n_gas();
    let lz2 = fp.lambda_z() * fp.lambda_z();
    // Net outflux per unit area of each cell, and the boundary total.
    let mut div = vec![0.0; grid.len()];
    let mut boundary = 0.0;
    let at = |v: &[f64], i: usize, j: usize| v[j * nn + i];
    // x-faces.
    for j in 0..nn {
        let (jm, jp) = (j.saturating_sub(1), (j + 1).min(nn - 1));
        for i in 0..nn - 1 {
            let (a, b) = (grid.idx(i, j), grid.idx(i + 1, j));
            let (da, db) = (da_cell(a), da_cell(b));
            let dxx = 0.5 * (da[0][0] + db[0][0]);
            let dxy = 0.5 * (da[0][1] + db[0][1]);
            let (qa, qb) = (cells[a].amb.q, cells[b].amb.q);
            let wxx = 0.5 * (qa[0][0] + qb[0][0]);
            let wxy = 0.5 * (qa[0][1] + qb[0][1]);
            let dn_x = (n[b] - n[a]) / dx;
            let dn_y =
                (at(n, i + 1, jp) + at(n, i, jp) - at(n, i + 1, jm) - at(n, i, jm)) / (4.0 * dx);
            let dt_x = (t[b] - t[a]) / dx;
            let dt_y =
                (at(t, i + 1, jp) + at(t, i, jp) - at(t, i + 1, jm) - at(t, i, jm)) / (4.0 * dx);
            let nf = 0.5 * (n[a] + n[b]);
            let g = -(dxx * dn_x + dxy * dn_y) - nf * (wxx * dt_x + wxy * dt_y);
            div[a] += g / dx;
            div[b] -= g / dx;
        }
        // Boundary faces: absorbing (ghost n = −n, so n = 0 on the face).
        for i in [0usize, nn - 1] {
            let a = grid.idx(i, j);
            let out = 2.0 * da_cell(a)[0][0] * n[a] / dx;
            div[a] += out / dx;
            boundary += out * dx;
        }
    }
    // y-faces.
    for j in 0..nn - 1 {
        for i in 0..nn {
            let (im, ip) = (i.saturating_sub(1), (i + 1).min(nn - 1));
            let (a, b) = (grid.idx(i, j), grid.idx(i, j + 1));
            let (da, db) = (da_cell(a), da_cell(b));
            let dyy = 0.5 * (da[1][1] + db[1][1]);
            let dyx = 0.5 * (da[1][0] + db[1][0]);
            let (qa, qb) = (cells[a].amb.q, cells[b].amb.q);
            let wyy = 0.5 * (qa[1][1] + qb[1][1]);
            let wyx = 0.5 * (qa[1][0] + qb[1][0]);
            let dn_y = (n[b] - n[a]) / dx;
            let dn_x =
                (at(n, ip, j + 1) + at(n, ip, j) - at(n, im, j + 1) - at(n, im, j)) / (4.0 * dx);
            let dt_y = (t[b] - t[a]) / dx;
            let dt_x =
                (at(t, ip, j + 1) + at(t, ip, j) - at(t, im, j + 1) - at(t, im, j)) / (4.0 * dx);
            let nf = 0.5 * (n[a] + n[b]);
            let g = -(dyy * dn_y + dyx * dn_x) - nf * (wyy * dt_y + wyx * dt_x);
            div[a] += g / dx;
            div[b] -= g / dx;
        }
    }
    for i in 0..nn {
        for j in [0usize, nn - 1] {
            let a = grid.idx(i, j);
            let out = 2.0 * da_cell(a)[1][1] * n[a] / dx;
            div[a] += out / dx;
            boundary += out * dx;
        }
    }
    let da_area = dx * dx;
    let mut big_n = 0.0;
    let mut mx = [0.0; 2];
    let mut te_w = 0.0;
    let mut te_peak = 0.0f64;
    let src: Vec<f64> = (0..grid.len())
        .map(|k| {
            let nu_z = da_cell(k)[2][2] / lz2;
            (n_gas * argon::k_iz(t[k]) - nu_z) * n[k]
        })
        .collect();
    for j in 0..nn {
        for i in 0..nn {
            let k = grid.idx(i, j);
            let (x, y) = (grid.off(i), grid.off(j));
            big_n += n[k] * da_area;
            mx[0] += x * n[k] * da_area;
            mx[1] += y * n[k] * da_area;
            te_w += t[k] * n[k] * da_area;
            te_peak = te_peak.max(t[k]);
        }
    }
    let xbar = [mx[0] / big_n, mx[1] / big_n];
    let mut vg = [0.0; 2];
    let mut vt = [0.0; 2];
    let mut dndt = 0.0;
    let mut div_total = 0.0;
    for j in 0..nn {
        for i in 0..nn {
            let k = grid.idx(i, j);
            let r = [grid.off(i) - xbar[0], grid.off(j) - xbar[1]];
            for c in 0..2 {
                vg[c] += r[c] * src[k] * da_area;
                vt[c] -= r[c] * div[k] * da_area;
            }
            dndt += (src[k] - div[k]) * da_area;
            div_total += div[k] * da_area;
        }
    }
    let vg = [vg[0] / big_n, vg[1] / big_n];
    let vt = [vt[0] / big_n, vt[1] / big_n];
    // E×B diagnostic within σ of the foot.
    let mut max_exb = 0.0f64;
    let s2 = fp.foot_sigma_m * fp.foot_sigma_m;
    let pe: Vec<f64> = (0..grid.len()).map(|q| n[q] * t[q]).collect();
    for j in 1..nn - 1 {
        for i in 1..nn - 1 {
            let (x, y) = (grid.off(i), grid.off(j));
            if x * x + y * y > s2 {
                continue;
            }
            let k = grid.idx(i, j);
            let c = &cells[k];
            let bm2 = c.b[0] * c.b[0] + c.b[1] * c.b[1] + c.b[2] * c.b[2];
            if bm2 <= 0.0 {
                continue;
            }
            let g = |v: &[f64]| {
                [
                    (at(v, i + 1, j) - at(v, i - 1, j)) / (2.0 * dx),
                    (at(v, i, j + 1) - at(v, i, j - 1)) / (2.0 * dx),
                ]
            };
            let gn = g(n);
            let gp = g(&pe);
            // (μ_i I + M_e) n E = D_i ∇n − M_e ∇(n T_e).
            let mut a = c.m_e;
            for (r, row) in a.iter_mut().enumerate() {
                row[r] += fp.mu_i();
            }
            let Some(ainv) = mat3_inv(&a) else { continue };
            let rhs = [
                fp.d_i() * gn[0] - (c.m_e[0][0] * gp[0] + c.m_e[0][1] * gp[1]),
                fp.d_i() * gn[1] - (c.m_e[1][0] * gp[0] + c.m_e[1][1] * gp[1]),
                -(c.m_e[2][0] * gp[0] + c.m_e[2][1] * gp[1]),
            ];
            let e: Vec<f64> = (0..3)
                .map(|r| (0..3).map(|q| ainv[r][q] * rhs[q]).sum::<f64>() / n[k])
                .collect();
            let exb = [
                e[1] * c.b[2] - e[2] * c.b[1],
                e[2] * c.b[0] - e[0] * c.b[2],
                e[0] * c.b[1] - e[1] * c.b[0],
            ];
            let s = (exb[0] * exb[0] + exb[1] * exb[1] + exb[2] * exb[2]).sqrt() / bm2;
            max_exb = max_exb.max(s);
        }
    }
    let center = grid.idx(nn / 2, nn / 2);
    let te_at = |x: f64| {
        let i = (((x + grid.half_width_m) / dx - 0.5).round().max(0.0) as usize).min(nn - 1);
        // Average the two rows straddling y = 0 (even cell counts).
        0.5 * (t[grid.idx(i, nn / 2)] + t[grid.idx(i, (nn - 1) / 2)])
    };
    FootDrift {
        foot_m: grid.center_m,
        velocity_m_s: [vg[0] + vt[0], vg[1] + vt[1]],
        growth_velocity_m_s: vg,
        transport_velocity_m_s: vt,
        growth_rate_per_s: dndt / big_n,
        te_peak_v: te_peak,
        te_mean_v: te_w / big_n,
        te_profile_x_v: [-2.0, -1.0, 0.0, 1.0, 2.0].map(|k| te_at(k * fp.foot_sigma_m)),
        beta_at_foot: cells[center].beta,
        max_exb_m_s: max_exb,
        conservation_residual: (div_total - boundary).abs() / boundary.abs().max(1e-30),
        energy_iterations: 0,
    }
}

// ---------------------------------------------------------------------------
// Public foot model
// ---------------------------------------------------------------------------

/// A calibrated foot: the fluid parameters plus the heating amplitude that
/// makes the unmagnetized foot exactly self-sustaining.
#[derive(Debug, Clone, PartialEq)]
pub struct FootModel {
    /// Fluid inputs.
    pub params: FluidParams,
    /// Peak per-electron RF heating `H₀`, V/s (W per electron / e).
    pub heating_per_electron: f64,
    /// The calibrated, unmagnetized `T_e` profile (grid-relative), used as
    /// the initial guess for every later solve.
    t_profile: Vec<f64>,
}

impl FootModel {
    /// Find `H₀` such that the unmagnetized foot's net growth is zero
    /// (Schottky condition), by bracketing + bisection in `ln H₀`.
    pub fn calibrate(params: FluidParams) -> Result<Self, FluidError> {
        params.validate()?;
        let grid = params.grid([0.0, 0.0]);
        let cells = sample_transport(&params, &grid, &|_| [0.0; 3]);
        let n = gaussian_foot(&params, &grid);
        let n_gas = params.n_gas();
        // Initial guess: uniform T* with ionization = out-of-plane loss.
        let nu_z0 = cells[0].amb.d_a(2.0)[2][2] / (params.lambda_z() * params.lambda_z());
        let mut lo_t: f64 = 0.5;
        let mut hi_t: f64 = 15.0;
        for _ in 0..80 {
            let m = 0.5 * (lo_t + hi_t);
            if n_gas * argon::k_iz(m) > nu_z0 {
                hi_t = m;
            } else {
                lo_t = m;
            }
        }
        let t_star = 0.5 * (lo_t + hi_t);
        let mut t = vec![t_star; grid.len()];
        let growth = |h0: f64, t: &mut Vec<f64>| -> Result<f64, FluidError> {
            let heat = heating_profile(&params, &grid, &cells, h0, &|_| 1.0);
            solve_energy(&params, &grid, &cells, &n, &heat, t)?;
            Ok(foot_moments(&params, &grid, &cells, &n, t).growth_rate_per_s)
        };
        let h_guess = energy_loss(&params, n_gas, &cells[0], t_star);
        let mut lo = h_guess;
        let mut hi = h_guess;
        let mut g_lo = growth(lo, &mut t)?;
        let mut g_hi = g_lo;
        let mut tries = 0;
        while g_lo > 0.0 {
            lo /= 1.5;
            g_lo = growth(lo, &mut t)?;
            tries += 1;
            if tries > 40 {
                return Err(FluidError::Calibration("no H₀ with negative growth".into()));
            }
        }
        while g_hi <= 0.0 {
            hi *= 1.5;
            g_hi = growth(hi, &mut t)?;
            tries += 1;
            if tries > 80 {
                return Err(FluidError::Calibration("no H₀ with positive growth".into()));
            }
        }
        let scale = nu_z0.max(1.0);
        let mut mid = (lo * hi).sqrt();
        for _ in 0..100 {
            mid = (lo * hi).sqrt();
            let g = growth(mid, &mut t)?;
            if g.abs() < 1e-9 * scale || hi / lo < 1.0 + 1e-12 {
                break;
            }
            if g > 0.0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        Ok(Self {
            params,
            heating_per_electron: mid,
            t_profile: t,
        })
    }

    /// The foot's centroid velocity at `foot_xy` (m, dish frame) in
    /// `field` (`None` = no magnet).
    pub fn drift(
        &self,
        field: Option<&FieldScene>,
        foot_xy: [f64; 2],
    ) -> Result<FootDrift, FluidError> {
        self.drift_with_heating(field, foot_xy, &|_| 1.0)
    }

    /// [`drift`](Self::drift) with the per-electron heating multiplied by
    /// `multiplier(x, y)` (world dish frame) — e.g. the `(E/E₀)²` of a
    /// conductor near the electrode ([`es_field_ratio`]).
    pub fn drift_with_heating(
        &self,
        field: Option<&FieldScene>,
        foot_xy: [f64; 2],
        multiplier: &dyn Fn([f64; 2]) -> f64,
    ) -> Result<FootDrift, FluidError> {
        match field {
            Some(f) => self.drift_in(&|p| f.b(p), foot_xy, multiplier),
            None => self.drift_in(&|_| [0.0; 3], foot_xy, multiplier),
        }
    }

    /// [`drift_with_heating`](Self::drift_with_heating) for any field
    /// `b_at(world point) → B` (tesla), e.g. an idealized uniform field.
    pub fn drift_in(
        &self,
        b_at: &dyn Fn([f64; 3]) -> [f64; 3],
        foot_xy: [f64; 2],
        multiplier: &dyn Fn([f64; 2]) -> f64,
    ) -> Result<FootDrift, FluidError> {
        let fp = &self.params;
        let grid = fp.grid(foot_xy);
        let cells = sample_transport(fp, &grid, b_at);
        let n = gaussian_foot(fp, &grid);
        let heat = heating_profile(fp, &grid, &cells, self.heating_per_electron, multiplier);
        let mut t = self.t_profile.clone();
        let iters = solve_energy(fp, &grid, &cells, &n, &heat, &mut t)?;
        let mut d = foot_moments(fp, &grid, &cells, &n, &t);
        d.energy_iterations = iters;
        Ok(d)
    }
}

// ---------------------------------------------------------------------------
// Lock map (feeds back into the reduced model)
// ---------------------------------------------------------------------------

/// The derived pull, tabulated for one disc magnet held north-pole-down
/// (axis −ẑ) at gap `g` (bottom face above the dish) with the foot at
/// horizontal distance `ρ` from its footprint.
#[derive(Debug, Clone, PartialEq)]
pub struct LockMap {
    /// Fluid inputs it was computed with.
    pub params: FluidParams,
    /// Calibrated `H₀`, V/s.
    pub heating_per_electron: f64,
    /// The magnet it applies to.
    pub magnet: DiscMagnet,
    /// Gap nodes (ascending), m.
    pub gaps_m: Vec<f64>,
    /// Offset nodes (ascending, starting at 0), m.
    pub rhos_m: Vec<f64>,
    /// Pull toward the footprint, m/s; `[gap][rho]` row-major.
    pub inward_m_s: Vec<f64>,
    /// Swirl about the magnet axis, m/s, counter-clockwise positive (seen
    /// from above) for the north-down magnet; flips with polarity.
    pub swirl_m_s: Vec<f64>,
    /// Net growth rate of the foot at each node, s⁻¹ (sustaining margin).
    pub growth_per_s: Vec<f64>,
}

fn disc_scene(magnet: &DiscMagnet, gap_m: f64, north_down: bool) -> FieldScene {
    let axis = if north_down {
        [0.0, 0.0, -1.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    crate::plasmoid::MagnetPose {
        center_m: [0.0, 0.0, gap_m + 0.5 * magnet.thickness_m],
        axis,
    }
    .scene(magnet)
}

impl LockMap {
    /// Tabulate `model`'s foot drift for `magnet` (north down, centred on
    /// the origin) over `gaps_m` × `rhos_m`; the foot sits at `(ρ, 0)`.
    pub fn compute(
        model: &FootModel,
        magnet: DiscMagnet,
        gaps_m: &[f64],
        rhos_m: &[f64],
    ) -> Result<Self, FluidError> {
        let ascending = |v: &[f64]| v.windows(2).all(|w| w[1] > w[0]);
        if gaps_m.is_empty() || rhos_m.len() < 2 || !ascending(gaps_m) || !ascending(rhos_m) {
            return Err(FluidError::BadInput("lock-map nodes must ascend".into()));
        }
        if rhos_m[0] != 0.0 {
            return Err(FluidError::BadInput("rho nodes must start at 0".into()));
        }
        let mut inward = Vec::with_capacity(gaps_m.len() * rhos_m.len());
        let mut swirl = Vec::with_capacity(inward.capacity());
        let mut growth = Vec::with_capacity(inward.capacity());
        for &g in gaps_m {
            let scene = disc_scene(&magnet, g, true);
            for &r in rhos_m {
                if r == 0.0 {
                    let d = model.drift(Some(&scene), [0.0, 0.0])?;
                    inward.push(0.0);
                    swirl.push(0.0);
                    growth.push(d.growth_rate_per_s);
                    continue;
                }
                let d = model.drift(Some(&scene), [r, 0.0])?;
                inward.push(-d.velocity_m_s[0]);
                swirl.push(d.velocity_m_s[1]);
                growth.push(d.growth_rate_per_s);
            }
        }
        Ok(Self {
            params: model.params,
            heating_per_electron: model.heating_per_electron,
            magnet,
            gaps_m: gaps_m.to_vec(),
            rhos_m: rhos_m.to_vec(),
            inward_m_s: inward,
            swirl_m_s: swirl,
            growth_per_s: growth,
        })
    }

    /// Largest |inward pull| in the table, m/s.
    pub fn peak_inward_magnitude(&self) -> f64 {
        self.inward_m_s.iter().fold(0.0, |a, v| a.max(v.abs()))
    }

    /// The map rescaled (inward and swirl alike) so its largest |inward
    /// pull| is `peak_m_s`. The fluid derives the pull's *shape, sign and
    /// swirl ratio*; its amplitude is the instantaneous skew rate of a
    /// frozen profile, not a foot speed (the channel's response time is not
    /// modelled), so a run that wants physical speeds fixes the amplitude —
    /// e.g. to the reduced model's `μ_fit · max|∇Φ|`.
    pub fn with_peak_inward(&self, peak_m_s: f64) -> Self {
        let k = peak_m_s / self.peak_inward_magnitude().max(1e-300);
        let mut m = self.clone();
        m.inward_m_s.iter_mut().for_each(|v| *v *= k);
        m.swirl_m_s.iter_mut().for_each(|v| *v *= k);
        m
    }

    fn bilinear(&self, table: &[f64], gap: f64, rho: f64) -> f64 {
        let nr = self.rhos_m.len();
        let rmax = *self.rhos_m.last().unwrap();
        if rho >= rmax || rho < 0.0 {
            return 0.0;
        }
        let gmax = *self.gaps_m.last().unwrap();
        if gap > gmax {
            return 0.0;
        }
        let locate = |v: &[f64], x: f64| -> (usize, f64) {
            if v.len() == 1 || x <= v[0] {
                return (0, 0.0);
            }
            let k = v.partition_point(|&a| a <= x).min(v.len() - 1).max(1) - 1;
            let u = ((x - v[k]) / (v[k + 1] - v[k])).clamp(0.0, 1.0);
            (k, u)
        };
        let (kr, ur) = locate(&self.rhos_m, rho);
        let (kg, ug) = locate(&self.gaps_m, gap);
        let at = |g: usize, r: usize| table[g * nr + r];
        let row = |g: usize| at(g, kr) * (1.0 - ur) + at(g, kr + 1) * ur;
        if self.gaps_m.len() == 1 || ug == 0.0 {
            row(kg)
        } else {
            row(kg) * (1.0 - ug) + row(kg + 1) * ug
        }
    }

    /// `(inward, swirl)` pull at gap `gap_m`, offset `rho_m` (bilinear;
    /// zero beyond the last ρ or gap node, clamped below the first gap).
    pub fn sample(&self, gap_m: f64, rho_m: f64) -> (f64, f64) {
        (
            self.bilinear(&self.inward_m_s, gap_m, rho_m),
            self.bilinear(&self.swirl_m_s, gap_m, rho_m),
        )
    }

    fn matches(&self, s: &FieldSource) -> bool {
        let m = &self.magnet;
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * b.abs().max(1e-12);
        matches!(*s, FieldSource::CylinderMagnet { outer_radius_m, inner_radius_m, length_m, remanence_t, .. }
            if close(outer_radius_m, m.radius_m) && inner_radius_m == 0.0
                && close(length_m, m.thickness_m) && close(remanence_t, m.remanence_t))
    }

    /// Sources in `scene` this map can represent (the tabulated magnet,
    /// axis within ~11° of vertical) and how many it cannot (ignored by
    /// [`pull_in`](Self::pull_in)).
    pub fn coverage(&self, scene: &FieldScene) -> (usize, usize) {
        let used = scene
            .sources
            .iter()
            .filter(|s| self.matches(&s.source) && s.pose.axis()[2].abs() > 0.98)
            .count();
        (used, scene.sources.len() - used)
    }

    /// Derived foot pull at `pos` (m, dish frame) in `scene`, m/s: the sum
    /// over the scene's matching vertical disc magnets of the tabulated
    /// inward pull and polarity-signed swirl. Superposing several magnets
    /// is an approximation (the fluid response is nonlinear); unmatched
    /// sources contribute nothing — check [`coverage`](Self::coverage).
    pub fn pull_in(&self, scene: &FieldScene, pos: [f64; 2]) -> [f64; 2] {
        let mut v = [0.0; 2];
        for s in &scene.sources {
            let axis = s.pose.axis();
            if !self.matches(&s.source) || axis[2].abs() <= 0.98 {
                continue;
            }
            let c = s.pose.position_m;
            let gap = c[2] - 0.5 * self.magnet.thickness_m;
            let d = [pos[0] - c[0], pos[1] - c[1]];
            let rho = (d[0] * d[0] + d[1] * d[1]).sqrt();
            if rho < 1e-12 {
                continue;
            }
            let (vin, vs) = self.sample(gap, rho);
            let sign = if axis[2] < 0.0 { 1.0 } else { -1.0 };
            v[0] += -vin * d[0] / rho - sign * vs * d[1] / rho;
            v[1] += -vin * d[1] / rho + sign * vs * d[0] / rho;
        }
        v
    }
}

// ---------------------------------------------------------------------------
// Electrostatic alternative
// ---------------------------------------------------------------------------

/// Electrical state of the (Ni-plated, conducting) magnet near the RF
/// electrode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConductorState {
    /// Isolated (e.g. on an insulating stick): an uncharged conductor that
    /// polarizes in the electrode's field — induced dipole `4πε₀a³E₀` plus
    /// its image in the electrode.
    Floating,
    /// RF-grounded (hand-held: the body's ~100 pF to ground is ~1 kΩ at
    /// 1 MHz, a short against a resonator's top load): carries charge
    /// `−4πε₀ a V₀` plus its image. `ground_distance_m` is the electrode's
    /// distance `D` to its surroundings (sets the background `E₀ = V₀/D`).
    Grounded {
        /// Distance from the electrode to the surrounding RF ground, m.
        ground_distance_m: f64,
    },
}

/// Volume-equivalent sphere radius of a disc magnet, m.
pub fn equivalent_sphere_radius(m: &DiscMagnet) -> f64 {
    (0.75 * m.radius_m * m.radius_m * m.thickness_m).cbrt()
}

/// Ratio `E_z / E₀` of the RF field normal to the electrode at height `z`
/// above it and horizontal offset `rho`, with a conducting sphere of radius
/// `a` centred `h` above the electrode (first-order image model; valid for
/// `a` well below `h − z`). Heating per electron scales as its square.
pub fn es_field_ratio(state: ConductorState, a: f64, h: f64, z: f64, rho: f64) -> f64 {
    let r1 = (rho * rho + (z - h) * (z - h)).sqrt();
    let r2 = (rho * rho + (z + h) * (z + h)).sqrt();
    match state {
        ConductorState::Floating => {
            // Vertical dipole p = 4πε₀a³E₀ at h, same-sign image at −h:
            // E_z = p/(4πε₀) (3 dz² − R²)/R⁵ each.
            let f = |dz: f64, r: f64| (3.0 * dz * dz - r * r) / r.powi(5);
            1.0 + a.powi(3) * (f(z - h, r1) + f(z + h, r2))
        }
        ConductorState::Grounded { ground_distance_m } => {
            // q = −4πε₀ a V₀ at h, image −q at −h; E₀ = V₀/D.
            1.0 + a * ground_distance_m * ((z + h) / r2.powi(3) - (z - h) / r1.powi(3))
        }
    }
}

// ---------------------------------------------------------------------------
// Comparison against the reduced model + claims
// ---------------------------------------------------------------------------

/// Schema tag for the plasmoid claim family.
pub const CLAIM_SCHEMA: &str = "vcad.plasmoid-claims/1";
/// Domain tag in the unified receipt.
pub const RECEIPT_DOMAIN: &str = "plasmoid";

/// Derived-vs-fitted summary at one operating point (pressure, gap).
#[derive(Debug, Clone, PartialEq)]
pub struct LockReport {
    /// Pressure, Torr.
    pub pressure_torr: f64,
    /// Magnet gap (bottom face above the dish), m.
    pub gap_m: f64,
    /// Offsets sampled, m.
    pub rhos_m: Vec<f64>,
    /// Derived inward pull, m/s (all channels, with `params.heating_drive`).
    pub derived_inward_m_s: Vec<f64>,
    /// Derived inward pull with only electron heat conduction magnetized
    /// (uniform heating, unmagnetized particle transport).
    pub energy_only_inward_m_s: Vec<f64>,
    /// Derived inward pull with only particle transport magnetized
    /// (unmagnetized conduction, uniform heating).
    pub particle_only_inward_m_s: Vec<f64>,
    /// Derived swirl (CCW positive, north-down), m/s.
    pub derived_swirl_m_s: Vec<f64>,
    /// Reduced model's fitted inward pull `μ_fit dΦ/dρ`, m/s.
    pub fitted_inward_m_s: Vec<f64>,
    /// Reduced model's fitted swirl (its `v_spin` term), m/s.
    pub fitted_swirl_m_s: Vec<f64>,
    /// Reduced model's `Φ = ln(1+β²)` at each offset.
    pub phi_fit: Vec<f64>,
    /// β at the glow at each offset.
    pub beta: Vec<f64>,
    /// Inward pull of a *floating* conducting magnet (no B), m/s.
    pub es_floating_inward_m_s: Vec<f64>,
    /// Peak RF heating multiplier `(E/E₀)²` under a floating magnet.
    pub es_floating_heating_peak: f64,
    /// Peak RF heating multiplier under a hand-grounded magnet.
    pub es_grounded_heating_peak: f64,
    /// Fitted mobility used for the comparison, m²/s.
    pub mu_fit_m2_s: f64,
    /// Race speed, m/s.
    pub race_speed_m_s: f64,
    /// `β_c = √(μ_e/μ_i)`.
    pub critical_beta: f64,
    /// Calibrated `H₀`, V/s.
    pub heating_per_electron: f64,
    /// Electron temperature of the calibrated foot (peak), V.
    pub te_peak_v: f64,
    /// Largest electron E×B fluid speed seen near the foot, m/s.
    pub max_exb_m_s: f64,
    /// `T_e` at `−2σ … 2σ` through the foot (negative = magnet side) at
    /// each offset, V.
    pub te_profiles_v: Vec<[f64; 5]>,
    /// Net growth rate of the foot at each offset, s⁻¹ (sustaining margin —
    /// a second "preference" criterion, distinct from the drift).
    pub growth_per_s: Vec<f64>,
    /// Derived inward pull with field-driven RF heating
    /// ([`HeatingDrive::FieldDriven`]), m/s.
    pub field_driven_inward_m_s: Vec<f64>,
    /// Derived inward pull with current-driven RF heating
    /// ([`HeatingDrive::CurrentDriven`]), m/s.
    pub current_driven_inward_m_s: Vec<f64>,
    /// Net growth of a foot centred under the magnet, s⁻¹ (sustaining
    /// margin; 0 = the unmagnetized calibration point).
    pub growth_under_magnet_per_s: f64,
    /// Grid cells per side.
    pub cells: usize,
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len()) as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa * sbb).sqrt().max(1e-300)
}

/// Index and signed value of the largest-magnitude entry.
fn peak(v: &[f64]) -> (usize, f64) {
    v.iter().enumerate().fold(
        (0, 0.0),
        |b, (i, &x)| if x.abs() > b.1.abs() { (i, x) } else { b },
    )
}

impl LockReport {
    /// Build the report: derived pulls (both / energy-only / particle-only
    /// channels), the floating-conductor pull, and the reduced model's
    /// fitted `μ_fit ∇Φ` and spin term, all at `pressure_torr`, `gap_m`.
    /// `mu_fit` is the reduced model's fitted mobility (1 Torr calibration).
    pub fn build(
        params: FluidParams,
        magnet: &DiscMagnet,
        gap_m: f64,
        rhos_m: &[f64],
        reduced: &crate::plasmoid::PlasmaParams,
    ) -> Result<Self, FluidError> {
        let scene = disc_scene(magnet, gap_m, true);
        let model = FootModel::calibrate(params)?;
        let mut energy = params;
        energy.magnetize_particles = false;
        energy.heating_drive = HeatingDrive::Uniform;
        let energy = FootModel {
            params: energy,
            ..model.clone()
        };
        let mut particle = params;
        particle.magnetize_energy = false;
        particle.heating_drive = HeatingDrive::Uniform;
        let particle = FootModel {
            params: particle,
            ..model.clone()
        };
        let with_drive = |drive: HeatingDrive| FootModel {
            params: FluidParams {
                heating_drive: drive,
                ..params
            },
            ..model.clone()
        };
        let field_driven = with_drive(HeatingDrive::FieldDriven);
        let current_driven = with_drive(HeatingDrive::CurrentDriven);
        let a = equivalent_sphere_radius(magnet);
        let hc = gap_m + 0.5 * magnet.thickness_m;
        let zg = params.glow_height_m;
        let floating = |p: [f64; 2]| {
            es_field_ratio(ConductorState::Floating, a, hc, zg, p[0].hypot(p[1])).powi(2)
        };
        let mut r = Self {
            pressure_torr: params.pressure_torr,
            gap_m,
            rhos_m: rhos_m.to_vec(),
            derived_inward_m_s: vec![],
            energy_only_inward_m_s: vec![],
            particle_only_inward_m_s: vec![],
            derived_swirl_m_s: vec![],
            fitted_inward_m_s: vec![],
            fitted_swirl_m_s: vec![],
            phi_fit: vec![],
            beta: vec![],
            es_floating_inward_m_s: vec![],
            es_floating_heating_peak: floating([0.0, 0.0]),
            es_grounded_heating_peak: es_field_ratio(
                ConductorState::Grounded {
                    ground_distance_m: 0.15,
                },
                a,
                hc,
                zg,
                0.0,
            )
            .powi(2),
            mu_fit_m2_s: reduced.mobility_m2_s,
            race_speed_m_s: reduced.race_speed_m_s,
            critical_beta: params.critical_beta(),
            heating_per_electron: model.heating_per_electron,
            te_peak_v: model.t_profile.iter().cloned().fold(0.0, f64::max),
            max_exb_m_s: 0.0,
            te_profiles_v: vec![],
            growth_per_s: vec![],
            field_driven_inward_m_s: vec![],
            current_driven_inward_m_s: vec![],
            growth_under_magnet_per_s: model.drift(Some(&scene), [0.0, 0.0])?.growth_rate_per_s,
            cells: params.cells,
        };
        for &rho in rhos_m {
            let foot = [rho, 0.0];
            let d = model.drift(Some(&scene), foot)?;
            r.derived_inward_m_s.push(-d.velocity_m_s[0]);
            r.derived_swirl_m_s.push(d.velocity_m_s[1]);
            r.max_exb_m_s = r.max_exb_m_s.max(d.max_exb_m_s);
            r.te_profiles_v.push(d.te_profile_x_v);
            r.growth_per_s.push(d.growth_rate_per_s);
            r.energy_only_inward_m_s
                .push(-energy.drift(Some(&scene), foot)?.velocity_m_s[0]);
            r.particle_only_inward_m_s
                .push(-particle.drift(Some(&scene), foot)?.velocity_m_s[0]);
            r.field_driven_inward_m_s
                .push(-field_driven.drift(Some(&scene), foot)?.velocity_m_s[0]);
            r.current_driven_inward_m_s
                .push(-current_driven.drift(Some(&scene), foot)?.velocity_m_s[0]);
            r.es_floating_inward_m_s.push(
                -model
                    .drift_with_heating(None, foot, &floating)?
                    .velocity_m_s[0],
            );
            let g = reduced.grad_potential_in(&scene, rho, 0.0);
            r.fitted_inward_m_s.push(-reduced.mobility_m2_s * g[0]);
            r.phi_fit.push(reduced.potential_in(&scene, rho, 0.0));
            r.beta.push(reduced.beta_in(&scene, rho, 0.0));
            // The reduced model's spin term, φ̂ component at (ρ, 0).
            let v_all = reduced.velocity_in(Some(&scene), foot);
            let mut no_spin = *reduced;
            no_spin.spin_speed_m_s = 0.0;
            let v_ns = no_spin.velocity_in(Some(&scene), foot);
            r.fitted_swirl_m_s.push(v_all[1] - v_ns[1]);
        }
        Ok(r)
    }

    /// `(ρ, value)` of the largest-magnitude derived inward pull (negative
    /// = pushed away from the magnet).
    pub fn derived_peak(&self) -> (f64, f64) {
        let (i, v) = peak(&self.derived_inward_m_s);
        (self.rhos_m[i], v)
    }

    /// `(ρ, value)` of the largest-magnitude fitted inward pull.
    pub fn fitted_peak(&self) -> (f64, f64) {
        let (i, v) = peak(&self.fitted_inward_m_s);
        (self.rhos_m[i], v)
    }

    /// Correlation of the derived and fitted inward-pull profiles over the
    /// sampled offsets (shape agreement; +1 same shape and sign, −1 same
    /// shape, opposite sign). Amplitude-free, so it is a derived quantity.
    pub fn shape_correlation(&self) -> f64 {
        correlation(&self.derived_inward_m_s, &self.fitted_inward_m_s)
    }

    /// Ratio of the largest-magnitude magnetic (derived) pull to the
    /// floating-conductor pull — same model, same amplitude convention, so
    /// the ratio is derived even though each amplitude is not.
    pub fn magnetic_to_floating_ratio(&self) -> f64 {
        self.derived_peak().1 / peak(&self.es_floating_inward_m_s).1
    }

    /// Predicted claims (basis Predicted — a receipt built from them rolls
    /// up Provisional, never Pass).
    pub fn claims(&self) -> Vec<vcad_receipt::ReceiptClaim> {
        let oracle = vcad_receipt::OracleRef::new(
            "vcad-kernel-particle/plasmoid_fluid",
            env!("CARGO_PKG_VERSION"),
        );
        let prov = format!(
            "{CLAIM_SCHEMA}; Ar {:.3} Torr, gap {:.1} mm, grid {}², H0 {:.3e} V/s, Te_peak {:.2} V, beta_c {:.1}; assumed: foot sigma, heated core, sheet thickness, frozen nu",
            self.pressure_torr,
            self.gap_m * 1e3,
            self.cells,
            self.heating_per_electron,
            self.te_peak_v,
            self.critical_beta
        );
        let (rp, vp) = self.derived_peak();
        let sign = |v: f64| {
            if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                0.0
            }
        };
        let (_, vf) = peak(&self.field_driven_inward_m_s);
        let (_, vc) = peak(&self.current_driven_inward_m_s);
        // Amplitudes are skew rates of a frozen profile, not foot speeds, so
        // every claim here is amplitude-free (signs, ratios, positions).
        let rows: [(&str, f64, &str, String); 7] = [
            (
                "derived_pull_sign",
                sign(vp),
                "1",
                "+1: the magnetized glow pulls the foot toward the footprint; -1: pushes it away (uniform RF heating)".into(),
            ),
            (
                "derived_peak_offset",
                rp,
                "m",
                "offset from the footprint where the derived |pull| peaks".into(),
            ),
            (
                "pull_shape_correlation",
                self.shape_correlation(),
                "1",
                "correlation of derived vs fitted mu*grad(ln(1+beta^2)) profiles (+1 same, -1 mirrored)".into(),
            ),
            (
                "magnetic_to_floating_conductor_ratio",
                self.magnetic_to_floating_ratio(),
                "1",
                "peak magnetic pull / peak pull of a floating conducting slug of the same size (signed)".into(),
            ),
            (
                "field_driven_pull_sign",
                sign(vf),
                "1",
                "derived pull sign with field-driven RF heating (sigma_zz-weighted)".into(),
            ),
            (
                "current_driven_pull_sign",
                sign(vc),
                "1",
                "derived pull sign with current-driven RF heating (1/sigma_zz-weighted)".into(),
            ),
            (
                "grounded_conductor_heating",
                self.es_grounded_heating_peak,
                "1",
                "RF heating multiplier under a hand-grounded conducting magnet (image model, D = 150 mm)".into(),
            ),
        ];
        rows.into_iter()
            .map(|(name, value, unit, note)| {
                let q = if unit == "1" {
                    vcad_receipt::ClaimQuantity::bare(value)
                } else {
                    vcad_receipt::ClaimQuantity::new(value, unit)
                };
                vcad_receipt::ReceiptClaim::pass(
                    format!("plasmoid.{name}"),
                    RECEIPT_DOMAIN,
                    note,
                    oracle.clone(),
                )
                .with_basis(vcad_receipt::ClaimBasis::Predicted)
                .with_measured(q)
                .with_details(prov.clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasmoid::{MagnetPose, PlasmaParams};

    fn coarse(p: f64) -> FluidParams {
        FluidParams {
            cells: 40,
            ..FluidParams::argon(p)
        }
    }

    fn scene(gap: f64, north_down: bool) -> FieldScene {
        disc_scene(&DiscMagnet::n52_10x5(), gap, north_down)
    }

    #[test]
    fn mobility_tensor_limits() {
        let mu = 47.0;
        let m0 = electron_mobility_tensor(mu, [0.0; 3]);
        for (i, row) in m0.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert_eq!(*v, if i == j { mu } else { 0.0 });
            }
        }
        // Strongly magnetized along z: v = −M E → E×B/B² for in-plane E.
        let b = 2.0;
        let m = electron_mobility_tensor(mu, [0.0, 0.0, b]);
        let e = [100.0, 0.0, 0.0];
        let v: Vec<f64> = (0..3)
            .map(|i| -(0..3).map(|j| m[i][j] * e[j]).sum::<f64>())
            .collect();
        let exb = [0.0, -e[0] * b / (b * b), 0.0]; // E×B/B² = (Ex x̂)×(B ẑ)/B² = −Ex/B ŷ
        let beta = mu * b;
        assert!((v[1] - exb[1]).abs() < 2.0 / beta * exb[1].abs(), "{v:?}");
        assert!(v[0].abs() < 2.0 / beta * exb[1].abs());
        // Parallel transport untouched.
        assert!((m[2][2] - mu).abs() < 1e-12);
    }

    #[test]
    fn ambipolar_closed_form_and_ion_limit() {
        let fp = FluidParams::argon(1.0);
        let (mu_e, mu_i, d_i) = (fp.mu_e(), fp.mu_i(), fp.d_i());
        let te = 2.0;
        let amb = AmbipolarTensors::new(mu_i, d_i, &electron_mobility_tensor(mu_e, [0.0; 3]));
        let d = amb.d_a(te);
        let exact = (mu_e * d_i + mu_i * mu_e * te) / (mu_i + mu_e);
        assert!((d[0][0] - exact).abs() < 1e-12 * exact);
        assert!(d[0][1].abs() < 1e-15);
        // β = 3 across the plane: in-plane D_a changes by only a few % —
        // ions limit it (β_c ≈ 19).
        let b = 3.0 / mu_e;
        let ampb = AmbipolarTensors::new(mu_i, d_i, &electron_mobility_tensor(mu_e, [0.0, 0.0, b]));
        let dm = ampb.d_a(te);
        let rel = (dm[0][0] - d[0][0]).abs() / d[0][0];
        assert!(rel < 0.05, "particle channel moved D_a by {rel}");
        assert!(fp.critical_beta() > 15.0 && fp.critical_beta() < 25.0);
        // Pressure-independent.
        assert!((FluidParams::argon(0.1).critical_beta() - fp.critical_beta()).abs() < 1e-9);
    }

    #[test]
    fn calibration_sustains_and_stays_in_fit_range() {
        let m = FootModel::calibrate(coarse(1.0)).unwrap();
        let d = m.drift(None, [0.0, 0.0]).unwrap();
        assert!(d.growth_rate_per_s.abs() < 1.0, "{}", d.growth_rate_per_s);
        assert!(
            d.te_peak_v > argon::FIT_TE_MIN_V && d.te_peak_v < argon::FIT_TE_MAX_V,
            "{}",
            d.te_peak_v
        );
    }

    #[test]
    fn unmagnetized_foot_does_not_move_and_conserves() {
        let m = FootModel::calibrate(coarse(1.0)).unwrap();
        for foot in [[0.0, 0.0], [0.02, -0.01]] {
            let d = m.drift(None, foot).unwrap();
            let v = d.velocity_m_s[0].hypot(d.velocity_m_s[1]);
            assert!(v < 1e-6, "no field but v = {v}");
            assert!(
                d.conservation_residual < 1e-9,
                "{}",
                d.conservation_residual
            );
        }
    }

    /// The magnet moves the foot (the *sign* of the radial pull is a
    /// result, reported in `docs/plasmoid-fluid-m0.md`, not asserted here).
    /// Null: a uniform vertical field (any strength) breaks no symmetry,
    /// so the foot cannot drift; it only confines heat, so growth rises.
    #[test]
    fn uniform_field_gives_no_drift_but_more_growth() {
        let m = FootModel::calibrate(coarse(1.0)).unwrap();
        let bz = 3.0 / m.params.mu_e(); // β = 3 everywhere
        let d = m
            .drift_in(&|_| [0.0, 0.0, bz], [0.01, 0.0], &|_| 1.0)
            .unwrap();
        let v = d.velocity_m_s[0].hypot(d.velocity_m_s[1]);
        assert!(v < 1e-6, "uniform B but v = {v}");
        assert!(d.growth_rate_per_s > 0.0, "{}", d.growth_rate_per_s);
    }

    /// Null: with uniform per-electron heating (`f_bg = 1`) and unmagnetized
    /// particle transport, T_e is uniform, so the conduction channel has
    /// nothing to act on.
    #[test]
    fn uniform_heating_zeroes_the_conduction_channel() {
        let m = FootModel::calibrate(FluidParams {
            background_heating: 1.0,
            magnetize_particles: false,
            ..coarse(1.0)
        })
        .unwrap();
        let d = m.drift(Some(&scene(10e-3, true)), [6e-3, 0.0]).unwrap();
        let v = d.velocity_m_s[0].hypot(d.velocity_m_s[1]);
        assert!(v < 1e-6, "uniform heating but v = {v}");
    }

    /// The moment sign convention: more heating on one side moves the foot
    /// toward it (no field involved).
    #[test]
    fn heating_tilt_moves_the_foot_toward_the_heat() {
        let m = FootModel::calibrate(coarse(1.0)).unwrap();
        let d = m
            .drift_with_heating(None, [0.0, 0.0], &|p| 1.0 + 5.0 * p[0])
            .unwrap();
        assert!(d.velocity_m_s[0] > 0.0 && d.velocity_m_s[1].abs() < 1e-6 * d.velocity_m_s[0]);
    }

    #[test]
    fn magnet_pulls_and_conserves() {
        let m = FootModel::calibrate(coarse(1.0)).unwrap();
        let d = m.drift(Some(&scene(10e-3, true)), [6e-3, 0.0]).unwrap();
        assert!(d.velocity_m_s[0].abs() > 1e-3, "{d:?}");
        assert!(
            d.conservation_residual < 1e-9,
            "{}",
            d.conservation_residual
        );
    }

    #[test]
    fn polarity_flips_swirl_not_pull() {
        let m = FootModel::calibrate(coarse(1.0)).unwrap();
        let a = m.drift(Some(&scene(10e-3, true)), [6e-3, 0.0]).unwrap();
        let b = m.drift(Some(&scene(10e-3, false)), [6e-3, 0.0]).unwrap();
        let rel = |x: f64, y: f64| (x - y).abs() / x.abs().max(y.abs()).max(1e-30);
        assert!(rel(a.velocity_m_s[0], b.velocity_m_s[0]) < 1e-6);
        assert!(a.velocity_m_s[1].abs() > 1e-6 * a.velocity_m_s[0].abs());
        assert!(rel(a.velocity_m_s[1], -b.velocity_m_s[1]) < 1e-6);
    }

    /// The magnet's effect, made dimensionless so that rates scaling with
    /// pressure cannot fake it: the shift of the foot's mean T_e, and the
    /// pull in units of `σ ν_iz`.
    #[test]
    fn atmosphere_kills_the_pull() {
        let s = scene(10e-3, true);
        let effect = |torr: f64| {
            let fp = coarse(torr);
            let m = FootModel::calibrate(fp).unwrap();
            let free = m.drift(None, [6e-3, 0.0]).unwrap();
            let mag = m.drift(Some(&s), [6e-3, 0.0]).unwrap();
            let nu_iz = fp.n_gas() * argon::k_iz(free.te_mean_v);
            (
                ((mag.te_mean_v - free.te_mean_v) / free.te_mean_v).abs(),
                (-mag.velocity_m_s[0]) / (fp.foot_sigma_m * nu_iz),
            )
        };
        let (dt_low, v_low) = effect(1.0);
        let (dt_atm, v_atm) = effect(760.0);
        assert!(
            dt_low > 1e-3 && v_low.abs() > 1e-4,
            "1 Torr: {dt_low} {v_low}"
        );
        assert!(
            dt_atm < 1e-3 * dt_low,
            "ΔT_e/T_e: 1 atm {dt_atm} vs 1 Torr {dt_low}"
        );
        assert!(
            v_atm.abs() < 1e-3 * v_low.abs(),
            "pull: 1 atm {v_atm} vs 1 Torr {v_low}"
        );
    }

    #[test]
    fn pull_converges_under_refinement() {
        let v = |cells: usize| {
            let m = FootModel::calibrate(FluidParams {
                cells,
                ..FluidParams::argon(1.0)
            })
            .unwrap();
            -m.drift(Some(&scene(10e-3, true)), [6e-3, 0.0])
                .unwrap()
                .velocity_m_s[0]
        };
        let (a, b, c) = (v(24), v(48), v(96));
        let (e1, e2) = ((a - b).abs(), (b - c).abs());
        assert!(e2 < 0.4 * e1, "not converging: {a} {b} {c}");
        assert!(e2 < 0.05 * c.abs(), "{a} {b} {c}");
    }

    #[test]
    fn es_field_ratios() {
        let (a, h) = (4e-3, 12e-3);
        // Directly under at the electrode surface: 1 + 4(a/h)³ (floating).
        let f = es_field_ratio(ConductorState::Floating, a, h, 0.0, 0.0);
        assert!((f - (1.0 + 4.0 * (a / h).powi(3))).abs() < 1e-12);
        let g = es_field_ratio(
            ConductorState::Grounded {
                ground_distance_m: 0.15,
            },
            a,
            h,
            0.0,
            0.0,
        );
        assert!((g - (1.0 + 2.0 * a * 0.15 / (h * h))).abs() < 1e-12);
        assert!(g > f);
        // Far away: no perturbation.
        let far = es_field_ratio(ConductorState::Floating, a, h, 0.0, 1.0);
        assert!((far - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lock_map_feeds_the_reduced_model() {
        let model = FootModel::calibrate(FluidParams {
            cells: 32,
            ..FluidParams::argon(1.0)
        })
        .unwrap();
        let mag = DiscMagnet::n52_10x5();
        let map = LockMap::compute(&model, mag, &[8e-3, 12e-3], &[0.0, 4e-3, 8e-3, 16e-3]).unwrap();
        // Nodes reproduce the table; polarity flips the swirl.
        let pose = MagnetPose {
            center_m: [0.01, 0.02, 8e-3 + 2.5e-3],
            axis: [0.0, 0.0, -1.0],
        };
        let sc = pose.scene(&mag);
        assert_eq!(map.coverage(&sc), (1, 0));
        let v = map.pull_in(&sc, [0.01 + 4e-3, 0.02]);
        assert!((-v[0] - map.inward_m_s[1]).abs() < 1e-12);
        assert!((v[1] - map.swirl_m_s[1]).abs() < 1e-12);
        let up = MagnetPose {
            axis: [0.0, 0.0, 1.0],
            ..pose
        }
        .scene(&mag);
        let vu = map.pull_in(&up, [0.01 + 4e-3, 0.02]);
        assert!((vu[1] + v[1]).abs() < 1e-12 && (vu[0] - v[0]).abs() < 1e-12);
        // Beyond the table: nothing.
        assert_eq!(map.pull_in(&sc, [0.01 + 0.03, 0.02]), [0.0, 0.0]);
        // Derived mode of the reduced model: race + radial + derived pull.
        let p = PlasmaParams::argon_dish(1.0);
        let pos = [0.01 + 4e-3, 0.02];
        let fitted_free = {
            let mut q = p;
            q.mobility_m2_s = 0.0;
            q.spin_speed_m_s = 0.0;
            q.velocity_in(None, pos)
        };
        let vd = p.velocity_with(crate::plasmoid::LockModel::Derived(&map), Some(&sc), pos);
        assert!((vd[0] - fitted_free[0] - v[0]).abs() < 1e-12);
        assert!((vd[1] - fitted_free[1] - v[1]).abs() < 1e-12);
        // The fitted option is the existing model, bit for bit.
        let mut pf = p;
        pf.mobility_m2_s = 3e-3;
        assert_eq!(
            pf.velocity_with(crate::plasmoid::LockModel::Fitted, Some(&sc), pos),
            pf.velocity_in(Some(&sc), pos)
        );
    }

    #[test]
    fn report_claims_roll_up_provisional() {
        let mut reduced = PlasmaParams::argon_dish(1.0);
        reduced.mobility_m2_s = 5e-3;
        let r = LockReport::build(
            FluidParams {
                cells: 24,
                ..FluidParams::argon(1.0)
            },
            &DiscMagnet::n52_10x5(),
            10e-3,
            &[3e-3, 6e-3, 9e-3],
            &reduced,
        )
        .unwrap();
        let claims = r.claims();
        assert_eq!(claims.len(), 7);
        for c in &claims {
            assert!(c.id.starts_with("plasmoid."));
            assert_eq!(c.basis, Some(vcad_receipt::ClaimBasis::Predicted));
        }
        let receipt = vcad_receipt::DesignReceipt::with_claims(claims);
        assert_eq!(receipt.verdict(), vcad_receipt::ReceiptVerdict::Provisional);
        // The particle channel alone is weak next to the full pull.
        let (i, full) = peak(&r.derived_inward_m_s);
        assert!(r.particle_only_inward_m_s[i].abs() < 0.5 * full.abs());
    }
}
