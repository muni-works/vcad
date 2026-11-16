//! Cross sections and surface yields.
//!
//! **Fusion:** the Bosch–Hale parameterization for the two D-D branches
//! (Bosch & Hale 1992, Nucl. Fusion 32, 611). σ(E) = S(E) / (E ·
//! exp(B_G/√E)), with S a polynomial in the center-of-mass energy E (keV)
//! and B_G the Gamow constant. Valid 0.5–4900 keV; below the validity floor
//! this module returns 0 (the true value there is astronomically small).
//!
//! **Atomic/molecular collisions with the D₂ background** (the CX chain,
//! see [`crate::chain`]). Every fit is for the hydrogen system and is
//! applied to deuterium **at equal velocity** — i.e. evaluated at the
//! projectile's lab energy *per amu*, `E/(2m)` for a `D_m⁺` ion of lab
//! energy `E` (a D⁺ at 30 keV reads the H⁺ fit at 15 keV). The isotope
//! effect at keV energies is small next to the stated ±25% data
//! uncertainty ([`ATOMIC_DATA_REL_UNCERTAINTY`]). Sources, all read from
//! the primary text for this work:
//!
//! - σ_cx(H⁺ + H₂(v=0) → H + H₂⁺): Janev, Reiter & Samm, *Collision
//!   Processes in Low-Temperature Hydrogen Plasmas*, FZ-Jülich report
//!   Jül-4105 (2003), Eq. (145a,b), Table 31 (v = 0 column); valid
//!   threshold–200 keV.
//! - σ_ion(H⁺ + H₂ → H⁺ + H₂⁺ + e): HYDHEL database (Janev, Langer et al.,
//!   *Elementary Processes in Hydrogen-Helium Plasmas*, Springer 1987, as
//!   maintained at eirene.de), fit H.1 reaction 3.2.5;
//!   `ln σ[cm²] = Σ aₙ (ln E[eV])ⁿ`, valid 30 eV–100 keV.
//! - σ_cx(H₂⁺ + H₂ → H₂ + H₂⁺), σ_ion(H₂⁺ + H₂ → 2H₂⁺ + e) and
//!   H₃⁺ formation (H₂⁺ + H₂ → H₃⁺ + H): HYDHEL H.1 reactions 4.3.1, 4.3.2,
//!   4.3.3 (energy scale = H₂⁺ lab energy; 4.3.3 valid ≤ 40 eV).
//! - Collision-induced dissociation of fast molecular ions on H₂:
//!   H₂⁺ + H₂ → H⁺ + H + H₂ (Jül-4105 Eq. 209, H₂⁺ lab energy) and fast-H⁺
//!   production from H₃⁺ + H₂ (Jül-4105 Eq. 243; the text quotes those data
//!   in centre-of-mass energy, which this module assumes — an ambiguity
//!   carried by the band's `molecular_breakup` component).
//! - e⁻ + H₂ total ionization: binary-encounter-Bethe model, Kim & Rudd,
//!   Phys. Rev. A 50, 3954 (1994), H₂ constants B = 15.43 eV,
//!   U = 15.98 eV, N = 2 (agrees with HYDHEL 2.2.9 to ~10–20% at
//!   30 eV–1 keV; see the tests).

/// Gamow constant for D-D, √keV.
const B_G_DD: f64 = 31.397;

/// Millibarn → m².
const MB_TO_M2: f64 = 1e-31;

#[inline]
fn bosch_hale_sigma_mb(e_cm_kev: f64, a: [f64; 5]) -> f64 {
    if e_cm_kev < 0.5 {
        return 0.0;
    }
    let e = e_cm_kev;
    let s = a[0] + e * (a[1] + e * (a[2] + e * (a[3] + e * a[4])));
    s / (e * (B_G_DD / e.sqrt()).exp())
}

/// D(d,n)³He cross section, m², at center-of-mass energy `e_cm_kev`.
///
/// This is the neutron branch (2.45 MeV neutrons) — the one a neutron
/// counter sees.
pub fn dd_n_sigma_m2(e_cm_kev: f64) -> f64 {
    bosch_hale_sigma_mb(
        e_cm_kev,
        [5.3701e4, 3.3027e2, -1.2706e-1, 2.9327e-5, -2.5151e-9],
    ) * MB_TO_M2
}

/// D(d,p)T cross section, m², at center-of-mass energy `e_cm_kev`.
pub fn dd_p_sigma_m2(e_cm_kev: f64) -> f64 {
    bosch_hale_sigma_mb(
        e_cm_kev,
        [5.5576e4, 2.1054e2, -3.2638e-2, 1.4987e-6, 1.8181e-10],
    ) * MB_TO_M2
}

/// Deuteron number density of D₂ gas at `pressure_mtorr` and
/// `temperature_k`, m⁻³ (two deuterons per molecule).
pub fn d2_deuteron_density_m3(pressure_mtorr: f64, temperature_k: f64) -> f64 {
    let pa = pressure_mtorr * 0.133_322;
    2.0 * pa / (1.380_649e-23 * temperature_k)
}

/// D₂ **molecule** number density at `pressure_mtorr` and `temperature_k`,
/// m⁻³ — the target density for charge exchange and ionization (half the
/// deuteron density used for fusion).
pub fn d2_molecule_density_m3(pressure_mtorr: f64, temperature_k: f64) -> f64 {
    0.5 * d2_deuteron_density_m3(pressure_mtorr, temperature_k)
}

/// Stated 1σ-ish relative uncertainty of the atomic/molecular fits above
/// (Janev et al. quote typical accuracies of 20–30% for these processes
/// in the keV range; the band treats ±25% as a 2σ excursion).
pub const ATOMIC_DATA_REL_UNCERTAINTY: f64 = 0.25;

const CM2_TO_M2: f64 = 1e-4;

/// HYDHEL H.1 form: `ln σ[cm²] = Σ aₙ (ln E)ⁿ`, returned in m².
fn hydhel_h1_m2(a: &[f64; 9], e_ev: f64) -> f64 {
    let l = e_ev.ln();
    let mut acc = 0.0;
    let mut pow = 1.0;
    for c in a {
        acc += c * pow;
        pow *= l;
    }
    acc.exp() * CM2_TO_M2
}

/// HYDHEL H.1 3.2.5, p + H₂ → p + H₂⁺ + e.
const H1_3_2_5: [f64; 9] = [
    1.681920096116e+01,
    -8.751084517093e+01,
    5.024534132103e+01,
    -1.560982391827e+01,
    2.931478842554e+00,
    -3.421137108600e-01,
    2.429148918124e-02,
    -9.612735627840e-04,
    1.625014598463e-05,
];
/// HYDHEL H.1 4.3.1, H₂⁺ + H₂ → H₂ + H₂⁺.
const H1_4_3_1: [f64; 9] = [
    -3.427958758517e+01,
    -7.121484125189e-02,
    4.690466187943e-02,
    -8.033946660540e-03,
    -2.265090924593e-03,
    -2.102414848737e-04,
    1.948869487515e-04,
    -2.208124950005e-05,
    7.262446915488e-07,
];
/// HYDHEL H.1 4.3.2, H₂⁺ + H₂ → H₂⁺ + H₂⁺ + e.
const H1_4_3_2: [f64; 9] = [
    -2.383489439121e+02,
    2.384368432909e+02,
    -1.263102889116e+02,
    3.746454397894e+01,
    -6.767700946931e+00,
    7.629123486032e-01,
    -5.246096809457e-02,
    2.014116839267e-03,
    -3.310073123768e-05,
];
/// HYDHEL H.1 4.3.3, H₂⁺ + H₂ → H₃⁺ + H.
const H1_4_3_3: [f64; 9] = [
    -3.422188283057e+01,
    -7.733921420462e-01,
    -4.625452844356e-01,
    -2.307540053739e-01,
    3.188925541844e-02,
    3.622033742962e-02,
    -5.093822617734e-03,
    -2.420060478318e-03,
    4.634259500837e-04,
];

/// Charge exchange D⁺ + D₂ → D + D₂⁺, m², at projectile energy per amu
/// `e_amu_ev` (Jül-4105 Eq. 145, v = 0; evaluated as H⁺ + H₂ at equal
/// velocity). Peaks ≈ 1.2×10⁻¹⁹ m² near 5 keV/amu.
pub fn cx_atomic_m2(e_amu_ev: f64) -> f64 {
    let e = e_amu_ev;
    if e <= 0.0 {
        return 0.0;
    }
    const E0: f64 = 2.67;
    let low = if e > E0 {
        18.60 * e.powf(-1.66) * (1.0 - (E0 / e).powf(2.53)).powf(1.93)
    } else {
        0.0
    };
    let high = 17.3 * (-105.0 / (e * e)).exp()
        / (1.0e4 * e.powf(-1.12)
            + 3.64e-4 * e.powf(0.90)
            + 5.03e-19 * e.powf(4.00)
            + 5.87e-28 * e.powf(5.50));
    // Fit returns units of 1e-16 cm² = 1e-20 m².
    (low + high) * 1.0e-20
}

/// Ionization of D₂ by a fast D⁺ (D⁺ + D₂ → D⁺ + D₂⁺ + e), m², at energy
/// per amu `e_amu_ev` (HYDHEL 3.2.5; zero below its 30 eV validity floor).
/// Also used for fast-neutral impact ionization (an approximation: H⁰ and
/// H⁺ impact ionization of H₂ agree within the data band above ~10
/// keV/amu, and the neutral one is smaller below — stated in the claim).
pub fn ion_atomic_m2(e_amu_ev: f64) -> f64 {
    if e_amu_ev < 30.0 {
        return 0.0;
    }
    hydhel_h1_m2(&H1_3_2_5, e_amu_ev.min(1.0e5))
}

/// Charge exchange of a fast molecular ion on D₂, m², at energy per amu
/// (HYDHEL 4.3.1 at equal velocity; applied to D₂⁺ and, for lack of
/// evaluated data, D₃⁺ — a stated model-form assumption).
pub fn cx_molecular_m2(e_amu_ev: f64) -> f64 {
    if e_amu_ev <= 0.0 {
        return 0.0;
    }
    hydhel_h1_m2(&H1_4_3_1, (2.0 * e_amu_ev).clamp(0.1, 1.0e5))
}

/// Ionization of D₂ by a fast molecular ion, m², at energy per amu
/// (HYDHEL 4.3.2; zero below its 36.7 eV validity floor in H₂⁺ energy).
pub fn ion_molecular_m2(e_amu_ev: f64) -> f64 {
    let e = 2.0 * e_amu_ev;
    if e < 36.7 {
        return 0.0;
    }
    hydhel_h1_m2(&H1_4_3_2, e.min(1.0e5))
}

/// D₃⁺ formation D₂⁺ + D₂ → D₃⁺ + D, m², at D₂⁺ energy per amu (HYDHEL
/// 4.3.3, valid ≤ 40 eV H₂⁺ lab energy; returns 0 above — the reaction
/// closes at keV energies).
pub fn d3_formation_m2(e_amu_ev: f64) -> f64 {
    let e = 2.0 * e_amu_ev;
    if e > 40.0 {
        return 0.0;
    }
    hydhel_h1_m2(&H1_4_3_3, e.max(0.1))
}

/// Collision-induced dissociation of a fast D₂⁺ on D₂ (→ D⁺ + D + D₂), m²,
/// at D₂⁺ energy per amu (Jül-4105 Eq. 209 at equal velocity; reliability
/// stated as low above 30–40 keV H₂⁺ lab energy).
pub fn cid_d2plus_m2(e_amu_ev: f64) -> f64 {
    let e = 2.0 * e_amu_ev;
    if e <= 2.0 {
        return 0.0;
    }
    let low = 4.05 / e.powf(0.653) * (-3.15 / (e - 2.0).powf(1.65)).exp();
    let high = 0.139 * e.powf(0.318) * (-680.0 / e.powf(2.10)).exp()
        / (1.0 + 2.75e-12 * e.powf(2.65) + 9.04e-23 * e.powf(4.65));
    (low + high) * 1.0e-20
}

/// Fast-D⁺ production by collision-induced dissociation of a fast D₃⁺ on
/// D₂ (→ D⁺ + D₂ + D₂), m², at D₃⁺ energy per amu (Jül-4105 Eq. 243,
/// evaluated at the H₃⁺ + H₂ centre-of-mass energy `E_cm = (2/5)·E_lab`).
pub fn cid_d3plus_m2(e_amu_ev: f64) -> f64 {
    let e = 0.4 * 3.0 * e_amu_ev;
    if e <= 4.32 {
        return 0.0;
    }
    let low = 18.5 * (-54.5 / e.powf(1.65)).exp() / (e.powf(0.63) * (1.0 + 4.66e-6 * e.powf(1.93)))
        * (1.0 - (4.32 / e).powf(2.32)).powf(4.06);
    let high = 3.84 * (-44.2 / e.powf(0.48)).exp()
        / (1.0 + 7.49e-11 * e.powf(1.85) + 7.55e-28 * e.powf(4.60));
    (low + high) * 1.0e-20
}

/// Electron-impact total ionization of D₂, m², at electron kinetic
/// energy `t_ev` (Kim–Rudd BEB with Q = 1, non-relativistic; the
/// relativistic correction is < 5% below 40 keV).
pub fn e_ionization_d2_m2(t_ev: f64) -> f64 {
    const B: f64 = 15.43;
    const U: f64 = 15.98;
    const N: f64 = 2.0;
    const A0_M: f64 = 0.529_177_210_9e-10;
    const RYD_EV: f64 = 13.605_693;
    if t_ev <= B {
        return 0.0;
    }
    let t = t_ev / B;
    let u = U / B;
    let s = 4.0 * std::f64::consts::PI * A0_M * A0_M * N * (RYD_EV / B).powi(2);
    let lt = t.ln();
    s / (t + u + 1.0) * (0.5 * (1.0 - 1.0 / (t * t)) * lt + (1.0 - 1.0 / t - lt / (t + 1.0)))
}

/// Kinetic ion-induced secondary-electron yield of a metal cathode.
///
/// `γ(E) = m · γ₁₅ · √(min(ε, 25 keV)/15 keV)` for a `D_m⁺` ion (or fast
/// `D_m` neutral) of lab energy `E`, with `ε = E/(2m)` the energy per amu:
/// kinetic emission tracks electronic stopping, which is
/// velocity-proportional (Lindhard–Scharff) below the ~25 keV/amu knee,
/// and a molecular projectile emits as its `m` fragments at equal
/// velocity (additivity). Potential emission (≲0.05 for hydrogen ions) is
/// ignored.
///
/// **The magnitude is the least-pinned input in the yield model.**
/// Technical (gas-covered, sputter-cleaned-in-operation) metal surfaces
/// under 10–20 keV/amu hydrogen bombardment are commonly quoted at
/// γ ≈ 1–2 per proton (Baragiola et al., Surf. Sci. 90, 240 (1979);
/// Hasselkamp, *Particle Induced Electron Emission II*, Springer Tracts
/// Mod. Phys. 123 (1992)); this work could not read a primary table, so
/// the band carries the whole range [`GAMMA_SE_15KEV_LOW`] …
/// [`GAMMA_SE_15KEV_HIGH`] around [`GAMMA_SE_15KEV_CENTRAL`].
pub fn gamma_se(e_lab_ev: f64, mass_deuterons: u32, gamma_15kev_amu: f64) -> f64 {
    let m = mass_deuterons.max(1) as f64;
    let e_amu = (e_lab_ev / (2.0 * m)).max(0.0);
    m * gamma_15kev_amu * (e_amu.min(25.0e3) / 15.0e3).sqrt()
}

/// Central γ_SE per proton-equivalent at 15 keV/amu (see [`gamma_se`]).
pub const GAMMA_SE_15KEV_CENTRAL: f64 = 1.2;
/// Low edge of the γ_SE range carried in the band.
pub const GAMMA_SE_15KEV_LOW: f64 = 0.6;
/// High edge of the γ_SE range carried in the band.
pub const GAMMA_SE_15KEV_HIGH: f64 = 2.4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_published_anchor_values() {
        // σ_DDn at E_cm = 50 keV (E_lab = 100 keV) ≈ 16.5 mb.
        let s50 = dd_n_sigma_m2(50.0) / MB_TO_M2;
        assert!((14.0..19.0).contains(&s50), "sigma_ddn(50 keV) = {s50} mb");
        // σ_DDn at E_cm = 15 keV ≈ 1.2 mb.
        let s15 = dd_n_sigma_m2(15.0) / MB_TO_M2;
        assert!((0.9..1.5).contains(&s15), "sigma_ddn(15 keV) = {s15} mb");
        // Both branches are within ~2x of each other at fusor energies.
        let p50 = dd_p_sigma_m2(50.0) / MB_TO_M2;
        assert!(
            (0.5..2.0).contains(&(p50 / s50)),
            "branch ratio off: n {s50} mb vs p {p50} mb"
        );
    }

    #[test]
    fn rises_steeply_through_fusor_energies() {
        let mut last = 0.0;
        for e in [1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 250.0] {
            let s = dd_n_sigma_m2(e);
            assert!(s > last, "sigma must rise with energy at {e} keV");
            last = s;
        }
        // The steepness that makes voltage worth chasing: 15 → 30 keV CM
        // buys more than 4x.
        assert!(dd_n_sigma_m2(30.0) / dd_n_sigma_m2(15.0) > 4.0);
    }

    #[test]
    fn below_validity_floor_is_zero() {
        assert_eq!(dd_n_sigma_m2(0.3), 0.0);
        assert_eq!(dd_p_sigma_m2(0.0), 0.0);
        assert_eq!(dd_n_sigma_m2(-5.0), 0.0);
    }

    #[test]
    fn gas_density_is_physical() {
        // 1 mTorr D₂ at 300 K ≈ 3.2e19 molecules/m³ → 6.4e19 deuterons/m³.
        let n = d2_deuteron_density_m3(1.0, 300.0);
        assert!((6.0e19..7.0e19).contains(&n), "n = {n}");
    }

    #[test]
    fn charge_exchange_matches_jul4105_shape() {
        // Peak ~1.16e-19 m² near 5 keV/amu, 7.4e-20 at 15 keV/amu (the
        // D⁺ at 30 keV point), falling to ~2.7e-21 at 100 keV/amu.
        let s5 = cx_atomic_m2(5.0e3);
        assert!((1.10e-19..1.22e-19).contains(&s5), "cx(5 keV/amu) = {s5:e}");
        let s15 = cx_atomic_m2(15.0e3);
        assert!(
            (7.0e-20..7.9e-20).contains(&s15),
            "cx(15 keV/amu) = {s15:e}"
        );
        assert!(cx_atomic_m2(1.0e5) < 4.0e-21);
        // Peak is between 1 and 15 keV/amu.
        assert!(s5 > cx_atomic_m2(1.0e3) && s5 > s15);
    }

    #[test]
    fn hydhel_anchor_values() {
        // HYDHEL 3.2.5 maximum ≈ 2e-16 cm² near 50–70 keV (Jül-4105 §5.4.2).
        let peak = ion_atomic_m2(6.0e4);
        assert!((1.8e-20..2.4e-20).contains(&peak), "ion(60 keV) = {peak:e}");
        assert_eq!(ion_atomic_m2(10.0), 0.0);
        // H₂⁺ + H₂ symmetric CX is large and flat at low energy (~1e-19 m²).
        let m = cx_molecular_m2(5.0);
        assert!((0.9e-19..1.4e-19).contains(&m), "cx_mol = {m:e}");
        // H₃⁺ formation is Langevin-like: large at thermal, gone at keV.
        assert!(d3_formation_m2(0.05) > 1.0e-19);
        assert_eq!(d3_formation_m2(1.0e3), 0.0);
        assert!(ion_molecular_m2(1.0e4) > 0.0);
    }

    #[test]
    fn beb_matches_hydhel_electron_ionization() {
        // BEB peak ~1.0e-20 m² near 70 eV (HYDHEL 2.2.9: 1.07e-20).
        let p = e_ionization_d2_m2(70.0);
        assert!((0.9e-20..1.15e-20).contains(&p), "BEB(70 eV) = {p:e}");
        // 1 keV: HYDHEL 1.98e-21, BEB 2.46e-21 — within the data band.
        let k = e_ionization_d2_m2(1.0e3);
        assert!((1.9e-21..2.6e-21).contains(&k), "BEB(1 keV) = {k:e}");
        assert_eq!(e_ionization_d2_m2(10.0), 0.0);
    }

    #[test]
    fn secondary_emission_scales_with_velocity_and_fragments() {
        let g = gamma_se(30.0e3, 1, GAMMA_SE_15KEV_CENTRAL);
        assert!((g - GAMMA_SE_15KEV_CENTRAL).abs() < 1e-12);
        // D₂⁺ at 30 keV = two D at 15 keV each (7.5 keV/amu).
        let g2 = gamma_se(30.0e3, 2, GAMMA_SE_15KEV_CENTRAL);
        assert!((g2 - 2.0 * GAMMA_SE_15KEV_CENTRAL * (0.5_f64).sqrt()).abs() < 1e-12);
        assert_eq!(gamma_se(0.0, 1, 1.2), 0.0);
    }

    #[test]
    fn molecule_density_is_half_the_deuteron_density() {
        let n = d2_molecule_density_m3(5.0, 300.0);
        assert!((n - 0.5 * d2_deuteron_density_m3(5.0, 300.0)).abs() < 1.0);
        assert!((1.55e20..1.66e20).contains(&n), "n = {n:e}");
    }

    #[test]
    fn molecular_breakup_is_alive_at_kev() {
        // D₂⁺ at 30 keV (7.5 keV/amu → H₂⁺ 15 keV): ≈2.2e-20 m².
        let d2 = cid_d2plus_m2(7.5e3);
        assert!((1.5e-20..3.0e-20).contains(&d2), "cid D2+ = {d2:e}");
        // D₃⁺ at 30 keV (5 keV/amu → E_cm 6 keV): ≈1.9e-20 m².
        let d3 = cid_d3plus_m2(5.0e3);
        assert!((1.2e-20..2.6e-20).contains(&d3), "cid D3+ = {d3:e}");
        assert_eq!(cid_d2plus_m2(0.5), 0.0);
    }
}
