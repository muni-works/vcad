//! Photon source terms: thick-target bremsstrahlung and discrete lines.
//!
//! **Bremsstrahlung (Kramers 1923, thick target).** An electron of
//! kinetic energy T₀ = e·V stopping in a thick target of atomic number Z
//! radiates an energy spectrum dI/dE = C·(T₀ − E), 0 < E < T₀ — linear,
//! vanishing at the Duane–Hunt endpoint. The normalization comes from
//! the measured radiative yield η = k·Z·V (V in volts) with
//! k ≈ 1.1×10⁻⁹ V⁻¹ (the classic x-ray-tube efficiency rule, Compton &
//! Allison; published values span ≈0.9–1.4×10⁻⁹), so ∫ dI/dE dE = η·T₀
//! gives C = 2η/T₀. The photon **number** spectrum per electron is then
//!
//! ```text
//!     dN/dE = (2η/T₀) · (T₀ − E)/E      [photons / keV / electron]
//! ```
//!
//! emitted isotropically over 4π (thick-target emission at tens of keV is
//! close to isotropic because the electrons diffuse in the target before
//! they radiate; the residual anisotropy is inside the source band).
//!
//! **Validity band**, enforced: 10 kV ≤ V ≤ 150 kV (below 10 kV nothing
//! relevant to H*(10) is produced; above ~150 kV relativistic forward
//! peaking and the Kramers shape both degrade). Systematic band on the
//! yield: ×/÷ [`KRAMERS_BAND`] (covers the k spread and Kramers'
//! spectral-shape error over 10–150 keV, ~±20–30% against measured
//! thick-target spectra). Characteristic lines (Fe/Cr/Ni K at 5.4–8.3
//! keV, O/Si K below 2 keV) sit below the 10 keV transport floor and are
//! dropped; Pb L lines (10.5–15 keV) only matter for a lead *target*,
//! which no library chamber uses.
//!
//! Spectra are represented as energy bins with exact integrated rates
//! (the Kramers integral is closed-form per bin); within a bin the
//! density is taken flat. A bin with `lo == hi` is a discrete line.

use serde::{Deserialize, Serialize};

use crate::materials::Material;

/// Kramers/Compton–Allison thick-target yield constant, 1/V.
pub const KRAMERS_K_PER_V: f64 = 1.1e-9;
/// Multiplicative systematic band on the bremsstrahlung yield.
pub const KRAMERS_BAND: f64 = 1.5;
/// Validity window for tube voltage, kV.
pub const BREMS_KV_RANGE: (f64, f64) = (10.0, 150.0);
/// Elementary charge, C.
pub const ELEMENTARY_CHARGE: f64 = 1.602_176_634e-19;

/// One spectrum bin (or a line when `lo_kev == hi_kev`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bin {
    /// Lower edge, keV.
    pub lo_kev: f64,
    /// Upper edge, keV.
    pub hi_kev: f64,
    /// Emission rate into 4π, photons/s.
    pub rate_per_s: f64,
}

impl Bin {
    /// True for a discrete line.
    pub fn is_line(&self) -> bool {
        self.hi_kev == self.lo_kev
    }
}

/// An isotropic point-source spectrum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spectrum {
    /// Bins, ascending in energy.
    pub bins: Vec<Bin>,
}

impl Spectrum {
    /// A single monoenergetic line.
    pub fn line(e_kev: f64, rate_per_s: f64) -> Spectrum {
        Spectrum {
            bins: vec![Bin {
                lo_kev: e_kev,
                hi_kev: e_kev,
                rate_per_s,
            }],
        }
    }

    /// Total emission rate, photons/s.
    pub fn total_rate(&self) -> f64 {
        self.bins.iter().map(|b| b.rate_per_s).sum()
    }

    /// Total emitted power, keV/s (bin midpoints).
    pub fn total_power_kev_per_s(&self) -> f64 {
        self.bins
            .iter()
            .map(|b| b.rate_per_s * 0.5 * (b.lo_kev + b.hi_kev))
            .sum()
    }

    /// Highest photon energy, keV.
    pub fn max_kev(&self) -> f64 {
        self.bins.iter().map(|b| b.hi_kev).fold(0.0, f64::max)
    }

    /// Lowest photon energy, keV.
    pub fn min_kev(&self) -> f64 {
        self.bins
            .iter()
            .map(|b| b.lo_kev)
            .fold(f64::INFINITY, f64::min)
    }

    /// Scale every rate by `k`.
    pub fn scaled(&self, k: f64) -> Spectrum {
        Spectrum {
            bins: self
                .bins
                .iter()
                .map(|b| Bin {
                    rate_per_s: b.rate_per_s * k,
                    ..*b
                })
                .collect(),
        }
    }
}

/// Source-construction failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum SourceError {
    /// Voltage outside [`BREMS_KV_RANGE`].
    VoltageOutOfBand(f64),
    /// Current must be positive and finite.
    BadCurrent(f64),
    /// Electron fraction must lie in (0, 1].
    BadElectronFraction(f64),
    /// Lower spectrum cut must lie inside (5 keV, T₀).
    BadCut(f64),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::VoltageOutOfBand(v) => write!(
                f,
                "{v} kV outside the Kramers validity band {:?} kV",
                BREMS_KV_RANGE
            ),
            SourceError::BadCurrent(i) => write!(f, "electron current {i} mA must be > 0"),
            SourceError::BadElectronFraction(x) => {
                write!(f, "electron fraction {x} must lie in (0, 1]")
            }
            SourceError::BadCut(e) => write!(f, "spectrum cut {e} keV out of range"),
        }
    }
}

impl std::error::Error for SourceError {}

/// Thick-target bremsstrahlung from electrons of energy e·V striking a
/// target material.
#[derive(Debug, Clone, PartialEq)]
pub struct ThickTarget {
    /// Accelerating potential magnitude, kV (T₀ = this many keV).
    pub voltage_kv: f64,
    /// Electron current reaching the target at full energy, mA.
    pub electron_current_ma: f64,
    /// Target material (Z_eff from [`Material::z_eff_bremsstrahlung`]).
    pub target: Material,
}

impl ThickTarget {
    /// Build, validating the band.
    pub fn new(
        voltage_kv: f64,
        electron_current_ma: f64,
        target: Material,
    ) -> Result<Self, SourceError> {
        if !(BREMS_KV_RANGE.0..=BREMS_KV_RANGE.1).contains(&voltage_kv) {
            return Err(SourceError::VoltageOutOfBand(voltage_kv));
        }
        if !(electron_current_ma.is_finite() && electron_current_ma > 0.0) {
            return Err(SourceError::BadCurrent(electron_current_ma));
        }
        Ok(ThickTarget {
            voltage_kv,
            electron_current_ma,
            target,
        })
    }

    /// Radiative yield η = k·Z·V (fraction of beam power radiated).
    pub fn radiative_yield(&self) -> f64 {
        KRAMERS_K_PER_V * self.target.z_eff_bremsstrahlung() * self.voltage_kv * 1.0e3
    }

    /// Electrons per second.
    pub fn electrons_per_s(&self) -> f64 {
        self.electron_current_ma * 1.0e-3 / ELEMENTARY_CHARGE
    }

    /// Photon number per electron in [e1, e2] keV (closed form).
    pub fn photons_per_electron(&self, e1_kev: f64, e2_kev: f64) -> f64 {
        let t0 = self.voltage_kv;
        let (a, b) = (e1_kev.max(1e-9), e2_kev.min(t0));
        if b <= a {
            return 0.0;
        }
        2.0 * self.radiative_yield() / t0 * (t0 * (b / a).ln() - (b - a))
    }

    /// Binned 4π spectrum from `e_min_kev` to the endpoint, `n_bins`
    /// equal-width bins.
    pub fn spectrum(&self, e_min_kev: f64, n_bins: usize) -> Result<Spectrum, SourceError> {
        let t0 = self.voltage_kv;
        if !(e_min_kev > 5.0 && e_min_kev < t0) {
            return Err(SourceError::BadCut(e_min_kev));
        }
        let n_e = self.electrons_per_s();
        let w = (t0 - e_min_kev) / n_bins as f64;
        let bins = (0..n_bins)
            .map(|i| {
                let lo = e_min_kev + i as f64 * w;
                let hi = lo + w;
                Bin {
                    lo_kev: lo,
                    hi_kev: hi,
                    rate_per_s: n_e * self.photons_per_electron(lo, hi),
                }
            })
            .collect();
        Ok(Spectrum { bins })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials;

    #[test]
    fn energy_integral_is_eta_t0() {
        let t = ThickTarget::new(40.0, 10.0, materials::ss304()).unwrap();
        // Integrate E·dN/dE numerically from ~0 to T₀: must equal η·T₀.
        let n = 200_000;
        let t0 = 40.0;
        let mut s = 0.0;
        for i in 0..n {
            let a = t0 * i as f64 / n as f64;
            let b = t0 * (i + 1) as f64 / n as f64;
            s += 0.5 * (a + b) * t.photons_per_electron(a.max(1e-12), b);
        }
        let eta = t.radiative_yield();
        assert!(
            (s / (eta * t0) - 1.0).abs() < 1e-3,
            "∫E dN = {s}, ηT₀ = {}",
            eta * t0
        );
        // Steel at 40 kV: η ≈ 1.1e-9 · 25.5 · 4e4 ≈ 1.1e-3.
        assert!(eta > 1.0e-3 && eta < 1.3e-3, "η = {eta}");
    }

    #[test]
    fn bins_sum_to_closed_form() {
        let t = ThickTarget::new(30.0, 1.0, materials::borosilicate()).unwrap();
        let s = t.spectrum(10.0, 64).unwrap();
        let direct = t.electrons_per_s() * t.photons_per_electron(10.0, 30.0);
        assert!((s.total_rate() / direct - 1.0).abs() < 1e-12);
        assert!((s.max_kev() - 30.0).abs() < 1e-9);
        // Number spectrum falls toward the endpoint.
        assert!(s.bins[0].rate_per_s > s.bins[63].rate_per_s * 10.0);
    }

    #[test]
    fn band_enforced() {
        assert!(ThickTarget::new(5.0, 1.0, materials::ss304()).is_err());
        assert!(ThickTarget::new(200.0, 1.0, materials::ss304()).is_err());
        assert!(ThickTarget::new(40.0, 0.0, materials::ss304()).is_err());
        let t = ThickTarget::new(40.0, 1.0, materials::ss304()).unwrap();
        assert!(t.spectrum(45.0, 10).is_err());
    }
}
