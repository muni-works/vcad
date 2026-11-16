//! Photon shielding materials: element mass fractions + density.
//!
//! Mixture rule: μ/ρ = Σ wᵢ (μ/ρ)ᵢ (exact for photons — no chemical
//! binding effects above a few keV). Each material carries a relative
//! μ uncertainty used in the dose band: 5% for well-defined
//! compositions (NIST table uncertainty + density tolerance), wider
//! where the composition is only nominal (glasses vary by vendor; lead
//! glass PbO content varies by grade and dominates its attenuation).
//!
//! Vocabulary ([`by_name`]): `air`, `hdpe`, `paraffin`,
//! `borated-hdpe-5`, `water`, `ss304`, `aluminum`, `lead`,
//! `borosilicate`, `lead-glass`. Unknown names fail closed.

use crate::xsdata::{self, Element, OutOfRange};

/// A photon material.
#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    /// Library key.
    pub name: String,
    /// Density, g/cm³.
    pub density_g_cc: f64,
    /// Element symbol → mass fraction (sums to 1).
    pub composition: Vec<(&'static str, f64)>,
    /// Relative 1σ-ish uncertainty on μ (data + composition + density),
    /// carried into dose bands.
    pub mu_rel_uncertainty: f64,
}

/// Linear coefficients at one energy, 1/cm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Linear {
    /// Total μ (NIST, coherent included), 1/cm.
    pub total: f64,
    /// Incoherent part: Klein–Nishina × electron density, ≤ total, 1/cm.
    pub incoherent: f64,
    /// μ_en, 1/cm.
    pub energy_absorption: f64,
}

impl Material {
    fn elements(&self) -> impl Iterator<Item = (Element, f64)> + '_ {
        self.composition.iter().map(|(s, w)| {
            (
                xsdata::element(s).expect("library compositions use bundled elements"),
                *w,
            )
        })
    }

    /// Mass coefficients `(μ/ρ, μ_en/ρ)`, cm²/g.
    pub fn mass_coefficients(&self, e_kev: f64) -> Result<(f64, f64), OutOfRange> {
        let mut m = 0.0;
        let mut n = 0.0;
        for (el, w) in self.elements() {
            let (a, b) = el.coefficients(e_kev)?;
            m += w * a;
            n += w * b;
        }
        Ok((m, n))
    }

    /// Electrons per gram of mixture.
    pub fn electrons_per_gram(&self) -> f64 {
        self.elements().map(|(el, w)| w * el.electrons_per_gram()).sum()
    }

    /// Linear coefficients at `e_kev`, 1/cm.
    pub fn linear(&self, e_kev: f64) -> Result<Linear, OutOfRange> {
        let (m, n) = self.mass_coefficients(e_kev)?;
        let total = m * self.density_g_cc;
        let kn = xsdata::klein_nishina_sigma_cm2(e_kev) * self.electrons_per_gram() * self.density_g_cc;
        Ok(Linear {
            total,
            incoherent: kn.min(total),
            energy_absorption: n * self.density_g_cc,
        })
    }

    /// Electron-weighted mean atomic number Σ wᵢ Zᵢ²/Aᵢ / Σ wᵢ Zᵢ/Aᵢ — the
    /// Z that enters thick-target bremsstrahlung yield (radiative stopping
    /// ∝ Z² per atom, collision stopping ∝ Z).
    pub fn z_eff_bremsstrahlung(&self) -> f64 {
        let (mut num, mut den) = (0.0, 0.0);
        for (el, w) in self.elements() {
            let z = el.z as f64;
            num += w * z * z / el.a;
            den += w * z / el.a;
        }
        num / den
    }
}

fn mat(name: &str, density: f64, comp: Vec<(&'static str, f64)>, unc: f64) -> Material {
    let total: f64 = comp.iter().map(|(_, w)| w).sum();
    assert!((total - 1.0).abs() < 1e-6, "{name}: mass fractions sum to {total}");
    Material {
        name: name.to_string(),
        density_g_cc: density,
        composition: comp,
        mu_rel_uncertainty: unc,
    }
}

/// Convert oxide weight fractions into element mass fractions.
fn from_oxides(oxides: &[(&str, f64)]) -> Vec<(&'static str, f64)> {
    // (oxide, cation symbol, cations per formula, oxygens per formula)
    const TABLE: [(&str, &str, f64, f64); 6] = [
        ("SiO2", "Si", 1.0, 2.0),
        ("B2O3", "B", 2.0, 3.0),
        ("Na2O", "Na", 2.0, 1.0),
        ("K2O", "K", 2.0, 1.0),
        ("Al2O3", "Al", 2.0, 3.0),
        ("PbO", "Pb", 1.0, 1.0),
    ];
    let mut out: Vec<(&'static str, f64)> = Vec::new();
    let o = xsdata::O.a;
    let mut oxygen = 0.0;
    for (ox, w) in oxides {
        let (_, cat, nc, no) = TABLE
            .iter()
            .find(|t| t.0 == *ox)
            .copied()
            .expect("known oxide");
        let a_cat = xsdata::element(cat).unwrap().a;
        let m = nc * a_cat + no * o;
        out.push((cat, w * nc * a_cat / m));
        oxygen += w * no * o / m;
    }
    out.push(("O", oxygen));
    out
}

/// Dry air, ρ = 1.205 mg/cm³ (20 °C, 1 atm). Argon (1.3 wt%) is folded
/// into nitrogen, which slightly **under**-attenuates below ~30 keV
/// (conservative for dose).
pub fn air() -> Material {
    mat("air", 1.205e-3, vec![("N", 0.768), ("O", 0.232)], 0.05)
}

/// HDPE (CH₂)ₙ, ρ = 0.95 g/cm³.
pub fn hdpe() -> Material {
    mat("hdpe", 0.95, vec![("H", 0.1437), ("C", 0.8563)], 0.05)
}

/// Paraffin wax (≈C₂₅H₅₂), ρ = 0.90 g/cm³.
pub fn paraffin() -> Material {
    mat("paraffin", 0.90, vec![("H", 0.1486), ("C", 0.8514)], 0.05)
}

/// 5 wt% borated polyethylene, ρ = 1.00 g/cm³ (same composition as the
/// neutronics library's `borated-HDPE-5%`).
pub fn borated_hdpe_5() -> Material {
    mat(
        "borated-hdpe-5",
        1.00,
        vec![("H", 0.95 * 0.1437), ("C", 0.95 * 0.8563), ("B", 0.05)],
        0.05,
    )
}

/// Light water, ρ = 0.998 g/cm³.
pub fn water() -> Material {
    mat("water", 0.998, vec![("H", 0.1119), ("O", 0.8881)], 0.05)
}

/// AISI 304 stainless steel, ρ = 8.00 g/cm³; nominal Fe 69.5 / Cr 19 /
/// Ni 9.5 / Mn 2 wt% (C, Si, P, S traces folded into Fe).
pub fn ss304() -> Material {
    mat(
        "ss304",
        8.00,
        vec![("Fe", 0.695), ("Cr", 0.19), ("Ni", 0.095), ("Mn", 0.02)],
        0.05,
    )
}

/// Aluminum, ρ = 2.699 g/cm³.
pub fn aluminum() -> Material {
    mat("aluminum", 2.699, vec![("Al", 1.0)], 0.05)
}

/// Lead, ρ = 11.35 g/cm³.
pub fn lead() -> Material {
    mat("lead", 11.35, vec![("Pb", 1.0)], 0.05)
}

/// Borosilicate glass (Corning 7740 / Schott Borofloat-33 class — also a
/// fair stand-in for Kodial/7056 CF viewport glass), ρ = 2.23 g/cm³;
/// SiO₂ 81 / B₂O₃ 13 / Na₂O 4 / Al₂O₃ 2 wt%. 8% μ uncertainty: viewport
/// glasses differ by vendor (K₂O, Li₂O, BaO in some grades).
pub fn borosilicate() -> Material {
    mat(
        "borosilicate",
        2.23,
        from_oxides(&[("SiO2", 0.81), ("B2O3", 0.13), ("Na2O", 0.04), ("Al2O3", 0.02)]),
        0.08,
    )
}

/// Radiation-shielding lead glass, **nominal** grade: PbO 65 / SiO₂ 30 /
/// K₂O 2.5 / Na₂O 2.5 wt%, ρ = 4.8 g/cm³. Real grades span ~55–75 wt%
/// PbO (ρ 4.3–5.2), so this carries a 15% μ uncertainty — use the
/// vendor's lead-equivalence figure to check
/// ([`crate::transport::lead_equivalent_mm`]).
pub fn lead_glass() -> Material {
    mat(
        "lead-glass",
        4.8,
        from_oxides(&[("PbO", 0.65), ("SiO2", 0.30), ("K2O", 0.025), ("Na2O", 0.025)]),
        0.15,
    )
}

/// Library lookup by name (fail-closed: `None`).
pub fn by_name(name: &str) -> Option<Material> {
    match name {
        "air" => Some(air()),
        "hdpe" | "HDPE" => Some(hdpe()),
        "paraffin" => Some(paraffin()),
        "borated-hdpe-5" | "borated-HDPE-5%" => Some(borated_hdpe_5()),
        "water" => Some(water()),
        "ss304" | "stainless-304" => Some(ss304()),
        "aluminum" | "aluminium" => Some(aluminum()),
        "lead" => Some(lead()),
        "borosilicate" | "glass" => Some(borosilicate()),
        "lead-glass" => Some(lead_glass()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_compositions_sum_and_resolve() {
        for n in [
            "air", "hdpe", "paraffin", "borated-hdpe-5", "water", "ss304", "aluminum", "lead",
            "borosilicate", "lead-glass",
        ] {
            let m = by_name(n).unwrap();
            let s: f64 = m.composition.iter().map(|c| c.1).sum();
            assert!((s - 1.0).abs() < 1e-6, "{n}");
            assert!(m.linear(30.0).unwrap().total > 0.0);
        }
        assert!(by_name("unobtainium").is_none());
    }

    #[test]
    fn water_matches_nist_compound_table() {
        // NIST SRD 126 water (liquid): μ/ρ = 0.3756 at 30 keV, 0.1707 at
        // 100 keV, 0.07072 at 1 MeV (cm²/g).
        let w = water();
        for (e, nist) in [(30.0, 0.3756), (100.0, 0.1707), (1000.0, 0.07072)] {
            let (m, _) = w.mass_coefficients(e).unwrap();
            assert!((m / nist - 1.0).abs() < 0.01, "water {e} keV: {m} vs {nist}");
        }
    }

    #[test]
    fn oxide_conversion_conserves_mass() {
        let g = borosilicate();
        let si = g.composition.iter().find(|c| c.0 == "Si").unwrap().1;
        // SiO₂ is 46.74 wt% Si.
        assert!((si - 0.81 * 28.086 / (28.086 + 2.0 * 15.999)).abs() < 1e-9);
        let pb = lead_glass().composition.iter().find(|c| c.0 == "Pb").unwrap().1;
        assert!((pb - 0.65 * 207.2 / (207.2 + 15.999)).abs() < 1e-9);
    }

    #[test]
    fn z_eff_ordering() {
        assert!(ss304().z_eff_bremsstrahlung() > 24.0 && ss304().z_eff_bremsstrahlung() < 27.0);
        assert!(borosilicate().z_eff_bremsstrahlung() < 12.0);
        assert!((lead().z_eff_bremsstrahlung() - 82.0).abs() < 1e-9);
    }
}
