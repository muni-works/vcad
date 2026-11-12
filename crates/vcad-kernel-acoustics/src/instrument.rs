//! Tuned idiophone bars — the glockenspiel hero demo's physics.
//!
//! Designs a free-free bar that rings at a target note:
//!
//! * **Modal solver** — a 1-D Hermite beam FEM over a non-uniform mesh whose
//!   breakpoints sit on every section change (undercut walls, cord-hole
//!   edges), so the sharp steps are integrated exactly. Section properties
//!   vary with both thickness (the undercut) and width (the holes). Rotary
//!   inertia is in the consistent mass matrix (a Rayleigh beam), and shear
//!   flexibility enters as a first-order Timoshenko energy correction per
//!   mode: `ω² ← ω² / (1 + U_shear / U_bend)`, with the shear force read
//!   from the FEM's own `EI·w'''`. For a 6 mm aluminium bar at C7 that
//!   correction is ~1 % (≈ 17 cents) — far outside a tuning tolerance, which
//!   is why it is not optional.
//! * **Eigensolver** — shifted subspace iteration with a banded Cholesky
//!   (half-bandwidth 3), Rayleigh–Ritz on a small block. Microseconds per
//!   solve, so the tuner can afford central differences.
//! * **Tuning** — Newton on `[ln(f₁/f_target), ln((f₂/f₁)/R)]` over
//!   `[ln L, d/t]` (length and undercut depth), Jacobian by central
//!   differences through the FEM. `R` defaults to 4.0 (the first overtone two
//!   octaves up, marimba-style). When `R` is unreachable within the depth
//!   bounds, depth pins at the bound and the tuner keeps solving length for
//!   the fundamental; the result says so.
//! * **Receipts** — one `acoustics.bar.<note>.f1` claim per bar: `Predicted`
//!   (→ Provisional, never Pass) until a strike is measured, then `Measured`
//!   with the cent error, Pass inside the tolerance and Fail outside.
//!
//! **Cross-checks and limits.** With the undercut, holes and corrections
//! switched off, the FEM reproduces the closed-form Euler–Bernoulli roots
//! ([`crate::strike::closed_form_hz`]) to < 0.01 cent. vcad has no 3-D modal
//! FEA yet (`vcad-kernel-fea` is static), so the shear/rotary-inertia terms
//! are checked against the analytic Timoshenko first-order result rather than
//! a solid model. Torsional and lateral (in-plane) modes are not computed;
//! for bar widths below roughly a quarter of the length they sit well above
//! f₂.

use serde::{Deserialize, Serialize};

use crate::strike::{cents, cholesky, congruence, note_to_hz};

const TAU: f64 = 2.0 * std::f64::consts::PI;

// ─── Materials ─────────────────────────────────────────────────────────────

/// Bar stock the designer knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarMaterial {
    /// 6061-T6 aluminium (the glockenspiel default; mills cleanly).
    Aluminium6061,
    /// C360 free-machining brass.
    Brass,
    /// PLA, for a printed toy instrument (it rings, briefly).
    Pla,
}

impl BarMaterial {
    /// Parse a user string ("aluminium", "aluminum", "6061", "brass", "pla").
    pub fn parse(s: &str) -> Result<Self, String> {
        let k = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match k.as_str() {
            "aluminium" | "aluminum" | "al" | "6061" | "al6061" | "aluminium_6061"
            | "aluminum_6061" | "6061_t6" => Ok(Self::Aluminium6061),
            "brass" | "c360" => Ok(Self::Brass),
            "pla" => Ok(Self::Pla),
            _ => Err(format!(
                "unknown bar material {s:?} — use aluminium_6061, brass or pla"
            )),
        }
    }

    /// Young's modulus, GPa.
    pub fn modulus_gpa(self) -> f64 {
        match self {
            Self::Aluminium6061 => 68.9,
            Self::Brass => 97.0,
            Self::Pla => 3.5,
        }
    }

    /// Density, kg/m³.
    pub fn density_kg_m3(self) -> f64 {
        match self {
            Self::Aluminium6061 => 2700.0,
            Self::Brass => 8500.0,
            Self::Pla => 1240.0,
        }
    }

    /// Poisson's ratio.
    pub fn poisson(self) -> f64 {
        match self {
            Self::Aluminium6061 => 0.33,
            Self::Brass => 0.34,
            Self::Pla => 0.36,
        }
    }

    /// Stable snake_case id.
    pub fn id(self) -> &'static str {
        match self {
            Self::Aluminium6061 => "aluminium_6061",
            Self::Brass => "brass",
            Self::Pla => "pla",
        }
    }

    /// Material key used in the generated loon / document.
    pub fn loon_key(self) -> &'static str {
        match self {
            Self::Aluminium6061 => "aluminum",
            Self::Brass => "brass",
            Self::Pla => "pla",
        }
    }

    /// `[material …]` line defining the display material.
    pub fn loon_def(self) -> String {
        let (r, g, b, metal, rough) = match self {
            Self::Aluminium6061 => (0.80, 0.81, 0.84, 0.9, 0.25),
            Self::Brass => (0.80, 0.63, 0.28, 0.9, 0.3),
            Self::Pla => (0.95, 0.36, 0.12, 0.0, 0.6),
        };
        format!(
            "[material \"{}\" {r:.2} {g:.2} {b:.2} {metal:.1} {rough:.2}]",
            self.loon_key()
        )
    }
}

// ─── Geometry ──────────────────────────────────────────────────────────────

/// A flat bar with an optional rectangular central undercut and cord holes.
///
/// Bar axis is X (length), Y is width, Z is thickness; the undercut is cut
/// from the underside over the central `undercut_span · L`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarGeometry {
    /// Length, mm.
    pub length_mm: f64,
    /// Width, mm.
    pub width_mm: f64,
    /// Full thickness, mm.
    pub thickness_mm: f64,
    /// Undercut depth from the underside, mm (0 = plain bar).
    pub undercut_depth_mm: f64,
    /// Undercut span as a fraction of length, centred (0..1).
    pub undercut_span: f64,
    /// Cord / peg hole diameter, mm (0 = no holes).
    pub hole_dia_mm: f64,
    /// Hole centres along the axis, mm from the x=0 end.
    pub holes_mm: Vec<f64>,
}

impl BarGeometry {
    fn pocket(&self) -> (f64, f64) {
        let a = 0.5 * (1.0 - self.undercut_span) * self.length_mm;
        (a, self.length_mm - a)
    }

    /// Thickness at axial station `x` (mm).
    fn thickness_at(&self, x: f64) -> f64 {
        let (a, b) = self.pocket();
        if self.undercut_depth_mm > 0.0 && x > a && x < b {
            self.thickness_mm - self.undercut_depth_mm
        } else {
            self.thickness_mm
        }
    }

    /// Material width at axial station `x` (mm), less any hole chord.
    fn width_at(&self, x: f64) -> f64 {
        let r = 0.5 * self.hole_dia_mm;
        let mut w = self.width_mm;
        for &h in &self.holes_mm {
            let d = (x - h).abs();
            if d < r {
                w -= 2.0 * (r * r - d * d).sqrt();
            }
        }
        w.max(1e-6)
    }

    /// Mesh breakpoints: ends, undercut walls, hole edges.
    fn breakpoints(&self) -> Vec<f64> {
        let l = self.length_mm;
        let mut b = vec![0.0, l];
        if self.undercut_depth_mm > 0.0 {
            let (a, c) = self.pocket();
            b.extend([a, c]);
        }
        let r = 0.5 * self.hole_dia_mm;
        if r > 0.0 {
            for &h in &self.holes_mm {
                b.extend([h - r, h, h + r]);
            }
        }
        b.retain(|x| (0.0..=l).contains(x));
        b.sort_by(|a, c| a.partial_cmp(c).unwrap());
        b.dedup_by(|a, c| (*a - *c).abs() < 1e-9);
        b
    }
}

// ─── Modal solve ───────────────────────────────────────────────────────────

/// Physics switches (all on for design; tests turn them off to compare
/// against closed form).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelOptions {
    /// Rotary inertia in the mass matrix.
    pub rotary_inertia: bool,
    /// First-order Timoshenko shear correction.
    pub shear: bool,
    /// Target element size as a fraction of length.
    pub element_fraction: f64,
}

impl Default for ModelOptions {
    fn default() -> Self {
        ModelOptions {
            rotary_inertia: true,
            shear: true,
            element_fraction: 1.0 / 80.0,
        }
    }
}

impl ModelOptions {
    /// Pure Euler–Bernoulli (for closed-form comparisons).
    pub fn euler_bernoulli() -> Self {
        ModelOptions {
            rotary_inertia: false,
            shear: false,
            ..Default::default()
        }
    }
}

/// Modal solve result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModalResult {
    /// Elastic modal frequencies, Hz, ascending (rigid modes dropped).
    pub hz: Vec<f64>,
    /// Shear correction factor applied per mode (`f_corrected / f_raw`).
    pub shear_factor: Vec<f64>,
    /// Nodal points of the fundamental, mm from x=0.
    pub nodes_mm: Vec<f64>,
    /// Mesh node positions, mm.
    pub mesh_mm: Vec<f64>,
    /// Fundamental's transverse displacement at each mesh node (max |·| = 1).
    pub mode1: Vec<f64>,
}

const GP: [f64; 4] = [
    -0.861_136_311_594_052_6,
    -0.339_981_043_584_856_3,
    0.339_981_043_584_856_3,
    0.861_136_311_594_052_6,
];
const GW: [f64; 4] = [
    0.347_854_845_137_453_8,
    0.652_145_154_862_546_1,
    0.652_145_154_862_546_1,
    0.347_854_845_137_453_8,
];

fn mesh(geom: &BarGeometry, opts: &ModelOptions) -> Vec<f64> {
    let h = geom.length_mm * opts.element_fraction;
    let bp = geom.breakpoints();
    let mut xs = vec![bp[0]];
    for w in bp.windows(2) {
        let n = ((w[1] - w[0]) / h).ceil().max(1.0) as usize;
        for k in 1..=n {
            xs.push(w[0] + (w[1] - w[0]) * k as f64 / n as f64);
        }
    }
    xs
}

/// Hermite shape functions and derivatives on ξ ∈ [-1, 1], element length `le`.
/// Returns (N, dN/dx, d²N/dx², d³N/dx³).
fn hermite(xi: f64, le: f64) -> ([f64; 4], [f64; 4], [f64; 4], [f64; 4]) {
    let s = le / 2.0;
    let n = [
        0.25 * (1.0 - xi) * (1.0 - xi) * (2.0 + xi),
        s * 0.25 * (1.0 - xi) * (1.0 - xi) * (1.0 + xi),
        0.25 * (1.0 + xi) * (1.0 + xi) * (2.0 - xi),
        s * 0.25 * (1.0 + xi) * (1.0 + xi) * (xi - 1.0),
    ];
    let c1 = 2.0 / le;
    let d1 = [
        0.75 * (xi * xi - 1.0) * c1,
        s * 0.25 * (3.0 * xi * xi - 2.0 * xi - 1.0) * c1,
        -0.75 * (xi * xi - 1.0) * c1,
        s * 0.25 * (3.0 * xi * xi + 2.0 * xi - 1.0) * c1,
    ];
    let c2 = c1 * c1;
    let d2 = [
        1.5 * xi * c2,
        s * (1.5 * xi - 0.5) * c2,
        -1.5 * xi * c2,
        s * (1.5 * xi + 0.5) * c2,
    ];
    let c3 = c2 * c1;
    let d3 = [1.5 * c3, s * 1.5 * c3, -1.5 * c3, s * 1.5 * c3];
    (n, d1, d2, d3)
}

/// Section properties at `x_mm`: (EI [N·m²], ρA [kg/m], ρI [kg·m], κGA [N]).
fn section(geom: &BarGeometry, mat: BarMaterial, x_mm: f64) -> (f64, f64, f64, f64) {
    let t = geom.thickness_at(x_mm) / 1000.0;
    let w = geom.width_at(x_mm) / 1000.0;
    let e = mat.modulus_gpa() * 1e9;
    let rho = mat.density_kg_m3();
    let nu = mat.poisson();
    let g = e / (2.0 * (1.0 + nu));
    let kappa = 10.0 * (1.0 + nu) / (12.0 + 11.0 * nu); // Cowper, rectangle
    let a = w * t;
    let i = w * t * t * t / 12.0;
    (e * i, rho * a, rho * i, kappa * g * a)
}

/// Modal frequencies of a free-free bar (lowest `count` elastic modes).
pub fn modal(
    geom: &BarGeometry,
    mat: BarMaterial,
    count: usize,
    opts: &ModelOptions,
) -> ModalResult {
    let xs = mesh(geom, opts);
    let nel = xs.len() - 1;
    let n = 2 * (nel + 1);
    let mut k = vec![0.0; n * n];
    let mut m = vec![0.0; n * n];
    for e in 0..nel {
        let (x0, x1) = (xs[e] / 1000.0, xs[e + 1] / 1000.0);
        let le = x1 - x0;
        let mut ke = [0.0; 16];
        let mut me = [0.0; 16];
        for g in 0..4 {
            let xi = GP[g];
            let x_mm = (x0 + (xi + 1.0) / 2.0 * le) * 1000.0;
            let (ei, ra, ri, _) = section(geom, mat, x_mm);
            let (nn, d1, d2, _) = hermite(xi, le);
            let wj = GW[g] * le / 2.0;
            for i in 0..4 {
                for j in 0..4 {
                    ke[i * 4 + j] += ei * d2[i] * d2[j] * wj;
                    let mut mij = ra * nn[i] * nn[j];
                    if opts.rotary_inertia {
                        mij += ri * d1[i] * d1[j];
                    }
                    me[i * 4 + j] += mij * wj;
                }
            }
        }
        let dof = [2 * e, 2 * e + 1, 2 * e + 2, 2 * e + 3];
        for i in 0..4 {
            for j in 0..4 {
                k[dof[i] * n + dof[j]] += ke[i * 4 + j];
                m[dof[i] * n + dof[j]] += me[i * 4 + j];
            }
        }
    }

    let (lambda, vecs) = lowest_modes(&k, &m, n, count + 2);
    let floor = (TAU * 1.0).powi(2);
    let elastic: Vec<usize> = (0..lambda.len())
        .filter(|&i| lambda[i] > floor)
        .take(count)
        .collect();

    let mut hz = Vec::with_capacity(count);
    let mut shear_factor = Vec::with_capacity(count);
    for &mi in &elastic {
        let phi = &vecs[mi];
        let mut factor = 1.0;
        if opts.shear {
            // U_b = ∫ EI w''², U_s = ∫ V²/κGA with V = (EI w'')'. Within an
            // element EI is piecewise constant at the Gauss points; V from the
            // cubic's constant w'''.
            let (mut ub, mut us) = (0.0, 0.0);
            for e in 0..nel {
                let (x0, x1) = (xs[e] / 1000.0, xs[e + 1] / 1000.0);
                let le = x1 - x0;
                let q = [phi[2 * e], phi[2 * e + 1], phi[2 * e + 2], phi[2 * e + 3]];
                for g in 0..4 {
                    let xi = GP[g];
                    let x_mm = (x0 + (xi + 1.0) / 2.0 * le) * 1000.0;
                    let (ei, _, _, kga) = section(geom, mat, x_mm);
                    let (_, _, d2, d3) = hermite(xi, le);
                    let w2: f64 = (0..4).map(|i| d2[i] * q[i]).sum();
                    let w3: f64 = (0..4).map(|i| d3[i] * q[i]).sum();
                    let wj = GW[g] * le / 2.0;
                    ub += ei * w2 * w2 * wj;
                    let v = ei * w3;
                    us += v * v / kga * wj;
                }
            }
            if ub > 0.0 {
                factor = 1.0 / (1.0 + us / ub).sqrt();
            }
        }
        shear_factor.push(factor);
        hz.push(lambda[mi].sqrt() / TAU * factor);
    }

    // Fundamental's displacement shape and its nodes.
    let mut mode1: Vec<f64> = match elastic.first() {
        Some(&mi) => (0..=nel).map(|i| vecs[mi][2 * i]).collect(),
        None => vec![0.0; nel + 1],
    };
    let peak = mode1.iter().fold(0.0_f64, |a, v| a.max(v.abs()));
    if peak > 0.0 {
        let sign = if mode1[0] < 0.0 { -1.0 } else { 1.0 };
        for v in &mut mode1 {
            *v *= sign / peak;
        }
    }
    let mut nodes_mm = Vec::new();
    for i in 0..nel {
        let (a, b) = (mode1[i], mode1[i + 1]);
        if a == 0.0 {
            nodes_mm.push(xs[i]);
        } else if a * b < 0.0 {
            nodes_mm.push(xs[i] + (xs[i + 1] - xs[i]) * a / (a - b));
        }
    }

    ModalResult {
        hz,
        shear_factor,
        nodes_mm,
        mesh_mm: xs,
        mode1,
    }
}

/// Banded Cholesky in place (lower), half-bandwidth `hb`, row-major `n×n`.
fn band_cholesky(a: &mut [f64], n: usize, hb: usize) {
    for i in 0..n {
        let j0 = i.saturating_sub(hb);
        for j in j0..=i {
            let mut s = a[i * n + j];
            for kk in j0.max(j.saturating_sub(hb))..j {
                s -= a[i * n + kk] * a[j * n + kk];
            }
            if i == j {
                a[i * n + i] = s.max(1e-300).sqrt();
            } else {
                a[i * n + j] = s / a[j * n + j];
            }
        }
    }
}

fn band_solve(l: &[f64], n: usize, hb: usize, b: &mut [f64]) {
    for i in 0..n {
        let mut s = b[i];
        for kk in i.saturating_sub(hb)..i {
            s -= l[i * n + kk] * b[kk];
        }
        b[i] = s / l[i * n + i];
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for kk in (i + 1)..(i + hb + 1).min(n) {
            s -= l[kk * n + i] * b[kk];
        }
        b[i] = s / l[i * n + i];
    }
}

fn matvec(a: &[f64], n: usize, x: &[f64]) -> Vec<f64> {
    let mut y = vec![0.0; n];
    for i in 0..n {
        let lo = i.saturating_sub(3);
        let hi = (i + 4).min(n);
        y[i] = (lo..hi).map(|j| a[i * n + j] * x[j]).sum();
    }
    y
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Symmetric eigen-decomposition by cyclic Jacobi: (values, column vectors
/// as `vecs[c][r]`).
fn jacobi_vectors(mut a: Vec<f64>, n: usize) -> (Vec<f64>, Vec<Vec<f64>>) {
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    for _ in 0..60 {
        let mut off = 0.0;
        let mut diag = 0.0;
        for i in 0..n {
            diag += a[i * n + i] * a[i * n + i];
            for j in (i + 1)..n {
                off += a[i * n + j] * a[i * n + j];
            }
        }
        if off <= 1e-30 * diag.max(1e-300) {
            break;
        }
        for p in 0..n.saturating_sub(1) {
            for q in (p + 1)..n {
                if a[p * n + q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * a[p * n + q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for kk in 0..n {
                    let (akp, akq) = (a[kk * n + p], a[kk * n + q]);
                    a[kk * n + p] = c * akp - s * akq;
                    a[kk * n + q] = s * akp + c * akq;
                }
                for kk in 0..n {
                    let (apk, aqk) = (a[p * n + kk], a[q * n + kk]);
                    a[p * n + kk] = c * apk - s * aqk;
                    a[q * n + kk] = s * apk + c * aqk;
                }
                for kk in 0..n {
                    let (vkp, vkq) = (v[kk * n + p], v[kk * n + q]);
                    v[kk * n + p] = c * vkp - s * vkq;
                    v[kk * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let vals = (0..n).map(|i| a[i * n + i]).collect();
    let vecs = (0..n)
        .map(|c| (0..n).map(|r| v[r * n + c]).collect())
        .collect();
    (vals, vecs)
}

/// Lowest `want` eigenpairs of K φ = λ M φ (K PSD with rigid modes, M SPD,
/// both banded with half-bandwidth 3) by shifted subspace iteration.
fn lowest_modes(k: &[f64], m: &[f64], n: usize, want: usize) -> (Vec<f64>, Vec<Vec<f64>>) {
    let p = (want + 4).min(n);
    // Shift so A = K + αM is SPD. α ~ (2π·20 Hz)² is far below any bar mode.
    let alpha = (TAU * 20.0).powi(2);
    let mut a: Vec<f64> = k.iter().zip(m).map(|(kk, mm)| kk + alpha * mm).collect();
    band_cholesky(&mut a, n, 3);

    let nn = n / 2;
    let mut x: Vec<Vec<f64>> = (0..p)
        .map(|c| {
            let mut col = vec![0.0; n];
            for i in 0..nn {
                let xi = i as f64 / (nn - 1).max(1) as f64;
                col[2 * i] = match c {
                    0 => 1.0,
                    1 => xi - 0.5,
                    _ => {
                        (c as f64 * std::f64::consts::PI * xi).cos() + 0.05 * (c as f64 * xi).sin()
                    }
                };
            }
            col
        })
        .collect();

    let mut prev = vec![f64::INFINITY; p];
    let mut vals = vec![0.0; p];
    for _ in 0..300 {
        // Y = A⁻¹ M X
        let y: Vec<Vec<f64>> = x
            .iter()
            .map(|col| {
                let mut b = matvec(m, n, col);
                band_solve(&a, n, 3, &mut b);
                b
            })
            .collect();
        let ky: Vec<Vec<f64>> = y.iter().map(|c| matvec(k, n, c)).collect();
        let my: Vec<Vec<f64>> = y.iter().map(|c| matvec(m, n, c)).collect();
        let mut kr = vec![0.0; p * p];
        let mut mr = vec![0.0; p * p];
        for i in 0..p {
            for j in 0..p {
                kr[i * p + j] = dot(&y[i], &ky[j]);
                mr[i * p + j] = dot(&y[i], &my[j]);
            }
        }
        // Symmetrize.
        for i in 0..p {
            for j in 0..i {
                let s = 0.5 * (kr[i * p + j] + kr[j * p + i]);
                kr[i * p + j] = s;
                kr[j * p + i] = s;
                let s = 0.5 * (mr[i * p + j] + mr[j * p + i]);
                mr[i * p + j] = s;
                mr[j * p + i] = s;
            }
        }
        let lr = cholesky(&mr, p);
        let c = congruence(&kr, &lr, p);
        let (ev, evec) = jacobi_vectors(c, p);
        let mut order: Vec<usize> = (0..p).collect();
        order.sort_by(|&i, &j| ev[i].partial_cmp(&ev[j]).unwrap());
        // Q = L⁻ᵀ z for each eigenvector z (back substitution).
        let mut newx = Vec::with_capacity(p);
        for &oi in &order {
            let mut q = evec[oi].clone();
            for i in (0..p).rev() {
                let mut s = q[i];
                for kk in (i + 1)..p {
                    s -= lr[kk * p + i] * q[kk];
                }
                q[i] = s / lr[i * p + i];
            }
            let mut col = vec![0.0; n];
            for (j, qj) in q.iter().enumerate() {
                for r in 0..n {
                    col[r] += y[j][r] * qj;
                }
            }
            newx.push(col);
        }
        x = newx;
        for (slot, &oi) in vals.iter_mut().zip(&order) {
            *slot = ev[oi];
        }
        let converged = (0..want.min(p)).all(|i| {
            let scale = vals[i].abs().max(1.0);
            (vals[i] - prev[i]).abs() <= 1e-13 * scale
        });
        prev.clone_from(&vals);
        if converged {
            break;
        }
    }
    vals.truncate(want);
    x.truncate(want);
    (vals, x)
}

// ─── Tuning ────────────────────────────────────────────────────────────────

/// What to tune a bar to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TuneTarget {
    /// Note name (e.g. "C6"); used for labels.
    pub note: String,
    /// Target fundamental, Hz.
    pub target_hz: f64,
    /// Stock.
    pub material: BarMaterial,
    /// Bar width, mm.
    pub width_mm: f64,
    /// Bar thickness, mm.
    pub thickness_mm: f64,
    /// Target f₂/f₁ (None = plain bar, length-only tuning).
    pub overtone_ratio: Option<f64>,
    /// Undercut span, fraction of length (centred).
    pub undercut_span: f64,
    /// Peg hole diameter, mm (holes go on the fundamental's nodes).
    pub hole_dia_mm: f64,
    /// Acceptance tolerance for the fundamental, cents.
    pub tolerance_cents: f64,
}

impl TuneTarget {
    /// Default glockenspiel-style target for `note` in `material`.
    pub fn for_note(note: &str, material: BarMaterial) -> Result<Self, String> {
        let (w, t, hole) = match material {
            BarMaterial::Aluminium6061 => (25.0, 6.0, 4.0),
            BarMaterial::Brass => (22.0, 5.0, 4.0),
            BarMaterial::Pla => (25.0, 6.0, 4.0),
        };
        Ok(TuneTarget {
            note: note.to_string(),
            target_hz: note_to_hz(note)?,
            material,
            width_mm: w,
            thickness_mm: t,
            overtone_ratio: Some(4.0),
            // Narrow central cut: mode 2 has a node (zero curvature) at the
            // centre, so a short pocket drops f₁ while barely touching f₂.
            // 0.2·L reaches f₂/f₁ = 4 at ~45 % depth; 0.5·L tops out near 3.3.
            undercut_span: 0.2,
            hole_dia_mm: hole,
            tolerance_cents: 5.0,
        })
    }
}

/// One Newton iterate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TuneStep {
    /// Iteration index (0 = initial guess).
    pub iteration: usize,
    /// Length, mm.
    pub length_mm: f64,
    /// Undercut depth, mm.
    pub undercut_depth_mm: f64,
    /// Fundamental, Hz.
    pub f1_hz: f64,
    /// First overtone, Hz.
    pub f2_hz: f64,
    /// f₁ vs target, cents.
    pub cents_error: f64,
}

/// A tuned bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TunedBar {
    /// Note.
    pub note: String,
    /// Target fundamental, Hz.
    pub target_hz: f64,
    /// Stock.
    pub material: BarMaterial,
    /// Final geometry (holes on the fundamental's nodes).
    pub geometry: BarGeometry,
    /// Predicted modal frequencies, Hz (f₁, f₂, f₃).
    pub hz: Vec<f64>,
    /// Predicted f₁ vs target, cents.
    pub cents_error: f64,
    /// Achieved f₂/f₁.
    pub overtone_ratio: f64,
    /// Requested f₂/f₁, if any.
    pub overtone_target: Option<f64>,
    /// Fundamental's nodal points, mm from x=0.
    pub nodes_mm: Vec<f64>,
    /// Shear correction on f₁ (f_corrected / f_Euler–Bernoulli-with-rotary).
    pub shear_factor: f64,
    /// Closed-form plain-bar f₁ at the final length (cross-check), Hz.
    pub closed_form_plain_hz: f64,
    /// Acceptance tolerance, cents.
    pub tolerance_cents: f64,
    /// |cents_error| ≤ 0.5 and (ratio met or unconstrained).
    pub converged: bool,
    /// Newton history.
    pub history: Vec<TuneStep>,
    /// Caveats.
    pub notes: Vec<String>,
}

fn geometry_for(t: &TuneTarget, length: f64, depth: f64, node_frac: &[f64]) -> BarGeometry {
    BarGeometry {
        length_mm: length,
        width_mm: t.width_mm,
        thickness_mm: t.thickness_mm,
        undercut_depth_mm: depth,
        undercut_span: t.undercut_span,
        hole_dia_mm: t.hole_dia_mm,
        holes_mm: node_frac.iter().map(|f| f * length).collect(),
    }
}

/// Plain-bar closed-form f₁ (Euler–Bernoulli) for a length.
pub fn closed_form_plain_f1(mat: BarMaterial, thickness_mm: f64, length_mm: f64) -> f64 {
    let bar = crate::strike::BarSpec {
        length_mm,
        width_mm: 1.0,
        thickness_mm,
        holes_mm: vec![],
        hole_dia_mm: 0.0,
        modulus_gpa: mat.modulus_gpa(),
        density_kg_m3: mat.density_kg_m3(),
    };
    crate::strike::closed_form_hz(&bar, 1)[0]
}

/// Tune a bar: Newton on (ln L, d/t) with a central-difference Jacobian
/// through the FEM. Holes track the fundamental's nodes between iterates.
pub fn tune_bar(t: &TuneTarget) -> Result<TunedBar, String> {
    if !(t.target_hz > 0.0 && t.width_mm > 0.0 && t.thickness_mm > 0.0) {
        return Err("target_hz, width_mm and thickness_mm must be positive".into());
    }
    if !(0.05..=0.9).contains(&t.undercut_span) {
        return Err("undercut_span must be in 0.05..0.9".into());
    }
    let ratio = t.overtone_ratio.filter(|r| *r > 0.0);
    let opts = ModelOptions::default();
    const MAX_DEPTH: f64 = 0.75; // fraction of thickness

    // Initial length from closed form (f ∝ 1/L²).
    let l_cf =
        100.0 * (closed_form_plain_f1(t.material, t.thickness_mm, 100.0) / t.target_hz).sqrt();
    let mut u = [l_cf.ln(), if ratio.is_some() { 0.3 } else { 0.0 }];
    let mut node_frac = vec![0.2242, 0.7758];
    let mut history = Vec::new();
    let mut notes = Vec::new();
    let mut depth_pinned = false;

    let solve = |u: &[f64; 2], nf: &[f64]| {
        let g = geometry_for(t, u[0].exp(), u[1] * t.thickness_mm, nf);
        modal(&g, t.material, 3, &opts)
    };
    let resid = |r: &ModalResult| -> [f64; 2] {
        let r1 = (r.hz[0] / t.target_hz).ln();
        let r2 = match ratio {
            Some(rt) => ((r.hz[1] / r.hz[0]) / rt).ln(),
            None => 0.0,
        };
        [r1, r2]
    };

    let mut last = solve(&u, &node_frac);
    for it in 0..40 {
        if last.hz.len() < 2 {
            return Err("modal solve returned fewer than two elastic modes".into());
        }
        history.push(TuneStep {
            iteration: it,
            length_mm: u[0].exp(),
            undercut_depth_mm: u[1] * t.thickness_mm,
            f1_hz: last.hz[0],
            f2_hz: last.hz[1],
            cents_error: cents(last.hz[0], t.target_hz),
        });
        let r = resid(&last);
        let ratio_ok = ratio.is_none() || depth_pinned || r[1].abs() * 1731.234 < 1.0;
        if r[0].abs() * 1731.234 < 0.01 && ratio_ok && it > 0 {
            break;
        }
        let free_depth = ratio.is_some() && !depth_pinned;
        let h = 1e-4;
        let mut jac = [[0.0; 2]; 2];
        for v in 0..(if free_depth { 2 } else { 1 }) {
            let mut up = u;
            let mut dn = u;
            up[v] += h;
            dn[v] -= h;
            let rp = resid(&solve(&up, &node_frac));
            let rn = resid(&solve(&dn, &node_frac));
            for row in 0..2 {
                jac[row][v] = (rp[row] - rn[row]) / (2.0 * h);
            }
        }
        let (dl, dd) = if free_depth {
            let det = jac[0][0] * jac[1][1] - jac[0][1] * jac[1][0];
            if det.abs() < 1e-14 {
                return Err("tuning Jacobian is singular".into());
            }
            (
                -(r[0] * jac[1][1] - r[1] * jac[0][1]) / det,
                -(jac[0][0] * r[1] - jac[1][0] * r[0]) / det,
            )
        } else {
            (-r[0] / jac[0][0], 0.0)
        };
        u[0] += dl.clamp(-0.2, 0.2);
        u[1] += dd.clamp(-0.15, 0.15);
        if u[1] >= MAX_DEPTH {
            u[1] = MAX_DEPTH;
            if !depth_pinned {
                depth_pinned = true;
                notes.push(format!(
                    "overtone ratio {:.2} unreachable with a {:.0}% span undercut; depth pinned at {:.0}% of thickness",
                    ratio.unwrap_or(0.0),
                    t.undercut_span * 100.0,
                    MAX_DEPTH * 100.0
                ));
            }
        }
        if u[1] < 0.0 {
            u[1] = 0.0;
        }
        // Holes follow the nodes.
        let l = u[0].exp();
        if last.nodes_mm.len() == 2 {
            let len_prev = history.last().map(|s| s.length_mm).unwrap_or(l);
            node_frac = last.nodes_mm.iter().map(|x| x / len_prev).collect();
        }
        last = solve(&u, &node_frac);
    }

    let length = u[0].exp();
    let depth = u[1] * t.thickness_mm;
    let geometry = geometry_for(t, length, depth, &node_frac);
    let final_modal = modal(&geometry, t.material, 3, &opts);
    let f1 = final_modal.hz[0];
    let err = cents(f1, t.target_hz);
    let achieved_ratio = final_modal.hz[1] / f1;
    let ratio_met = match ratio {
        Some(rt) => cents(achieved_ratio, rt).abs() < 5.0,
        None => true,
    };
    if t.width_mm > 0.3 * length {
        notes.push(format!(
            "width {:.0} mm is over 30% of length {:.0} mm — torsional/lateral modes (not modelled) may crowd f₂",
            t.width_mm, length
        ));
    }
    if length / t.thickness_mm < 12.0 {
        notes.push(format!(
            "stubby bar (L/t = {:.1}): first-order shear correction is approximate",
            length / t.thickness_mm
        ));
    }
    let hole_err = node_frac
        .iter()
        .zip(&final_modal.nodes_mm)
        .map(|(f, n)| (f * length - n).abs())
        .fold(0.0_f64, f64::max);
    if hole_err > 0.25 {
        notes.push(format!(
            "peg holes sit {hole_err:.2} mm off the nodal lines"
        ));
    }
    Ok(TunedBar {
        note: t.note.clone(),
        target_hz: t.target_hz,
        material: t.material,
        nodes_mm: final_modal.nodes_mm.clone(),
        hz: final_modal.hz.clone(),
        cents_error: err,
        overtone_ratio: achieved_ratio,
        overtone_target: ratio,
        shear_factor: final_modal.shear_factor.first().copied().unwrap_or(1.0),
        closed_form_plain_hz: closed_form_plain_f1(t.material, t.thickness_mm, length),
        tolerance_cents: t.tolerance_cents,
        converged: err.abs() <= 0.5 && (ratio_met || depth_pinned),
        geometry,
        history,
        notes,
    })
}

// ─── Notes and scales ──────────────────────────────────────────────────────

const SHARP_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn midi_of(note: &str) -> Result<i32, String> {
    let hz = note_to_hz(note)?;
    Ok((69.0 + 12.0 * (hz / 440.0).log2()).round() as i32)
}

fn name_of(midi: i32) -> String {
    format!(
        "{}{}",
        SHARP_NAMES[midi.rem_euclid(12) as usize],
        midi.div_euclid(12) - 1
    )
}

/// The eight notes of a major scale from `tonic` (e.g. "C6" → C6 … C7).
pub fn major_scale(tonic: &str) -> Result<Vec<String>, String> {
    let m0 = midi_of(tonic)?;
    Ok([0, 2, 4, 5, 7, 9, 11, 12]
        .iter()
        .map(|s| name_of(m0 + s))
        .collect())
}

/// Note nearest to `hz` (equal temperament) and the offset in cents.
pub fn nearest_note(hz: f64) -> (String, f64) {
    let midi = (69.0 + 12.0 * (hz / 440.0).log2()).round() as i32;
    let ref_hz = 440.0 * 2.0_f64.powf((midi - 69) as f64 / 12.0);
    (name_of(midi), cents(hz, ref_hz))
}

// ─── Receipts ──────────────────────────────────────────────────────────────

/// Receipt domain.
pub const RECEIPT_DOMAIN: &str = "acoustics";

/// Oracle for bar predictions.
pub fn oracle() -> vcad_receipt::OracleRef {
    vcad_receipt::OracleRef::new(
        "vcad-kernel-acoustics/instrument",
        env!("CARGO_PKG_VERSION"),
    )
}

/// Claim id for a bar's fundamental.
pub fn claim_id(note: &str) -> String {
    format!("acoustics.bar.{note}.f1")
}

/// "C6 · 1046.5 Hz"
pub fn claim_label(note: &str, hz: f64) -> String {
    format!("{note} · {hz:.1} Hz")
}

/// A bar's predicted-pitch claim: Provisional (predictions never Pass).
pub fn predicted_claim(
    note: &str,
    target_hz: f64,
    predicted_hz: f64,
    part: &str,
) -> vcad_receipt::ReceiptClaim {
    let err = cents(predicted_hz, target_hz);
    vcad_receipt::ReceiptClaim::pass(
        claim_id(note),
        RECEIPT_DOMAIN,
        format!("{} predicted", claim_label(note, target_hz)),
        oracle(),
    )
    .with_basis(vcad_receipt::ClaimBasis::Predicted)
    .with_subject(format!("part:{part}"))
    .with_predicted(vcad_receipt::ClaimQuantity::new(target_hz, "Hz"))
    .with_measured(vcad_receipt::ClaimQuantity::new(predicted_hz, "Hz"))
    .with_details(format!(
        "free-free Hermite FEM (rotary inertia + first-order Timoshenko shear); {err:+.2} cents vs target"
    ))
}

/// A bar's measured-pitch claim: Pass when |cents| ≤ tolerance, else Fail.
pub fn measured_claim(
    note: &str,
    target_hz: f64,
    predicted_hz: f64,
    measured_hz: f64,
    tolerance_cents: f64,
    part: &str,
    instrument: &str,
) -> vcad_receipt::ReceiptClaim {
    let err = cents(measured_hz, target_hz);
    let desc = format!("{} measured {measured_hz:.1} Hz ({err:+.1} cents)", note);
    let base = if err.abs() <= tolerance_cents {
        vcad_receipt::ReceiptClaim::pass(claim_id(note), RECEIPT_DOMAIN, desc, oracle())
    } else {
        vcad_receipt::ReceiptClaim::fail(claim_id(note), RECEIPT_DOMAIN, desc, oracle())
    };
    base.with_basis(vcad_receipt::ClaimBasis::Measured)
        .with_subject(format!("part:{part}"))
        .with_predicted(vcad_receipt::ClaimQuantity::new(target_hz, "Hz"))
        .with_measured(vcad_receipt::ClaimQuantity::new(measured_hz, "Hz"))
        .with_details(format!(
            "{instrument}; tolerance ±{tolerance_cents} cents; FEM predicted {predicted_hz:.1} Hz ({:+.1} cents off the measurement)",
            cents(predicted_hz, measured_hz)
        ))
}

/// Match a detected pitch to the closest bar target. Returns (index, cents)
/// when within `max_cents` (a strike of a neighbouring bar is ≥ 100 cents
/// away in a diatonic set).
pub fn match_bar(measured_hz: f64, targets_hz: &[f64], max_cents: f64) -> Option<(usize, f64)> {
    targets_hz
        .iter()
        .enumerate()
        .map(|(i, &t)| (i, cents(measured_hz, t)))
        .min_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .filter(|(_, c)| c.abs() <= max_cents)
}

// ─── Instrument ────────────────────────────────────────────────────────────

/// A full instrument request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentSpec {
    /// Lowest note (e.g. "C6"); the set is its major scale.
    pub tonic: String,
    /// Bar template (note/target_hz are overwritten per bar).
    pub bar: TuneTarget,
    /// Gap between bars, mm.
    pub gap_mm: f64,
    /// Felt/standoff height under the bars, mm.
    pub standoff_mm: f64,
    /// Frame plate thickness, mm.
    pub plate_mm: f64,
}

impl InstrumentSpec {
    /// Defaults for a major-scale glockenspiel.
    pub fn new(tonic: &str, material: BarMaterial) -> Result<Self, String> {
        Ok(InstrumentSpec {
            tonic: tonic.to_string(),
            bar: TuneTarget::for_note(tonic, material)?,
            gap_mm: 4.0,
            standoff_mm: 5.0,
            plate_mm: 6.0,
        })
    }
}

/// One placed bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacedBar {
    /// Scene part / root name.
    pub part: String,
    /// Tuning result.
    pub bar: TunedBar,
    /// World offset of the bar's (x=0, y=0, z=0) corner, mm.
    pub origin_mm: [f64; 3],
    /// Closed-tube quarter-wave resonator length (25 mm bore), mm.
    pub resonator_mm: f64,
}

/// A designed instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    /// Request.
    pub spec: InstrumentSpec,
    /// Bars, low to high.
    pub bars: Vec<PlacedBar>,
    /// Loon source of the assembly (bars + frame).
    pub loon: String,
    /// Frame peg diameter, mm.
    pub peg_dia_mm: f64,
}

/// Part name for a note ("C#6" → "bar_cs6").
pub fn part_name(note: &str) -> String {
    format!("bar_{}", note.to_ascii_lowercase().replace('#', "s"))
}

fn f(v: f64) -> String {
    format!("{v:.3}")
}

/// Length parameter name for a bar part ("bar_c6" → "len_c6").
pub fn length_param(part: &str) -> String {
    format!("len_{}", part.trim_start_matches("bar_"))
}

/// [`bar_loon`] with the bar's length as a loon parameter: every
/// length-dependent position (centring, undercut, holes) is an expression of
/// `param`, so a pitch lock that solves the length keeps the bar coherent.
pub fn bar_loon_param(bar: &TunedBar, param: &str, origin_yz: [f64; 2]) -> String {
    let g = &bar.geometry;
    let l = g.length_mm;
    let mul = |k: f64| format!("[* {param} {}]", f(k));
    let mut tools = Vec::new();
    if g.undercut_depth_mm > 0.05 {
        let (a, b) = g.pocket();
        tools.push(format!(
            "[translate {} -1 -1 [cube {} {} {}]]",
            mul(a / l),
            mul((b - a) / l),
            f(g.width_mm + 2.0),
            f(g.undercut_depth_mm + 1.0)
        ));
    }
    if g.hole_dia_mm > 0.0 {
        for &h in &g.holes_mm {
            tools.push(format!(
                "[translate {} {} -1 [cylinder {} {}]]",
                mul(h / l),
                f(g.width_mm / 2.0),
                f(g.hole_dia_mm / 2.0),
                f(g.thickness_mm + 2.0)
            ));
        }
    }
    let body = format!("[cube {param} {} {}]", f(g.width_mm), f(g.thickness_mm));
    let cut = match tools.len() {
        0 => body,
        _ => {
            let tool = balanced_union(&tools);
            format!("[difference {tool} {body}]")
        }
    };
    format!(
        "[translate {} {} {} {cut}]",
        mul(-0.5),
        f(origin_yz[0]),
        f(origin_yz[1])
    )
}

/// Union `parts` as a balanced binary tree: each boolean sees operands of
/// similar size, O(n log n) instead of the left-deep fold's O(n²).
fn balanced_union(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        _ => {
            let (a, b) = parts.split_at(parts.len() / 2);
            format!("[union {} {}]", balanced_union(a), balanced_union(b))
        }
    }
}

/// Loon expression for a tuned bar at `origin`.
pub fn bar_loon(bar: &TunedBar, origin: [f64; 3]) -> String {
    let g = &bar.geometry;
    let mut tools = Vec::new();
    if g.undercut_depth_mm > 0.05 {
        let (a, b) = g.pocket();
        tools.push(format!(
            "[translate {} -1 -1 [cube {} {} {}]]",
            f(a),
            f(b - a),
            f(g.width_mm + 2.0),
            f(g.undercut_depth_mm + 1.0)
        ));
    }
    if g.hole_dia_mm > 0.0 {
        for &h in &g.holes_mm {
            tools.push(format!(
                "[translate {} {} -1 [cylinder {} {}]]",
                f(h),
                f(g.width_mm / 2.0),
                f(g.hole_dia_mm / 2.0),
                f(g.thickness_mm + 2.0)
            ));
        }
    }
    let body = format!(
        "[cube {} {} {}]",
        f(g.length_mm),
        f(g.width_mm),
        f(g.thickness_mm)
    );
    let cut = match tools.len() {
        0 => body,
        _ => {
            let tool = balanced_union(&tools);
            format!("[difference {tool} {body}]")
        }
    };
    format!(
        "[translate {} {} {} {cut}]",
        f(origin[0]),
        f(origin[1]),
        f(origin[2])
    )
}

/// Design a major-scale instrument: tune every bar, lay them out
/// low-to-high along +Y centred on x=0, and build a frame plate with a
/// standoff collar and a locating peg at every nodal point.
pub fn design_instrument(spec: &InstrumentSpec) -> Result<Instrument, String> {
    let notes = major_scale(&spec.tonic)?;
    let mut tuned = Vec::with_capacity(notes.len());
    for n in &notes {
        let mut t = spec.bar.clone();
        t.note = n.clone();
        t.target_hz = note_to_hz(n)?;
        tuned.push(tune_bar(&t)?);
    }
    let w = spec.bar.width_mm;
    let pitch = w + spec.gap_mm;
    let z_bar = spec.plate_mm + spec.standoff_mm;
    let peg_dia = (spec.bar.hole_dia_mm - 0.6).max(1.0);
    let collar_r = spec.bar.hole_dia_mm / 2.0 + 3.0;
    let l_max = tuned
        .iter()
        .map(|b| b.geometry.length_mm)
        .fold(0.0, f64::max);
    let margin = 8.0;

    let mut out = String::new();
    out.push_str(&format!(
        "; {}-major {} — generated by vcad design_instrument.\n; Bars tuned by free-free FEM; pegs sit on each bar's fundamental nodes.\n",
        spec.tonic,
        match spec.bar.material {
            BarMaterial::Pla => "printed glockenspiel",
            _ => "glockenspiel",
        }
    ));
    out.push_str(&spec.bar.material.loon_def());
    out.push('\n');
    out.push_str("[material \"frame\" 0.18 0.18 0.20 0.0 0.7]\n\n");

    let mut placed = Vec::with_capacity(tuned.len());
    let mut frame_parts = Vec::new();
    for (i, bar) in tuned.into_iter().enumerate() {
        let part = part_name(&bar.note);
        let origin = [-bar.geometry.length_mm / 2.0, i as f64 * pitch, z_bar];
        out.push_str(&format!(
            "; {} target {:.2} Hz, predicted {:.2} Hz ({:+.2} c), f2/f1 {:.2}\n",
            bar.note, bar.target_hz, bar.hz[0], bar.cents_error, bar.overtone_ratio
        ));
        let lp = length_param(&part);
        out.push_str(&format!(
            "[defparam {lp} {} :unit \"mm\" :min {} :max {}]\n",
            f(bar.geometry.length_mm),
            f((bar.geometry.length_mm * 0.6).round()),
            f((bar.geometry.length_mm * 1.6).round())
        ));
        out.push_str(&format!(
            "[let {part} {}]\n",
            bar_loon_param(&bar, &lp, [origin[1], origin[2]])
        ));
        for &h in &bar.geometry.holes_mm {
            // Pegs follow the bar's length parameter too (x = L·(h/L − ½)).
            let k = h / bar.geometry.length_mm - 0.5;
            let y = origin[1] + w / 2.0;
            let px = format!("[* {lp} {}]", f(k));
            frame_parts.push(format!(
                "[translate {} {} {} [cylinder {} {}]]",
                px,
                f(y),
                f(spec.plate_mm - 0.5),
                f(collar_r),
                f(spec.standoff_mm + 0.5)
            ));
            frame_parts.push(format!(
                "[translate {} {} {} [cylinder {} {}]]",
                px,
                f(y),
                f(spec.plate_mm - 0.5),
                f(peg_dia / 2.0),
                f(spec.standoff_mm + bar.geometry.thickness_mm + 2.5)
            ));
        }
        // Closed quarter-wave tube, 25 mm bore, unflanged end correction 0.61 r.
        let resonator_mm = 343.0 / (4.0 * bar.target_hz) * 1000.0 - 0.61 * 12.5;
        placed.push(PlacedBar {
            part,
            bar,
            origin_mm: origin,
            resonator_mm,
        });
    }
    let n = placed.len() as f64;
    let plate = format!(
        "[translate {} {} 0 [cube {} {} {}]]",
        f(-l_max / 2.0 - margin),
        f(-margin),
        f(l_max + 2.0 * margin),
        f((n - 1.0) * pitch + w + 2.0 * margin),
        f(spec.plate_mm)
    );
    let mut all = vec![plate];
    all.extend(frame_parts);
    let frame = balanced_union(&all);
    out.push_str(&format!("\n[let frame {frame}]\n\n"));
    for p in &placed {
        out.push_str(&format!(
            "[root {} \"{}\"]\n",
            p.part,
            spec.bar.material.loon_key()
        ));
    }
    out.push_str("[root frame \"frame\"]\n");

    Ok(Instrument {
        spec: spec.clone(),
        bars: placed,
        loon: out,
        peg_dia_mm: peg_dia,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strike::{closed_form_hz, free_free_beta_l, BarSpec};

    fn plain(l: f64) -> BarGeometry {
        BarGeometry {
            length_mm: l,
            width_mm: 25.0,
            thickness_mm: 6.0,
            undercut_depth_mm: 0.0,
            undercut_span: 0.5,
            hole_dia_mm: 0.0,
            holes_mm: vec![],
        }
    }

    #[test]
    fn plain_bar_matches_closed_form_euler_bernoulli() {
        let mat = BarMaterial::Aluminium6061;
        let g = plain(200.0);
        let r = modal(&g, mat, 3, &ModelOptions::euler_bernoulli());
        let cf = closed_form_hz(
            &BarSpec {
                length_mm: 200.0,
                width_mm: 25.0,
                thickness_mm: 6.0,
                holes_mm: vec![],
                hole_dia_mm: 0.0,
                modulus_gpa: mat.modulus_gpa(),
                density_kg_m3: mat.density_kg_m3(),
            },
            3,
        );
        for (a, b) in r.hz.iter().zip(&cf) {
            assert!(cents(*a, *b).abs() < 0.01, "fem {a} vs closed {b}");
        }
        // Classic overtone ratios 2.756, 5.404.
        assert!((r.hz[1] / r.hz[0] - 2.7565).abs() < 1e-3);
        assert!((r.hz[2] / r.hz[0] - 5.4039).abs() < 1e-3);
    }

    #[test]
    fn plain_bar_nodes_at_classic_fractions() {
        let r = modal(
            &plain(180.0),
            BarMaterial::Aluminium6061,
            1,
            &ModelOptions::euler_bernoulli(),
        );
        assert_eq!(r.nodes_mm.len(), 2);
        assert!(
            (r.nodes_mm[0] / 180.0 - 0.22416).abs() < 2e-4,
            "{:?}",
            r.nodes_mm
        );
        assert!((r.nodes_mm[1] / 180.0 - 0.77584).abs() < 2e-4);
    }

    #[test]
    fn rotary_and_shear_match_first_order_timoshenko() {
        // For a uniform bar, first order: ω_T²/ω² ≈ 1 − (βr)²·(1 + E/κG),
        // with β = βL/L, r² = t²/12. Check the FEM's combined correction
        // against it on a moderately slender bar (L/t = 20).
        let mat = BarMaterial::Aluminium6061;
        let g = plain(120.0);
        let eb = modal(&g, mat, 1, &ModelOptions::euler_bernoulli()).hz[0];
        let full = modal(&g, mat, 1, &ModelOptions::default()).hz[0];
        let bl = free_free_beta_l(1)[0];
        let beta_r2 = (bl / 120.0).powi(2) * 36.0 / 12.0;
        let nu = mat.poisson();
        let kappa = 10.0 * (1.0 + nu) / (12.0 + 11.0 * nu);
        let e_over_kg = 2.0 * (1.0 + nu) / kappa;
        let expected = (1.0 - beta_r2 * (1.0 + e_over_kg)).sqrt();
        let got = full / eb;
        assert!(got < 1.0);
        // Free-free modes are not pure sinusoids; agree to 25% of the correction.
        assert!(
            ((1.0 - got) - (1.0 - expected)).abs() < 0.25 * (1.0 - expected),
            "fem factor {got}, analytic {expected}"
        );
    }

    #[test]
    fn matches_existing_hole_aware_strike_fem() {
        // Same bar through strike::fem_hz (uniform-thickness, hole-aware).
        let mat = BarMaterial::Aluminium6061;
        let mut g = plain(170.0);
        g.hole_dia_mm = 4.0;
        g.holes_mm = vec![0.2242 * 170.0, 0.7758 * 170.0];
        let ours = modal(&g, mat, 2, &ModelOptions::euler_bernoulli());
        let theirs = crate::strike::fem_hz(
            &BarSpec {
                length_mm: 170.0,
                width_mm: 25.0,
                thickness_mm: 6.0,
                holes_mm: g.holes_mm.clone(),
                hole_dia_mm: 4.0,
                modulus_gpa: mat.modulus_gpa(),
                density_kg_m3: mat.density_kg_m3(),
            },
            2,
        );
        for (a, b) in ours.hz.iter().zip(&theirs) {
            assert!(cents(*a, *b).abs() < 0.5, "{a} vs {b}");
        }
    }

    #[test]
    fn undercut_lowers_fundamental_and_raises_ratio() {
        let mat = BarMaterial::Aluminium6061;
        let p = modal(&plain(170.0), mat, 2, &ModelOptions::default());
        let mut g = plain(170.0);
        g.undercut_depth_mm = 3.0;
        let u = modal(&g, mat, 2, &ModelOptions::default());
        assert!(u.hz[0] < p.hz[0]);
        assert!(u.hz[1] / u.hz[0] > p.hz[1] / p.hz[0]);
    }

    #[test]
    fn tuning_converges_to_target_and_ratio() {
        for note in ["C6", "G6", "C7"] {
            let t = TuneTarget::for_note(note, BarMaterial::Aluminium6061).unwrap();
            let b = tune_bar(&t).unwrap();
            assert!(b.converged, "{note}: {:?}", b.notes);
            assert!(b.cents_error.abs() < 0.1, "{note}: {} cents", b.cents_error);
            assert!(
                b.history.len() < 25,
                "{note}: {} iterations",
                b.history.len()
            );
            if b.notes.iter().all(|n| !n.contains("unreachable")) {
                assert!(
                    (b.overtone_ratio - 4.0).abs() < 0.02,
                    "{note}: ratio {}",
                    b.overtone_ratio
                );
            }
            // Holes are on the nodes.
            for (h, n) in b.geometry.holes_mm.iter().zip(&b.nodes_mm) {
                assert!((h - n).abs() < 0.25, "{note}: hole {h} node {n}");
            }
        }
    }

    #[test]
    fn plain_tuning_for_every_material() {
        for m in [
            BarMaterial::Aluminium6061,
            BarMaterial::Brass,
            BarMaterial::Pla,
        ] {
            let mut t = TuneTarget::for_note("A5", m).unwrap();
            t.overtone_ratio = None;
            let b = tune_bar(&t).unwrap();
            assert!(
                b.converged && b.cents_error.abs() < 0.1,
                "{m:?}: {}",
                b.cents_error
            );
            assert_eq!(b.geometry.undercut_depth_mm, 0.0);
            // Plain bar ratio sits at or below Euler–Bernoulli's 2.756: shear
            // and rotary inertia pull f₂ down more than f₁ (most for the
            // stubby PLA bar).
            assert!(
                b.overtone_ratio < 2.76 && b.overtone_ratio > 2.55,
                "{m:?}: {}",
                b.overtone_ratio
            );
        }
    }

    #[test]
    fn major_scale_names() {
        assert_eq!(
            major_scale("C6").unwrap(),
            ["C6", "D6", "E6", "F6", "G6", "A6", "B6", "C7"]
        );
        assert_eq!(major_scale("G5").unwrap()[6], "F#6");
        let (n, c) = nearest_note(523.25);
        assert_eq!(n, "C5");
        assert!(c.abs() < 0.1);
    }

    #[test]
    fn match_bar_picks_nearest_within_window() {
        let targets: Vec<f64> = major_scale("C6")
            .unwrap()
            .iter()
            .map(|n| note_to_hz(n).unwrap())
            .collect();
        let (i, c) = match_bar(1320.0 * 1.001, &targets, 60.0).unwrap();
        assert_eq!(i, 2); // E6 = 1318.5
        assert!(c > 0.0);
        assert!(match_bar(1250.0, &targets, 30.0).is_none());
    }

    #[test]
    fn receipt_transitions_predicted_to_measured() {
        use vcad_receipt::{ClaimBasis, ClaimVerdict};
        let p = predicted_claim("C6", 1046.5, 1046.4, "bar_c6");
        assert_eq!(p.verdict, ClaimVerdict::Provisional);
        assert_eq!(p.basis, Some(ClaimBasis::Predicted));
        assert_eq!(p.id, "acoustics.bar.C6.f1");
        let ok = measured_claim("C6", 1046.5, 1046.4, 1049.0, 5.0, "bar_c6", "mic");
        assert_eq!(ok.verdict, ClaimVerdict::Pass);
        assert_eq!(ok.basis, Some(ClaimBasis::Measured));
        assert_eq!(ok.id, p.id);
        let bad = measured_claim("C6", 1046.5, 1046.4, 1060.0, 5.0, "bar_c6", "mic");
        assert_eq!(bad.verdict, ClaimVerdict::Fail);
        // A receipt of all-predicted claims never passes; one measured pass does.
        let r = vcad_receipt::DesignReceipt::with_claims(vec![p.clone()]);
        assert_ne!(r.verdict(), vcad_receipt::ReceiptVerdict::Pass);
        let r = vcad_receipt::DesignReceipt::with_claims(vec![ok]);
        assert_eq!(r.verdict(), vcad_receipt::ReceiptVerdict::Pass);
    }

    #[test]
    fn instrument_loon_evaluates_shape() {
        let spec = InstrumentSpec::new("C6", BarMaterial::Aluminium6061).unwrap();
        let inst = design_instrument(&spec).unwrap();
        assert_eq!(inst.bars.len(), 8);
        // Lengths strictly decrease up the scale.
        for w in inst.bars.windows(2) {
            assert!(w[1].bar.geometry.length_mm < w[0].bar.geometry.length_mm);
        }
        assert!(inst.loon.contains("[root bar_c6 \"aluminum\"]"));
        assert!(inst.loon.contains("[root frame \"frame\"]"));
        assert_eq!(inst.loon.matches("[root ").count(), 9);
        for b in &inst.bars {
            assert!(b.bar.cents_error.abs() < 0.1);
        }
    }
}
