//! Layered-slab photon transport: exact uncollided term + Monte Carlo
//! scattered term, scored at point detectors on the source axis.
//!
//! Geometry: an isotropic point source sits on the inner face (z = 0) of
//! a stack of infinite slabs; detectors sit on the z axis beyond the
//! stack, separated from it by an air gap of at least [`MIN_GAP_MM`].
//! Photons heading into z < 0 (back into the chamber vacuum) are lost.
//!
//! - **Uncollided** flux is computed deterministically, per spectrum bin
//!   with 4-point Gauss–Legendre in energy:
//!   φ₀(E) = S(E)·e^{−τ(E)}/(4πR²), τ = Σ μₖtₖ + μ_air·gap.
//! - **Scattered** flux is Monte Carlo with a next-event (point-detector)
//!   estimator: at every Compton collision the history scores
//!   w·p(Ω→det)·e^{−τ(E′)}/R² with p the Klein–Nishina angular density
//!   and E′ the Compton energy toward the detector. Implicit capture
//!   (weight × μ_inc/μ), Russian roulette, KN direction sampling.
//!   Collisions only happen inside the slabs, so R ≥ gap and the
//!   estimator variance is bounded — the reason the air gap is
//!   attenuate-only and a minimum gap is enforced.
//! - Source energies are importance-sampled toward penetrating bins
//!   (defensive mixture: ½ analog + ½ ∝ rate × uncollided transmission),
//!   with exact weight correction — unbiased for any spectrum.
//!
//! Physics bounds (stated on every claim): infinite lateral slabs
//! (over-counts scatter for a small window — conservative), coherent
//! scatter and photo/pair absorption are removal (no fluorescence, no
//! annihilation photons), free-electron Klein–Nishina, air gap
//! attenuates but does not scatter (no skyshine / room return),
//! scattered photons below 10 keV are terminated (H*(10) ≈ 0 there).
//!
//! Every reported dose is a [`Estimate`]: uncollided (exact) plus the
//! batch-statistics scattered term, whose standard error is the
//! reported uncertainty.

use vcad_kernel_neutronics::rng::Rng;
use vcad_kernel_neutronics::tally::Estimate;

use crate::dose::{h10_psv_cm2, kerma_pgy_cm2, DOSE_TABLE_MIN_KEV, PSV_PER_S_TO_USV_PER_H};
use crate::materials::{self, Material};
use crate::source::Spectrum;
use crate::xsdata::{self, DATA_MAX_KEV};

/// Minimum air gap between the last slab and any detector, mm.
pub const MIN_GAP_MM: f64 = 10.0;
/// Scattered photons below this energy are terminated, keV.
pub const TRANSPORT_CUT_KEV: f64 = DOSE_TABLE_MIN_KEV;
/// A continuous spectrum whose lowest bin is transmitted above this
/// fraction is refused: the 10 keV floor would hide soft dose.
pub const SOFT_LEAK_LIMIT: f64 = 1.0e-3;
/// Collision cap per history (truncation is reported, and refused by
/// the receipt).
pub const MAX_COLLISIONS: u32 = 2000;

/// One slab.
#[derive(Debug, Clone, PartialEq)]
pub struct Slab {
    /// Material.
    pub material: Material,
    /// Thickness, mm.
    pub thickness_mm: f64,
}

impl Slab {
    /// Construct.
    pub fn new(material: Material, thickness_mm: f64) -> Self {
        Slab {
            material,
            thickness_mm,
        }
    }
}

/// A transport problem.
#[derive(Debug, Clone, PartialEq)]
pub struct PathConfig {
    /// Slabs from the source face outward.
    pub slabs: Vec<Slab>,
    /// Detector distances from the source point along the axis, mm.
    pub detector_distances_mm: Vec<f64>,
    /// Histories per batch.
    pub histories_per_batch: usize,
    /// Batches (≥ 2).
    pub batches: usize,
    /// RNG seed.
    pub seed: u64,
    /// Transport scattered photons (false = uncollided only, the
    /// narrow-beam validation mode).
    pub scatter: bool,
}

impl PathConfig {
    /// Defaults: 20 × 20 000 histories, seed 20261116, scatter on.
    pub fn new(slabs: Vec<Slab>, detector_distances_mm: Vec<f64>) -> Self {
        PathConfig {
            slabs,
            detector_distances_mm,
            histories_per_batch: 20_000,
            batches: 20,
            seed: 20261116,
            scatter: true,
        }
    }

    /// Total slab thickness, mm.
    pub fn stack_mm(&self) -> f64 {
        self.slabs.iter().map(|s| s.thickness_mm).sum()
    }
}

/// Transport failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum TransportError {
    /// A slab thickness was not positive and finite.
    BadThickness(usize),
    /// No detectors.
    NoDetectors,
    /// Detector too close to (or inside) the stack.
    DetectorTooClose {
        /// Offending distance, mm.
        distance_mm: f64,
    },
    /// Fewer than 2 batches or zero histories.
    BadStatistics,
    /// Spectrum empty, non-positive, or outside the data window.
    BadSpectrum(String),
    /// The shield transmits the soft end of a continuous spectrum.
    SoftLeak {
        /// Energy of the lowest bin, keV.
        e_kev: f64,
        /// Its uncollided transmission.
        transmission: f64,
    },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::BadThickness(i) => write!(f, "slab {i}: thickness must be > 0"),
            TransportError::NoDetectors => write!(f, "no detectors"),
            TransportError::DetectorTooClose { distance_mm } => write!(
                f,
                "detector at {distance_mm} mm is inside the stack or within the {MIN_GAP_MM} mm minimum gap"
            ),
            TransportError::BadStatistics => write!(f, "need ≥ 2 batches and ≥ 1 history"),
            TransportError::BadSpectrum(s) => write!(f, "bad spectrum: {s}"),
            TransportError::SoftLeak { e_kev, transmission } => write!(
                f,
                "shield transmits {transmission:.2e} at {e_kev} keV: the 10 keV dose-table floor would \
                 hide soft dose — add filtration or measure"
            ),
        }
    }
}

impl std::error::Error for TransportError {}

/// Per-detector results.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorResult {
    /// Distance from the source, mm.
    pub distance_mm: f64,
    /// Ambient dose equivalent rate H*(10), µSv/h.
    pub h10_usv_h: Estimate,
    /// Air kerma rate, µGy/h.
    pub kerma_ugy_h: Estimate,
    /// Photon fluence rate, 1/cm²/s.
    pub fluence_cm2_s: Estimate,
    /// Uncollided H*(10), µSv/h (exact).
    pub uncollided_h10_usv_h: f64,
    /// Dose buildup factor, total / uncollided H*(10).
    pub buildup_h10: f64,
    /// Fraction of uncollided H*(10) from the top 10% of the spectrum's
    /// energy range (the Kramers-shape sensitivity indicator).
    pub tip_fraction: f64,
    /// H*(10)-weighted uncollided optical depth per slab (for the data
    /// band).
    pub tau_per_slab: Vec<f64>,
    /// H*(10)-weighted uncollided optical depth of the air gap.
    pub tau_air: f64,
}

/// Reproducibility provenance.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhotonProvenance {
    /// RNG seed.
    pub seed: u64,
    /// Histories per batch.
    pub histories_per_batch: usize,
    /// Batches.
    pub batches: usize,
    /// Interaction data.
    pub data: String,
    /// Dose conversion.
    pub dose_coefficients: String,
    /// Transport model.
    pub model: String,
}

/// A full run.
#[derive(Debug, Clone, PartialEq)]
pub struct PathResult {
    /// One per detector, in input order.
    pub detectors: Vec<DetectorResult>,
    /// Histories killed at the collision cap (must be 0 for claims).
    pub truncated_histories: u64,
    /// True when at least one batch scored scattered dose (always true
    /// when `scatter` is off — nothing was asked).
    pub scatter_scored: bool,
    /// Provenance.
    pub provenance: PhotonProvenance,
}

/// Uncollided transmission e^{−Σμt} through slabs at `e_kev`.
pub fn transmission(slabs: &[Slab], e_kev: f64) -> f64 {
    let tau: f64 = slabs
        .iter()
        .map(|s| s.material.linear(e_kev).map(|l| l.total).unwrap_or(f64::INFINITY) * s.thickness_mm * 0.1)
        .sum();
    (-tau).exp()
}

/// Narrow-beam (uncollided) air-kerma transmission of a spectrum.
pub fn kerma_transmission(slabs: &[Slab], spectrum: &Spectrum) -> f64 {
    let (mut num, mut den) = (0.0, 0.0);
    for_each_node(spectrum, |e, w| {
        let k = kerma_pgy_cm2(e) * w;
        den += k;
        num += k * transmission(slabs, e);
    });
    num / den
}

/// Narrow-beam air-kerma half-value layer of `material` for a spectrum
/// already filtered by `prefilter`, mm (bisection on 1/2 transmission).
pub fn hvl_mm(material: &Material, prefilter: &[Slab], spectrum: &Spectrum) -> f64 {
    let base = kerma_transmission(prefilter, spectrum);
    let t_of = |x: f64| {
        let mut s = prefilter.to_vec();
        s.push(Slab::new(material.clone(), x));
        kerma_transmission(&s, spectrum) / base
    };
    bisect(|x| t_of(x) - 0.5, 1e-6, 1e4)
}

/// Lead thickness (mm) giving the same narrow-beam air-kerma
/// transmission as `slab` for this spectrum — the "lead equivalence"
/// figure vendors quote for shielding glass.
pub fn lead_equivalent_mm(slab: &Slab, spectrum: &Spectrum) -> f64 {
    let target = kerma_transmission(std::slice::from_ref(slab), spectrum);
    let pb = materials::lead();
    bisect(
        |x| kerma_transmission(&[Slab::new(pb.clone(), x)], spectrum) - target,
        1e-7,
        1e3,
    )
}

fn bisect(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64) -> f64 {
    // f decreasing in x.
    for _ in 0..200 {
        let mid = (lo * hi).sqrt();
        if f(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo * hi).sqrt()
}

/// 4-point Gauss–Legendre nodes on [-1, 1].
const GL4: [(f64, f64); 4] = [
    (-0.861_136_311_594_052_6, 0.347_854_845_137_453_9),
    (-0.339_981_043_584_856_3, 0.652_145_154_862_546_1),
    (0.339_981_043_584_856_3, 0.652_145_154_862_546_1),
    (0.861_136_311_594_052_6, 0.347_854_845_137_453_9),
];

/// Visit (energy, photons/s) quadrature nodes of a spectrum.
fn for_each_node(spectrum: &Spectrum, mut f: impl FnMut(f64, f64)) {
    for b in &spectrum.bins {
        if b.is_line() {
            f(b.lo_kev, b.rate_per_s);
        } else {
            let (c, h) = (0.5 * (b.lo_kev + b.hi_kev), 0.5 * (b.hi_kev - b.lo_kev));
            for (x, w) in GL4 {
                f(c + h * x, b.rate_per_s * 0.5 * w);
            }
        }
    }
}

struct Geometry<'a> {
    slabs: &'a [Slab],
    bounds_cm: Vec<f64>,
    air: Material,
    end_cm: f64,
}

impl Geometry<'_> {
    fn mu(&self, k: usize, e: f64) -> (f64, f64) {
        let l = self.slabs[k].material.linear(e).expect("energy validated");
        (l.total, l.incoherent)
    }

    /// Optical depth from (z in slab k) to a detector at zd along a ray
    /// with axial direction cosine `dz` (> 0).
    fn tau_to(&self, k: usize, z: f64, zd: f64, dz: f64, e: f64) -> f64 {
        let mut tau = self.mu(k, e).0 * (self.bounds_cm[k + 1] - z);
        for j in k + 1..self.slabs.len() {
            tau += self.mu(j, e).0 * (self.bounds_cm[j + 1] - self.bounds_cm[j]);
        }
        tau += self.air.linear(e).expect("energy validated").total * (zd - self.end_cm);
        tau / dz
    }
}

/// Run the transport problem for an isotropic point source spectrum.
pub fn run(spectrum: &Spectrum, cfg: &PathConfig) -> Result<PathResult, TransportError> {
    // ---- validation (fail-closed) ----
    for (i, s) in cfg.slabs.iter().enumerate() {
        if !(s.thickness_mm.is_finite() && s.thickness_mm > 0.0) {
            return Err(TransportError::BadThickness(i));
        }
    }
    if cfg.detector_distances_mm.is_empty() {
        return Err(TransportError::NoDetectors);
    }
    let stack = cfg.stack_mm();
    for &d in &cfg.detector_distances_mm {
        if !(d.is_finite() && d >= stack + MIN_GAP_MM) {
            return Err(TransportError::DetectorTooClose { distance_mm: d });
        }
    }
    if cfg.batches < 2 || cfg.histories_per_batch == 0 {
        return Err(TransportError::BadStatistics);
    }
    if spectrum.bins.is_empty() || spectrum.total_rate() <= 0.0 {
        return Err(TransportError::BadSpectrum("empty or zero-rate".into()));
    }
    for b in &spectrum.bins {
        if !(b.rate_per_s >= 0.0 && b.lo_kev >= TRANSPORT_CUT_KEV && b.hi_kev <= DATA_MAX_KEV && b.hi_kev >= b.lo_kev) {
            return Err(TransportError::BadSpectrum(format!(
                "bin [{}, {}] keV outside [{TRANSPORT_CUT_KEV}, {DATA_MAX_KEV}] keV or negative",
                b.lo_kev, b.hi_kev
            )));
        }
    }
    let lowest = spectrum
        .bins
        .iter()
        .filter(|b| b.rate_per_s > 0.0)
        .min_by(|a, b| a.lo_kev.total_cmp(&b.lo_kev))
        .unwrap();
    if !lowest.is_line() {
        let t = transmission(&cfg.slabs, lowest.lo_kev);
        if t > SOFT_LEAK_LIMIT {
            return Err(TransportError::SoftLeak {
                e_kev: lowest.lo_kev,
                transmission: t,
            });
        }
    }

    let mut bounds_cm = vec![0.0];
    for s in &cfg.slabs {
        bounds_cm.push(bounds_cm.last().unwrap() + s.thickness_mm * 0.1);
    }
    let geo = Geometry {
        slabs: &cfg.slabs,
        end_cm: *bounds_cm.last().unwrap(),
        bounds_cm,
        air: materials::air(),
    };
    let zds: Vec<f64> = cfg.detector_distances_mm.iter().map(|d| d * 0.1).collect();
    let nd = zds.len();
    let ns = cfg.slabs.len();
    let e_hi = spectrum.max_kev();
    let e_lo = spectrum.min_kev();
    let tip_threshold = e_hi - 0.1 * (e_hi - e_lo).max(0.0);

    // ---- uncollided (exact) ----
    let mut u_h = vec![0.0; nd];
    let mut u_k = vec![0.0; nd];
    let mut u_f = vec![0.0; nd];
    let mut u_tip = vec![0.0; nd];
    let mut tau_w = vec![vec![0.0; ns]; nd];
    let mut tau_air = vec![0.0; nd];
    for_each_node(spectrum, |e, w| {
        let taus: Vec<f64> = (0..ns).map(|k| geo.mu(k, e).0 * (geo.bounds_cm[k + 1] - geo.bounds_cm[k])).collect();
        let mu_air = geo.air.linear(e).unwrap().total;
        let slab_tau: f64 = taus.iter().sum();
        for d in 0..nd {
            let ta = mu_air * (zds[d] - geo.end_cm);
            let phi = w * (-(slab_tau + ta)).exp() / (4.0 * std::f64::consts::PI * zds[d] * zds[d]);
            let h = phi * h10_psv_cm2(e);
            u_h[d] += h;
            u_k[d] += phi * kerma_pgy_cm2(e);
            u_f[d] += phi;
            if e >= tip_threshold {
                u_tip[d] += h;
            }
            for k in 0..ns {
                tau_w[d][k] += h * taus[k];
            }
            tau_air[d] += h * ta;
        }
    });

    // ---- scattered (Monte Carlo) ----
    let mut s_h = vec![vec![0.0; cfg.batches]; nd];
    let mut s_k = vec![vec![0.0; cfg.batches]; nd];
    let mut s_f = vec![vec![0.0; cfg.batches]; nd];
    let mut truncated = 0u64;
    let mut scored_any = !cfg.scatter || cfg.slabs.is_empty();
    if cfg.scatter && ns > 0 {
        // Importance pdf over bins.
        let rates: Vec<f64> = spectrum.bins.iter().map(|b| b.rate_per_s).collect();
        let trans: Vec<f64> = spectrum
            .bins
            .iter()
            .map(|b| transmission(&cfg.slabs, 0.5 * (b.lo_kev + b.hi_kev)))
            .collect();
        let r_sum: f64 = rates.iter().sum();
        let rt_sum: f64 = rates.iter().zip(&trans).map(|(r, t)| r * t).sum();
        let q: Vec<f64> = rates
            .iter()
            .zip(&trans)
            .map(|(r, t)| {
                if rt_sum > 0.0 {
                    0.5 * r / r_sum + 0.5 * r * t / rt_sum
                } else {
                    r / r_sum
                }
            })
            .collect();
        let mut cdf = Vec::with_capacity(q.len());
        let mut acc = 0.0;
        for x in &q {
            acc += x;
            cdf.push(acc);
        }
        let n = cfg.histories_per_batch as f64;
        for batch in 0..cfg.batches {
            let mut rng = Rng::stream(cfg.seed, batch as u64);
            for _ in 0..cfg.histories_per_batch {
                let u = rng.uniform() * acc;
                let bi = cdf.partition_point(|c| *c < u).min(q.len() - 1);
                let bin = spectrum.bins[bi];
                if q[bi] <= 0.0 {
                    continue;
                }
                let mut e = if bin.is_line() {
                    bin.lo_kev
                } else {
                    bin.lo_kev + rng.uniform() * (bin.hi_kev - bin.lo_kev)
                };
                // Weight: photons/s per history; ×½ for hemisphere-only emission.
                let w_start = rates[bi] / q[bi] / n * 0.5;
                let mut w = w_start;
                let mu0 = rng.uniform();
                let phi0 = 2.0 * std::f64::consts::PI * rng.uniform();
                let st0 = (1.0 - mu0 * mu0).max(0.0).sqrt();
                let mut dir = [st0 * phi0.cos(), st0 * phi0.sin(), mu0];
                let mut pos = [0.0f64, 0.0, 0.0];
                let mut k = 0usize;
                let mut collisions = 0u32;
                loop {
                    let (mt, mi) = geo.mu(k, e);
                    let s = -rng.uniform().ln() / mt;
                    let db = if dir[2] > 0.0 {
                        (geo.bounds_cm[k + 1] - pos[2]) / dir[2]
                    } else if dir[2] < 0.0 {
                        (geo.bounds_cm[k] - pos[2]) / dir[2]
                    } else {
                        f64::INFINITY
                    };
                    if s >= db {
                        for i in 0..3 {
                            pos[i] += db * dir[i];
                        }
                        if dir[2] > 0.0 {
                            pos[2] = geo.bounds_cm[k + 1];
                            k += 1;
                            if k >= ns {
                                break;
                            }
                        } else {
                            pos[2] = geo.bounds_cm[k];
                            if k == 0 {
                                break;
                            }
                            k -= 1;
                        }
                        continue;
                    }
                    for i in 0..3 {
                        pos[i] += s * dir[i];
                    }
                    collisions += 1;
                    if collisions > MAX_COLLISIONS {
                        truncated += 1;
                        break;
                    }
                    w *= mi / mt;
                    let sig = xsdata::klein_nishina_sigma_cm2(e);
                    for d in 0..nd {
                        let v = [-pos[0], -pos[1], zds[d] - pos[2]];
                        let r2 = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
                        let r = r2.sqrt();
                        let od = [v[0] / r, v[1] / r, v[2] / r];
                        let c = (dir[0] * od[0] + dir[1] * od[1] + dir[2] * od[2]).clamp(-1.0, 1.0);
                        let e2 = xsdata::compton_energy_kev(e, c);
                        if e2 < TRANSPORT_CUT_KEV {
                            continue;
                        }
                        let p = xsdata::klein_nishina_dsigma_domega(e, c) / sig;
                        let tau = geo.tau_to(k, pos[2], zds[d], od[2], e2);
                        let phi = w * p * (-tau).exp() / r2;
                        s_f[d][batch] += phi;
                        s_h[d][batch] += phi * h10_psv_cm2(e2);
                        s_k[d][batch] += phi * kerma_pgy_cm2(e2);
                    }
                    // Sample the scatter (KN rejection on μ).
                    let c = loop {
                        let m = rng.uniform_mu();
                        let ratio = xsdata::klein_nishina_dsigma_domega(e, m)
                            / xsdata::klein_nishina_dsigma_domega(e, 1.0);
                        if rng.uniform() <= ratio {
                            break m;
                        }
                    };
                    e = xsdata::compton_energy_kev(e, c);
                    if e < TRANSPORT_CUT_KEV {
                        break;
                    }
                    dir = rotate(dir, c, 2.0 * std::f64::consts::PI * rng.uniform());
                    // Russian roulette.
                    if w < 1.0e-3 * w_start {
                        if rng.uniform() < 0.1 {
                            w *= 10.0;
                        } else {
                            break;
                        }
                    }
                }
            }
            if (0..nd).any(|d| s_h[d][batch] > 0.0) {
                scored_any = true;
            }
        }
    }

    let to_usv = PSV_PER_S_TO_USV_PER_H;
    let to_ugy = PSV_PER_S_TO_USV_PER_H; // pGy/s → µGy/h, same factor
    let combine = |u: f64, s: &[f64], k: f64| -> Estimate {
        let est = Estimate::from_batches(s);
        let mean = u + est.mean;
        let sigma = if est.mean == 0.0 { 0.0 } else { est.abs_sigma() };
        Estimate {
            mean: mean * k,
            rse: if mean > 0.0 { sigma / mean } else { f64::INFINITY },
            batches: s.len(),
        }
    };
    let detectors = (0..nd)
        .map(|d| {
            let h = combine(u_h[d], &s_h[d], to_usv);
            DetectorResult {
                distance_mm: cfg.detector_distances_mm[d],
                kerma_ugy_h: combine(u_k[d], &s_k[d], to_ugy),
                fluence_cm2_s: combine(u_f[d], &s_f[d], 1.0),
                uncollided_h10_usv_h: u_h[d] * to_usv,
                buildup_h10: if u_h[d] > 0.0 { h.mean / (u_h[d] * to_usv) } else { f64::INFINITY },
                tip_fraction: if u_h[d] > 0.0 { u_tip[d] / u_h[d] } else { 0.0 },
                tau_per_slab: tau_w[d].iter().map(|t| if u_h[d] > 0.0 { t / u_h[d] } else { 0.0 }).collect(),
                tau_air: if u_h[d] > 0.0 { tau_air[d] / u_h[d] } else { 0.0 },
                h10_usv_h: h,
            }
        })
        .collect();
    Ok(PathResult {
        detectors,
        truncated_histories: truncated,
        scatter_scored: scored_any,
        provenance: PhotonProvenance {
            seed: cfg.seed,
            histories_per_batch: cfg.histories_per_batch,
            batches: cfg.batches,
            data: "NIST SRD 126 (Hubbell & Seltzer) mu/rho, mu_en/rho; free-electron Klein-Nishina".into(),
            dose_coefficients: "ICRP 74 photon H*(10)/phi and K_air/phi".into(),
            model: if cfg.scatter {
                "slab point-kernel: exact uncollided + next-event MC scatter (KN, implicit capture); \
                 coherent/photo/pair as removal; air gap attenuate-only"
                    .into()
            } else {
                "narrow-beam uncollided only (validation mode)".into()
            },
        },
    })
}

/// Rotate a unit direction by polar cosine `mu` and azimuth `phi`.
fn rotate(d: [f64; 3], mu: f64, phi: f64) -> [f64; 3] {
    let st = (1.0 - mu * mu).max(0.0).sqrt();
    let (sp, cp) = phi.sin_cos();
    if d[2].abs() > 0.999_99 {
        let sgn = d[2].signum();
        return [st * cp, st * sp, sgn * mu];
    }
    let tmp = (1.0 - d[2] * d[2]).sqrt();
    [
        mu * d[0] + st * (d[0] * d[2] * cp - d[1] * sp) / tmp,
        mu * d[1] + st * (d[1] * d[2] * cp + d[0] * sp) / tmp,
        mu * d[2] - st * cp * tmp,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_preserves_norm_and_angle() {
        let d = [0.3f64, -0.4, (1.0f64 - 0.25).sqrt()];
        let r = rotate(d, 0.2, 1.3);
        let n = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
        assert!((n - 1.0).abs() < 1e-12);
        let c = d[0] * r[0] + d[1] * r[1] + d[2] * r[2];
        assert!((c - 0.2).abs() < 1e-12);
    }

    #[test]
    fn validation_fails_closed() {
        let s = Spectrum::line(662.0, 1.0);
        let slabs = vec![Slab::new(materials::lead(), 10.0)];
        let mut c = PathConfig::new(slabs.clone(), vec![15.0]);
        assert!(matches!(run(&s, &c), Err(TransportError::DetectorTooClose { .. })));
        c.detector_distances_mm = vec![];
        assert!(matches!(run(&s, &c), Err(TransportError::NoDetectors)));
        c.detector_distances_mm = vec![100.0];
        c.batches = 1;
        assert!(matches!(run(&s, &c), Err(TransportError::BadStatistics)));
        let c = PathConfig::new(slabs, vec![100.0]);
        assert!(matches!(run(&Spectrum::line(5000.0, 1.0), &c), Err(TransportError::BadSpectrum(_))));
    }
}
