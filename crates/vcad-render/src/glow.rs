//! HDR glow: an energy-conserving bloom and the tone map that follows it.
//!
//! Everything here is a **post-process** on the linear-radiance film — the
//! physically based part of glow is the emission itself (surface `emissive`,
//! volumetric [`vcad_ir::VolumeEmission`]), traced by the path tracer. What a
//! post-process adds is what a camera does to very bright light: its point
//! spread function is not a delta. Lens and sensor scatter put a small
//! fraction of every pixel's energy into a wide, soft skirt, and for ordinary
//! scene radiance that skirt is invisible; for a plasma core a hundred times
//! brighter than the room it is the halo that makes it read as hot.
//!
//! [`bloom`] models exactly that and no more: the image is convolved with a
//! normalised PSF `(1 − s)·δ + s·Σₖ wₖ·Gₖ`, a delta plus a sum of Gaussians
//! whose weights sum to one. Total radiance is preserved to rounding — the
//! halo is energy *moved* out of the core, never added — which is what keeps
//! a bloomed render's exposure meaningful. The Gaussians are three passes of
//! a running-sum box filter (O(1) per pixel at any radius) with half-sample
//! symmetric reflection at the borders; that boundary rule makes each 1-D
//! pass a symmetric matrix with unit row sums, hence unit column sums, hence
//! energy-conserving at the edges too.
//!
//! [`Tonemap::Exp`] is the `1 − e^{−x}` curve: it never clips, so a hot core
//! saturates smoothly to white while its halo keeps its hue — the look of the
//! offline plasmoid reference. [`Tonemap::Aces`] is the default and is the
//! film's own curve, bit for bit.

use vcad_kernel_raytrace::pathtrace::{linear_to_srgb, tonemap_aces, Film};

/// Relative scales of the bloom's Gaussians, as fractions of
/// [`Bloom::radius`] — a ladder from a tight glow to the widest halo.
const SCALES: [f32; 4] = [1.0 / 30.0, 1.0 / 9.0, 1.0 / 2.8, 1.0];

/// Weights of the Gaussians in [`SCALES`]; they sum to one. Most of the
/// redistributed energy goes wide, as in a real glare PSF.
const WEIGHTS: [f32; 4] = [0.04, 0.11, 0.30, 0.55];

/// Bloom parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bloom {
    /// Fraction of every pixel's energy the PSF moves into the halo, in
    /// `0..=1`. Real glare is a few percent; `0.04` is the default.
    pub strength: f32,
    /// Standard deviation of the widest Gaussian, as a fraction of the image
    /// height. `0.035` is the default.
    pub radius: f32,
}

impl Default for Bloom {
    fn default() -> Self {
        Self {
            strength: 0.04,
            radius: 0.035,
        }
    }
}

/// The tone curve from linear radiance to display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tonemap {
    /// ACES filmic (Narkowicz fit), the film's own curve. Default.
    #[default]
    Aces,
    /// `1 − e^{−x}`: never clips, keeps a saturated core's halo in hue.
    Exp,
}

impl std::str::FromStr for Tonemap {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "aces" => Ok(Tonemap::Aces),
            "exp" | "exponential" => Ok(Tonemap::Exp),
            other => Err(format!("unknown tonemap '{other}' (expected aces|exp)")),
        }
    }
}

/// Glow options for a photoreal render: an optional bloom and the tone map.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Glow {
    /// Energy-conserving bloom, if any.
    pub bloom: Option<Bloom>,
    /// Tone curve.
    pub tonemap: Tonemap,
}

impl Glow {
    /// Whether this is the default (no bloom, ACES) — the film's own
    /// encoder, byte-identical to a render that predates glow.
    pub fn is_default(&self) -> bool {
        self.bloom.is_none() && self.tonemap == Tonemap::Aces
    }

    /// Parse the CLI pair `--bloom [strength]` / `--bloom-radius` /
    /// `--tonemap` into a [`Glow`].
    pub fn from_args(
        bloom: Option<f32>,
        radius: Option<f32>,
        tonemap: Option<&str>,
    ) -> Result<Self, String> {
        let bloom = match (bloom, radius) {
            (None, None) => None,
            (s, r) => {
                let d = Bloom::default();
                let b = Bloom {
                    strength: s.unwrap_or(d.strength),
                    radius: r.unwrap_or(d.radius),
                };
                if !(0.0..=1.0).contains(&b.strength) {
                    return Err(format!("--bloom {} out of range 0..=1", b.strength));
                }
                if b.radius.is_nan() || b.radius <= 0.0 || b.radius > 1.0 {
                    return Err(format!("--bloom-radius {} out of range (0, 1]", b.radius));
                }
                Some(b)
            }
        };
        let tonemap = tonemap.map(str::parse).transpose()?.unwrap_or_default();
        Ok(Self { bloom, tonemap })
    }
}

/// Box radius whose three-pass repetition approximates a Gaussian of
/// standard deviation `sigma` (variance of a width-`w` box is `(w² − 1)/12`).
fn box_radius(sigma: f32) -> usize {
    let w = (4.0 * sigma * sigma + 1.0).sqrt();
    ((w - 1.0) * 0.5).round().max(0.0) as usize
}

/// Half-sample symmetric reflection of index `j` into `0..n` (needs
/// `-n <= j < 2n`).
#[inline]
fn reflect(j: isize, n: isize) -> usize {
    if j < 0 {
        (-1 - j) as usize
    } else if j >= n {
        (2 * n - 1 - j) as usize
    } else {
        j as usize
    }
}

/// One box pass of radius `r` over `src` (length `n`) into `dst`.
fn box_line(src: &[f32], dst: &mut [f32], r: usize) {
    let n = src.len();
    let r = r.min(n.saturating_sub(1));
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let (ni, ri) = (n as isize, r as isize);
    let inv = 1.0 / (2 * r + 1) as f64;
    // f64 running sum: a 1000-tap window over HDR values must not drift.
    let mut acc: f64 = (-ri..=ri).map(|j| src[reflect(j, ni)] as f64).sum();
    for (i, d) in dst.iter_mut().enumerate().take(n) {
        *d = (acc * inv) as f32;
        let ii = i as isize;
        acc += src[reflect(ii + ri + 1, ni)] as f64 - src[reflect(ii - ri, ni)] as f64;
    }
}

/// Gaussian-blur one plane (`w × h`, row-major) in place: three box passes
/// along rows, then three along columns.
fn gauss_plane(plane: &mut [f32], w: usize, h: usize, sigma: f32) {
    let r = box_radius(sigma);
    if r == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; w.max(h)];
    for row in plane.chunks_mut(w) {
        for _ in 0..3 {
            box_line(row, &mut tmp[..w], r);
            row.copy_from_slice(&tmp[..w]);
        }
    }
    let mut col = vec![0.0f32; h];
    for x in 0..w {
        for (y, c) in col.iter_mut().enumerate() {
            *c = plane[y * w + x];
        }
        for _ in 0..3 {
            box_line(&col, &mut tmp[..h], r);
            col.copy_from_slice(&tmp[..h]);
        }
        for (y, c) in col.iter().enumerate() {
            plane[y * w + x] = *c;
        }
    }
}

/// Apply an energy-conserving bloom to interleaved linear RGB (`w × h × 3`).
///
/// `out = (1 − s)·in + s·Σₖ wₖ·Gₖ(in)`; `Σ out = Σ in` to rounding.
pub fn bloom(rgb: &mut [f32], w: usize, h: usize, b: &Bloom) {
    let s = b.strength.clamp(0.0, 1.0);
    if s <= 0.0 || w == 0 || h == 0 {
        return;
    }
    let n = w * h;
    assert_eq!(rgb.len(), n * 3, "bloom: buffer is not w*h*3");
    let big = b.radius * h as f32;
    let planes: Vec<Vec<f32>> = (0..3)
        .map(|c| (0..n).map(|i| rgb[i * 3 + c]).collect())
        .collect();
    let blurred: Vec<Vec<f32>> = {
        use rayon::prelude::*;
        planes
            .par_iter()
            .map(|p| {
                let mut halo = vec![0.0f32; n];
                for (scale, weight) in SCALES.iter().zip(WEIGHTS) {
                    let mut g = p.clone();
                    gauss_plane(&mut g, w, h, scale * big);
                    for (hv, gv) in halo.iter_mut().zip(&g) {
                        *hv += weight * gv;
                    }
                }
                halo
            })
            .collect()
    };
    for (c, halo) in blurred.iter().enumerate() {
        for (i, hv) in halo.iter().enumerate() {
            let v = &mut rgb[i * 3 + c];
            *v = (1.0 - s) * *v + s * hv;
        }
    }
}

/// Bloom (if asked) and tone-map a film to 8-bit sRGB RGBA.
///
/// With the default [`Glow`] this is exactly `Film::to_srgb8`.
pub fn encode_film(film: &Film, exposure: f32, transparent: bool, glow: &Glow) -> Vec<u8> {
    if glow.is_default() {
        return film.to_srgb8(exposure, transparent);
    }
    let (w, h) = (film.width as usize, film.height as usize);
    let mut rgb = film.rgb.clone();
    if let Some(b) = &glow.bloom {
        bloom(&mut rgb, w, h, b);
    }
    let curve = |x: f32| match glow.tonemap {
        Tonemap::Aces => tonemap_aces(x),
        Tonemap::Exp => 1.0 - (-x.max(0.0)).exp(),
    };
    let n = w * h;
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        for c in 0..3 {
            let v = curve(rgb[i * 3 + c] * exposure);
            out[i * 4 + c] = (linear_to_srgb(v) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
        out[i * 4 + 3] = if transparent {
            (film.alpha[i] * 255.0 + 0.5).clamp(0.0, 255.0) as u8
        } else {
            255
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(v: &[f32]) -> f64 {
        v.iter().map(|&x| x as f64).sum()
    }

    /// A tiny deterministic LCG, so the test image is reproducible without a
    /// dependency.
    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((s >> 40) as f32 / (1u64 << 24) as f32).powi(8) * 50.0
            })
            .collect()
    }

    #[test]
    fn a_box_pass_conserves_energy_at_the_border() {
        let src: Vec<f32> = (0..37).map(|i| (i * i % 11) as f32).collect();
        let mut dst = vec![0.0; 37];
        for r in [1, 5, 20, 36, 80] {
            box_line(&src, &mut dst, r);
            assert!((sum(&dst) - sum(&src)).abs() < 1e-3, "r = {r}");
        }
    }

    #[test]
    fn bloom_conserves_total_radiance() {
        let (w, h) = (97, 61);
        // An HDR impulse in a corner (the hardest case for the boundary) plus
        // heavy-tailed noise.
        let mut rgb = noise(w * h * 3, 9);
        rgb[0] = 5000.0;
        rgb[(w * h - 1) * 3 + 2] = 3000.0;
        let before: [f64; 3] = [0, 1, 2].map(|c| (0..w * h).map(|i| rgb[i * 3 + c] as f64).sum());
        bloom(
            &mut rgb,
            w,
            h,
            &Bloom {
                strength: 0.3,
                radius: 0.5,
            },
        );
        for c in 0..3 {
            let after: f64 = (0..w * h).map(|i| rgb[i * 3 + c] as f64).sum();
            assert!(
                ((after - before[c]) / before[c]).abs() < 1e-5,
                "channel {c}: {} → {after}",
                before[c]
            );
        }
    }

    #[test]
    fn bloom_leaves_a_flat_field_flat_and_spreads_an_impulse() {
        let (w, h) = (64, 48);
        let mut flat = vec![2.0f32; w * h * 3];
        bloom(&mut flat, w, h, &Bloom::default());
        assert!(flat.iter().all(|&v| (v - 2.0).abs() < 1e-4));

        let mut imp = vec![0.0f32; w * h * 3];
        let centre = (h / 2) * w + w / 2;
        imp[centre * 3] = 1000.0;
        // A halo wide enough to cross a 64 px frame: widest sigma 24 px.
        bloom(
            &mut imp,
            w,
            h,
            &Bloom {
                strength: 0.04,
                radius: 0.5,
            },
        );
        // The core keeps (1 − s) plus the tight Gaussian's share; the halo is
        // non-zero far out and falls off monotonically along a row.
        assert!(imp[centre * 3] > 1000.0 * 0.96);
        let row: Vec<f32> = (w / 2..w).map(|x| imp[((h / 2) * w + x) * 3]).collect();
        assert!(row.windows(2).all(|p| p[1] <= p[0] + 1e-6), "{row:?}");
        assert!(row[10] > 0.0, "the halo reaches 10 px");
        assert_eq!(imp[centre * 3 + 1], 0.0, "channels stay separate");
    }

    #[test]
    fn the_exp_curve_never_clips_and_glow_parses() {
        let g = Glow::from_args(Some(0.1), None, Some("exp")).unwrap();
        assert_eq!(g.tonemap, Tonemap::Exp);
        assert_eq!(g.bloom.unwrap().strength, 0.1);
        assert!(Glow::from_args(None, None, None).unwrap().is_default());
        assert!(Glow::from_args(Some(2.0), None, None).is_err());
        assert!(Glow::from_args(None, None, Some("filmic")).is_err());
    }

    #[test]
    fn default_glow_is_the_films_own_encoder() {
        let mut film = Film::new(4, 3);
        for (i, v) in film.rgb.iter_mut().enumerate() {
            *v = i as f32 * 0.37;
        }
        film.alpha.iter_mut().for_each(|a| *a = 0.5);
        for t in [false, true] {
            assert_eq!(
                encode_film(&film, 1.3, t, &Glow::default()),
                film.to_srgb8(1.3, t)
            );
        }
        // And the explicit-ACES non-default path agrees with it when the
        // bloom is a no-op.
        let g = Glow {
            bloom: Some(Bloom {
                strength: 0.0,
                radius: 0.1,
            }),
            tonemap: Tonemap::Aces,
        };
        assert_eq!(
            encode_film(&film, 1.3, false, &g),
            film.to_srgb8(1.3, false)
        );
    }
}
