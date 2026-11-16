//! Photon fluence → ambient dose equivalent H*(10) and air kerma.
//!
//! Conversion coefficients from ICRP Publication 74 (1996), Table A.1
//! (air kerma per fluence, K_a/Φ) and Table A.21 (H*(10)/Φ), 10 keV –
//! 10 MeV, log-log interpolated. The table ends at 10 keV; below it
//! H*(10) is effectively zero (10 mm of tissue stops those photons) and
//! both functions return **0** — which is why the bremsstrahlung source
//! fails closed when the shield passes a non-negligible part of its
//! spectrum below 10 keV ([`crate::source`]).
//!
//! H*(10) is the **deep-dose** operational quantity (what a calibrated
//! survey meter reads). For soft x-rays through a thin window the
//! shallow dose to skin and eye lens, H′(0.07), can exceed it several-
//! fold; ICRP 74 gives H′(0.07)/K_a ≈ 0.95–1.6 over 10–100 keV, so the
//! air-kerma claim this crate emits alongside H*(10) bounds the shallow
//! dose to within that factor. Both are reported on every claim set.

/// ICRP-74 photon coefficients: (E MeV, H*(10)/Φ pSv·cm², K_a/Φ pGy·cm²).
pub const ICRP74_PHOTON: [(f64, f64, f64); 25] = [
    (0.010, 0.061, 7.43),
    (0.015, 0.83, 3.12),
    (0.020, 1.05, 1.68),
    (0.030, 0.81, 0.721),
    (0.040, 0.64, 0.429),
    (0.050, 0.55, 0.323),
    (0.060, 0.51, 0.289),
    (0.080, 0.53, 0.307),
    (0.100, 0.61, 0.371),
    (0.150, 0.89, 0.599),
    (0.200, 1.20, 0.856),
    (0.300, 1.80, 1.38),
    (0.400, 2.38, 1.89),
    (0.500, 2.93, 2.38),
    (0.600, 3.44, 2.84),
    (0.800, 4.38, 3.69),
    (1.000, 5.20, 4.47),
    (1.500, 6.90, 6.14),
    (2.000, 8.60, 7.55),
    (3.000, 11.1, 9.96),
    (4.000, 13.4, 12.1),
    (5.000, 15.5, 14.1),
    (6.000, 17.6, 16.1),
    (8.000, 21.6, 20.1),
    (10.000, 25.6, 24.0),
];

/// Lowest tabulated energy, keV (below → 0).
pub const DOSE_TABLE_MIN_KEV: f64 = 10.0;
/// Highest tabulated energy, keV (above → error at the call sites that
/// care; the interpolators clamp).
pub const DOSE_TABLE_MAX_KEV: f64 = 10_000.0;

fn interp(e_kev: f64, col: usize) -> f64 {
    let e = e_kev * 1.0e-3;
    let t = &ICRP74_PHOTON;
    if e < t[0].0 {
        return 0.0;
    }
    let get = |r: &(f64, f64, f64)| if col == 1 { r.1 } else { r.2 };
    if e >= t[t.len() - 1].0 {
        return get(&t[t.len() - 1]);
    }
    let i = t.partition_point(|r| r.0 <= e).max(1);
    let (a, b) = (&t[i - 1], &t[i]);
    let s = (e / a.0).ln() / (b.0 / a.0).ln();
    get(a) * (get(b) / get(a)).powf(s)
}

/// H*(10) per unit fluence at `e_kev`, pSv·cm² (0 below 10 keV).
pub fn h10_psv_cm2(e_kev: f64) -> f64 {
    interp(e_kev, 1)
}

/// Air kerma per unit fluence at `e_kev`, pGy·cm² (0 below 10 keV).
pub fn kerma_pgy_cm2(e_kev: f64) -> f64 {
    interp(e_kev, 2)
}

/// pSv/s → µSv/h.
pub const PSV_PER_S_TO_USV_PER_H: f64 = 3600.0 * 1.0e-6;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials;

    #[test]
    fn anchors() {
        let h662 = h10_psv_cm2(662.0);
        assert!(h662 > 3.44 && h662 < 4.38, "{h662}");
        assert!((h10_psv_cm2(1000.0) - 5.20).abs() < 1e-12);
        assert_eq!(h10_psv_cm2(9.0), 0.0);
        // The H*(10) hump near 20 keV and dip near 60 keV.
        assert!(h10_psv_cm2(20.0) > h10_psv_cm2(60.0));
    }

    #[test]
    fn kerma_table_agrees_with_e_times_mu_en_air() {
        // K/Φ = E·(μ_en/ρ)_air — an independent check that the ICRP
        // column and the NIST data in this crate are mutually
        // consistent. Above 30 keV the argon fold-in is invisible.
        let air = materials::air();
        for e in [30.0, 60.0, 100.0, 300.0, 662.0, 1000.0, 2000.0, 3000.0] {
            let (_, men) = air.mass_coefficients(e).unwrap();
            // MeV/g → pGy: × 1.602e-13 J/MeV × 1e3 g/kg × 1e12 pGy/Gy.
            let k = e * 1.0e-3 * men * 1.602_176_634e-13 * 1.0e3 * 1.0e12;
            let tab = kerma_pgy_cm2(e);
            assert!((k / tab - 1.0).abs() < 0.06, "{e} keV: E·μen {k} vs ICRP {tab}");
        }
    }
}
