//! Photon interaction data: NIST mass attenuation tables + Klein–Nishina.
//!
//! Elemental μ/ρ and μ_en/ρ are transcribed **verbatim** from the NIST
//! X-Ray Mass Attenuation Coefficients tables (J. H. Hubbell & S. M.
//! Seltzer, NIST Standard Reference Database 126, NISTIR 5632;
//! `physics.nist.gov/PhysRefData/XrayMassCoef/ElemTab/zNN.html`, read
//! 2026-11-16), 5 keV – 3 MeV, including the absorption-edge rows inside
//! that window (Cr/Mn/Fe/Ni K, Pb L₁–L₃ and K). Interpolation is log-log
//! within an edge-free segment, the standard practice for these tables.
//! Hubbell & Seltzer quote uncertainties of roughly 1–2% above 10 keV
//! rising to 5% near edges; this crate carries a flat 5% per-material
//! μ uncertainty into its dose bands (wider for compositions known only
//! nominally — see [`crate::materials`]).
//!
//! The table μ/ρ includes coherent scattering. Transport splits each
//! material's μ into an **incoherent (Compton) part** — free-electron
//! Klein–Nishina × electrons per gram — and an **"absorption" remainder**
//! (photoelectric + coherent + pair production, clamped ≥ 0). Two stated
//! approximations follow: coherent scattering is treated as removal
//! (it is ≤ 10% of μ in the 10–150 keV window and strongly forward-
//! peaked), and free-electron Klein–Nishina ignores binding (it
//! overstates incoherent scatter by up to ~15% below 20 keV in heavy
//! elements, where photoabsorption dominates anyway).
//! Outside 5 keV – 3 MeV every lookup fails closed.

/// Lower edge of the bundled data window, keV.
pub const DATA_MIN_KEV: f64 = 5.0;
/// Upper edge of the bundled data window, keV.
pub const DATA_MAX_KEV: f64 = 3000.0;

/// Avogadro's number, 1/mol.
pub const AVOGADRO: f64 = 6.022_140_76e23;
/// Electron rest energy, keV.
pub const ELECTRON_MASS_KEV: f64 = 510.998_95;
/// Classical electron radius squared, cm².
pub const R_E2_CM2: f64 = 7.940_787_5e-26;

/// One element: Z, A and the NIST rows `(E MeV, μ/ρ cm²/g, μ_en/ρ cm²/g)`.
#[derive(Debug, Clone, Copy)]
pub struct Element {
    /// Symbol.
    pub symbol: &'static str,
    /// Atomic number.
    pub z: u32,
    /// Standard atomic weight, g/mol.
    pub a: f64,
    /// NIST rows; edges appear as duplicated energies.
    pub rows: &'static [(f64, f64, f64)],
}

macro_rules! element {
    ($name:ident, $sym:expr, $z:expr, $a:expr, [$(($e:expr, $m:expr, $n:expr)),* $(,)?]) => {
        #[doc = concat!("NIST SRD 126 table for ", $sym, ".")]
        pub const $name: Element = Element {
            symbol: $sym,
            z: $z,
            a: $a,
            rows: &[$(($e, $m, $n)),*],
        };
    };
}

element!(H, "H", 1, 1.008, [
    (5.0e-3, 4.193e-1, 3.278e-2), (6.0e-3, 4.042e-1, 1.996e-2), (8.0e-3, 3.914e-1, 1.160e-2),
    (1.0e-2, 3.854e-1, 9.849e-3), (1.5e-2, 3.764e-1, 1.102e-2), (2.0e-2, 3.695e-1, 1.355e-2),
    (3.0e-2, 3.570e-1, 1.863e-2), (4.0e-2, 3.458e-1, 2.315e-2), (5.0e-2, 3.355e-1, 2.709e-2),
    (6.0e-2, 3.260e-1, 3.053e-2), (8.0e-2, 3.091e-1, 3.620e-2), (1.0e-1, 2.944e-1, 4.063e-2),
    (1.5e-1, 2.651e-1, 4.813e-2), (2.0e-1, 2.429e-1, 5.254e-2), (3.0e-1, 2.112e-1, 5.695e-2),
    (4.0e-1, 1.893e-1, 5.860e-2), (5.0e-1, 1.729e-1, 5.900e-2), (6.0e-1, 1.599e-1, 5.875e-2),
    (8.0e-1, 1.405e-1, 5.739e-2), (1.0, 1.263e-1, 5.556e-2), (1.25, 1.129e-1, 5.311e-2),
    (1.5, 1.027e-1, 5.075e-2), (2.0, 8.769e-2, 4.650e-2), (3.0, 6.921e-2, 3.992e-2),
]);

element!(B, "B", 5, 10.81, [
    (5.0e-3, 9.683, 9.332), (6.0e-3, 5.538, 5.223), (8.0e-3, 2.346, 2.072),
    (1.0e-2, 1.255, 1.006), (1.5e-2, 4.827e-1, 2.698e-1), (2.0e-2, 3.014e-1, 1.084e-1),
    (3.0e-2, 2.063e-1, 3.506e-2), (4.0e-2, 1.793e-1, 2.084e-2), (5.0e-2, 1.665e-1, 1.737e-2),
    (6.0e-2, 1.583e-1, 1.680e-2), (8.0e-2, 1.472e-1, 1.785e-2), (1.0e-1, 1.391e-1, 1.940e-2),
    (1.5e-1, 1.243e-1, 2.255e-2), (2.0e-1, 1.136e-1, 2.453e-2), (3.0e-1, 9.862e-2, 2.654e-2),
    (4.0e-1, 8.834e-2, 2.731e-2), (5.0e-1, 8.065e-2, 2.749e-2), (6.0e-1, 7.460e-2, 2.737e-2),
    (8.0e-1, 6.549e-2, 2.671e-2), (1.0, 5.890e-2, 2.586e-2), (1.25, 5.266e-2, 2.472e-2),
    (1.5, 4.791e-2, 2.362e-2), (2.0, 4.108e-2, 2.171e-2), (3.0, 3.284e-2, 1.889e-2),
]);

element!(C, "C", 6, 12.011, [
    (5.0e-3, 1.912e1, 1.866e1), (6.0e-3, 1.095e1, 1.054e1), (8.0e-3, 4.576, 4.242),
    (1.0e-2, 2.373, 2.078), (1.5e-2, 8.071e-1, 5.627e-1), (2.0e-2, 4.420e-1, 2.238e-1),
    (3.0e-2, 2.562e-1, 6.614e-2), (4.0e-2, 2.076e-1, 3.343e-2), (5.0e-2, 1.871e-1, 2.397e-2),
    (6.0e-2, 1.753e-1, 2.098e-2), (8.0e-2, 1.610e-1, 2.037e-2), (1.0e-1, 1.514e-1, 2.147e-2),
    (1.5e-1, 1.347e-1, 2.449e-2), (2.0e-1, 1.229e-1, 2.655e-2), (3.0e-1, 1.066e-1, 2.870e-2),
    (4.0e-1, 9.546e-2, 2.950e-2), (5.0e-1, 8.715e-2, 2.969e-2), (6.0e-1, 8.058e-2, 2.956e-2),
    (8.0e-1, 7.076e-2, 2.885e-2), (1.0, 6.361e-2, 2.792e-2), (1.25, 5.690e-2, 2.669e-2),
    (1.5, 5.179e-2, 2.551e-2), (2.0, 4.442e-2, 2.345e-2), (3.0, 3.562e-2, 2.048e-2),
]);

element!(N, "N", 7, 14.007, [
    (5.0e-3, 3.144e1, 3.086e1), (6.0e-3, 1.809e1, 1.759e1), (8.0e-3, 7.562, 7.170),
    (1.0e-2, 3.879, 3.545), (1.5e-2, 1.236, 9.715e-1), (2.0e-2, 6.178e-1, 3.867e-1),
    (3.0e-2, 3.066e-1, 1.099e-1), (4.0e-2, 2.288e-1, 5.051e-2), (5.0e-2, 1.980e-1, 3.217e-2),
    (6.0e-2, 1.817e-1, 2.548e-2), (8.0e-2, 1.639e-1, 2.211e-2), (1.0e-1, 1.529e-1, 2.231e-2),
    (1.5e-1, 1.353e-1, 2.472e-2), (2.0e-1, 1.233e-1, 2.665e-2), (3.0e-1, 1.068e-1, 2.873e-2),
    (4.0e-1, 9.557e-2, 2.952e-2), (5.0e-1, 8.719e-2, 2.969e-2), (6.0e-1, 8.063e-2, 2.956e-2),
    (8.0e-1, 7.081e-2, 2.886e-2), (1.0, 6.364e-2, 2.792e-2), (1.25, 5.693e-2, 2.669e-2),
    (1.5, 5.180e-2, 2.550e-2), (2.0, 4.450e-2, 2.347e-2), (3.0, 3.579e-2, 2.057e-2),
]);

element!(O, "O", 8, 15.999, [
    (5.0e-3, 4.790e1, 4.715e1), (6.0e-3, 2.770e1, 2.708e1), (8.0e-3, 1.163e1, 1.116e1),
    (1.0e-2, 5.952, 5.565), (1.5e-2, 1.836, 1.545), (2.0e-2, 8.651e-1, 6.179e-1),
    (3.0e-2, 3.779e-1, 1.729e-1), (4.0e-2, 2.585e-1, 7.530e-2), (5.0e-2, 2.132e-1, 4.414e-2),
    (6.0e-2, 1.907e-1, 3.207e-2), (8.0e-2, 1.678e-1, 2.468e-2), (1.0e-1, 1.551e-1, 2.355e-2),
    (1.5e-1, 1.361e-1, 2.506e-2), (2.0e-1, 1.237e-1, 2.679e-2), (3.0e-1, 1.070e-1, 2.877e-2),
    (4.0e-1, 9.566e-2, 2.953e-2), (5.0e-1, 8.729e-2, 2.971e-2), (6.0e-1, 8.070e-2, 2.957e-2),
    (8.0e-1, 7.087e-2, 2.887e-2), (1.0, 6.372e-2, 2.794e-2), (1.25, 5.697e-2, 2.669e-2),
    (1.5, 5.185e-2, 2.551e-2), (2.0, 4.459e-2, 2.350e-2), (3.0, 3.597e-2, 2.066e-2),
]);

element!(NA, "Na", 11, 22.990, [
    (5.0e-3, 1.194e2, 1.178e2), (6.0e-3, 7.030e1, 6.915e1), (8.0e-3, 3.018e1, 2.941e1),
    (1.0e-2, 1.557e1, 1.499e1), (1.5e-2, 4.694, 4.313), (2.0e-2, 2.057, 1.759),
    (3.0e-2, 7.197e-1, 4.928e-1), (4.0e-2, 3.969e-1, 2.031e-1), (5.0e-2, 2.804e-1, 1.063e-1),
    (6.0e-2, 2.268e-1, 6.625e-2), (8.0e-2, 1.796e-1, 3.761e-2), (1.0e-1, 1.585e-1, 2.931e-2),
    (1.5e-1, 1.335e-1, 2.579e-2), (2.0e-1, 1.199e-1, 2.635e-2), (3.0e-1, 1.029e-1, 2.771e-2),
    (4.0e-1, 9.185e-2, 2.833e-2), (5.0e-1, 8.372e-2, 2.845e-2), (6.0e-1, 7.736e-2, 2.830e-2),
    (8.0e-1, 6.788e-2, 2.760e-2), (1.0, 6.100e-2, 2.669e-2), (1.25, 5.454e-2, 2.549e-2),
    (1.5, 4.968e-2, 2.437e-2), (2.0, 4.282e-2, 2.249e-2), (3.0, 3.487e-2, 1.997e-2),
]);

element!(AL, "Al", 13, 26.982, [
    (5.0e-3, 1.934e2, 1.902e2), (6.0e-3, 1.153e2, 1.133e2), (8.0e-3, 5.033e1, 4.918e1),
    (1.0e-2, 2.623e1, 2.543e1), (1.5e-2, 7.955, 7.487), (2.0e-2, 3.441, 3.094),
    (3.0e-2, 1.128, 8.778e-1), (4.0e-2, 5.685e-1, 3.601e-1), (5.0e-2, 3.681e-1, 1.840e-1),
    (6.0e-2, 2.778e-1, 1.099e-1), (8.0e-2, 2.018e-1, 5.511e-2), (1.0e-1, 1.704e-1, 3.794e-2),
    (1.5e-1, 1.378e-1, 2.827e-2), (2.0e-1, 1.223e-1, 2.745e-2), (3.0e-1, 1.042e-1, 2.816e-2),
    (4.0e-1, 9.276e-2, 2.862e-2), (5.0e-1, 8.445e-2, 2.868e-2), (6.0e-1, 7.802e-2, 2.851e-2),
    (8.0e-1, 6.841e-2, 2.778e-2), (1.0, 6.146e-2, 2.686e-2), (1.25, 5.496e-2, 2.565e-2),
    (1.5, 5.006e-2, 2.451e-2), (2.0, 4.324e-2, 2.266e-2), (3.0, 3.541e-2, 2.024e-2),
]);

element!(SI, "Si", 14, 28.086, [
    (5.0e-3, 2.450e2, 2.400e2), (6.0e-3, 1.470e2, 1.439e2), (8.0e-3, 6.468e1, 6.313e1),
    (1.0e-2, 3.389e1, 3.289e1), (1.5e-2, 1.034e1, 9.794), (2.0e-2, 4.464, 4.076),
    (3.0e-2, 1.436, 1.164), (4.0e-2, 7.012e-1, 4.782e-1), (5.0e-2, 4.385e-1, 2.430e-1),
    (6.0e-2, 3.207e-1, 1.434e-1), (8.0e-2, 2.228e-1, 6.896e-2), (1.0e-1, 1.835e-1, 4.513e-2),
    (1.5e-1, 1.448e-1, 3.086e-2), (2.0e-1, 1.275e-1, 2.905e-2), (3.0e-1, 1.082e-1, 2.932e-2),
    (4.0e-1, 9.614e-2, 2.968e-2), (5.0e-1, 8.748e-2, 2.971e-2), (6.0e-1, 8.077e-2, 2.951e-2),
    (8.0e-1, 7.082e-2, 2.875e-2), (1.0, 6.361e-2, 2.778e-2), (1.25, 5.688e-2, 2.652e-2),
    (1.5, 5.183e-2, 2.535e-2), (2.0, 4.480e-2, 2.345e-2), (3.0, 3.678e-2, 2.101e-2),
]);

element!(K, "K", 19, 39.098, [
    (5.0e-3, 5.189e2, 4.752e2), (6.0e-3, 3.205e2, 2.974e2), (8.0e-3, 1.469e2, 1.383e2),
    (1.0e-2, 7.907e1, 7.490e1), (1.5e-2, 2.503e1, 2.370e1), (2.0e-2, 1.093e1, 1.023e1),
    (3.0e-2, 3.413, 3.045), (4.0e-2, 1.541, 1.272), (5.0e-2, 8.679e-1, 6.454e-1),
    (6.0e-2, 5.678e-1, 3.730e-1), (8.0e-2, 3.251e-1, 1.628e-1), (1.0e-1, 2.345e-1, 9.161e-2),
    (1.5e-1, 1.582e-1, 4.346e-2), (2.0e-1, 1.319e-1, 3.378e-2), (3.0e-1, 1.080e-1, 3.015e-2),
    (4.0e-1, 9.495e-2, 2.959e-2), (5.0e-1, 8.600e-2, 2.929e-2), (6.0e-1, 7.922e-2, 2.895e-2),
    (8.0e-1, 6.929e-2, 2.807e-2), (1.0, 6.216e-2, 2.707e-2), (1.25, 5.556e-2, 2.581e-2),
    (1.5, 5.068e-2, 2.467e-2), (2.0, 4.399e-2, 2.290e-2), (3.0, 3.666e-2, 2.084e-2),
]);

element!(CR, "Cr", 24, 51.996, [
    (5.0e-3, 1.080e2, 1.057e2), (5.9892e-3, 6.574e1, 6.383e1), (5.9892e-3, 5.977e2, 4.027e2),
    (6.0e-3, 5.160e2, 4.027e2), (8.0e-3, 2.513e2, 2.087e2), (1.0e-2, 1.386e2, 1.193e2),
    (1.5e-2, 4.571e1, 4.093e1), (2.0e-2, 2.038e1, 1.846e1), (3.0e-2, 6.434, 5.780),
    (4.0e-2, 2.856, 2.482), (5.0e-2, 1.550, 1.278), (6.0e-2, 9.639e-1, 7.420e-1),
    (8.0e-2, 4.905e-1, 3.182e-1), (1.0e-1, 3.166e-1, 1.701e-1), (1.5e-1, 1.788e-1, 6.536e-2),
    (2.0e-1, 1.378e-1, 4.211e-2), (3.0e-1, 1.067e-1, 3.160e-2), (4.0e-1, 9.213e-2, 2.938e-2),
    (5.0e-1, 8.281e-2, 2.849e-2), (6.0e-1, 7.598e-2, 2.788e-2), (8.0e-1, 6.620e-2, 2.680e-2),
    (1.0, 5.930e-2, 2.576e-2), (1.25, 5.295e-2, 2.450e-2), (1.5, 4.832e-2, 2.340e-2),
    (2.0, 4.213e-2, 2.178e-2), (3.0, 3.559e-2, 2.011e-2),
]);

element!(MN, "Mn", 25, 54.938, [
    (5.0e-3, 1.212e2, 1.188e2), (6.0e-3, 7.350e1, 7.150e1), (6.539e-3, 5.803e1, 5.619e1),
    (6.539e-3, 4.520e2, 3.415e2), (8.0e-3, 2.734e2, 2.177e2), (1.0e-2, 1.514e2, 1.262e2),
    (1.5e-2, 5.027e1, 4.414e1), (2.0e-2, 2.253e1, 2.015e1), (3.0e-2, 7.141, 6.382),
    (4.0e-2, 3.169, 2.757), (5.0e-2, 1.714, 1.425), (6.0e-2, 1.060, 8.294e-1),
    (8.0e-2, 5.306e-1, 3.558e-1), (1.0e-1, 3.367e-1, 1.894e-1), (1.5e-1, 1.838e-1, 7.085e-2),
    (2.0e-1, 1.391e-1, 4.421e-2), (3.0e-1, 1.062e-1, 3.196e-2), (4.0e-1, 9.133e-2, 2.932e-2),
    (5.0e-1, 8.192e-2, 2.827e-2), (6.0e-1, 7.509e-2, 2.760e-2), (8.0e-1, 6.537e-2, 2.647e-2),
    (1.0, 5.852e-2, 2.541e-2), (1.25, 5.224e-2, 2.416e-2), (1.5, 4.769e-2, 2.307e-2),
    (2.0, 4.162e-2, 2.149e-2), (3.0, 3.524e-2, 1.989e-2),
]);

element!(FE, "Fe", 26, 55.845, [
    (5.0e-3, 1.398e2, 1.372e2), (6.0e-3, 8.484e1, 8.265e1), (7.112e-3, 5.319e1, 5.133e1),
    (7.112e-3, 4.076e2, 2.978e2), (8.0e-3, 3.056e2, 2.316e2), (1.0e-2, 1.706e2, 1.369e2),
    (1.5e-2, 5.708e1, 4.896e1), (2.0e-2, 2.568e1, 2.260e1), (3.0e-2, 8.176, 7.251),
    (4.0e-2, 3.629, 3.155), (5.0e-2, 1.958, 1.638), (6.0e-2, 1.205, 9.555e-1),
    (8.0e-2, 5.952e-1, 4.104e-1), (1.0e-1, 3.717e-1, 2.177e-1), (1.5e-1, 1.964e-1, 7.961e-2),
    (2.0e-1, 1.460e-1, 4.825e-2), (3.0e-1, 1.099e-1, 3.361e-2), (4.0e-1, 9.400e-2, 3.039e-2),
    (5.0e-1, 8.414e-2, 2.914e-2), (6.0e-1, 7.704e-2, 2.836e-2), (8.0e-1, 6.699e-2, 2.714e-2),
    (1.0, 5.995e-2, 2.603e-2), (1.25, 5.350e-2, 2.472e-2), (1.5, 4.883e-2, 2.360e-2),
    (2.0, 4.265e-2, 2.199e-2), (3.0, 3.621e-2, 2.042e-2),
]);

element!(NI, "Ni", 28, 58.693, [
    (5.0e-3, 1.793e2, 1.761e2), (6.0e-3, 1.090e2, 1.064e2), (8.0e-3, 4.952e1, 4.758e1),
    (8.3328e-3, 4.428e1, 4.242e1), (8.3328e-3, 3.294e2, 2.240e2), (1.0e-2, 2.090e2, 1.524e2),
    (1.5e-2, 7.081e1, 5.734e1), (2.0e-2, 3.220e1, 2.722e1), (3.0e-2, 1.034e1, 8.982),
    (4.0e-2, 4.600, 3.967), (5.0e-2, 2.474, 2.078), (6.0e-2, 1.512, 1.219),
    (8.0e-2, 7.306e-1, 5.259e-1), (1.0e-1, 4.440e-1, 2.781e-1), (1.5e-1, 2.208e-1, 9.812e-2),
    (2.0e-1, 1.582e-1, 5.649e-2), (3.0e-1, 1.154e-1, 3.659e-2), (4.0e-1, 9.765e-2, 3.209e-2),
    (5.0e-1, 8.698e-2, 3.036e-2), (6.0e-1, 7.944e-2, 2.937e-2), (8.0e-1, 6.891e-2, 2.795e-2),
    (1.0, 6.160e-2, 2.674e-2), (1.25, 5.494e-2, 2.536e-2), (1.5, 5.015e-2, 2.420e-2),
    (2.0, 4.387e-2, 2.257e-2), (3.0, 3.745e-2, 2.107e-2),
]);

element!(PB, "Pb", 82, 207.2, [
    (5.0e-3, 7.304e2, 7.124e2), (6.0e-3, 4.672e2, 4.546e2), (8.0e-3, 2.287e2, 2.207e2),
    (1.0e-2, 1.306e2, 1.247e2), (1.30352e-2, 6.701e1, 6.270e1), (1.30352e-2, 1.621e2, 1.291e2),
    (1.5e-2, 1.116e2, 9.100e1), (1.52e-2, 1.078e2, 8.807e1), (1.52e-2, 1.485e2, 1.131e2),
    (1.55269e-2, 1.416e2, 1.083e2), (1.58608e-2, 1.344e2, 1.032e2), (1.58608e-2, 1.548e2, 1.180e2),
    (2.0e-2, 8.636e1, 6.899e1), (3.0e-2, 3.032e1, 2.536e1), (4.0e-2, 1.436e1, 1.211e1),
    (5.0e-2, 8.041, 6.740), (6.0e-2, 5.021, 4.149), (8.0e-2, 2.419, 1.916),
    (8.80045e-2, 1.910, 1.482), (8.80045e-2, 7.683, 2.160), (1.0e-1, 5.549, 1.976),
    (1.5e-1, 2.014, 1.056), (2.0e-1, 9.985e-1, 5.870e-1), (3.0e-1, 4.031e-1, 2.455e-1),
    (4.0e-1, 2.323e-1, 1.370e-1), (5.0e-1, 1.614e-1, 9.128e-2), (6.0e-1, 1.248e-1, 6.819e-2),
    (8.0e-1, 8.870e-2, 4.644e-2), (1.0, 7.102e-2, 3.654e-2), (1.25, 5.876e-2, 2.988e-2),
    (1.5, 5.222e-2, 2.640e-2), (2.0, 4.606e-2, 2.360e-2), (3.0, 4.234e-2, 2.322e-2),
]);

/// Every bundled element.
pub const ELEMENTS: [Element; 14] = [H, B, C, N, O, NA, AL, SI, K, CR, MN, FE, NI, PB];

/// Energy outside the bundled data window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutOfRange {
    /// The rejected energy, keV.
    pub e_kev: f64,
}

impl std::fmt::Display for OutOfRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "photon energy {} keV outside the bundled NIST window [{DATA_MIN_KEV}, {DATA_MAX_KEV}] keV",
            self.e_kev
        )
    }
}

impl std::error::Error for OutOfRange {}

impl Element {
    /// `(μ/ρ, μ_en/ρ)` in cm²/g at `e_kev`, log-log interpolated inside
    /// the edge-free segment containing the energy. Exactly at an edge
    /// the above-edge value is returned.
    pub fn coefficients(&self, e_kev: f64) -> Result<(f64, f64), OutOfRange> {
        if !(DATA_MIN_KEV..=DATA_MAX_KEV).contains(&e_kev) {
            return Err(OutOfRange { e_kev });
        }
        let e = e_kev * 1.0e-3;
        let rows = self.rows;
        // Last row with E ≤ e. At a duplicated edge energy this is the
        // second (above-edge) row, whose successor is strictly larger.
        let i = rows.partition_point(|r| r.0 <= e).max(1) - 1;
        if i + 1 >= rows.len() {
            let r = rows[rows.len() - 1];
            return Ok((r.1, r.2));
        }
        let (e0, m0, n0) = rows[i];
        let (e1, m1, n1) = rows[i + 1];
        let t = (e / e0).ln() / (e1 / e0).ln();
        Ok((m0 * (m1 / m0).powf(t), n0 * (n1 / n0).powf(t)))
    }

    /// Electrons per gram, `Z N_A / A`.
    pub fn electrons_per_gram(&self) -> f64 {
        self.z as f64 * AVOGADRO / self.a
    }
}

/// Look up a bundled element by symbol (fail-closed: `None`).
pub fn element(symbol: &str) -> Option<Element> {
    ELEMENTS.iter().copied().find(|e| e.symbol == symbol)
}

/// Total Klein–Nishina cross section per free electron, cm².
pub fn klein_nishina_sigma_cm2(e_kev: f64) -> f64 {
    let k = e_kev / ELECTRON_MASS_KEV;
    let l = (1.0 + 2.0 * k).ln();
    2.0 * std::f64::consts::PI
        * R_E2_CM2
        * ((1.0 + k) / (k * k) * (2.0 * (1.0 + k) / (1.0 + 2.0 * k) - l / k) + l / (2.0 * k)
            - (1.0 + 3.0 * k) / (1.0 + 2.0 * k).powi(2))
}

/// Klein–Nishina differential cross section per electron, cm²/sr, at
/// scattering cosine `mu`.
pub fn klein_nishina_dsigma_domega(e_kev: f64, mu: f64) -> f64 {
    let k = e_kev / ELECTRON_MASS_KEV;
    let p = 1.0 / (1.0 + k * (1.0 - mu));
    0.5 * R_E2_CM2 * p * p * (p + 1.0 / p - (1.0 - mu * mu))
}

/// Compton-scattered photon energy at cosine `mu`, keV.
pub fn compton_energy_kev(e_kev: f64, mu: f64) -> f64 {
    e_kev / (1.0 + e_kev / ELECTRON_MASS_KEV * (1.0 - mu))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_points_reproduce_exactly() {
        let (m, _) = PB.coefficients(100.0).unwrap();
        assert!((m - 5.549).abs() < 1e-9);
        let (m, _) = FE.coefficients(40.0).unwrap();
        assert!((m - 3.629).abs() < 1e-9);
    }

    #[test]
    fn edges_jump_the_right_way() {
        // Just below / above the Pb K edge (88.0045 keV).
        let (below, _) = PB.coefficients(88.0).unwrap();
        let (above, _) = PB.coefficients(88.01).unwrap();
        assert!(below < 2.0 && above > 7.0, "below {below} above {above}");
        let (fe_below, _) = FE.coefficients(7.0).unwrap();
        let (fe_above, _) = FE.coefficients(7.2).unwrap();
        assert!(fe_above > 5.0 * fe_below);
    }

    #[test]
    fn out_of_window_fails_closed() {
        assert!(PB.coefficients(4.0).is_err());
        assert!(PB.coefficients(3500.0).is_err());
        assert!(element("Xx").is_none());
    }

    #[test]
    fn klein_nishina_limits() {
        // Thomson limit: 8π r_e²/3 = 0.6652 b.
        let thomson = 8.0 * std::f64::consts::PI * R_E2_CM2 / 3.0;
        assert!((klein_nishina_sigma_cm2(0.01) / thomson - 1.0).abs() < 1e-3);
        // Hydrogen: KN × electrons/g ≈ the NIST total above ~100 keV
        // (hydrogen has no photoabsorption to speak of).
        for e in [200.0, 662.0, 1000.0, 2000.0] {
            let kn = klein_nishina_sigma_cm2(e) * H.electrons_per_gram();
            let (tot, _) = H.coefficients(e).unwrap();
            assert!((kn / tot - 1.0).abs() < 0.01, "H at {e} keV: KN {kn} vs NIST {tot}");
        }
        // dσ/dΩ integrates to σ.
        let e = 300.0;
        let n = 4000;
        let s: f64 = (0..n)
            .map(|i| {
                let mu = -1.0 + 2.0 * (i as f64 + 0.5) / n as f64;
                klein_nishina_dsigma_domega(e, mu) * 2.0 * std::f64::consts::PI * 2.0 / n as f64
            })
            .sum();
        assert!((s / klein_nishina_sigma_cm2(e) - 1.0).abs() < 1e-5);
    }
}
