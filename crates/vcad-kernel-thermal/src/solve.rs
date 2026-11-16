//! Steady-state finite-volume conduction solver.
//!
//! Discretization: integrate ∇·(k∇T) + q‴ = 0 over each voxel. The flux
//! between voxels P and N through their shared face of area A at spacing d
//! is G·(T_N − T_P) with the **series-resistance (harmonic-mean) face
//! conductance**
//!
//! ```text
//! G = A / ( d/(2·k_P) + d/(2·k_N) )
//! ```
//!
//! — the two half-cells conduct in series. This is the standard
//! finite-volume interface treatment, and it is *exact* for 1D layered
//! composites (the composite-slab test reproduces the series-resistance
//! formula to machine precision). The arithmetic mean (k_P + k_N)/2 is
//! wrong at material interfaces: it lets the high-k side short across the
//! face — a copper|air face would conduct like half-strength copper
//! instead of like air, an error of ~4 orders of magnitude. Harmonic
//! recovers the limiting resistance of the worse conductor, which is the
//! physics.
//!
//! Boundary faces put the half-cell resistance d/(2k) in series with the
//! surface condition: a fixed-temperature face is G = 2kA/d to the surface
//! temperature; a convection face adds the 1/h film, G = A/(d/(2k) + 1/h),
//! so h → ∞ recovers Dirichlet. Because every internal face conductance is
//! shared symmetrically between its two voxels, the scheme is
//! **conservative by construction**: heat leaving P through a face enters
//! N exactly, and global energy balance closes to solver tolerance (it is
//! reported on every solution, never assumed).
//!
//! The linear system is symmetric positive definite (given every free
//! region has some path to a temperature reference — checked, fail-closed)
//! and is solved matrix-free by Jacobi-preconditioned conjugate gradients.
//! The stopping criterion is ‖r‖ ≤ tol·‖b‖ — **relative to the
//! right-hand-side norm** (sources + boundary drives), the scale-invariant
//! choice: an absolute epsilon would read a milliwatt problem as converged
//! at iteration zero.
//!
//! **Radiation (M7).** A [`Boundary::Radiation`] face adds a nonlinear
//! surface loss q(T_p) per face: the half-cell resistance r = d/(2k) in
//! series with the gray surface q″ = ε_eff·σ·(T_s⁴ − T_∞⁴) + h·(T_s − T_∞),
//! with the surface temperature T_s solved per face by safeguarded
//! Newton–bisection (q″ is monotone in T_s, so the root is bracketed by
//! T_∞ and T_p). The global system R(T) = A·T − b + Q(T) = 0 is then
//! solved by **Newton**: the Jacobian is A + diag(∂Q/∂T_p) — still SPD,
//! since ∂Q/∂T_p = A_f·h_s/(1 + h_s·r) > 0 with h_s = 4ε_effσT_s³ + h — so
//! every Newton step is one more PCG solve with the same matrix-free
//! operator and a bigger diagonal. The steady solve starts from a uniform
//! field at the radiation ambient, whose first Newton step is the problem
//! linearized at ambient (a supersolution: T⁴ is convex, so linearizing
//! at T_∞ under-counts cooling); from above, Newton on a convex monotone
//! loss descends monotonically. Later steps are backtracked on ‖R‖.
//! Inner tolerances follow Eisenstat–Walker (η = min(0.1, ‖R‖_rel),
//! never tighter than needed for the outer tolerance). The stopping test
//! is ‖R‖ ≤ tol·max(‖b‖, ‖Q‖) — the same scale-invariant criterion, with
//! the radiated flux standing in for b when a part radiates with no
//! source of its own. Conduction-only models never touch this path.

use crate::model::{
    face_index, Boundary, Enclosure, ModelError, ThermalModel, KELVIN_OFFSET_C, STEFAN_BOLTZMANN,
};
use serde::Serialize;

/// BC-slot index used for `ThermalModel::exposed` in convection links and
/// film-coefficient gradients (slots 0..=5 are the domain faces in
/// [`face_index`] order).
pub(crate) const EXPOSED_SLOT: usize = 6;

/// Options for [`solve_steady`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolveOptions {
    /// Relative residual tolerance: stop when ‖r‖₂ ≤ tol·‖b‖₂.
    pub tol: f64,
    /// Hard cap on CG iterations.
    pub max_iters: usize,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            tol: 1e-8,
            max_iters: 50_000,
        }
    }
}

/// Failure modes of [`solve_steady`] (all fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum SolveError {
    /// The model failed validation.
    Model(ModelError),
    /// A connected region of free voxels has no path to any temperature
    /// reference (no Dirichlet face, reservoir, or convection link): its
    /// temperature is undefined (or unbounded, if it holds a source).
    FloatingRegion {
        /// Voxel count of the floating component.
        voxels: usize,
        /// Center of one voxel in the component, mm.
        near_mm: [f64; 3],
    },
    /// Sources are present but no θ reference is resolvable: set
    /// `ThermalModel::reference_c`, or use a single convection ambient.
    AmbiguousReference,
    /// CG did not reach `tol` within `max_iters`.
    NotConverged {
        /// Final relative residual ‖r‖/‖b‖.
        residual_rel: f64,
        /// Iterations performed.
        iterations: usize,
    },
    /// A transient solve was asked for a non-positive time step or zero
    /// steps.
    InvalidTimeStep,
    /// A transient schedule segment named a source, face, or fixed region
    /// it cannot override (unknown name, out-of-range slot, or an
    /// adiabatic face with no temperature to move).
    BadScheduleOverride(String),
    /// The smooth-max exponent must be finite and > 1.
    InvalidSmoothingExponent,
    /// The Newton iteration for a radiating model did not reach `tol`
    /// (iteration cap, or a line search that could not reduce ‖R‖).
    NonlinearNotConverged {
        /// Final relative nonlinear residual.
        residual_rel: f64,
        /// Newton iterations performed.
        iterations: usize,
    },
    /// The requested path does not support radiation faces yet (the
    /// string names it) — fail-closed rather than silently dropping the
    /// nonlinear loss.
    RadiationUnsupported(String),
}

impl std::fmt::Display for SolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SolveError::Model(e) => write!(f, "invalid model: {e}"),
            SolveError::FloatingRegion { voxels, near_mm } => write!(
                f,
                "floating region of {voxels} voxels near ({:.2}, {:.2}, {:.2}) mm has no \
                 temperature reference (all faces adiabatic); its temperature is undefined",
                near_mm[0], near_mm[1], near_mm[2]
            ),
            SolveError::AmbiguousReference => write!(
                f,
                "theta reference unresolvable: set reference_c or use a single convection ambient"
            ),
            SolveError::NotConverged {
                residual_rel,
                iterations,
            } => write!(
                f,
                "CG not converged after {iterations} iterations (relative residual {residual_rel:.3e})"
            ),
            SolveError::InvalidTimeStep => {
                write!(f, "transient solve requires dt > 0 and at least one step")
            }
            SolveError::BadScheduleOverride(why) => {
                write!(f, "invalid schedule override: {why}")
            }
            SolveError::InvalidSmoothingExponent => {
                write!(f, "smooth-max exponent p must be finite and > 1")
            }
            SolveError::NonlinearNotConverged {
                residual_rel,
                iterations,
            } => write!(
                f,
                "radiation Newton iteration not converged after {iterations} iterations \
                 (relative residual {residual_rel:.3e})"
            ),
            SolveError::RadiationUnsupported(what) => write!(
                f,
                "{what} does not support radiation boundaries yet (the nonlinear surface loss \
                 would be silently dropped)"
            ),
        }
    }
}

impl std::error::Error for SolveError {}

impl From<ModelError> for SolveError {
    fn from(e: ModelError) -> Self {
        SolveError::Model(e)
    }
}

/// Per-source figures of merit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceReport {
    /// Source name.
    pub name: String,
    /// Total source power, W.
    pub power_w: f64,
    /// Hottest temperature over the source's voxels, °C.
    pub t_max_c: f64,
    /// Center of the hottest source voxel, mm.
    pub t_max_at_mm: [f64; 3],
    /// Thermal resistance θ = (t_max_c − reference_c)/power_w, K/W.
    /// `None` when the source power is zero.
    pub theta_c_per_w: Option<f64>,
}

/// Global energy bookkeeping, from the solved field.
///
/// Flows are positive **out of** the conducting system. In steady state the
/// source power must equal the net outflow; `residual_rel` is how far the
/// solved field misses that, normalized by the larger of the source power
/// and the gross boundary traffic. It closes to solver tolerance — if it
/// doesn't, the solution is wrong and the number says so.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnergyBalance {
    /// Total source power into free voxels, W.
    pub source_w: f64,
    /// Net heat out through fixed-temperature domain/exposed faces, W.
    pub fixed_face_out_w: f64,
    /// Net heat out through convection faces, W.
    pub convection_out_w: f64,
    /// Net heat out into fixed-temperature reservoirs, W.
    pub fixed_region_out_w: f64,
    /// Net heat out through radiation faces (radiative plus any parallel
    /// film on those faces), W. Zero for conduction-only models.
    pub radiation_out_w: f64,
    /// Sum of the four outflows, W.
    pub net_out_w: f64,
    /// |source − net out| / max(|source|, gross flow).
    pub residual_rel: f64,
}

/// The solved temperature field and its figures of merit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Solution {
    /// Voxel counts per axis.
    pub divisions: [usize; 3],
    /// Domain minimum corner, mm.
    pub origin_mm: [f64; 3],
    /// Voxel edge lengths, mm.
    pub voxel_mm: [f64; 3],
    /// Temperature per voxel, °C; `NaN` for void voxels.
    /// Layout: `index = (k·ny + j)·nx + i`.
    pub t_c: Vec<f64>,
    /// Which voxels carry material.
    pub solid: Vec<bool>,
    /// CG iterations used.
    pub iterations: usize,
    /// Final relative residual ‖r‖/‖b‖.
    pub residual_rel: f64,
    /// Hottest solid-voxel temperature, °C.
    pub t_max_c: f64,
    /// Center of the hottest voxel, mm.
    pub t_max_at_mm: [f64; 3],
    /// θ reference temperature, °C (`None` when no sources needed one).
    pub reference_c: Option<f64>,
    /// Per-source reports, in model order.
    pub sources: Vec<SourceReport>,
    /// Per-reservoir heat flows, in `ThermalModel::fixed` order.
    pub reservoirs: Vec<ReservoirReport>,
    /// Energy balance of the solved field.
    pub energy: EnergyBalance,
    /// Newton iterations of the nonlinear (radiation) solve; 0 when the
    /// model is linear. `iterations` is then the CG total across steps.
    pub newton_iterations: usize,
    /// Per-slot radiation reports (empty for conduction-only models).
    pub radiation: Vec<RadiationReport>,
}

/// What one radiating BC slot did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RadiationReport {
    /// Slot label (`-x … +z`, `exposed`).
    pub label: String,
    /// Surface emissivity as specified.
    pub emissivity: f64,
    /// Effective emissivity after the enclosure exchange (= ε without
    /// an enclosure).
    pub effective_emissivity: f64,
    /// Parallel film coefficient on these faces, W/(m²·K).
    pub h_w_m2k: f64,
    /// Radiating voxel-face count.
    pub faces: usize,
    /// Raw voxel-staircase area of the slot, m² (before `area_scale`).
    pub voxel_area_m2: f64,
    /// Radiating area actually used, m² (= voxel area × `area_scale`).
    pub area_m2: f64,
    /// Heat leaving through the slot, W (radiation + parallel film).
    pub heat_out_w: f64,
    /// Of which radiation alone, W.
    pub radiated_w: f64,
    /// Area-weighted mean surface temperature of the slot, °C.
    pub mean_surface_c: f64,
    /// Hottest face surface temperature, °C.
    pub max_surface_c: f64,
}

impl Solution {
    /// Linear index of voxel `(i, j, k)`.
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (k * self.divisions[1] + j) * self.divisions[0] + i
    }

    /// Temperature of voxel `(i, j, k)`, °C (`NaN` for void).
    pub fn temperature_c(&self, i: usize, j: usize, k: usize) -> f64 {
        self.t_c[self.index(i, j, k)]
    }

    /// Center of voxel `(i, j, k)`, mm.
    pub fn voxel_center_mm(&self, i: usize, j: usize, k: usize) -> [f64; 3] {
        [
            self.origin_mm[0] + (i as f64 + 0.5) * self.voxel_mm[0],
            self.origin_mm[1] + (j as f64 + 0.5) * self.voxel_mm[1],
            self.origin_mm[2] + (k as f64 + 0.5) * self.voxel_mm[2],
        ]
    }
}

/// Heat absorbed by one fixed-temperature reservoir.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReservoirReport {
    /// The reservoir's pinned temperature, °C.
    pub temperature_c: f64,
    /// Net heat flowing from the free system into this reservoir, W
    /// (negative when the reservoir heats the part).
    pub heat_absorbed_w: f64,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum LinkKind {
    FixedFace,
    /// Convection link; the payload is the BC slot that created it:
    /// 0..=5 for the domain faces (in `face_index` order), 6 for
    /// `ThermalModel::exposed`.
    Convection(usize),
    /// Link into a fixed region; the payload is the region index in
    /// `ThermalModel::fixed` that owns the pinned voxel (painter's order).
    FixedRegion(usize),
}

pub(crate) struct BcLink {
    pub(crate) voxel: usize,
    /// Face axis (0, 1, 2) — the conductance's half-cell distance and
    /// area follow it.
    pub(crate) axis: usize,
    pub(crate) g: f64,
    pub(crate) t_ref: f64,
    pub(crate) kind: LinkKind,
}

/// A nonlinear radiation face: half-cell resistance in series with a gray
/// surface (+ optional parallel film) to an ambient.
#[derive(Clone)]
pub(crate) struct RadLink {
    pub(crate) voxel: usize,
    pub(crate) axis: usize,
    /// Radiating area (voxel face × `area_scale`), m².
    pub(crate) area_m2: f64,
    /// Half-cell resistance per unit area d/(2k), m²·K/W.
    pub(crate) r_half: f64,
    /// Effective emissivity (enclosure applied).
    pub(crate) eps_eff: f64,
    /// Parallel film, W/(m²·K).
    pub(crate) h: f64,
    /// Ambient, °C.
    pub(crate) t_amb_c: f64,
    /// BC slot (0..=5 domain faces, 6 exposed).
    pub(crate) slot: usize,
}

/// One face's solved surface state.
pub(crate) struct RadFace {
    /// Surface temperature, °C.
    pub(crate) ts_c: f64,
    /// Heat out, W.
    pub(crate) q_w: f64,
    /// Radiative part of `q_w`, W.
    pub(crate) q_rad_w: f64,
    /// ∂q/∂T_p, W/K.
    pub(crate) dq_dtp: f64,
    /// 1 + h_s·r (the series denominator, for the adjoint's partials).
    pub(crate) denom: f64,
    /// h_s = ∂q″/∂T_s, W/(m²·K).
    pub(crate) hs: f64,
}

impl RadLink {
    /// Solve the surface temperature for voxel temperature `tp_c` and
    /// return the flux and its derivative. q″(T_s) is monotone, and the
    /// root lies between T_∞ and T_p, so Newton safeguarded by bisection
    /// always converges.
    pub(crate) fn face(&self, tp_c: f64) -> RadFace {
        let ta_k = self.t_amb_c + KELVIN_OFFSET_C;
        let ta4 = ta_k.powi(4);
        let es = self.eps_eff * STEFAN_BOLTZMANN;
        let r = self.r_half;
        // Surface flux per area and its slope at T_s (°C); the kelvin
        // temperature is clamped at 0 so the loss stays monotone even if
        // an iterate dips below absolute zero.
        let flux = |ts_c: f64| {
            let tk = (ts_c + KELVIN_OFFSET_C).max(0.0);
            let rad = es * (tk.powi(4) - ta4);
            let f = rad + self.h * (ts_c - self.t_amb_c);
            let df = 4.0 * es * tk.powi(3) + self.h;
            (f, df, rad)
        };
        let (mut lo, mut hi) = if tp_c >= self.t_amb_c {
            (self.t_amb_c, tp_c)
        } else {
            (tp_c, self.t_amb_c)
        };
        // Start from the voxel temperature (the r → 0 answer).
        let mut ts = tp_c;
        for _ in 0..100 {
            let (f, df, _) = flux(ts);
            let g = f - (tp_c - ts) / r;
            if g > 0.0 {
                hi = ts;
            } else {
                lo = ts;
            }
            let dg = df + 1.0 / r;
            let mut next = ts - g / dg;
            if !(next > lo && next < hi) {
                next = 0.5 * (lo + hi);
            }
            let step = (next - ts).abs();
            ts = next;
            if step <= 1e-13 * (1.0 + ts.abs()) || hi - lo <= 1e-13 * (1.0 + ts.abs()) {
                break;
            }
        }
        let (f, df, rad) = flux(ts);
        let denom = 1.0 + df * r;
        RadFace {
            ts_c: ts,
            q_w: self.area_m2 * f,
            q_rad_w: self.area_m2 * rad,
            dq_dtp: self.area_m2 * df / denom,
            denom,
            hs: df,
        }
    }
}

/// Per-slot radiation bookkeeping fixed at build time.
#[derive(Clone, Copy)]
pub(crate) struct RadSlot {
    pub(crate) emissivity: f64,
    pub(crate) eps_eff: f64,
    pub(crate) h: f64,
    pub(crate) faces: usize,
    pub(crate) voxel_area_m2: f64,
    pub(crate) area_m2: f64,
    pub(crate) enclosure: Option<Enclosure>,
}

/// The assembled discrete system (crate-internal; the transient solver
/// reuses it with a time term added to the diagonal).
pub(crate) struct System {
    pub(crate) n: [usize; 3],
    pub(crate) solid: Vec<bool>,
    pub(crate) free: Vec<bool>,
    pub(crate) tfix: Vec<f64>,
    /// Pair conductance to the +axis neighbor, W/K (0 when absent).
    pub(crate) g_pos: [Vec<f64>; 3],
    pub(crate) diag: Vec<f64>,
    pub(crate) b: Vec<f64>,
    pub(crate) links: Vec<BcLink>,
    /// (name, total power, voxel ids) per source.
    pub(crate) sources: Vec<(String, f64, Vec<usize>)>,
    pub(crate) source_w_total: f64,
    /// Volumetric heat capacity per voxel, J/(m³·K); −1 marks a painted
    /// material that declared none (steady solves never read this).
    pub(crate) rc: Vec<f64>,
    /// Index into `ThermalModel::materials` that painted each voxel.
    pub(crate) mat_id: Vec<usize>,
    pub(crate) cell_volume_m3: f64,
    /// Per-axis conductivity per voxel (0 for void) — kept for the
    /// adjoint's conductance chain rules.
    pub(crate) kfield: [Vec<f64>; 3],
    /// Voxel spacing per axis, m.
    pub(crate) d_m: [f64; 3],
    /// Face area per axis, m².
    pub(crate) area: [f64; 3],
    /// Nonlinear radiation faces (empty for conduction-only models).
    pub(crate) rad_links: Vec<RadLink>,
    /// Radiation bookkeeping per BC slot (0..=5 domain faces, 6 exposed).
    pub(crate) rad_slots: [Option<RadSlot>; 7],
}

impl System {
    fn idx(&self, i: usize, j: usize, k: usize) -> usize {
        (k * self.n[1] + j) * self.n[0] + i
    }

    /// y = diag·x − Σ G·x_neighbor on the free subspace (x, y are
    /// full-grid vectors; non-free entries are ignored/zeroed). `diag`
    /// is passed in so the transient solver can add its C/Δt time term
    /// without rebuilding the links.
    pub(crate) fn apply(&self, diag: &[f64], x: &[f64], y: &mut [f64]) {
        let [nx, ny, nz] = self.n;
        for (p, yy) in y.iter_mut().enumerate() {
            *yy = if self.free[p] { diag[p] * x[p] } else { 0.0 };
        }
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let p = self.idx(i, j, k);
                    for (axis, stride) in [(0usize, 1usize), (1, nx), (2, nx * ny)] {
                        let last = match axis {
                            0 => i + 1 >= nx,
                            1 => j + 1 >= ny,
                            _ => k + 1 >= nz,
                        };
                        if last {
                            continue;
                        }
                        let g = self.g_pos[axis][p];
                        if g <= 0.0 {
                            continue;
                        }
                        let q = p + stride;
                        if self.free[p] && self.free[q] {
                            y[p] -= g * x[q];
                            y[q] -= g * x[p];
                        }
                    }
                }
            }
        }
    }

    /// Net heat leaving the free system through every boundary link
    /// (Dirichlet faces, convection faces, reservoir contacts) for the
    /// free-node temperature vector `t`, W.
    pub(crate) fn boundary_outflow_w(&self, t: &[f64]) -> f64 {
        let linear: f64 = self
            .links
            .iter()
            .map(|l| l.g * (t[l.voxel] - l.t_ref))
            .sum();
        let rad: f64 = self.rad_links.iter().map(|l| l.face(t[l.voxel]).q_w).sum();
        linear + rad
    }

    /// Is any face radiating (the nonlinear path)?
    pub(crate) fn radiates(&self) -> bool {
        !self.rad_links.is_empty()
    }
}

pub(crate) fn build(model: &ThermalModel) -> Result<System, SolveError> {
    let n = model.divisions;
    if n.contains(&0) || model.size_mm.iter().any(|&s| s <= 0.0) {
        return Err(ModelError::EmptyDomain.into());
    }
    for (index, m) in model.materials.iter().enumerate() {
        if m.k_w_mk.iter().any(|&k| k <= 0.0) {
            return Err(ModelError::NonPositiveConductivity { index }.into());
        }
    }
    for bc in model
        .domain_faces
        .iter()
        .chain(std::iter::once(&model.exposed))
    {
        match bc {
            Boundary::Convection { h_w_m2k, .. } if *h_w_m2k <= 0.0 => {
                return Err(ModelError::NonPositiveFilmCoefficient.into());
            }
            Boundary::Radiation {
                emissivity,
                ambient_c,
                h_w_m2k,
                area_scale,
                enclosure,
            } => validate_radiation(*emissivity, *ambient_c, *h_w_m2k, *area_scale, *enclosure)?,
            _ => {}
        }
    }

    let [nx, ny, nz] = n;
    let nvox = nx * ny * nz;
    let d_mm = model.voxel_mm();
    let d_m = [d_mm[0] * 1e-3, d_mm[1] * 1e-3, d_mm[2] * 1e-3];
    // Face areas per axis, m².
    let area = [d_m[1] * d_m[2], d_m[0] * d_m[2], d_m[0] * d_m[1]];
    let idx = |i: usize, j: usize, k: usize| (k * ny + j) * nx + i;

    // Paint materials. Conductivity is a per-axis diagonal tensor; heat
    // capacity rides along for the transient solver (-1.0 marks "painted
    // with no capacity"). Two producers: an external per-voxel index
    // (the tessellated-part seam — overrides shapes entirely), or region
    // shapes in painter's order (later regions win).
    let mut kfield = [
        vec![0.0_f64; nvox],
        vec![0.0_f64; nvox],
        vec![0.0_f64; nvox],
    ];
    let mut rc = vec![-1.0_f64; nvox];
    let mut mat_id = vec![usize::MAX; nvox];
    if let Some(vm) = &model.voxel_materials {
        if vm.material_index.len() != nvox {
            return Err(ModelError::VoxelFieldWrongLength {
                expected: nvox,
                got: vm.material_index.len(),
            }
            .into());
        }
        for (p, &raw) in vm.material_index.iter().enumerate() {
            if raw < 0 {
                continue;
            }
            let mi = raw as usize;
            let Some(m) = model.materials.get(mi) else {
                return Err(ModelError::VoxelFieldBadIndex {
                    index: raw,
                    materials: model.materials.len(),
                }
                .into());
            };
            for (axis, kf) in kfield.iter_mut().enumerate() {
                kf[p] = m.k_w_mk[axis];
            }
            rc[p] = m.heat_capacity_j_m3k.unwrap_or(-1.0);
            mat_id[p] = mi;
        }
    } else {
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let c = model.voxel_center_mm(i, j, k);
                    for (mi, m) in model.materials.iter().enumerate() {
                        if m.shape.contains(c) {
                            let p = idx(i, j, k);
                            for (axis, kf) in kfield.iter_mut().enumerate() {
                                kf[p] = m.k_w_mk[axis];
                            }
                            rc[p] = m.heat_capacity_j_m3k.unwrap_or(-1.0);
                            mat_id[p] = mi;
                        }
                    }
                }
            }
        }
    }
    let solid: Vec<bool> = kfield[0].iter().map(|&k| k > 0.0).collect();
    if !solid.iter().any(|&s| s) {
        return Err(ModelError::NoSolidVoxels.into());
    }

    // Pin reservoirs (later regions win); only solid voxels can be pinned.
    let mut fixed = vec![false; nvox];
    let mut tfix = vec![0.0_f64; nvox];
    let mut fixed_id = vec![usize::MAX; nvox];
    for (index, fx) in model.fixed.iter().enumerate() {
        let mut hits = 0usize;
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let p = idx(i, j, k);
                    if solid[p] && fx.shape.contains(model.voxel_center_mm(i, j, k)) {
                        fixed[p] = true;
                        tfix[p] = fx.temperature_c;
                        fixed_id[p] = index;
                        hits += 1;
                    }
                }
            }
        }
        if hits == 0 {
            return Err(ModelError::FixedCoversNoSolid { index }.into());
        }
    }
    let free: Vec<bool> = (0..nvox).map(|p| solid[p] && !fixed[p]).collect();

    // Sources: total watts split equally over covered free voxels.
    let mut b = vec![0.0_f64; nvox];
    let mut sources = Vec::with_capacity(model.sources.len());
    let mut source_w_total = 0.0;
    for s in &model.sources {
        let mut ids = Vec::new();
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let p = idx(i, j, k);
                    if free[p] && s.shape.contains(model.voxel_center_mm(i, j, k)) {
                        ids.push(p);
                    }
                }
            }
        }
        if ids.is_empty() {
            return Err(ModelError::SourceCoversNoFreeSolid {
                name: s.name.clone(),
            }
            .into());
        }
        let share = s.power_w / ids.len() as f64;
        for &p in &ids {
            b[p] += share;
        }
        source_w_total += s.power_w;
        sources.push((s.name.clone(), s.power_w, ids));
    }

    // Internal pair links: harmonic-mean face conductance between solid
    // voxels (the two half-cells in series — see module docs).
    let mut g_pos = [vec![0.0; nvox], vec![0.0; nvox], vec![0.0; nvox]];
    let mut diag = vec![0.0_f64; nvox];
    let mut links: Vec<BcLink> = Vec::new();
    let mut rad_links: Vec<RadLink> = Vec::new();
    let mut grounded = vec![false; nvox];
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let p = idx(i, j, k);
                if !solid[p] {
                    continue;
                }
                for axis in 0..3 {
                    let (ii, jj, kk) = match axis {
                        0 => (i + 1, j, k),
                        1 => (i, j + 1, k),
                        _ => (i, j, k + 1),
                    };
                    if ii >= nx || jj >= ny || kk >= nz {
                        continue;
                    }
                    let q = idx(ii, jj, kk);
                    if !solid[q] {
                        continue;
                    }
                    let g = area[axis]
                        / (0.5 * d_m[axis] / kfield[axis][p] + 0.5 * d_m[axis] / kfield[axis][q]);
                    g_pos[axis][p] = g;
                    match (free[p], free[q]) {
                        (true, true) => {
                            diag[p] += g;
                            diag[q] += g;
                        }
                        (true, false) => {
                            diag[p] += g;
                            b[p] += g * tfix[q];
                            grounded[p] = true;
                            links.push(BcLink {
                                voxel: p,
                                axis,
                                g,
                                t_ref: tfix[q],
                                kind: LinkKind::FixedRegion(fixed_id[q]),
                            });
                        }
                        (false, true) => {
                            diag[q] += g;
                            b[q] += g * tfix[p];
                            grounded[q] = true;
                            links.push(BcLink {
                                voxel: q,
                                axis,
                                g,
                                t_ref: tfix[p],
                                kind: LinkKind::FixedRegion(fixed_id[p]),
                            });
                        }
                        (false, false) => {}
                    }
                }
            }
        }
    }

    // Boundary faces: domain-box faces and exposed solid↔void faces.
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let p = idx(i, j, k);
                if !free[p] {
                    continue;
                }
                for axis in 0..3 {
                    for positive in [false, true] {
                        let coord = [i, j, k][axis];
                        let limit = n[axis];
                        let at_domain_edge = if positive {
                            coord + 1 >= limit
                        } else {
                            coord == 0
                        };
                        let (bc, slot) = if at_domain_edge {
                            let s = face_index(axis, positive);
                            (model.domain_faces[s], s)
                        } else {
                            let (ii, jj, kk) = match (axis, positive) {
                                (0, true) => (i + 1, j, k),
                                (0, false) => (i - 1, j, k),
                                (1, true) => (i, j + 1, k),
                                (1, false) => (i, j - 1, k),
                                (2, true) => (i, j, k + 1),
                                _ => (i, j, k - 1),
                            };
                            if solid[idx(ii, jj, kk)] {
                                continue; // internal face, handled above
                            }
                            (model.exposed, EXPOSED_SLOT)
                        };
                        let half = 0.5 * d_m[axis] / kfield[axis][p];
                        match bc {
                            Boundary::Adiabatic => {}
                            Boundary::FixedTemperature { temperature_c } => {
                                let g = area[axis] / half;
                                diag[p] += g;
                                b[p] += g * temperature_c;
                                grounded[p] = true;
                                links.push(BcLink {
                                    voxel: p,
                                    axis,
                                    g,
                                    t_ref: temperature_c,
                                    kind: LinkKind::FixedFace,
                                });
                            }
                            Boundary::Convection { h_w_m2k, ambient_c } => {
                                let g = area[axis] / (half + 1.0 / h_w_m2k);
                                diag[p] += g;
                                b[p] += g * ambient_c;
                                grounded[p] = true;
                                links.push(BcLink {
                                    voxel: p,
                                    axis,
                                    g,
                                    t_ref: ambient_c,
                                    kind: LinkKind::Convection(slot),
                                });
                            }
                            Boundary::Radiation {
                                emissivity,
                                ambient_c,
                                h_w_m2k,
                                area_scale,
                                ..
                            } => {
                                grounded[p] = true;
                                rad_links.push(RadLink {
                                    voxel: p,
                                    axis,
                                    area_m2: area[axis] * area_scale,
                                    r_half: half,
                                    // Enclosure exchange is applied below,
                                    // once the slot's total area is known.
                                    eps_eff: emissivity,
                                    h: h_w_m2k,
                                    t_amb_c: ambient_c,
                                    slot,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Radiation slots: total area, then the two-surface enclosure
    // exchange (needs the slot's whole radiating area, so it is applied
    // after every face is known).
    let mut rad_slots: [Option<RadSlot>; 7] = [None; 7];
    for l in &rad_links {
        let bc = if l.slot == EXPOSED_SLOT {
            model.exposed
        } else {
            model.domain_faces[l.slot]
        };
        let Boundary::Radiation {
            emissivity,
            h_w_m2k,
            area_scale,
            enclosure,
            ..
        } = bc
        else {
            unreachable!("radiation link from a non-radiation slot");
        };
        let slot = rad_slots[l.slot].get_or_insert(RadSlot {
            emissivity,
            eps_eff: emissivity,
            h: h_w_m2k,
            faces: 0,
            voxel_area_m2: 0.0,
            area_m2: 0.0,
            enclosure,
        });
        slot.faces += 1;
        slot.area_m2 += l.area_m2;
        slot.voxel_area_m2 += l.area_m2 / area_scale;
    }
    for (slot_index, rs) in rad_slots.iter_mut().enumerate() {
        let Some(rs) = rs else { continue };
        if let Some(enc) = rs.enclosure {
            if enc.area_m2 <= rs.area_m2 {
                return Err(ModelError::InvalidRadiation(format!(
                    "slot {}: enclosure area {:.4e} m2 must exceed the enclosed radiating area \
                     {:.4e} m2",
                    slot_label(slot_index),
                    enc.area_m2,
                    rs.area_m2
                ))
                .into());
            }
            rs.eps_eff = effective_emissivity(rs.emissivity, rs.area_m2, enc);
        }
    }
    for l in &mut rad_links {
        if let Some(rs) = rad_slots[l.slot] {
            l.eps_eff = rs.eps_eff;
        }
    }

    let sys = System {
        n,
        solid,
        free,
        tfix,
        g_pos,
        diag,
        b,
        links,
        sources,
        source_w_total,
        rc,
        mat_id,
        cell_volume_m3: d_m[0] * d_m[1] * d_m[2],
        kfield,
        d_m,
        area,
        rad_links,
        rad_slots,
    };

    // Fail closed on floating components: BFS over free voxels through
    // positive pair conductances; every component must touch a reference.
    check_grounding(&sys, &grounded, model)?;
    Ok(sys)
}

/// Label of a BC slot (`-x … +z`, `exposed`).
pub(crate) fn slot_label(slot: usize) -> &'static str {
    ["-x", "+x", "-y", "+y", "-z", "+z", "exposed"][slot]
}

/// Two-surface gray enclosure: ε_eff = 1/(1/ε + (A_s/A_w)(1/ε_w − 1)).
pub(crate) fn effective_emissivity(emissivity: f64, area_m2: f64, enc: Enclosure) -> f64 {
    1.0 / (1.0 / emissivity + area_m2 / enc.area_m2 * (1.0 / enc.emissivity - 1.0))
}

fn validate_radiation(
    emissivity: f64,
    ambient_c: f64,
    h_w_m2k: f64,
    area_scale: f64,
    enclosure: Option<Enclosure>,
) -> Result<(), ModelError> {
    let bad = |why: String| Err(ModelError::InvalidRadiation(why));
    if !(emissivity > 0.0 && emissivity <= 1.0) {
        return bad(format!("emissivity {emissivity} must be in (0, 1]"));
    }
    if !(ambient_c.is_finite() && ambient_c > -KELVIN_OFFSET_C) {
        return bad(format!(
            "ambient {ambient_c} C must be finite and above absolute zero"
        ));
    }
    if !(h_w_m2k.is_finite() && h_w_m2k >= 0.0) {
        return bad(format!("parallel film h = {h_w_m2k} must be >= 0"));
    }
    if !(area_scale.is_finite() && area_scale > 0.0) {
        return bad(format!("area_scale {area_scale} must be > 0"));
    }
    if let Some(enc) = enclosure {
        if !(enc.emissivity > 0.0 && enc.emissivity <= 1.0) {
            return bad(format!(
                "enclosure emissivity {} must be in (0, 1]",
                enc.emissivity
            ));
        }
        if !(enc.area_m2.is_finite() && enc.area_m2 > 0.0) {
            return bad(format!("enclosure area {} m2 must be > 0", enc.area_m2));
        }
    }
    Ok(())
}

fn check_grounding(
    sys: &System,
    grounded: &[bool],
    model: &ThermalModel,
) -> Result<(), SolveError> {
    let [nx, ny, nz] = sys.n;
    let nvox = nx * ny * nz;
    let mut visited = vec![false; nvox];
    let mut stack = Vec::new();
    for start in 0..nvox {
        if !sys.free[start] || visited[start] {
            continue;
        }
        let mut component = Vec::new();
        let mut ok = false;
        visited[start] = true;
        stack.push(start);
        while let Some(p) = stack.pop() {
            component.push(p);
            if grounded[p] {
                ok = true;
            }
            let k = p / (nx * ny);
            let j = (p / nx) % ny;
            let i = p % nx;
            for (axis, stride) in [(0usize, 1usize), (1, nx), (2, nx * ny)] {
                let coord = [i, j, k][axis];
                if coord + 1 < sys.n[axis] && sys.g_pos[axis][p] > 0.0 {
                    let q = p + stride;
                    if sys.free[q] && !visited[q] {
                        visited[q] = true;
                        stack.push(q);
                    }
                }
                if coord > 0 {
                    let q = p - stride;
                    if sys.g_pos[axis][q] > 0.0 && sys.free[q] && !visited[q] {
                        visited[q] = true;
                        stack.push(q);
                    }
                }
            }
        }
        if !ok {
            let p = component[0];
            let k = p / (nx * ny);
            let j = (p / nx) % ny;
            let i = p % nx;
            return Err(SolveError::FloatingRegion {
                voxels: component.len(),
                near_mm: model.voxel_center_mm(i, j, k),
            });
        }
    }
    Ok(())
}

/// Jacobi-preconditioned conjugate gradients, matrix-free, warm-startable
/// (`tang_sparse::pcg` over [`System::apply`]).
///
/// `diag` and `b` are passed explicitly so the transient solver can reuse
/// the assembled links with a C/Δt time term folded into the diagonal and
/// a per-step right-hand side; `x0` seeds the iteration (the previous time
/// step in transient runs — steady solves start from zero). Vectors are
/// full-grid; non-free entries are masked out of the operator and the
/// preconditioner, so they never enter the iteration.
pub(crate) fn pcg(
    sys: &System,
    diag: &[f64],
    b: &[f64],
    x0: &[f64],
    opts: &SolveOptions,
) -> Result<(Vec<f64>, usize, f64), SolveError> {
    let masked = |v: &[f64]| -> Vec<f64> {
        v.iter()
            .zip(&sys.free)
            .map(|(&x, &f)| if f { x } else { 0.0 })
            .collect()
    };
    let sol = tang_sparse::pcg(
        |x, y| sys.apply(diag, x, y),
        &tang_sparse::Jacobi::new(masked(diag)),
        &masked(b),
        x0,
        &tang_sparse::PcgOptions {
            tol: opts.tol,
            max_iters: opts.max_iters,
        },
    );
    match sol.status {
        tang_sparse::PcgStatus::Converged => Ok((sol.x, sol.iterations, sol.residual_rel)),
        // A broken-down direction (SPD violation) or the iteration cap.
        _ => Err(SolveError::NotConverged {
            residual_rel: sol.residual_rel,
            iterations: sol.iterations,
        }),
    }
}

/// A solved free-node field (linear or Newton).
pub(crate) struct FieldSolve {
    pub(crate) x: Vec<f64>,
    /// CG iterations (summed over Newton steps).
    pub(crate) cg_iterations: usize,
    /// Final relative residual (linear: CG; nonlinear: Newton).
    pub(crate) residual_rel: f64,
    pub(crate) newton_iterations: usize,
    /// ∂Q/∂T_p per voxel at the solution (zeros when linear) — the
    /// Jacobian's extra diagonal, reused by the adjoint.
    pub(crate) jac_rad: Vec<f64>,
}

/// Newton iteration cap for radiating solves.
const MAX_NEWTON: usize = 200;

/// Solve (diag − offdiag)·x − rhs + Q(x) = 0 on the free voxels. Without
/// radiation faces this is exactly one PCG solve. `trust_first` accepts
/// the first Newton step without a line search (the steady solve's
/// linearized-at-ambient supersolution; see the module docs).
pub(crate) fn solve_field(
    sys: &System,
    diag: &[f64],
    rhs: &[f64],
    x0: &[f64],
    opts: &SolveOptions,
    trust_first: bool,
) -> Result<FieldSolve, SolveError> {
    let n = rhs.len();
    if !sys.radiates() {
        let (x, cg_iterations, residual_rel) = pcg(sys, diag, rhs, x0, opts)?;
        return Ok(FieldSolve {
            x,
            cg_iterations,
            residual_rel,
            newton_iterations: 0,
            jac_rad: vec![0.0; n],
        });
    }
    let rhs_norm = rhs
        .iter()
        .zip(&sys.free)
        .filter(|(_, &f)| f)
        .map(|(v, _)| v * v)
        .sum::<f64>()
        .sqrt();
    // Residual R(x) and the radiation Jacobian diagonal. Returns
    // (‖R‖, relative residual ‖R‖/max(‖b‖, ‖Q‖), roundoff floor). The
    // line search compares the *absolute* norm (the relative one is not
    // monotone along a Newton path: far above the root ‖Q‖ dominates
    // both numerator and denominator). The floor is the residual the
    // arithmetic can resolve — R cancels terms of size ~diag·|x| — so a
    // tolerance below it is clamped to it rather than chased forever.
    let eval = |x: &[f64], r: &mut [f64], jac: &mut [f64]| -> (f64, f64, f64) {
        sys.apply(diag, x, r);
        for p in 0..n {
            if sys.free[p] {
                r[p] -= rhs[p];
            } else {
                r[p] = 0.0;
            }
        }
        jac.iter_mut().for_each(|j| *j = 0.0);
        let mut qv = vec![0.0_f64; n];
        for l in &sys.rad_links {
            let f = l.face(x[l.voxel]);
            r[l.voxel] += f.q_w;
            qv[l.voxel] += f.q_w;
            jac[l.voxel] += f.dq_dtp;
        }
        let q_norm = qv.iter().map(|v| v * v).sum::<f64>().sqrt();
        let r_norm = r.iter().map(|v| v * v).sum::<f64>().sqrt();
        let ax_norm = (0..n)
            .filter(|&p| sys.free[p])
            .map(|p| (2.0 * (diag[p] + jac[p]) * x[p].abs()).powi(2))
            .sum::<f64>()
            .sqrt();
        let scale = rhs_norm.max(q_norm).max(1e-300);
        (
            r_norm,
            r_norm / scale,
            64.0 * f64::EPSILON * ax_norm / scale,
        )
    };

    let mut x = x0.to_vec();
    let mut r = vec![0.0_f64; n];
    let mut jac = vec![0.0_f64; n];
    let (mut r_norm, mut rel, mut floor) = eval(&x, &mut r, &mut jac);
    let mut cg_total = 0usize;
    let zeros = vec![0.0_f64; n];
    let mut x_try = vec![0.0_f64; n];
    let mut r_try = vec![0.0_f64; n];
    let mut jac_try = vec![0.0_f64; n];
    for it in 0..MAX_NEWTON {
        if rel <= opts.tol.max(floor) {
            return Ok(FieldSolve {
                x,
                cg_iterations: cg_total,
                residual_rel: rel,
                newton_iterations: it,
                jac_rad: jac,
            });
        }
        let diag_j: Vec<f64> = diag.iter().zip(&jac).map(|(d, j)| d + j).collect();
        let neg_r: Vec<f64> = r.iter().map(|v| -v).collect();
        // Eisenstat–Walker forcing: loose while far, never tighter than
        // the outer tolerance needs.
        let eta = rel.min(0.1).max(0.5 * opts.tol / rel).clamp(1e-14, 0.1);
        let (delta, cg_it, _) = pcg(
            sys,
            &diag_j,
            &neg_r,
            &zeros,
            &SolveOptions {
                tol: eta,
                max_iters: opts.max_iters,
            },
        )?;
        cg_total += cg_it;
        let mut lambda = 1.0_f64;
        let mut accepted = false;
        for _ in 0..40 {
            for p in 0..n {
                x_try[p] = if sys.free[p] {
                    x[p] + lambda * delta[p]
                } else {
                    x[p]
                };
            }
            let (r_norm_try, rel_try, floor_try) = eval(&x_try, &mut r_try, &mut jac_try);
            if (it == 0 && trust_first) || r_norm_try < r_norm * (1.0 - 1e-4 * lambda) {
                std::mem::swap(&mut x, &mut x_try);
                std::mem::swap(&mut r, &mut r_try);
                std::mem::swap(&mut jac, &mut jac_try);
                (r_norm, rel, floor) = (r_norm_try, rel_try, floor_try);
                accepted = true;
                break;
            }
            lambda *= 0.5;
        }
        if !accepted {
            // Stagnation within a few ulps of the floor is convergence
            // to the precision the arithmetic allows.
            if rel <= opts.tol.max(16.0 * floor) {
                return Ok(FieldSolve {
                    x,
                    cg_iterations: cg_total,
                    residual_rel: rel,
                    newton_iterations: it,
                    jac_rad: jac,
                });
            }
            return Err(SolveError::NonlinearNotConverged {
                residual_rel: rel,
                iterations: it + 1,
            });
        }
    }
    if rel <= opts.tol.max(floor) {
        return Ok(FieldSolve {
            x,
            cg_iterations: cg_total,
            residual_rel: rel,
            newton_iterations: MAX_NEWTON,
            jac_rad: jac,
        });
    }
    Err(SolveError::NonlinearNotConverged {
        residual_rel: rel,
        iterations: MAX_NEWTON,
    })
}

/// The steady solve's Newton starting field: uniform at the mean
/// radiation ambient (its first Newton step is the problem linearized at
/// ambient).
pub(crate) fn radiation_start(sys: &System) -> Vec<f64> {
    let n = sys.b.len();
    let mean =
        sys.rad_links.iter().map(|l| l.t_amb_c).sum::<f64>() / sys.rad_links.len().max(1) as f64;
    (0..n)
        .map(|p| if sys.free[p] { mean } else { 0.0 })
        .collect()
}

/// Resolve the θ reference: an explicit `reference_c` wins; otherwise the
/// convection ambients must be unique. With sources present and no
/// resolvable reference, this is an error (fail-closed).
pub(crate) fn resolve_reference(
    sys: &System,
    model: &ThermalModel,
) -> Result<Option<f64>, SolveError> {
    if let Some(r) = model.reference_c {
        return Ok(Some(r));
    }
    let mut ambient: Option<f64> = None;
    let mut unique = true;
    let ambients = sys
        .links
        .iter()
        .filter(|l| matches!(l.kind, LinkKind::Convection(_)))
        .map(|l| l.t_ref)
        .chain(sys.rad_links.iter().map(|l| l.t_amb_c));
    for t_ref in ambients {
        match ambient {
            None => ambient = Some(t_ref),
            Some(a) if (a - t_ref).abs() > 1e-9 => unique = false,
            _ => {}
        }
    }
    match (ambient, unique) {
        (Some(a), true) => Ok(Some(a)),
        _ if sys.sources.is_empty() => Ok(None),
        _ => Err(SolveError::AmbiguousReference),
    }
}

/// Voxel-staircase radiating area per radiation slot, `(label, m²)` —
/// geometry only (the system is built and validated, nothing is solved).
/// The area is *before* `area_scale`, so `area_scale = true_area / this`
/// is the staircase correction for a part whose true surface area is
/// known analytically or from the BRep.
pub fn radiation_areas(model: &ThermalModel) -> Result<Vec<(String, f64)>, SolveError> {
    let sys = build(model)?;
    Ok(sys
        .rad_slots
        .iter()
        .enumerate()
        .filter_map(|(slot, rs)| rs.map(|rs| (slot_label(slot).to_string(), rs.voxel_area_m2)))
        .collect())
}

/// Solve the steady conduction problem.
pub fn solve_steady(model: &ThermalModel, opts: &SolveOptions) -> Result<Solution, SolveError> {
    let sys = build(model)?;
    let reference_c = resolve_reference(&sys, model)?;
    if sys.radiates() {
        let x0 = radiation_start(&sys);
        let fs = solve_field(&sys, &sys.diag, &sys.b, &x0, opts, true)?;
        let mut sol = assemble_solution(
            &sys,
            model,
            &fs.x,
            fs.cg_iterations,
            fs.residual_rel,
            reference_c,
        );
        sol.newton_iterations = fs.newton_iterations;
        return Ok(sol);
    }
    let (x, iterations, residual_rel) = steady_field(model, &sys, opts)?;
    Ok(assemble_solution(
        &sys,
        model,
        &x,
        iterations,
        residual_rel,
        reference_c,
    ))
}

/// Solve the free-node field. Native targets run Assemble → LinearSolve as
/// a tang tensor graph (see `crate::graph`); wasm32 has no clock for the
/// graph's solve timing (`Instant::now` panics there), so it runs the same
/// PCG directly.
#[cfg(not(target_arch = "wasm32"))]
fn steady_field(
    model: &ThermalModel,
    sys: &System,
    opts: &SolveOptions,
) -> Result<(Vec<f64>, usize, f64), SolveError> {
    let g = crate::graph::build_graph(model, sys, opts, None);
    let t_free = g.eval()?.t_free;
    let (iterations, residual_rel) = g.last_forward();
    let mut x = vec![0.0_f64; sys.b.len()];
    for (&p, &t) in g.free_voxels.iter().zip(&t_free) {
        x[p] = t;
    }
    Ok((x, iterations, residual_rel))
}

#[cfg(target_arch = "wasm32")]
fn steady_field(
    _model: &ThermalModel,
    sys: &System,
    opts: &SolveOptions,
) -> Result<(Vec<f64>, usize, f64), SolveError> {
    let x0 = vec![0.0_f64; sys.b.len()];
    pcg(sys, &sys.diag, &sys.b, &x0, opts)
}

/// Assemble the public [`Solution`] (field, T_max, per-source θ,
/// reservoirs, energy balance) from a solved free-node vector. Shared
/// between the steady solver and the transient solver's snapshots.
pub(crate) fn assemble_solution(
    sys: &System,
    model: &ThermalModel,
    x: &[f64],
    iterations: usize,
    residual_rel: f64,
    reference_c: Option<f64>,
) -> Solution {
    let [nx, ny, nz] = sys.n;
    let nvox = nx * ny * nz;
    let mut t_c = vec![f64::NAN; nvox];
    for (p, t) in t_c.iter_mut().enumerate() {
        if sys.free[p] {
            *t = x[p];
        } else if sys.solid[p] {
            *t = sys.tfix[p];
        }
    }

    // Hottest solid voxel.
    let mut t_max_c = f64::NEG_INFINITY;
    let mut t_max_p = 0usize;
    for (p, &t) in t_c.iter().enumerate() {
        if sys.solid[p] && t > t_max_c {
            t_max_c = t;
            t_max_p = p;
        }
    }
    let coords = |p: usize| {
        let k = p / (nx * ny);
        let j = (p / nx) % ny;
        let i = p % nx;
        (i, j, k)
    };
    let center = |p: usize| {
        let (i, j, k) = coords(p);
        model.voxel_center_mm(i, j, k)
    };
    let t_max_at_mm = center(t_max_p);

    // Per-source reports.
    let sources = sys
        .sources
        .iter()
        .map(|(name, power_w, ids)| {
            let mut hot = ids[0];
            for &p in ids {
                if t_c[p] > t_c[hot] {
                    hot = p;
                }
            }
            let t_src = t_c[hot];
            let theta = reference_c.and_then(|r| {
                if *power_w != 0.0 {
                    Some((t_src - r) / power_w)
                } else {
                    None
                }
            });
            SourceReport {
                name: name.clone(),
                power_w: *power_w,
                t_max_c: t_src,
                t_max_at_mm: center(hot),
                theta_c_per_w: theta,
            }
        })
        .collect::<Vec<_>>();

    // Energy balance from the solved field: flows positive out of the
    // conducting system.
    let mut fixed_face_out_w = 0.0;
    let mut convection_out_w = 0.0;
    let mut fixed_region_out_w = 0.0;
    let mut gross = 0.0;
    let mut reservoirs: Vec<ReservoirReport> = model
        .fixed
        .iter()
        .map(|f| ReservoirReport {
            temperature_c: f.temperature_c,
            heat_absorbed_w: 0.0,
        })
        .collect();
    for l in &sys.links {
        let flow = l.g * (t_c[l.voxel] - l.t_ref);
        gross += flow.abs();
        match l.kind {
            LinkKind::FixedFace => fixed_face_out_w += flow,
            LinkKind::Convection(_) => convection_out_w += flow,
            LinkKind::FixedRegion(region) => {
                fixed_region_out_w += flow;
                reservoirs[region].heat_absorbed_w += flow;
            }
        }
    }
    // Radiation faces: per-slot reports and the fourth outflow.
    let mut radiation_out_w = 0.0;
    let mut radiation = Vec::new();
    for (slot, rs) in sys.rad_slots.iter().enumerate() {
        let Some(rs) = rs else { continue };
        let mut heat = 0.0;
        let mut radiated = 0.0;
        let mut ts_area = 0.0;
        let mut ts_max = f64::NEG_INFINITY;
        for l in sys.rad_links.iter().filter(|l| l.slot == slot) {
            let f = l.face(t_c[l.voxel]);
            heat += f.q_w;
            radiated += f.q_rad_w;
            ts_area += f.ts_c * l.area_m2;
            ts_max = ts_max.max(f.ts_c);
            gross += f.q_w.abs();
        }
        radiation_out_w += heat;
        radiation.push(RadiationReport {
            label: slot_label(slot).to_string(),
            emissivity: rs.emissivity,
            effective_emissivity: rs.eps_eff,
            h_w_m2k: rs.h,
            faces: rs.faces,
            voxel_area_m2: rs.voxel_area_m2,
            area_m2: rs.area_m2,
            heat_out_w: heat,
            radiated_w: radiated,
            mean_surface_c: ts_area / rs.area_m2,
            max_surface_c: ts_max,
        });
    }
    let net_out_w = fixed_face_out_w + convection_out_w + fixed_region_out_w + radiation_out_w;
    let scale = sys.source_w_total.abs().max(gross).max(1e-30);
    let energy = EnergyBalance {
        source_w: sys.source_w_total,
        fixed_face_out_w,
        convection_out_w,
        fixed_region_out_w,
        radiation_out_w,
        net_out_w,
        residual_rel: (sys.source_w_total - net_out_w).abs() / scale,
    };

    Solution {
        divisions: sys.n,
        origin_mm: model.origin_mm,
        voxel_mm: model.voxel_mm(),
        t_c,
        solid: sys.solid.clone(),
        iterations,
        residual_rel,
        t_max_c,
        t_max_at_mm,
        reference_c,
        sources,
        reservoirs,
        energy,
        newton_iterations: 0,
        radiation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Boundary, MaterialRegion, PowerSource, Shape, ThermalModel};

    fn slab_model(nx: usize) -> ThermalModel {
        let mut m = ThermalModel::new([0.0, 0.0, 0.0], [20.0, 5.0, 5.0], [nx, 1, 1]);
        m.materials.push(MaterialRegion::isotropic(
            Shape::Box {
                min_mm: [0.0, 0.0, 0.0],
                size_mm: [20.0, 5.0, 5.0],
            },
            50.0,
        ));
        m
    }

    #[test]
    fn dirichlet_slab_reproduces_the_linear_profile_exactly() {
        // 1D slab, T = 100 at x = 0, T = 0 at x = L: the finite-volume
        // solution is exact at voxel centers for a linear profile (second
        // differences of a linear function vanish; the half-cell boundary
        // conductance closes the end equations exactly).
        let mut m = slab_model(8);
        m.domain_faces[0] = Boundary::FixedTemperature {
            temperature_c: 100.0,
        };
        m.domain_faces[1] = Boundary::FixedTemperature { temperature_c: 0.0 };
        let sol = solve_steady(&m, &SolveOptions::default()).unwrap();
        let dx = 20.0 / 8.0;
        for i in 0..8 {
            let x = (i as f64 + 0.5) * dx;
            let exact = 100.0 * (1.0 - x / 20.0);
            let got = sol.temperature_c(i, 0, 0);
            assert!(
                (got - exact).abs() < 1e-9,
                "voxel {i}: computed {got}, exact {exact}"
            );
        }
        // No sources: net Dirichlet flow must vanish (heat in one face,
        // out the other) — the conservation statement itself.
        assert!(
            sol.energy.fixed_face_out_w.abs() < 1e-9,
            "net Dirichlet outflow must vanish: {}",
            sol.energy.fixed_face_out_w
        );
        assert!(sol.energy.residual_rel < 1e-9);
    }

    #[test]
    fn convection_face_is_the_exact_series_circuit() {
        // Dirichlet 100 °C at x = 0, convection (h, T∞ = 0) at x = L: the
        // exact 1D solution is a series circuit with flux per area
        // q = ΔT / (L/k + 1/h), and T(x) = q·(1/h + (L − x)/k). The
        // finite-volume solution matches it at voxel centers to machine
        // precision — the film resistance rides in series with the
        // half-cell, so h → ∞ recovers the Dirichlet limit smoothly.
        for h in [50.0, 5_000.0, 1e9] {
            let mut m = slab_model(8);
            m.domain_faces[0] = Boundary::FixedTemperature {
                temperature_c: 100.0,
            };
            m.domain_faces[1] = Boundary::Convection {
                h_w_m2k: h,
                ambient_c: 0.0,
            };
            let sol = solve_steady(&m, &SolveOptions::default()).unwrap();
            let (l, k) = (0.020, 50.0);
            let q = 100.0 / (l / k + 1.0 / h);
            let dx = l / 8.0;
            for i in 0..8 {
                let x = (i as f64 + 0.5) * dx;
                let exact = q * (1.0 / h + (l - x) / k);
                let got = sol.temperature_c(i, 0, 0);
                assert!(
                    (got - exact).abs() < 1e-8,
                    "h={h}, voxel {i}: computed {got:.9}, exact {exact:.9}"
                );
            }
        }
    }

    #[test]
    fn floating_region_fails_closed() {
        // Two solid islands; only one is pinned. The other must be
        // reported as floating, not silently solved.
        let mut m = ThermalModel::new([0.0, 0.0, 0.0], [30.0, 5.0, 5.0], [6, 1, 1]);
        for min_x in [0.0, 20.0] {
            m.materials.push(MaterialRegion::isotropic(
                Shape::Box {
                    min_mm: [min_x, 0.0, 0.0],
                    size_mm: [10.0, 5.0, 5.0],
                },
                10.0,
            ));
        }
        m.domain_faces[0] = Boundary::FixedTemperature {
            temperature_c: 50.0,
        };
        let err = solve_steady(&m, &SolveOptions::default()).unwrap_err();
        match err {
            SolveError::FloatingRegion { voxels, near_mm } => {
                assert_eq!(voxels, 2);
                assert!(near_mm[0] > 20.0, "floating island is the right one");
            }
            other => panic!("expected FloatingRegion, got {other:?}"),
        }
    }

    #[test]
    fn source_on_no_solid_fails_closed() {
        let mut m = slab_model(4);
        m.domain_faces[0] = Boundary::FixedTemperature { temperature_c: 0.0 };
        m.sources.push(PowerSource {
            name: "ghost".into(),
            shape: Shape::Box {
                min_mm: [100.0, 0.0, 0.0],
                size_mm: [1.0, 1.0, 1.0],
            },
            power_w: 1.0,
        });
        let err = solve_steady(&m, &SolveOptions::default()).unwrap_err();
        assert!(matches!(
            err,
            SolveError::Model(ModelError::SourceCoversNoFreeSolid { .. })
        ));
    }

    #[test]
    fn iteration_cap_reports_not_converged() {
        let mut m = slab_model(16);
        m.domain_faces[0] = Boundary::FixedTemperature {
            temperature_c: 100.0,
        };
        m.domain_faces[1] = Boundary::FixedTemperature { temperature_c: 0.0 };
        let err = solve_steady(
            &m,
            &SolveOptions {
                tol: 1e-12,
                max_iters: 1,
            },
        )
        .unwrap_err();
        match err {
            SolveError::NotConverged {
                residual_rel,
                iterations,
            } => {
                assert_eq!(iterations, 1);
                assert!(residual_rel > 0.0);
            }
            other => panic!("expected NotConverged, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_reference_fails_closed_only_when_needed() {
        // Two different convection ambients + a source: ambiguous.
        let mut m = slab_model(8);
        m.domain_faces[0] = Boundary::Convection {
            h_w_m2k: 10.0,
            ambient_c: 25.0,
        };
        m.domain_faces[1] = Boundary::Convection {
            h_w_m2k: 10.0,
            ambient_c: 60.0,
        };
        m.sources.push(PowerSource {
            name: "die".into(),
            shape: Shape::Box {
                min_mm: [0.0, 0.0, 0.0],
                size_mm: [20.0, 5.0, 5.0],
            },
            power_w: 1.0,
        });
        assert!(matches!(
            solve_steady(&m, &SolveOptions::default()),
            Err(SolveError::AmbiguousReference)
        ));
        // An explicit reference resolves it.
        m.reference_c = Some(25.0);
        let sol = solve_steady(&m, &SolveOptions::default()).unwrap();
        assert_eq!(sol.reference_c, Some(25.0));
        assert!(sol.sources[0].theta_c_per_w.is_some());
        // Without sources the ambiguity is irrelevant — no error.
        let mut m2 = slab_model(8);
        m2.domain_faces[0] = Boundary::Convection {
            h_w_m2k: 10.0,
            ambient_c: 25.0,
        };
        m2.domain_faces[1] = Boundary::Convection {
            h_w_m2k: 10.0,
            ambient_c: 60.0,
        };
        let sol2 = solve_steady(&m2, &SolveOptions::default()).unwrap();
        assert_eq!(sol2.reference_c, None);
    }
}
