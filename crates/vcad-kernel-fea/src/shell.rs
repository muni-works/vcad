//! Closed-form external-pressure buckling screen for thin shells:
//! cylinders, spheres and dished heads — the governing failure mode of a
//! vacuum chamber, a viewport cover, or a custom welded fusor shell.
//!
//! A vessel under vacuum carries one atmosphere *inward*, and a thin wall
//! that is nowhere near yield will still snap through. Linear static FEA
//! ([`crate::solve`]) cannot see this at all; the eigenvalue route
//! ([`crate::buckling`]) can, but a bifurcation load is **unconservative**
//! for shells — real ones collapse well below it because of geometric
//! imperfections. This module is the screening oracle that states that
//! knockdown explicitly and cites where every number came from.
//!
//! **Cylinders** (simply supported ends held circular, i.e. flanges or
//! stiffening rings at spacing `L`): NASA/SP-8007-2020/REV 2, *Buckling of
//! Thin-Walled Circular Cylinders*, §"External Pressure":
//!
//! * Eq. 19–22 (Batdorf's shallow-shell form): `N_y = p·r = k·π²D/L²`,
//!   `k_y = [(1+β²)² + 12γ²Z²/(π⁴(1+β²)²)]/β²` for lateral pressure, the
//!   leading `β²` replaced by `β² + ½` for hydrostatic pressure (pressure on
//!   the ends too — what a capped vacuum vessel sees), with
//!   `Z = L²√(1−ν²)/(r·t)`, `β = nL/(πr)`, `D = Et³/(12(1−ν²))`. We minimize
//!   over integer `n ≥ 2` circumferential waves rather than a continuous β.
//! * Eq. 27 for long cylinders that ovalize (`n = 2`, Eq. 26 regime
//!   `γZ > 4000`): `p = γE/(4(1−ν²))·(t/r)³` (Bresse/Bryan).
//! * Knockdowns: `√γ = 0.75` (Eq. 28) with the Batdorf form, `γ = 0.90`
//!   (Eq. 29) for the ovalizing long cylinder.
//!
//! Shallow-shell (Donnell/Batdorf) theory drops the `−1` of the ring term
//! (`n²` where Flügge/von Mises has `n² − 1`), so at `n = 2…3` it reads
//! high by `n²/(n²−1)` — 33 % at `n = 2`. When the minimizing `n` is that
//! low we scale by `(n²−1)/n²` and say so in a caution; the eigenvalue FE
//! ([`crate::buckling`]) is the independent check of that correction.
//!
//! **Spheres and heads:** classical (Zoelly 1915) bifurcation
//! `p_cl = 2E/√(3(1−ν²))·(t/R)²`. Measured collapse of real spheres sits at
//! a small fraction of that — the most imperfection-sensitive shell there
//! is. The screen uses ASME BPVC VIII-1 UG-28(d) in its elastic branch:
//! `A = 0.125/(R_o/t)`, `B = A·E/2`, `P_a = B/(R_o/t)`, i.e.
//! `P_a = 0.0625·E/(R_o/t)²`. Unfolded, that is a collapse estimate of
//! `0.25·E·(t/R_o)²` (≈ 0.21 × classical at ν = 0.3 — the knockdown) with
//! a design factor of 4. Heads follow UG-33(d)/(e): a 2:1 ellipsoidal head
//! is checked as a sphere of outside radius `K₁·D_o`, `K₁ = 0.9`
//! (Table UG-33.1); a torispherical head as a sphere of its crown radius.
//!
//! **What this is not:** a code stamp. UG-28's cylinder rules read factor
//! `A` off a geometry chart (Fig. G) and `B` off a material chart; we use
//! SP-8007's analytic curves with its knockdowns instead, and a design
//! factor (default 3, the margin UG-28 builds into its cylinder curves).
//! Elastic only: when the wall would yield before (or near) the elastic
//! collapse pressure, the governing number is capped at membrane yield and
//! a caution says the true elastic-plastic collapse is lower — UG-28's
//! material charts cover that region; this screen does not.
//!
//! Units: the crate-wide mm-N-MPa system. `1 atm = 0.101325 MPa`.

use serde::{Deserialize, Serialize};

use crate::section::BeamVerdict;
use crate::spec::SpecError;

/// One standard atmosphere, MPa.
pub const ATM_MPA: f64 = 0.101_325;

fn default_pressure() -> f64 {
    ATM_MPA
}

fn default_design_factor() -> f64 {
    3.0
}

/// How the external pressure reaches a cylinder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CylinderLoading {
    /// Pressure on the curved wall *and* the ends (a capped vessel under
    /// vacuum). SP-8007 Eq. 21–22.
    #[default]
    Hydrostatic,
    /// Pressure on the curved wall only (ends reacted elsewhere).
    /// SP-8007 Eq. 19–20.
    Lateral,
}

/// A dished head under pressure on its convex side.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HeadKind {
    /// Hemispherical: a sphere of the head's outside radius.
    Hemispherical,
    /// 2:1 semi-ellipsoidal: equivalent sphere `R_o = 0.9·D_o` (UG-33.1).
    Ellipsoidal2To1,
    /// Torispherical (flanged and dished): sphere of the crown radius.
    Torispherical {
        /// Outside crown radius, mm.
        crown_radius_mm: f64,
    },
}

/// Shell geometry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellGeometry {
    /// Circular cylinder between supports that hold it round (flanges,
    /// stiffening rings) a distance `length_mm` apart.
    Cylinder {
        /// Outside diameter, mm.
        outer_diameter_mm: f64,
        /// Wall thickness, mm.
        wall_mm: f64,
        /// Unsupported length between rings/flanges, mm.
        length_mm: f64,
        /// Lateral or hydrostatic pressure.
        #[serde(default)]
        loading: CylinderLoading,
    },
    /// Complete sphere (or a hemisphere far from its junction).
    Sphere {
        /// Outside diameter, mm.
        outer_diameter_mm: f64,
        /// Wall thickness, mm.
        wall_mm: f64,
    },
    /// Dished head, pressure on the convex side.
    Head {
        /// Head type.
        head: HeadKind,
        /// Outside diameter of the head skirt, mm.
        outer_diameter_mm: f64,
        /// Minimum wall thickness after forming, mm.
        wall_mm: f64,
    },
}

/// A shell buckling case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellCase {
    /// Geometry.
    pub geometry: ShellGeometry,
    /// Young's modulus, MPa.
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio.
    pub poisson: f64,
    /// Yield strength, MPa (0.2 % offset). Optional; without it the yield
    /// cap is skipped and a caution says so.
    #[serde(default)]
    pub yield_strength_mpa: Option<f64>,
    /// Applied external pressure (net, outside minus inside), MPa.
    /// Default one atmosphere (a vacuum chamber).
    #[serde(default = "default_pressure")]
    pub external_pressure_mpa: f64,
    /// Design factor on the knocked-down cylinder collapse pressure
    /// (spheres/heads use UG-28(d)'s own factor of 4). Default 3.
    #[serde(default = "default_design_factor")]
    pub design_factor: f64,
}

impl ShellCase {
    /// Geometry-independent validation.
    pub fn validate(&self) -> Result<(), SpecError> {
        let bad = |m: &str| Err(SpecError::Invalid(m.into()));
        if !(self.youngs_modulus_mpa > 0.0 && self.youngs_modulus_mpa.is_finite()) {
            return bad("youngs_modulus_mpa must be positive");
        }
        if !(self.poisson > -1.0 && self.poisson < 0.5) {
            return bad("poisson must be in (-1, 0.5)");
        }
        if !(self.external_pressure_mpa > 0.0 && self.external_pressure_mpa.is_finite()) {
            return bad("external_pressure_mpa must be positive");
        }
        if !(self.design_factor >= 1.0 && self.design_factor.is_finite()) {
            return bad("design_factor must be >= 1");
        }
        if let Some(y) = self.yield_strength_mpa {
            if !(y > 0.0 && y.is_finite()) {
                return bad("yield_strength_mpa must be positive");
            }
        }
        let (d, t) = match self.geometry {
            ShellGeometry::Cylinder {
                outer_diameter_mm,
                wall_mm,
                length_mm,
                ..
            } => {
                if !(length_mm > 0.0 && length_mm.is_finite()) {
                    return bad("length_mm must be positive");
                }
                (outer_diameter_mm, wall_mm)
            }
            ShellGeometry::Sphere {
                outer_diameter_mm,
                wall_mm,
            } => (outer_diameter_mm, wall_mm),
            ShellGeometry::Head {
                head,
                outer_diameter_mm,
                wall_mm,
            } => {
                if let HeadKind::Torispherical { crown_radius_mm } = head {
                    if !(crown_radius_mm > 0.0 && crown_radius_mm.is_finite()) {
                        return bad("crown_radius_mm must be positive");
                    }
                }
                (outer_diameter_mm, wall_mm)
            }
        };
        if !(d > 0.0 && t > 0.0 && d.is_finite() && t.is_finite() && 2.0 * t < d) {
            return bad("need 0 < 2·wall < outer diameter");
        }
        Ok(())
    }
}

/// Which closed form produced the elastic number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellRegime {
    /// Finite cylinder, Batdorf form (SP-8007 Eq. 19–22, knockdown Eq. 28).
    CylinderBatdorf,
    /// Long ovalizing cylinder (SP-8007 Eq. 26–27, knockdown Eq. 29).
    CylinderLong,
    /// Sphere or head as an equivalent sphere (Zoelly; UG-28(d)/UG-33).
    Sphere,
}

/// The screen's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCheck {
    /// Which closed form governed.
    pub regime: ShellRegime,
    /// Radius used by the formulas, mm (mean radius for cylinders; outside
    /// radius of the equivalent sphere for spheres/heads, as UG-28(d)).
    pub radius_mm: f64,
    /// Radius-to-thickness ratio at that radius.
    pub radius_over_thickness: f64,
    /// Batdorf curvature parameter `Z` (cylinders only).
    pub batdorf_z: Option<f64>,
    /// Circumferential waves of the governing mode (cylinders only).
    pub circumferential_waves: Option<u32>,
    /// Classical (perfect-shell, linear bifurcation) pressure, MPa. This is
    /// what an eigenvalue analysis converges to — and it is UNCONSERVATIVE.
    pub classical_pressure_mpa: f64,
    /// Knockdown actually applied, as `knocked_down / classical`.
    pub knockdown: f64,
    /// The cited knockdown rule, human-readable.
    pub knockdown_basis: String,
    /// Imperfection-knocked-down elastic collapse estimate, MPa.
    pub knocked_down_pressure_mpa: f64,
    /// Membrane yield pressure, MPa (`σ_y t/r` cylinder, `2σ_y t/r` sphere).
    pub yield_pressure_mpa: Option<f64>,
    /// `min(knocked-down elastic, yield)`, MPa.
    pub collapse_pressure_mpa: f64,
    /// Design factor applied to `collapse_pressure_mpa`.
    pub design_factor: f64,
    /// Allowable external pressure, MPa.
    pub allowable_pressure_mpa: f64,
    /// `allowable / applied` — ≥ 1 passes the screen.
    pub margin: f64,
    /// `knocked-down collapse / applied` — the margin with no design factor.
    pub collapse_margin: f64,
    /// Applied external pressure, MPa.
    pub applied_pressure_mpa: f64,
    /// Applicability verdict (shared with the beam route).
    pub verdict: BeamVerdict,
    /// Non-blocking cautions that travel into the claims.
    pub cautions: Vec<String>,
}

impl ShellCheck {
    /// True when every closed-form assumption held.
    pub fn applicable(&self) -> bool {
        self.verdict == BeamVerdict::Applicable
    }

    /// True when applicable and the allowable covers the applied pressure.
    pub fn passes(&self) -> bool {
        self.applicable() && self.margin >= 1.0
    }
}

/// SP-8007 Eq. 20/22 buckling coefficient for `n` waves.
fn batdorf_k(n: f64, l: f64, r: f64, z: f64, gamma: f64, loading: CylinderLoading) -> f64 {
    let beta = n * l / (std::f64::consts::PI * r);
    let b2 = beta * beta;
    let pi4 = std::f64::consts::PI.powi(4);
    let bracket = (1.0 + b2).powi(2) + 12.0 * gamma * gamma * z * z / (pi4 * (1.0 + b2).powi(2));
    match loading {
        CylinderLoading::Lateral => bracket / b2,
        CylinderLoading::Hydrostatic => bracket / (b2 + 0.5),
    }
}

/// Minimum-over-`n` Batdorf pressure (MPa) and its wave count.
fn batdorf_pressure(
    e: f64,
    nu: f64,
    r: f64,
    t: f64,
    l: f64,
    gamma: f64,
    loading: CylinderLoading,
) -> (f64, u32) {
    let z = l * l * (1.0 - nu * nu).sqrt() / (r * t);
    let d = e * t.powi(3) / (12.0 * (1.0 - nu * nu));
    let scale = std::f64::consts::PI.powi(2) * d / (l * l * r);
    let mut best = (f64::INFINITY, 2u32);
    for n in 2..=400u32 {
        let p = batdorf_k(n as f64, l, r, z, gamma, loading) * scale;
        if p < best.0 {
            best = (p, n);
        }
    }
    best
}

/// Flügge/von Mises ring-term correction for shallow-shell theory at low
/// wave numbers: `(n²−1)/n²`.
fn low_n_factor(n: u32) -> f64 {
    let n2 = (n * n) as f64;
    (n2 - 1.0) / n2
}

/// Classical long-cylinder (ovalizing, `n = 2`) pressure, SP-8007 Eq. 27
/// at γ = 1.
pub fn long_cylinder_pressure(e: f64, nu: f64, r_mean: f64, t: f64) -> f64 {
    e / (4.0 * (1.0 - nu * nu)) * (t / r_mean).powi(3)
}

/// Classical (Zoelly) sphere bifurcation pressure at mean radius `r`.
pub fn zoelly_pressure(e: f64, nu: f64, r: f64, t: f64) -> f64 {
    2.0 * e / (3.0 * (1.0 - nu * nu)).sqrt() * (t / r).powi(2)
}

/// Classical finite-cylinder bifurcation pressure (MPa) and wave count:
/// SP-8007 Eq. 19–22 at γ = 1, minimized over integer `n ≥ 2`, with the
/// low-`n` ring-term correction, floored by the long-cylinder limit when
/// `Z > 4000` (Eq. 26 regime). This is the number an eigenvalue FE of a
/// perfect, simply supported cylinder should approach.
pub fn classical_cylinder_pressure(
    e: f64,
    nu: f64,
    r_mean: f64,
    t: f64,
    l: f64,
    loading: CylinderLoading,
) -> (f64, u32) {
    let z = l * l * (1.0 - nu * nu).sqrt() / (r_mean * t);
    if z > 4000.0 {
        return (long_cylinder_pressure(e, nu, r_mean, t), 2);
    }
    let (p, n) = batdorf_pressure(e, nu, r_mean, t, l, 1.0, loading);
    (p * low_n_factor(n), n)
}

/// Run the screen. Fail-closed: the verdict is `Unverifiable` when the
/// thin-shell closed forms do not apply; the numbers are still returned as
/// diagnostics but nothing may be claimed from them.
pub fn check_shell(case: &ShellCase) -> Result<ShellCheck, SpecError> {
    case.validate()?;
    let (e, nu) = (case.youngs_modulus_mpa, case.poisson);
    let p_app = case.external_pressure_mpa;
    let mut reasons = Vec::new();
    let mut cautions = Vec::new();

    let out = match case.geometry {
        ShellGeometry::Cylinder {
            outer_diameter_mm: d_o,
            wall_mm: t,
            length_mm: l,
            loading,
        } => {
            let r = 0.5 * (d_o - t);
            let z = l * l * (1.0 - nu * nu).sqrt() / (r * t);
            if d_o / t < 10.0 {
                reasons.push(format!(
                    "D_o/t = {:.1} < 10: not a thin shell — UG-28(c)(2) has a separate \
                     thick-cylinder procedure; this screen does not apply",
                    d_o / t
                ));
            }
            let (p_cl, n, regime, p_kd, kd_basis) = if z > 4000.0 {
                let p_cl = long_cylinder_pressure(e, nu, r, t);
                (
                    p_cl,
                    2,
                    ShellRegime::CylinderLong,
                    0.90 * p_cl,
                    "gamma = 0.90 on the ovalizing long cylinder (NASA/SP-8007-2020/REV 2 \
                     Eq. 27 with Eq. 29)"
                        .to_string(),
                )
            } else {
                let (p_cl, n) = classical_cylinder_pressure(e, nu, r, t, l, loading);
                // Knockdown enters inside the Batdorf bracket (γ² Z²), not
                // as a plain multiplier — evaluate Eq. 20/22 again at γ.
                let (p_kd_raw, n_kd) = batdorf_pressure(e, nu, r, t, l, 0.5625, loading);
                let p_kd = p_kd_raw * low_n_factor(n_kd);
                let mut basis = "sqrt(gamma) = 0.75 inside the Batdorf form (NASA/SP-8007-2020/REV 2 \
                     Eq. 19-22 with Eq. 28)"
                    .to_string();
                if 0.5625 * z < 100.0 {
                    basis.push_str(
                        "; gamma·Z < 100 is outside the range Eq. 28 is stated for (short \
                         cylinder) — the knockdown is applied anyway",
                    );
                    cautions.push(format!(
                        "short cylinder (gamma·Z = {:.0} < 100): SP-8007 reports large test \
                         scatter here and end restraint matters — the knockdown is a guess, \
                         treat the margin with suspicion",
                        0.5625 * z
                    ));
                }
                if n <= 3 || n_kd <= 3 {
                    cautions.push(format!(
                        "governing mode has n = {n} waves: shallow-shell theory reads high by \
                         n²/(n²−1) there, so the classical and knocked-down pressures were \
                         scaled by (n²−1)/n² (Flügge/von Mises ring term)"
                    ));
                }
                // The long-cylinder bound is a floor on a ring-supported
                // span's real buckling pressure, never a ceiling — but the
                // knocked-down number must not exceed the classical one.
                (
                    p_cl,
                    n,
                    ShellRegime::CylinderBatdorf,
                    p_kd.min(p_cl),
                    basis,
                )
            };
            if matches!(loading, CylinderLoading::Lateral) {
                cautions.push(
                    "lateral loading: ends assumed reacted elsewhere — a capped vacuum vessel is \
                     hydrostatic"
                        .into(),
                );
            }
            cautions.push(format!(
                "ends assumed simply supported and held circular at L = {l:.0} mm (flanges or \
                 rings); restraining axial end movement can raise the theoretical pressure by up \
                 to 50% (SP-8007), a free or flexible end lowers it"
            ));
            let p_y = case.yield_strength_mpa.map(|y| y * t / r);
            let hoop = p_kd * r / t;
            (r, r / t, Some(z), Some(n), regime, p_cl, p_kd, kd_basis, p_y, hoop, case.design_factor)
        }
        ShellGeometry::Sphere {
            outer_diameter_mm: d_o,
            wall_mm: t,
        } => sphere_numbers(case, 0.5 * d_o, t, d_o, &mut reasons),
        ShellGeometry::Head {
            head,
            outer_diameter_mm: d_o,
            wall_mm: t,
        } => {
            let r_o = match head {
                HeadKind::Hemispherical => 0.5 * d_o,
                HeadKind::Ellipsoidal2To1 => 0.9 * d_o,
                HeadKind::Torispherical { crown_radius_mm } => {
                    if crown_radius_mm > d_o * 1.0 + 1e-9 {
                        cautions.push(format!(
                            "crown radius {crown_radius_mm:.0} mm exceeds the skirt OD \
                             {d_o:.0} mm — UG-32 limits a torispherical crown radius to the \
                             skirt OD"
                        ));
                    }
                    crown_radius_mm
                }
            };
            cautions.push(
                "head checked as an equivalent sphere (UG-33(d)/(e)); the knuckle and the \
                 head-to-shell junction are NOT checked"
                    .into(),
            );
            sphere_numbers(case, r_o, t, d_o, &mut reasons)
        }
    };
    let (r, r_over_t, z, n, regime, p_cl, p_kd, kd_basis, p_y, sigma_kd, df) = out;

    if regime == ShellRegime::Sphere {
        cautions.push(
            "spheres are the most imperfection-sensitive shells: measured collapse scatters far \
             below classical, and a dent, flat spot or weld peak-out can lose most of the rest — \
             inspect for out-of-roundness"
                .into(),
        );
    }
    let collapse = match p_y {
        Some(py) => {
            if p_kd > 0.5 * py {
                cautions.push(format!(
                    "elastic collapse ({p_kd:.3} MPa) is above half the membrane yield pressure \
                     ({py:.3} MPa): elastic-plastic interaction makes the real collapse lower \
                     than either — this screen caps at yield but does not model plasticity \
                     (use the UG-28 material chart)"
                ));
            }
            p_kd.min(py)
        }
        None => {
            cautions.push(format!(
                "no yield strength given: the {sigma_kd:.0} MPa membrane stress at the elastic \
                 collapse pressure was not checked against yield"
            ));
            p_kd
        }
    };
    let allowable = collapse / df;
    let verdict = if reasons.is_empty() {
        BeamVerdict::Applicable
    } else {
        BeamVerdict::Unverifiable { reasons }
    };
    Ok(ShellCheck {
        regime,
        radius_mm: r,
        radius_over_thickness: r_over_t,
        batdorf_z: z,
        circumferential_waves: n,
        classical_pressure_mpa: p_cl,
        knockdown: p_kd / p_cl,
        knockdown_basis: kd_basis,
        knocked_down_pressure_mpa: p_kd,
        yield_pressure_mpa: p_y,
        collapse_pressure_mpa: collapse,
        design_factor: df,
        allowable_pressure_mpa: allowable,
        margin: allowable / p_app,
        collapse_margin: collapse / p_app,
        applied_pressure_mpa: p_app,
        verdict,
        cautions,
    })
}

type Numbers = (
    f64,
    f64,
    Option<f64>,
    Option<u32>,
    ShellRegime,
    f64,
    f64,
    String,
    Option<f64>,
    f64,
    f64,
);

/// Sphere of outside radius `r_o` (UG-28(d) convention).
fn sphere_numbers(
    case: &ShellCase,
    r_o: f64,
    t: f64,
    d_o: f64,
    reasons: &mut Vec<String>,
) -> Numbers {
    let (e, nu) = (case.youngs_modulus_mpa, case.poisson);
    if r_o / t < 10.0 || d_o / t < 10.0 {
        reasons.push(format!(
            "R_o/t = {:.1}: not a thin shell — the elastic sphere formulas do not apply",
            r_o / t
        ));
    }
    let r_mean = r_o - 0.5 * t;
    let p_cl = zoelly_pressure(e, nu, r_mean, t);
    // UG-28(d): A = 0.125/(Ro/t), B = A·E/2, Pa = B/(Ro/t) — so the implied
    // collapse estimate is 4·Pa = 0.25·E·(t/Ro)² with a factor of 4.
    let p_kd = 0.25 * e * (t / r_o).powi(2);
    let p_y = case.yield_strength_mpa.map(|y| 2.0 * y * t / r_mean);
    let sigma = p_kd * r_mean / (2.0 * t);
    (
        r_o,
        r_o / t,
        None,
        None,
        ShellRegime::Sphere,
        p_cl,
        p_kd.min(p_cl),
        "ASME BPVC VIII-1 UG-28(d) elastic branch: A = 0.125/(Ro/t), B = A·E/2, \
         Pa = B/(Ro/t) = 0.0625·E/(Ro/t)², i.e. collapse 0.25·E·(t/Ro)² with a design factor \
         of 4 (the sphere knockdown is the ratio to Zoelly's classical value)"
            .to_string(),
        p_y,
        sigma,
        4.0,
    )
}

fn caveat() -> &'static str {
    "closed-form thin-shell screen: linear-elastic, perfect geometry knocked down by the cited \
     empirical factor; a linear (eigenvalue) bifurcation pressure is UNCONSERVATIVE for shells — \
     real ones collapse below it because of imperfections, which is why the knockdown is applied; \
     idealized end support, no welds, nozzles, ports or out-of-roundness beyond what the \
     knockdown covers, elastic-plastic interaction only capped at membrane yield; a screen, not \
     ASME code compliance"
}

fn provenance_lines(check: &ShellCheck, case: &ShellCase) -> Vec<String> {
    let mut lines = vec![
        format!(
            "geometry {:?}; R = {:.2} mm, R/t = {:.1}{}{}",
            case.geometry,
            check.radius_mm,
            check.radius_over_thickness,
            check
                .batdorf_z
                .map(|z| format!(", Z = {z:.0}"))
                .unwrap_or_default(),
            check
                .circumferential_waves
                .map(|n| format!(", n = {n}"))
                .unwrap_or_default()
        ),
        format!(
            "regime {:?}: classical {:.4} MPa, knocked down {:.4} MPa (x{:.3}: {})",
            check.regime,
            check.classical_pressure_mpa,
            check.knocked_down_pressure_mpa,
            check.knockdown,
            check.knockdown_basis
        ),
        format!(
            "collapse {:.4} MPa / design factor {} = allowable {:.4} MPa vs applied {:.4} MPa",
            check.collapse_pressure_mpa,
            check.design_factor,
            check.allowable_pressure_mpa,
            check.applied_pressure_mpa
        ),
    ];
    lines.extend(check.cautions.iter().map(|c| format!("caution: {c}")));
    lines
}

/// Predicted claims for an **applicable** screen (`vcad.fea-claims/1`).
/// Refuses (fail-closed) when the verdict is `Unverifiable`.
pub fn predicted_claims(
    check: &ShellCheck,
    case: &ShellCase,
) -> Result<crate::receipt::ClaimSet, crate::receipt::ClaimError> {
    if let BeamVerdict::Unverifiable { reasons } = &check.verdict {
        return Err(crate::receipt::ClaimError::Unverifiable(reasons.clone()));
    }
    let cav = caveat();
    let mk = |name: &str, value: f64, unit: &str, note: String| crate::receipt::Claim {
        name: name.into(),
        value,
        unit: unit.into(),
        basis: "predicted".into(),
        note,
    };
    let claims = vec![
        mk(
            "classical_buckling_pressure_mpa",
            check.classical_pressure_mpa,
            "MPa",
            format!(
                "perfect-shell linear bifurcation pressure ({:?}) — what an eigenvalue analysis \
                 converges to; UNCONSERVATIVE, do not design to it; {cav}",
                check.regime
            ),
        ),
        mk(
            "imperfection_knockdown",
            check.knockdown,
            "1",
            format!(
                "knocked-down / classical; {} — linear buckling over-predicts real shell \
                 collapse, this factor is the stated correction",
                check.knockdown_basis
            ),
        ),
        mk(
            "collapse_pressure_mpa",
            check.collapse_pressure_mpa,
            "MPa",
            format!(
                "min(knocked-down elastic {:.4} MPa, membrane yield {}); {cav}",
                check.knocked_down_pressure_mpa,
                check
                    .yield_pressure_mpa
                    .map(|p| format!("{p:.4} MPa"))
                    .unwrap_or_else(|| "not given".into())
            ),
        ),
        mk(
            "allowable_external_pressure_mpa",
            check.allowable_pressure_mpa,
            "MPa",
            format!(
                "collapse / design factor {}; {cav}",
                check.design_factor
            ),
        ),
        mk(
            "external_pressure_margin",
            check.margin,
            "1",
            format!(
                "allowable / applied {:.4} MPa ({:.2} atm); >= 1 passes the screen; collapse \
                 margin with no design factor is {:.2}",
                check.applied_pressure_mpa,
                check.applied_pressure_mpa / ATM_MPA,
                check.collapse_margin
            ),
        ),
    ];
    Ok(crate::receipt::ClaimSet {
        schema: crate::receipt::CLAIM_SCHEMA.to_string(),
        provenance: crate::receipt::SolverProvenance {
            levels: provenance_lines(check, case),
            material: match case.yield_strength_mpa {
                Some(y) => format!(
                    "E {} MPa, nu {}, yield {} MPa",
                    case.youngs_modulus_mpa, case.poisson, y
                ),
                None => format!("E {} MPa, nu {}", case.youngs_modulus_mpa, case.poisson),
            },
            loads: vec![format!(
                "external pressure {:.5} MPa ({:.3} atm)",
                case.external_pressure_mpa,
                case.external_pressure_mpa / ATM_MPA
            )],
            supports: vec![match case.geometry {
                ShellGeometry::Cylinder { length_mm, .. } => format!(
                    "simply supported, held circular, every {length_mm:.0} mm"
                ),
                _ => "complete shell (no support idealization)".into(),
            }],
            displacement_change_rel: 0.0,
            stress_change_rel: 0.0,
        },
        claims,
    })
}

/// Oracle reference for the shell screen.
pub fn oracle() -> vcad_receipt::OracleRef {
    vcad_receipt::OracleRef::new("vcad-kernel-fea/shell", env!("CARGO_PKG_VERSION"))
}

/// Unified-receipt claims under `structure.shell.*`, basis `Predicted`.
pub fn design_claims(set: &crate::receipt::ClaimSet) -> Vec<vcad_receipt::ReceiptClaim> {
    let oracle = oracle();
    let provenance = set.provenance.levels.join(" | ");
    set.claims
        .iter()
        .map(|c| {
            vcad_receipt::ReceiptClaim::pass(
                format!("structure.shell.{}", c.name),
                crate::receipt::RECEIPT_DOMAIN,
                c.note.clone(),
                oracle.clone(),
            )
            .with_basis(vcad_receipt::ClaimBasis::Predicted)
            .with_measured(if c.unit == "1" {
                vcad_receipt::ClaimQuantity::bare(c.value)
            } else {
                vcad_receipt::ClaimQuantity::new(c.value, &c.unit)
            })
            .with_details(provenance.clone())
        })
        .collect()
}

/// One Unverifiable claim carrying the reasons.
pub fn design_claims_unverifiable(reasons: &[String]) -> Vec<vcad_receipt::ReceiptClaim> {
    vec![vcad_receipt::ReceiptClaim::unverifiable(
        "structure.shell.applicability",
        crate::receipt::RECEIPT_DOMAIN,
        "closed-form shell buckling screen applicability gates",
        oracle(),
        format!(
            "the thin-shell closed forms do not apply — no buckling QoI is claimed: {}",
            reasons.join("; ")
        ),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    const E_SS: f64 = 193_000.0;
    const NU_SS: f64 = 0.29;

    fn approx(a: f64, b: f64, rel: f64, what: &str) {
        let d = (a - b).abs() / b.abs().max(1e-300);
        assert!(d < rel, "{what}: {a} vs {b} (rel {d:.4} >= {rel})");
    }

    fn cyl(d: f64, t: f64, l: f64) -> ShellCase {
        ShellCase {
            geometry: ShellGeometry::Cylinder {
                outer_diameter_mm: d,
                wall_mm: t,
                length_mm: l,
                loading: CylinderLoading::Hydrostatic,
            },
            youngs_modulus_mpa: E_SS,
            poisson: NU_SS,
            yield_strength_mpa: Some(205.0),
            external_pressure_mpa: ATM_MPA,
            design_factor: 3.0,
        }
    }

    fn sphere(d: f64, t: f64) -> ShellCase {
        ShellCase {
            geometry: ShellGeometry::Sphere {
                outer_diameter_mm: d,
                wall_mm: t,
            },
            ..cyl(d, t, 1.0)
        }
    }

    #[test]
    fn zoelly_constant() {
        // ν = 0.3 → 2/√(3·0.91) = 1.2104.
        approx(zoelly_pressure(1.0, 0.3, 1.0, 1.0), 1.2104, 1e-4, "zoelly");
    }

    #[test]
    fn sphere_matches_ug28d_elastic_branch() {
        let c = check_shell(&sphere(152.4, 1.0)).unwrap();
        let ro_t = 76.2 / 1.0;
        // Pa = 0.0625 E / (Ro/t)².
        approx(
            c.knocked_down_pressure_mpa / c.design_factor,
            0.0625 * E_SS / (ro_t * ro_t),
            1e-9,
            "UG-28(d)",
        );
        assert_eq!(c.design_factor, 4.0);
        assert!(c.knockdown > 0.19 && c.knockdown < 0.23, "kd {}", c.knockdown);
        assert!(c.passes(), "6\" x 1 mm SS sphere holds a vacuum: {c:?}");
    }

    #[test]
    fn long_cylinder_limit_is_bresse_with_gamma_090() {
        // 152.4 x 1 mm, 10 m long: Z ≫ 4000.
        let c = check_shell(&cyl(152.4, 1.0, 10_000.0)).unwrap();
        assert_eq!(c.regime, ShellRegime::CylinderLong);
        let r = 0.5 * (152.4 - 1.0);
        approx(
            c.classical_pressure_mpa,
            E_SS / (4.0 * (1.0 - NU_SS * NU_SS)) * (1.0 / r).powi(3),
            1e-12,
            "bresse",
        );
        approx(c.knockdown, 0.90, 1e-12, "eq 29");
        assert_eq!(c.circumferential_waves, Some(2));
    }

    #[test]
    fn batdorf_regime_matches_eq24_asymptote() {
        // Moderate length, 100 < Z ≪ 4000: k → 1.04√(γZ) (Eq. 23), so the
        // minimized Batdorf pressure must land on Eq. 24 within a few %.
        let (d, t, l) = (1000.0, 2.0, 1500.0);
        let r = 0.5 * (d - t);
        let (p, n) = batdorf_pressure(E_SS, NU_SS, r, t, l, 1.0, CylinderLoading::Lateral);
        let eq24 = 0.855 * E_SS / (1.0 - NU_SS * NU_SS).powf(0.75) * (t / r).powf(2.5) * (r / l);
        assert!(n > 3, "mid-length cylinder buckles in many waves (n {n})");
        approx(p, eq24, 0.05, "Eq. 24 asymptote");
    }

    #[test]
    fn knockdown_only_lowers() {
        for (t, l) in [(0.5, 100.0), (1.0, 300.0), (3.0, 600.0), (2.0, 50.0)] {
            let c = check_shell(&cyl(152.4, t, l)).unwrap();
            assert!(c.knockdown > 0.0 && c.knockdown <= 1.0, "{t} {l}: {}", c.knockdown);
            assert!(c.allowable_pressure_mpa <= c.collapse_pressure_mpa);
        }
    }

    #[test]
    fn six_inch_tube_wall_sweep_is_monotone_and_thin_fails() {
        // 6" OD SS tube, 300 mm between CF flanges, under one atmosphere.
        let mut last = 0.0;
        for t in [0.5, 1.0, 1.5, 2.0, 3.0] {
            let c = check_shell(&cyl(152.4, t, 300.0)).unwrap();
            assert!(c.applicable(), "{t}: {:?}", c.verdict);
            assert!(c.collapse_pressure_mpa > last, "monotone in wall");
            last = c.collapse_pressure_mpa;
        }
        let thin = check_shell(&cyl(152.4, 0.5, 300.0)).unwrap();
        let thick = check_shell(&cyl(152.4, 3.0, 300.0)).unwrap();
        assert!(!thin.passes(), "0.5 mm wall must not pass under vacuum: {thin:?}");
        assert!(thick.passes(), "3 mm wall passes: {thick:?}");
    }

    #[test]
    fn thick_wall_is_unverifiable_and_claims_nothing() {
        let case = cyl(50.0, 10.0, 100.0);
        let c = check_shell(&case).unwrap();
        assert!(!c.applicable());
        assert!(predicted_claims(&c, &case).is_err());
        let receipt =
            vcad_receipt::DesignReceipt::with_claims(design_claims_unverifiable(match &c.verdict {
                BeamVerdict::Unverifiable { reasons } => reasons,
                _ => unreachable!(),
            }));
        assert_ne!(receipt.verdict(), vcad_receipt::ReceiptVerdict::Pass);
    }

    #[test]
    fn claims_state_knockdown_and_ride_provisional() {
        let case = cyl(152.4, 2.0, 300.0);
        let c = check_shell(&case).unwrap();
        let set = predicted_claims(&c, &case).unwrap();
        let kd = set
            .claims
            .iter()
            .find(|c| c.name == "imperfection_knockdown")
            .unwrap();
        assert!(kd.note.contains("SP-8007"), "{}", kd.note);
        let cl = set
            .claims
            .iter()
            .find(|c| c.name == "classical_buckling_pressure_mpa")
            .unwrap();
        assert!(cl.note.contains("UNCONSERVATIVE"));
        let claims = design_claims(&set);
        assert!(claims.iter().all(|c| c.id.starts_with("structure.shell.")));
        let receipt = vcad_receipt::DesignReceipt::with_claims(claims);
        assert_eq!(receipt.verdict(), vcad_receipt::ReceiptVerdict::Provisional);
    }

    #[test]
    fn heads_use_equivalent_sphere_radius() {
        let mut case = sphere(152.4, 1.0);
        case.geometry = ShellGeometry::Head {
            head: HeadKind::Ellipsoidal2To1,
            outer_diameter_mm: 152.4,
            wall_mm: 1.0,
        };
        let c = check_shell(&case).unwrap();
        approx(c.radius_mm, 0.9 * 152.4, 1e-12, "K1 Do");
        let hemi = check_shell(&sphere(152.4, 1.0)).unwrap();
        assert!(c.collapse_pressure_mpa < hemi.collapse_pressure_mpa);
    }

    #[test]
    fn json_round_trip_and_defaults() {
        let case: ShellCase = serde_json::from_str(
            r#"{"geometry":{"type":"cylinder","outer_diameter_mm":152.4,"wall_mm":1.5,
                "length_mm":300},"youngs_modulus_mpa":193000,"poisson":0.29}"#,
        )
        .unwrap();
        assert_eq!(case.external_pressure_mpa, ATM_MPA);
        assert_eq!(case.design_factor, 3.0);
        let c = check_shell(&case).unwrap();
        assert!(serde_json::to_string(&c).unwrap().contains("classical_pressure_mpa"));
    }
}
