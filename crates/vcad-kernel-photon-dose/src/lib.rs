#![warn(missing_docs)]

//! Photon (x-ray and gamma) shielding dose, M0.
//!
//! The companion to `vcad-kernel-neutronics`: that crate answers "what
//! neutron dose reaches the operator?"; this one answers the other half
//! of a radiation-producing bench's safety case — **bremsstrahlung
//! x-rays** from electrons striking a chamber wall or window, and the
//! **capture gammas** a hydrogenous neutron shield makes (2.22 MeV from
//! H(n,γ)) — so neutron + photon dose can be summed with honest bands.
//! First customer: the open-fusor video (`docs/videos/open-fusor.md`),
//! where x-rays through the glass viewport are the dominant hazard.
//!
//! Pipeline:
//!
//! 1. [`xsdata`] — NIST SRD 126 elemental μ/ρ and μ_en/ρ (5 keV – 3 MeV,
//!    edges included, transcribed verbatim) + free-electron
//!    Klein–Nishina.
//! 2. [`materials`] — steel, glass, lead glass, lead, aluminum, HDPE,
//!    borated poly, water, air, by mass fraction, each with a μ
//!    uncertainty.
//! 3. [`source`] — Kramers thick-target bremsstrahlung (yield η = kZV,
//!    10–150 kV validity band, enforced) and discrete lines.
//! 4. [`transport`] — layered-slab photon transport: exact uncollided
//!    term + next-event Monte Carlo for scatter (buildup computed, not
//!    tabulated), batch statistics.
//! 5. [`dose`] — ICRP-74 photon H*(10) and air-kerma coefficients.
//! 6. [`spec`] — serde schemas: a generic slab path and the IEC-chamber
//!    model (wall path + viewport path); [`capture`] — capture-gamma
//!    source from a neutronics run.
//! 7. [`report`] / [`receipt`] — every dose is mean ± MC error **and**
//!    a named systematic band; `vcad.photon-claims/1` claims, neutron +
//!    photon totals, survey comparison, unified-receipt translation
//!    (always Predicted → Provisional, never Pass).
//!
//! Honesty bounds (M0), carried as caveats on every claim set: H*(10)
//! is deep dose (shallow-dose proxy via air kerma), infinite slabs at
//! normal incidence, no fluorescence/annihilation photons, no room
//! return, Kramers spectral shape. The answer this crate gives is a
//! **design estimate with a band**; a survey meter replaces it.
//!
//! Units: thicknesses and distances in mm (vcad convention), energies
//! in keV, dose rates in µSv/h (H*(10)) and µGy/h (air kerma).

pub mod capture;
pub mod dose;
pub mod materials;
pub mod receipt;
pub mod report;
pub mod source;
pub mod spec;
pub mod transport;
pub mod xsdata;
