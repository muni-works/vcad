//! Emissive participating media — a glowing volume with no surface.
//!
//! A plasma, a flame or a gas discharge is not a surface that emits; it is a
//! volume that emits (and, weakly, absorbs) at every point along a ray. The
//! radiance a ray picks up crossing a homogeneous slab of emission coefficient
//! `ε` (radiance per millimetre) and absorption coefficient `κ` (per
//! millimetre) over a length `L` is the radiative-transfer solution
//!
//! ```text
//! L_out = (ε/κ)·(1 − e^{−κL}) + e^{−κL}·L_behind   →   ε·L   as κ → 0
//! ```
//!
//! # How it is traced
//!
//! The integrator is `kosm-render`'s, and it already composites one volumetric
//! field along **every** ray segment — the camera ray and every bounce, each
//! stopped at the first surface it meets: [`Scene::splats`], a cloud of
//! Gaussian splats, accumulated front to back as `C += T·α·c; T *= 1 − α`.
//! That is exactly the emission–absorption model above, discretised: a cloud
//! whose splats all carry the same colour `c` is a medium with source
//! function `S = c`, and its accumulated opacity is `1 − e^{−κL}`.
//!
//! So a medium becomes a cloud. [`MediumCloud::add`] fills the medium's solid
//! with a regular lattice of isotropic Gaussians (spacing `h`, standard
//! deviation `σ = SIGMA_OVER_H·h`), and picks each splat's opacity and colour
//! so the cloud's *thin-limit* gain matches `ε` and `κ`.
//!
//! ## The calibration (derived, not tuned)
//!
//! kosm reports a splat at its peak response along the ray, `α·e^{−d²/2σ²}`,
//! for closest-approach distance `d < 3σ`. A ray through a lattice of number
//! density `1/h³` meets, per millimetre of path, splats spread uniformly over
//! the plane normal to it, so the summed opacity per millimetre is
//!
//! ```text
//! K·α,   K = (1/h³)·∫∫_{r<3σ} e^{−r²/2σ²} dA = 2πσ²·(1 − e^{−4.5}) / h³
//! ```
//!
//! — the `1 − e^{−4.5}` (≈ 0.989) is kosm's hard 3σ cut-off. Hence
//! `κ = K·α` and `ε = K·α·c`, i.e. `α = κ/K`, `c = ε/κ`. An emission-only
//! medium (`κ = 0`) is the limit: a vanishing [`THIN_ALPHA`] and
//! `c = ε/(K·α)`, which leaves an absorption of `K·THIN_ALPHA` per mm — about
//! `4e-6`/mm at the default spacing, far below anything an image shows.
//!
//! The lattice sum is not perfectly uniform across the plane a ray crosses;
//! by Poisson summation its ripple is `~2·e^{−2π²σ²/h²}`, which at
//! `σ/h = 0.75` is `3e-5` — invisible. The integral is exact only for a
//! medium thick compared with `h`; the boundary is softened over about `σ`,
//! and a slab's apparent thickness is quantised to the lattice (the tests pin
//! both).
//!
//! ## What this does and does not model
//!
//! * Emission and absorption: yes, physically, along camera *and* bounce
//!   rays. A glowing volume therefore lights the surfaces around it — but only
//!   through BSDF-sampled bounces (kosm does not importance-sample a splat
//!   cloud), so that light is noisier than an area light's.
//! * Occlusion and depth: yes — the walk stops at the first surface, so a wall
//!   in front hides the glow and a wall behind does not.
//! * Scattering: no. A plasma is optically thin and its emission dominates;
//!   an in-scattering medium (fog, smoke lit from outside) is out of scope.
//!
//! [`Scene::splats`]: kosm_render::pathtrace::Scene::splats

use std::sync::Arc;

use kosm_render::{Aabb, Bvh as KBvh, Geometry, Point3, Ray, Splats, Vec3};

/// The hierarchy over a medium's splats — what `Scene::splats` holds.
pub type SplatBvh = KBvh<Splats>;

/// Gaussian standard deviation as a fraction of lattice spacing.
///
/// Large enough that the lattice sum is flat to `3e-5` (see the module
/// docs); small enough that each ray meets as few splats as possible —
/// kosm gathers and sorts every splat along a segment, so `σ/h` squared is
/// the cost.
pub const SIGMA_OVER_H: f64 = 0.75;

/// Per-splat opacity used for an emission-only medium.
pub const THIN_ALPHA: f64 = 1e-6;

/// Largest per-splat opacity the calibration trusts. Above it the
/// `κ = K·α` thin-limit relation is off by more than ~2.5 %, so a medium
/// asking for more absorption than this lattice can carry is clamped (and
/// reported through [`MediumCloud::clamped`]).
pub const MAX_ALPHA: f64 = 0.05;

/// The 3DGS degree-0 SH constant: kosm's splat colour is `0.5 + SH_C0·sh₀`.
const SH_C0: f64 = 0.282_094_79;

/// Default splat budget per medium.
pub const DEFAULT_BUDGET: usize = 250_000;

/// Fraction of the 3σ disc's Gaussian mass kosm counts.
fn cutoff_mass() -> f64 {
    1.0 - (-4.5f64).exp()
}

/// The lattice's thin-limit gain `K`: summed splat opacity per millimetre of
/// path, per unit per-splat opacity, for spacing `h` (mm) and the fixed
/// [`SIGMA_OVER_H`].
pub fn lattice_gain(h: f64) -> f64 {
    let sigma = SIGMA_OVER_H * h;
    2.0 * std::f64::consts::PI * sigma * sigma * cutoff_mass() / (h * h * h)
}

/// One emissive medium, in renderer units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmissiveMedium {
    /// Emission coefficient: radiance added per millimetre of path, linear RGB.
    pub emission: [f32; 3],
    /// Absorption coefficient, per millimetre. `0` is emission-only.
    pub absorption: f32,
    /// Depth (mm) over which density ramps from zero at the boundary to full
    /// inside — a soft edge. `0` is a hard-edged, uniform medium.
    pub falloff: f32,
}

impl EmissiveMedium {
    /// Derive a medium from an IR material, if it declares one.
    pub fn from_material_def(m: &vcad_ir::MaterialDef) -> Option<Self> {
        let v = m.volume_emission.as_ref()?;
        let e = v.radiance();
        Some(Self {
            emission: [e[0] as f32, e[1] as f32, e[2] as f32],
            absorption: v.absorption.unwrap_or(0.0).max(0.0) as f32,
            falloff: v.falloff.unwrap_or(0.0).max(0.0) as f32,
        })
    }

    /// The analytic radiance of a uniform slab of thickness `l` mm seen
    /// against black — the formula the cloud is calibrated to reproduce.
    pub fn slab_radiance(&self, l: f64) -> [f64; 3] {
        let k = self.absorption as f64;
        let f = if k > 0.0 {
            (1.0 - (-k * l).exp()) / k
        } else {
            l
        };
        [
            self.emission[0] as f64 * f,
            self.emission[1] as f64 * f,
            self.emission[2] as f64 * f,
        ]
    }
}

/// Splats accumulated from one or more media, ready to hand to the scene.
#[derive(Debug, Default)]
pub struct MediumCloud {
    positions: Vec<[f32; 3]>,
    scales: Vec<[f32; 3]>,
    opacities: Vec<f32>,
    sh: Vec<[f32; 3]>,
    clamped: bool,
}

/// Sorted crossings of one axis-aligned line with a closed boundary, merged
/// so a crossing shared by two adjacent triangles counts once.
fn crossings<G: Geometry>(bvh: &KBvh<G>, origin: Point3, dir: Vec3, tol: f64) -> Vec<f64> {
    let mut ts: Vec<f64> = Vec::new();
    for hit in bvh.trace(&Ray::new(origin, dir)) {
        if ts.last().is_none_or(|&last| hit.t - last > tol) {
            ts.push(hit.t);
        }
    }
    ts
}

/// For a regular sample line, mark which samples are inside (by crossing
/// parity) and how deep (distance to the nearer crossing).
fn sweep_line(ts: &[f64], samples: usize, h: f64, out_depth: &mut [f32]) {
    // Sample k sits at t = (k + 0.5)·h from the line's origin.
    let mut j = 0usize;
    for (k, d) in out_depth.iter_mut().enumerate().take(samples) {
        let t = (k as f64 + 0.5) * h;
        while j < ts.len() && ts[j] <= t {
            j += 1;
        }
        if j % 2 == 1 && j < ts.len() {
            let depth = (t - ts[j - 1]).min(ts[j] - t);
            *d = depth as f32;
        } else {
            *d = -1.0;
        }
    }
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

impl MediumCloud {
    /// An empty cloud.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any medium asked for more absorption than its lattice could
    /// carry (see [`MAX_ALPHA`]).
    pub fn clamped(&self) -> bool {
        self.clamped
    }

    /// Splats so far.
    pub fn len(&self) -> usize {
        self.opacities.len()
    }

    /// Whether no medium has contributed a splat.
    pub fn is_empty(&self) -> bool {
        self.opacities.is_empty()
    }

    /// Lattice spacing (mm) that fills `bounds` with about `budget` samples,
    /// counting only the fraction `inside` of the box the solid occupies.
    pub fn auto_spacing(bounds: &Aabb, inside: f64, budget: usize) -> f64 {
        let e = bounds.max - bounds.min;
        let vol = (e.x * e.y * e.z).max(1e-9) * inside.clamp(1e-6, 1.0);
        (vol / budget.max(1) as f64).cbrt()
    }

    /// Fill the closed solid behind `bvh` with medium `m` at lattice spacing
    /// `h` (mm). Returns the number of splats added.
    ///
    /// Inside-ness is decided by crossing parity along three axis-aligned
    /// sweeps and a 2-of-3 vote, so a line that grazes an edge and miscounts
    /// does not punch a streak through the medium. Depth below the boundary,
    /// for [`EmissiveMedium::falloff`], is the smallest per-axis distance to a
    /// crossing — exact for slabs and boxes, an overestimate along diagonals.
    pub fn add<G: Geometry>(&mut self, bvh: &KBvh<G>, m: &EmissiveMedium, h: f64) -> usize {
        let Some(b) = bvh.bounds() else {
            return 0;
        };
        if h.is_nan() || h <= 0.0 || m.emission.iter().all(|&e| e <= 0.0) {
            return 0;
        }
        let e = b.max - b.min;
        let n = [
            ((e.x / h).ceil() as usize).max(1),
            ((e.y / h).ceil() as usize).max(1),
            ((e.z / h).ceil() as usize).max(1),
        ];
        let cells = n[0] * n[1] * n[2];
        // Lattice origin: centre the samples in the box.
        let o = [
            b.min.x + 0.5 * (e.x - n[0] as f64 * h),
            b.min.y + 0.5 * (e.y - n[1] as f64 * h),
            b.min.z + 0.5 * (e.z - n[2] as f64 * h),
        ];
        let idx = |i: usize, j: usize, k: usize| (k * n[1] + j) * n[0] + i;
        let tol = 1e-6 * (e.x.max(e.y).max(e.z)).max(1.0);
        // Rays start a whole cell outside the box so the first crossing has
        // positive t; shifting every crossing back by `pad` puts sample k at
        // t = (k + 0.5)·h, where `sweep_line` reads it.
        let pad = h;

        let mut depth = [
            vec![-1.0f32; cells],
            vec![-1.0f32; cells],
            vec![-1.0f32; cells],
        ];
        let mut line = Vec::new();
        for axis in 0..3 {
            let (a, c) = match axis {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            for v in 0..n[c] {
                for u in 0..n[a] {
                    let mut p = o;
                    p[a] = o[a] + (u as f64 + 0.5) * h;
                    p[c] = o[c] + (v as f64 + 0.5) * h;
                    p[axis] = o[axis] - pad;
                    let mut d = [0.0; 3];
                    d[axis] = 1.0;
                    let ts = crossings(
                        bvh,
                        Point3::new(p[0], p[1], p[2]),
                        Vec3::new(d[0], d[1], d[2]),
                        tol,
                    );
                    // Shift t so sample 0 sits at t = 0.5·h.
                    let shifted: Vec<f64> = ts.iter().map(|t| t - pad).collect();
                    line.clear();
                    line.resize(n[axis], -1.0);
                    sweep_line(&shifted, n[axis], h, &mut line);
                    for (s, &dv) in line.iter().enumerate() {
                        let mut ijk = [0usize; 3];
                        ijk[axis] = s;
                        ijk[a] = u;
                        ijk[c] = v;
                        depth[axis][idx(ijk[0], ijk[1], ijk[2])] = dv;
                    }
                }
            }
        }

        let k_gain = lattice_gain(h);
        let (alpha, colour) = if m.absorption > 0.0 {
            let mut a = m.absorption as f64 / k_gain;
            if a > MAX_ALPHA {
                self.clamped = true;
                a = MAX_ALPHA;
            }
            let kappa = k_gain * a;
            (a, m.emission.map(|e| e as f64 / kappa))
        } else {
            (
                THIN_ALPHA,
                m.emission.map(|e| e as f64 / (k_gain * THIN_ALPHA)),
            )
        };
        let sh0 = colour.map(|c| ((c - 0.5) / SH_C0) as f32);
        let sigma = (SIGMA_OVER_H * h) as f32;

        let before = self.len();
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    let c = idx(i, j, k);
                    let ds = [depth[0][c], depth[1][c], depth[2][c]];
                    let votes = ds.iter().filter(|&&d| d >= 0.0).count();
                    if votes < 2 {
                        continue;
                    }
                    let dmin = ds
                        .iter()
                        .filter(|&&d| d >= 0.0)
                        .fold(f32::INFINITY, |acc, &d| acc.min(d))
                        as f64;
                    let density = if m.falloff > 0.0 {
                        smoothstep(dmin / m.falloff as f64)
                    } else {
                        1.0
                    };
                    if density <= 0.0 {
                        continue;
                    }
                    self.positions.push([
                        (o[0] + (i as f64 + 0.5) * h) as f32,
                        (o[1] + (j as f64 + 0.5) * h) as f32,
                        (o[2] + (k as f64 + 0.5) * h) as f32,
                    ]);
                    self.scales.push([sigma; 3]);
                    self.opacities.push((alpha * density) as f32);
                    self.sh.push(sh0);
                }
            }
        }
        self.len() - before
    }

    /// Fill with an automatically chosen spacing for about `budget` splats.
    ///
    /// A coarse pre-pass (about 32k lattice cells) measures what fraction of
    /// the bounding box the solid fills, so a thin diagonal filament gets the
    /// budget spent inside it rather than across its empty box.
    pub fn add_auto<G: Geometry>(
        &mut self,
        bvh: &KBvh<G>,
        m: &EmissiveMedium,
        budget: usize,
    ) -> f64 {
        let Some(b) = bvh.bounds() else {
            return 0.0;
        };
        let mut probe = MediumCloud::new();
        let h0 = Self::auto_spacing(&b, 1.0, 32_768);
        let unit = EmissiveMedium {
            emission: [1.0; 3],
            absorption: 0.0,
            falloff: 0.0,
        };
        let inside = probe.add(bvh, &unit, h0);
        let e = b.max - b.min;
        let n0 = ((e.x / h0).ceil() * (e.y / h0).ceil() * (e.z / h0).ceil()).max(1.0);
        let frac = (inside as f64 / n0).max(1.0 / n0);
        let h = Self::auto_spacing(&b, frac, budget);
        self.add(bvh, m, h);
        h
    }

    /// The finished cloud, as the hierarchy [`Scene::splats`] takes. `None`
    /// when no medium contributed.
    ///
    /// [`Scene::splats`]: kosm_render::pathtrace::Scene::splats
    pub fn build(self) -> Option<Arc<SplatBvh>> {
        if self.is_empty() {
            return None;
        }
        let quats = vec![[1.0f32, 0.0, 0.0, 0.0]; self.len()];
        let splats = Splats::from_parts(
            &self.positions,
            &self.scales,
            &quats,
            &self.opacities,
            &self.sh,
        );
        Some(Arc::new(KBvh::build(splats)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_matches_its_closed_form() {
        // h = 1: 2π·0.5625·0.98889 ≈ 3.4950
        assert!((lattice_gain(1.0) - 3.4950).abs() < 1e-3);
        // K scales as 1/h.
        assert!((lattice_gain(0.5) - 2.0 * lattice_gain(1.0)).abs() < 1e-9);
    }

    #[test]
    fn slab_formula_has_the_emission_only_limit() {
        let m = EmissiveMedium {
            emission: [2.0, 1.0, 0.5],
            absorption: 0.0,
            falloff: 0.0,
        };
        assert_eq!(m.slab_radiance(3.0), [6.0, 3.0, 1.5]);
        let thick = EmissiveMedium {
            absorption: 1.0,
            ..m
        };
        let r = thick.slab_radiance(100.0);
        assert!((r[0] - 2.0).abs() < 1e-9, "optically thick → S = ε/κ");
    }
}
