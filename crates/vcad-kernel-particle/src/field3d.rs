//! Posed 3D magnetostatic field sources.
//!
//! Static magnetic fields in free space from coils and permanent magnets
//! placed anywhere with a full rigid pose, superposed into a
//! [`FieldScene`] and sampled at arbitrary 3D points. The rest of this crate
//! is axisymmetric: [`crate::field::FieldMap`] handles rings coaxial with Z
//! only. This module is the general case: a magnet over a dish, a pair of
//! confinement coils at an angle, the magnets of a motor rotor.
//!
//! Every source is defined in its own **local frame**: centered at the
//! origin, with the coil axis or magnetization along local `+z`. A [`Pose`]
//! (rotation matrix + translation) places it in the world. All units are
//! SI (m, A, T). The mm/degree JSON seam lives in [`crate::field3d_spec`].
//!
//! What is exact and what is discretized:
//!
//! | source | method |
//! |---|---|
//! | [`FieldSource::CurrentLoop`] | exact (complete elliptic integrals, [`crate::field::b_ring`]) |
//! | [`FieldSource::Solenoid`], `n_loops: None` | exact thin finite solenoid (Derby & Olbert 2010, Bulirsch `cel`) |
//! | [`FieldSource::CylinderMagnet`], `n_loops: None` | exact: a uniformly, axially magnetized cylinder or ring is a solenoidal bound-current sheet `K = B_r/μ0` (one sheet per radius) |
//! | either of the two above, `n_loops: Some(n)` | midpoint stack of `n` exact loops (the [`DiscMagnet`] / plasmoid path) |
//! | [`FieldSource::BlockMagnet`] | exact closed form (magnetic-charge sheets on the two poles; Furlani's cuboid result, with the `ln` terms rewritten so they cannot cancel catastrophically) |
//!
//! Magnets are ideal: rigid uniform magnetization (μ_r = 1, no demagnetizing
//! knee, no mutual influence between magnets, no soft iron). For NdFeB
//! (μ_r ≈ 1.05) that is the standard textbook model. Fields inside a magnet
//! are **B** (including the magnetization), not H.

use crate::constants::MU_0;
use crate::field::{b_ring, RingCoil};

// ---------------------------------------------------------------------------
// Small vector helpers
// ---------------------------------------------------------------------------

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
pub(crate) fn normalize(a: [f64; 3]) -> [f64; 3] {
    let n = norm(a);
    [a[0] / n, a[1] / n, a[2] / n]
}
pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
/// Orthonormal `(u, v)` completing unit `w` to a right-handed frame.
pub(crate) fn basis(w: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let a = if w[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = normalize(cross(a, w));
    (u, cross(w, u))
}

type Mat3 = [[f64; 3]; 3];

fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    m
}

fn from_columns(u: [f64; 3], v: [f64; 3], w: [f64; 3]) -> Mat3 {
    [[u[0], v[0], w[0]], [u[1], v[1], w[1]], [u[2], v[2], w[2]]]
}

// ---------------------------------------------------------------------------
// Pose
// ---------------------------------------------------------------------------

/// A rigid placement: `world = position + R · local`.
///
/// `rotation` is a proper orthonormal matrix, row-major (`rotation[i][j]`),
/// whose **columns** are the source's local x, y, z axes in world
/// coordinates. A source's axis / magnetization direction is its local `+z`,
/// i.e. column 2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    /// Origin of the local frame in world coordinates, m.
    pub position_m: [f64; 3],
    /// Rotation, local → world. Columns are the local axes.
    pub rotation: [[f64; 3]; 3],
}

impl Default for Pose {
    fn default() -> Self {
        Self::identity()
    }
}

impl Pose {
    /// The world frame itself.
    pub fn identity() -> Self {
        Self {
            position_m: [0.0; 3],
            rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// Pure translation.
    pub fn translation(position_m: [f64; 3]) -> Self {
        Self {
            position_m,
            ..Self::identity()
        }
    }

    /// Local `+z` along `axis` (need not be normalized; must be nonzero).
    /// The local x/y axes are chosen deterministically — irrelevant for
    /// axisymmetric sources; for a [`FieldSource::BlockMagnet`] use
    /// [`Pose::from_axis_and_x`] or [`Pose::from_euler_deg`] to fix its
    /// roll.
    pub fn from_axis(position_m: [f64; 3], axis: [f64; 3]) -> Self {
        let w = normalize(axis);
        let (u, v) = basis(w);
        Self {
            position_m,
            rotation: from_columns(u, v, w),
        }
    }

    /// Local `+z` along `axis`, local `+x` along the part of `x_hint`
    /// orthogonal to it. `None` if either is zero or they are parallel.
    pub fn from_axis_and_x(position_m: [f64; 3], axis: [f64; 3], x_hint: [f64; 3]) -> Option<Self> {
        if norm(axis) == 0.0 {
            return None;
        }
        let w = normalize(axis);
        let xp = sub(x_hint, scale(w, dot(x_hint, w)));
        if norm(xp) <= 1e-12 * norm(x_hint).max(1e-300) {
            return None;
        }
        let u = normalize(xp);
        Some(Self {
            position_m,
            rotation: from_columns(u, cross(w, u), w),
        })
    }

    /// Rotation from Euler angles in degrees about X, Y, Z, using the same
    /// convention as the IR `Rotate` op and `Solid::rotate`:
    /// `R = Rx(x) · Ry(y) · Rz(z)` (Z applied first, then Y, then X).
    pub fn from_euler_deg(position_m: [f64; 3], angles_deg: [f64; 3]) -> Self {
        let (sx, cx) = angles_deg[0].to_radians().sin_cos();
        let (sy, cy) = angles_deg[1].to_radians().sin_cos();
        let (sz, cz) = angles_deg[2].to_radians().sin_cos();
        let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
        let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
        let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
        Self {
            position_m,
            rotation: mat_mul(&mat_mul(&rx, &ry), &rz),
        }
    }

    /// Rotation by `angle_rad` about `axis` (right-hand rule, Rodrigues).
    pub fn from_axis_angle(position_m: [f64; 3], axis: [f64; 3], angle_rad: f64) -> Self {
        let k = normalize(axis);
        let (s, c) = angle_rad.sin_cos();
        let t = 1.0 - c;
        let rotation = [
            [
                c + k[0] * k[0] * t,
                k[0] * k[1] * t - k[2] * s,
                k[0] * k[2] * t + k[1] * s,
            ],
            [
                k[1] * k[0] * t + k[2] * s,
                c + k[1] * k[1] * t,
                k[1] * k[2] * t - k[0] * s,
            ],
            [
                k[2] * k[0] * t - k[1] * s,
                k[2] * k[1] * t + k[0] * s,
                c + k[2] * k[2] * t,
            ],
        ];
        Self {
            position_m,
            rotation,
        }
    }

    /// The source's axis (local `+z`) in world coordinates.
    pub fn axis(&self) -> [f64; 3] {
        [self.rotation[0][2], self.rotation[1][2], self.rotation[2][2]]
    }

    /// World point → local point.
    pub fn to_local(&self, p: [f64; 3]) -> [f64; 3] {
        let d = sub(p, self.position_m);
        let r = &self.rotation;
        [
            r[0][0] * d[0] + r[1][0] * d[1] + r[2][0] * d[2],
            r[0][1] * d[0] + r[1][1] * d[1] + r[2][1] * d[2],
            r[0][2] * d[0] + r[1][2] * d[1] + r[2][2] * d[2],
        ]
    }

    /// Local vector → world vector (rotation only).
    pub fn vec_to_world(&self, v: [f64; 3]) -> [f64; 3] {
        let r = &self.rotation;
        [
            r[0][0] * v[0] + r[0][1] * v[1] + r[0][2] * v[2],
            r[1][0] * v[0] + r[1][1] * v[1] + r[1][2] * v[2],
            r[2][0] * v[0] + r[2][1] * v[1] + r[2][2] * v[2],
        ]
    }

    /// Local point → world point.
    pub fn to_world(&self, p: [f64; 3]) -> [f64; 3] {
        add(self.position_m, self.vec_to_world(p))
    }

    /// `self ∘ inner`: place `inner` (expressed in this pose's frame) in
    /// the world — e.g. a magnet posed on a part that is itself posed.
    pub fn compose(&self, inner: &Pose) -> Pose {
        Pose {
            position_m: self.to_world(inner.position_m),
            rotation: mat_mul(&self.rotation, &inner.rotation),
        }
    }
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

// ---------------------------------------------------------------------------
// Field sources
// ---------------------------------------------------------------------------

/// A magnetostatic source in its local frame (centered, axis along `+z`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldSource {
    /// A circular current loop in the local xy plane, current CCW about
    /// `+z` for positive `ampere_turns`.
    CurrentLoop {
        /// Loop radius, m.
        radius_m: f64,
        /// Current × turns, A.
        ampere_turns: f64,
        /// Conductor radius, m. Inside it the field is regularized
        /// (uniform-current-density interior); `0` = filamentary, and a
        /// query exactly on the filament returns NaN.
        wire_radius_m: f64,
    },
    /// A thin finite solenoid (current sheet) of radius `radius_m`
    /// spanning `z ∈ [−L/2, L/2]`, total current × turns `ampere_turns`.
    Solenoid {
        /// Winding radius, m.
        radius_m: f64,
        /// Winding length, m.
        length_m: f64,
        /// Total current × turns (N·I), A.
        ampere_turns: f64,
        /// `None` = exact closed form; `Some(n)` = stack of `n` loops.
        n_loops: Option<usize>,
    },
    /// An axially magnetized cylinder (`inner_radius_m = 0`) or ring
    /// magnet spanning `z ∈ [−L/2, L/2]`, north pole at `+z`.
    CylinderMagnet {
        /// Outer radius, m.
        outer_radius_m: f64,
        /// Inner (bore) radius, m. `0` for a solid disc/rod.
        inner_radius_m: f64,
        /// Length along the magnetization, m.
        length_m: f64,
        /// Remanence B_r, T (N52 ≈ 1.43).
        remanence_t: f64,
        /// `None` = exact closed form; `Some(n)` = stack of `n` loops per
        /// bound-current sheet.
        n_loops: Option<usize>,
    },
    /// A rectangular block magnet centered at the origin, edges along the
    /// local axes, magnetized along local `+z` (north pole at `+z`).
    BlockMagnet {
        /// Full edge lengths along local x, y, z, m.
        size_m: [f64; 3],
        /// Remanence B_r, T.
        remanence_t: f64,
    },
}

impl FieldSource {
    /// Field in the local frame at local point `p`, tesla.
    pub fn b_local(&self, p: [f64; 3]) -> [f64; 3] {
        match *self {
            FieldSource::CurrentLoop {
                radius_m,
                ampere_turns,
                wire_radius_m,
            } => {
                let rho = (p[0] * p[0] + p[1] * p[1]).sqrt();
                if wire_radius_m <= 0.0 {
                    let dw = ((rho - radius_m).powi(2) + p[2] * p[2]).sqrt();
                    if dw <= 1e-12 * radius_m {
                        return [f64::NAN; 3];
                    }
                }
                let coil = RingCoil {
                    radius_m,
                    z_m: 0.0,
                    ampere_turns,
                    wire_radius_m: wire_radius_m.max(0.0),
                };
                let (br, bz) = b_ring(&coil, rho, p[2]);
                radial_to_cart(br, bz, p, rho)
            }
            FieldSource::Solenoid {
                radius_m,
                length_m,
                ampere_turns,
                n_loops,
            } => sheet(radius_m, length_m, ampere_turns / length_m, n_loops, p),
            FieldSource::CylinderMagnet {
                outer_radius_m,
                inner_radius_m,
                length_m,
                remanence_t,
                n_loops,
            } => {
                let k = remanence_t / MU_0;
                let outer = sheet(outer_radius_m, length_m, k, n_loops, p);
                if inner_radius_m <= 0.0 {
                    return outer;
                }
                let inner = sheet(inner_radius_m, length_m, k, n_loops, p);
                sub(outer, inner)
            }
            FieldSource::BlockMagnet {
                size_m,
                remanence_t,
            } => block_b_local(size_m, remanence_t, p),
        }
    }

    /// World-frame field of this source placed at `pose`, at world point
    /// `p`, tesla.
    pub fn b_at(&self, pose: &Pose, p: [f64; 3]) -> [f64; 3] {
        pose.vec_to_world(self.b_local(pose.to_local(p)))
    }

    /// Magnetic dipole moment along local `+z`, A·m² — the far-field
    /// strength (see [`b_dipole`]).
    pub fn moment(&self) -> f64 {
        use std::f64::consts::PI;
        match *self {
            FieldSource::CurrentLoop {
                radius_m,
                ampere_turns,
                ..
            } => ampere_turns * PI * radius_m * radius_m,
            FieldSource::Solenoid {
                radius_m,
                ampere_turns,
                ..
            } => ampere_turns * PI * radius_m * radius_m,
            FieldSource::CylinderMagnet {
                outer_radius_m,
                inner_radius_m,
                length_m,
                remanence_t,
                ..
            } => {
                remanence_t / MU_0
                    * PI
                    * (outer_radius_m * outer_radius_m - inner_radius_m * inner_radius_m)
                    * length_m
            }
            FieldSource::BlockMagnet {
                size_m,
                remanence_t,
            } => remanence_t / MU_0 * size_m[0] * size_m[1] * size_m[2],
        }
    }

    /// Short machine-readable kind name.
    pub fn kind(&self) -> &'static str {
        match self {
            FieldSource::CurrentLoop { .. } => "loop",
            FieldSource::Solenoid { .. } => "solenoid",
            FieldSource::CylinderMagnet { .. } => "cylinder_magnet",
            FieldSource::BlockMagnet { .. } => "block_magnet",
        }
    }

    /// How the field is evaluated: `"exact"` or `"loop_stack(n)"`.
    pub fn method(&self) -> String {
        match self {
            FieldSource::Solenoid {
                n_loops: Some(n), ..
            }
            | FieldSource::CylinderMagnet {
                n_loops: Some(n), ..
            } => format!("loop_stack({n})"),
            _ => "exact".to_string(),
        }
    }
}

fn radial_to_cart(br: f64, bz: f64, p: [f64; 3], rho: f64) -> [f64; 3] {
    if rho < 1e-15 {
        return [0.0, 0.0, bz];
    }
    [br * p[0] / rho, br * p[1] / rho, bz]
}

/// Solenoidal current sheet of radius `radius`, length `length`, surface
/// current density `k_a_per_m` (A/m, CCW about +z), centered at the origin.
fn sheet(radius: f64, length: f64, k_a_per_m: f64, n_loops: Option<usize>, p: [f64; 3]) -> [f64; 3] {
    match n_loops {
        Some(n) => sheet_loops(radius, length, k_a_per_m, n, p),
        None => sheet_exact(radius, length, k_a_per_m, p),
    }
}

/// Midpoint stack of `n` exact loops (conductor radius `dz/2`).
///
/// Kept arithmetically identical to the original `plasmoid::DiscMagnet`
/// evaluation so that model's calibrated thresholds do not move.
fn sheet_loops(radius: f64, length: f64, k_a_per_m: f64, n: usize, p: [f64; 3]) -> [f64; 3] {
    let n = n.max(1);
    let dz = length / n as f64;
    let at = k_a_per_m * dz;
    let rho = (p[0] * p[0] + p[1] * p[1]).sqrt();
    let (mut br, mut bz) = (0.0, 0.0);
    for i in 0..n {
        let coil = RingCoil {
            radius_m: radius,
            z_m: -0.5 * length + (i as f64 + 0.5) * dz,
            ampere_turns: at,
            wire_radius_m: 0.5 * dz,
        };
        let (r, z) = b_ring(&coil, rho, p[2]);
        br += r;
        bz += z;
    }
    radial_to_cart(br, bz, p, rho)
}

/// Exact field of a thin finite solenoid (Derby & Olbert, Am. J. Phys. 78,
/// 229 (2010)), via Bulirsch's generalized complete elliptic integral.
fn sheet_exact(radius: f64, length: f64, k_a_per_m: f64, p: [f64; 3]) -> [f64; 3] {
    let a = radius;
    let b = 0.5 * length;
    let rho = (p[0] * p[0] + p[1] * p[1]).sqrt();
    let z = p[2];
    let b0 = MU_0 * k_a_per_m / std::f64::consts::PI;
    let gamma = (a - rho) / (a + rho);
    let (mut br, mut bz) = (0.0, 0.0);
    for (zs, sign) in [(z + b, 1.0), (z - b, -1.0)] {
        let den = (zs * zs + (rho + a) * (rho + a)).sqrt();
        let alpha = a / den;
        let beta = zs / den;
        let kc = (zs * zs + (a - rho) * (a - rho)).sqrt() / den;
        br += sign * alpha * cel(kc, 1.0, 1.0, -1.0);
        bz += sign * beta * cel(kc, gamma * gamma, 1.0, gamma);
    }
    radial_to_cart(b0 * br, b0 * a / (a + rho) * bz, p, rho)
}

/// Bulirsch's generalized complete elliptic integral
/// `cel(kc, p, c, s) = ∫₀^{π/2} (c cos²φ + s sin²φ) /
/// ((cos²φ + p sin²φ) √(cos²φ + kc² sin²φ)) dφ`.
///
/// NaN at `kc = 0` (the field of a sheet is log-singular on its rim).
pub(crate) fn cel(kc: f64, p: f64, c: f64, s: f64) -> f64 {
    if kc == 0.0 {
        return f64::NAN;
    }
    let errtol = 1e-8; // AGM is quadratic: the next step's error is ~errtol².
    let mut k = kc.abs();
    let mut pp = p;
    let mut cc = c;
    let mut ss = s;
    let mut em = 1.0;
    if p > 0.0 {
        pp = p.sqrt();
        ss = s / pp;
    } else {
        let mut f = kc * kc;
        let mut q = 1.0 - f;
        let g = 1.0 - pp;
        f -= pp;
        q *= ss - c * pp;
        pp = (f / g).sqrt();
        cc = (c - ss) / g;
        ss = -q / (g * g * pp) + cc * pp;
    }
    let mut f = cc;
    cc += ss / pp;
    let mut g = k / pp;
    ss = 2.0 * (ss + f * g);
    pp += g;
    g = em;
    em += k;
    let mut kk = k;
    let mut iters = 0;
    while (g - k).abs() > g * errtol && iters < 60 {
        k = 2.0 * kk.sqrt();
        kk = k * em;
        f = cc;
        cc += ss / pp;
        g = kk / pp;
        ss = 2.0 * (ss + f * g);
        pp += g;
        g = em;
        em += k;
        iters += 1;
    }
    std::f64::consts::FRAC_PI_2 * (ss + cc * em) / (em * (em + pp))
}

/// `ln(y1 + R1) − ln(y2 + R2)` with `R = √(y² + ρ²)`, `y1 ≥ y2`, evaluated
/// without catastrophic cancellation: for negative `y` the identity
/// `ln(y + R) = ln(ρ²) − ln(R − y)` is used, and when both are negative
/// the `ln ρ²` terms cancel analytically (finite on an extended edge line).
fn ln_pair(y1: f64, y2: f64, rho2: f64) -> f64 {
    let r1 = (y1 * y1 + rho2).sqrt();
    let r2 = (y2 * y2 + rho2).sqrt();
    if y2 >= 0.0 {
        ((y1 + r1) / (y2 + r2)).ln()
    } else if y1 < 0.0 {
        ((r2 - y2) / (r1 - y1)).ln()
    } else {
        // y1 ≥ 0 > y2: the point's projection lies on the segment; ρ² = 0
        // means the point is on the edge itself (log-singular).
        if rho2 == 0.0 {
            return f64::INFINITY;
        }
        (y1 + r1).ln() - (rho2.ln() - (r2 - y2).ln())
    }
}

/// Exact field of a uniformly magnetized cuboid (M along +z), local frame.
///
/// Charge model: σ = ±M on the ±z faces, H = (1/4π)∬σ (r − r′)/|r − r′|³,
/// integrated in closed form per face; then B = μ0(H + M·χ_inside).
fn block_b_local(size: [f64; 3], br: f64, p: [f64; 3]) -> [f64; 3] {
    let (a, b, c) = (0.5 * size[0], 0.5 * size[1], 0.5 * size[2]);
    let xs = [p[0] + a, p[0] - a]; // x − x_i, x_1 = −a, x_2 = +a
    let ys = [p[1] + b, p[1] - b];
    let mut h = [0.0; 3];
    for (z0, sigma) in [(c, 1.0), (-c, -1.0)] {
        let zz = p[2] - z0;
        let z2 = zz * zz;
        // G_x = −ln(Y + R): Σ_{ij} (−1)^{i+j} G_x = −Σ_i (−1)^i' ln_pair over j.
        let mut hx = 0.0;
        for (i, &x) in xs.iter().enumerate() {
            let s = if i == 0 { 1.0 } else { -1.0 };
            hx -= s * ln_pair(ys[0], ys[1], x * x + z2);
        }
        let mut hy = 0.0;
        for (j, &y) in ys.iter().enumerate() {
            let s = if j == 0 { 1.0 } else { -1.0 };
            hy -= s * ln_pair(xs[0], xs[1], y * y + z2);
        }
        let mut hz = 0.0;
        if zz != 0.0 {
            for (i, &x) in xs.iter().enumerate() {
                for (j, &y) in ys.iter().enumerate() {
                    let s = if (i + j) % 2 == 0 { 1.0 } else { -1.0 };
                    let r = (x * x + y * y + z2).sqrt();
                    hz += s * (x * y / (zz * r)).atan();
                }
            }
        }
        h[0] += sigma * hx;
        h[1] += sigma * hy;
        h[2] += sigma * hz;
    }
    // μ0 · M/(4π) with M = B_r/μ0.
    let k = br / (4.0 * std::f64::consts::PI);
    // Interior B includes μ0M; on a face, half of it (the average of the
    // two sides, which keeps B_z continuous across the poles).
    let w = |q: f64, half: f64| {
        let d = q.abs();
        if d < half {
            1.0
        } else if d == half {
            0.5
        } else {
            0.0
        }
    };
    let inside = w(p[0], a) * w(p[1], b) * w(p[2], c);
    [k * h[0], k * h[1], k * h[2] + br * inside]
}

// ---------------------------------------------------------------------------
// Disc magnet (the plasmoid's source) + dipole
// ---------------------------------------------------------------------------

/// An axially magnetized cylindrical permanent magnet (disc or rod), SI —
/// the loop-stack [`FieldSource::CylinderMagnet`] with `inner_radius = 0`,
/// kept as a named type for the plasmoid model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiscMagnet {
    /// Radius, m.
    pub radius_m: f64,
    /// Thickness along the magnetization axis, m.
    pub thickness_m: f64,
    /// Remanence B_r, tesla (N52 ≈ 1.43).
    pub remanence_t: f64,
    /// Number of loops the bound-current sheet is discretized into.
    pub n_loops: usize,
}

impl DiscMagnet {
    /// A 10 mm × 5 mm N52 disc — the classic "tiny neodymium magnet".
    pub fn n52_10x5() -> Self {
        Self {
            radius_m: 5e-3,
            thickness_m: 5e-3,
            remanence_t: 1.43,
            n_loops: 48,
        }
    }

    /// Magnetic dipole moment, A·m² (`M·V = B_r V / μ0`).
    pub fn moment(&self) -> f64 {
        self.remanence_t / MU_0
            * std::f64::consts::PI
            * self.radius_m
            * self.radius_m
            * self.thickness_m
    }

    /// This magnet as a general [`FieldSource`] (same loop stack).
    pub fn as_source(&self) -> FieldSource {
        FieldSource::CylinderMagnet {
            outer_radius_m: self.radius_m,
            inner_radius_m: 0.0,
            length_m: self.thickness_m,
            remanence_t: self.remanence_t,
            n_loops: Some(self.n_loops),
        }
    }

    /// Field in the magnet frame (center at origin, magnetization +z),
    /// returned as Cartesian `[Bx, By, Bz]`, tesla.
    pub fn b_local(&self, p: [f64; 3]) -> [f64; 3] {
        sheet_loops(
            self.radius_m,
            self.thickness_m,
            self.remanence_t / MU_0,
            self.n_loops,
            p,
        )
    }

    /// Closed-form on-axis field `B_z(z)` (magnet frame), tesla — the exact
    /// solid-cylinder result, valid outside the magnet (`|z| > h/2`).
    pub fn b_axis_exact(&self, z: f64) -> f64 {
        let (r, h) = (self.radius_m, self.thickness_m);
        let a = z + 0.5 * h;
        let b = z - 0.5 * h;
        0.5 * self.remanence_t * (a / (r * r + a * a).sqrt() - b / (r * r + b * b).sqrt())
    }
}

/// Field of a point dipole with moment `m` (A·m²) along unit `axis`, at
/// offset `d` from it, tesla.
pub fn b_dipole(m: f64, axis: [f64; 3], d: [f64; 3]) -> [f64; 3] {
    let r2 = dot(d, d);
    let r = r2.sqrt();
    let mr = m * dot(axis, d) / r;
    let k = MU_0 / (4.0 * std::f64::consts::PI * r2 * r);
    [
        k * (3.0 * mr * d[0] / r - m * axis[0]),
        k * (3.0 * mr * d[1] / r - m * axis[1]),
        k * (3.0 * mr * d[2] / r - m * axis[2]),
    ]
}

// ---------------------------------------------------------------------------
// Scene
// ---------------------------------------------------------------------------

/// A source and where it sits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PosedSource {
    /// Placement of the source's local frame.
    pub pose: Pose,
    /// The source.
    pub source: FieldSource,
}

/// A superposition of posed sources (free space, linear).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FieldScene {
    /// The sources.
    pub sources: Vec<PosedSource>,
}

impl FieldScene {
    /// Empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a source at a pose (builder style).
    pub fn with(mut self, source: FieldSource, pose: Pose) -> Self {
        self.sources.push(PosedSource { pose, source });
        self
    }

    /// Add a source at a pose.
    pub fn push(&mut self, source: FieldSource, pose: Pose) {
        self.sources.push(PosedSource { pose, source });
    }

    /// Total B at world point `p`, tesla.
    pub fn b(&self, p: [f64; 3]) -> [f64; 3] {
        self.sources
            .iter()
            .fold([0.0; 3], |acc, s| add(acc, s.source.b_at(&s.pose, p)))
    }

    /// |B| at `p`, tesla.
    pub fn b_magnitude(&self, p: [f64; 3]) -> f64 {
        norm(self.b(p))
    }

    /// Jacobian `J[i][j] = ∂B_i/∂x_j` at `p` by central differences with
    /// step `h_m`, T/m. Outside sources it is symmetric (∇×B = 0) and
    /// traceless (∇·B = 0).
    pub fn jacobian(&self, p: [f64; 3], h_m: f64) -> [[f64; 3]; 3] {
        let mut j = [[0.0; 3]; 3];
        for axis in 0..3 {
            let mut pp = p;
            let mut pm = p;
            pp[axis] += h_m;
            pm[axis] -= h_m;
            let (bp, bm) = (self.b(pp), self.b(pm));
            for (i, row) in j.iter_mut().enumerate() {
                row[axis] = (bp[i] - bm[i]) / (2.0 * h_m);
            }
        }
        j
    }

    /// ∇|B| at `p` by central differences with step `h_m`, T/m — the
    /// direction a diamagnet / magnetized-transport gradient pushes along
    /// (and, times a dipole moment, the force on a small aligned magnet).
    pub fn grad_magnitude(&self, p: [f64; 3], h_m: f64) -> [f64; 3] {
        let mut g = [0.0; 3];
        for (axis, gi) in g.iter_mut().enumerate() {
            let mut pp = p;
            let mut pm = p;
            pp[axis] += h_m;
            pm[axis] -= h_m;
            *gi = (self.b_magnitude(pp) - self.b_magnitude(pm)) / (2.0 * h_m);
        }
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elliptic::ellip_ke;

    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs().max(1e-300)
    }
    fn vrel(a: [f64; 3], b: [f64; 3]) -> f64 {
        norm(sub(a, b)) / norm(b).max(1e-300)
    }

    #[test]
    fn cel_reduces_to_legendre_k_and_e() {
        for m in [0.0, 0.1, 0.5, 0.9, 0.999] {
            let kc = (1.0f64 - m).sqrt();
            let (k, e) = ellip_ke(m);
            assert!(rel(cel(kc, 1.0, 1.0, 1.0), k) < 1e-13, "K m={m}");
            assert!(rel(cel(kc, 1.0, 1.0, kc * kc), e) < 1e-13, "E m={m}");
        }
    }

    /// Exact field of a straight segment a→b carrying current `i` (A).
    fn segment_b(a: [f64; 3], b: [f64; 3], i: f64, p: [f64; 3]) -> [f64; 3] {
        let dl = sub(b, a);
        let r1 = sub(p, a);
        let r2 = sub(p, b);
        let c = cross(dl, r1);
        let c2 = dot(c, c);
        let f = MU_0 * i / (4.0 * std::f64::consts::PI) / c2
            * (dot(dl, r1) / norm(r1) - dot(dl, r2) / norm(r2));
        scale(c, f)
    }

    /// Oracle for the block: its bound current is K = M × n on the four
    /// side faces, i.e. a stack of rectangular loops (CCW about +z), each
    /// summed from exact segment fields.
    fn block_oracle(size: [f64; 3], br: f64, n: usize, p: [f64; 3]) -> [f64; 3] {
        let (a, b, c) = (0.5 * size[0], 0.5 * size[1], 0.5 * size[2]);
        let dz = size[2] / n as f64;
        let i = br / MU_0 * dz;
        let mut acc = [0.0; 3];
        for k in 0..n {
            let z = -c + (k as f64 + 0.5) * dz;
            let v = [[a, -b, z], [a, b, z], [-a, b, z], [-a, -b, z]];
            for s in 0..4 {
                acc = add(acc, segment_b(v[s], v[(s + 1) % 4], i, p));
            }
        }
        acc
    }

    const BLOCK: [f64; 3] = [10e-3, 6e-3, 4e-3];

    #[test]
    fn block_matches_rectangular_loop_stack_near_field() {
        let src = FieldSource::BlockMagnet {
            size_m: BLOCK,
            remanence_t: 1.3,
        };
        // Generic off-symmetry points: outside near each face, a far-ish
        // point, and two interior points (exercises the +μ0M term).
        let pts = [
            [7.3e-3, -4.1e-3, 6.2e-3],
            [1.2e-3, 0.7e-3, 3.0e-3],
            [-6.0e-3, 2.5e-3, -0.4e-3],
            [0.3e-3, -4.0e-3, 1.1e-3],
            [-20e-3, 13e-3, -9e-3],
            [1.0e-3, 0.5e-3, 0.3e-3],
            [-3.0e-3, -1.5e-3, -1.0e-3],
        ];
        for p in pts {
            let b = src.b_local(p);
            let o = block_oracle(BLOCK, 1.3, 4000, p);
            assert!(vrel(b, o) < 1e-5, "p={p:?}: {b:?} vs {o:?}");
        }
    }

    #[test]
    fn block_on_axis_closed_form() {
        let br = 1.2;
        let src = FieldSource::BlockMagnet {
            size_m: BLOCK,
            remanence_t: br,
        };
        let (a, b, c) = (5e-3, 3e-3, 2e-3);
        let f = |d: f64| (a * b / (d * (d * d + a * a + b * b).sqrt())).atan();
        for z in [2.5e-3, 4e-3, 10e-3, 40e-3] {
            let exact = br / std::f64::consts::PI * (f(z - c) - f(z + c));
            let v = src.b_local([0.0, 0.0, z]);
            assert!(rel(v[2], exact) < 1e-12, "z={z}: {} vs {exact}", v[2]);
            assert!(v[0].abs() < 1e-15 && v[1].abs() < 1e-15);
        }
    }

    #[test]
    fn block_far_field_is_a_dipole() {
        let src = FieldSource::BlockMagnet {
            size_m: BLOCK,
            remanence_t: 1.43,
        };
        let m = src.moment();
        for d in [[0.4, 0.0, 0.0], [0.22, -0.14, 0.26], [0.0, 0.1, -0.5]] {
            let err = vrel(src.b_local(d), b_dipole(m, [0.0, 0.0, 1.0], d));
            assert!(err < 2e-3, "d={d:?}: {err}");
        }
    }

    #[test]
    fn block_is_finite_and_continuous_on_face_planes_and_edge_lines() {
        let src = FieldSource::BlockMagnet {
            size_m: BLOCK,
            remanence_t: 1.0,
        };
        let (a, b, c) = (5e-3, 3e-3, 2e-3);
        // On a pole-face plane outside the footprint; on the extension of
        // a top edge; on the extension of a vertical edge; on a side-face
        // plane. Round-mm grids hit exactly these.
        for p in [
            [12e-3, 1e-3, c],
            [a, 20e-3, c],
            [a, -b, 9e-3],
            [-a, 1e-3, 15e-3],
            [3e-3, 15e-3, -c],
        ] {
            let v = src.b_local(p);
            assert!(v.iter().all(|x| x.is_finite()), "p={p:?}: {v:?}");
            let q = add(p, [1e-9, -1e-9, 1e-9]);
            assert!(vrel(v, src.b_local(q)) < 1e-5, "p={p:?}");
        }
        // B_z is continuous through a pole face (inside → face → outside).
        let inside = src.b_local([1e-3, 1e-3, c - 1e-9])[2];
        let face = src.b_local([1e-3, 1e-3, c])[2];
        let out = src.b_local([1e-3, 1e-3, c + 1e-9])[2];
        assert!(rel(face, out) < 1e-5 && rel(inside, out) < 1e-5);
        // On an edge itself the field is genuinely infinite: not finite.
        assert!(!src.b_local([a, 0.0, c]).iter().all(|x| x.is_finite()));
    }

    #[test]
    fn solenoid_exact_on_axis_and_long_limit() {
        let (r, l, ni) = (0.02, 0.1, 500.0);
        let s = FieldSource::Solenoid {
            radius_m: r,
            length_m: l,
            ampere_turns: ni,
            n_loops: None,
        };
        let k = ni / l;
        for z in [0.0, 0.03, 0.05 + 1e-3, 0.12, -0.2] {
            let (u, d) = (z + 0.5 * l, z - 0.5 * l);
            let exact =
                0.5 * MU_0 * k * (u / (r * r + u * u).sqrt() - d / (r * r + d * d).sqrt());
            let b = s.b_local([0.0, 0.0, z]);
            assert!(rel(b[2], exact) < 1e-12, "z={z}: {} vs {exact}", b[2]);
        }
        // Long solenoid: center → μ0 N I / L.
        let long = FieldSource::Solenoid {
            radius_m: 0.005,
            length_m: 2.0,
            ampere_turns: 1000.0,
            n_loops: None,
        };
        assert!(rel(long.b_local([0.002, 0.001, 0.0])[2], MU_0 * 500.0) < 1e-4);
    }

    #[test]
    fn exact_sheet_matches_fine_loop_stack_off_axis() {
        let exact = FieldSource::CylinderMagnet {
            outer_radius_m: 6e-3,
            inner_radius_m: 2e-3,
            length_m: 4e-3,
            remanence_t: 1.2,
            n_loops: None,
        };
        let stacked = FieldSource::CylinderMagnet {
            outer_radius_m: 6e-3,
            inner_radius_m: 2e-3,
            length_m: 4e-3,
            remanence_t: 1.2,
            n_loops: Some(4000),
        };
        // Outside, in the bore, inside the material, beside the rim.
        for p in [
            [9e-3, 2e-3, 3.5e-3],
            [0.5e-3, -0.4e-3, 0.7e-3],
            [3.0e-3, 2.5e-3, -1.0e-3],
            [7e-3, 0.0, -2.6e-3],
            [-2e-3, 25e-3, 11e-3],
        ] {
            let (a, b) = (exact.b_local(p), stacked.b_local(p));
            assert!(vrel(a, b) < 2e-5, "p={p:?}: {a:?} vs {b:?}");
        }
    }

    #[test]
    fn cylinder_exact_matches_disc_closed_form_and_dipole() {
        let d = DiscMagnet::n52_10x5();
        let exact = FieldSource::CylinderMagnet {
            outer_radius_m: d.radius_m,
            inner_radius_m: 0.0,
            length_m: d.thickness_m,
            remanence_t: d.remanence_t,
            n_loops: None,
        };
        for z in [2.6e-3, 5e-3, 20e-3, -9e-3] {
            assert!(rel(exact.b_local([0.0, 0.0, z])[2], d.b_axis_exact(z)) < 1e-12);
        }
        for p in [[0.1, 0.0, 0.0], [0.06, 0.03, 0.08]] {
            let err = vrel(exact.b_local(p), b_dipole(exact.moment(), [0.0, 0.0, 1.0], p));
            assert!(err < 1e-2, "{err}");
        }
    }

    #[test]
    fn disc_magnet_is_bit_identical_to_its_source() {
        let d = DiscMagnet::n52_10x5();
        let s = d.as_source();
        for p in [[0.0, 0.0, 7e-3], [3e-3, -2e-3, 4e-3], [0.02, 0.01, -0.03]] {
            assert_eq!(d.b_local(p), s.b_local(p));
        }
    }

    #[test]
    fn loop_center_and_filament_guard() {
        let l = FieldSource::CurrentLoop {
            radius_m: 0.05,
            ampere_turns: 10.0,
            wire_radius_m: 0.0,
        };
        assert!(rel(l.b_local([0.0; 3])[2], MU_0 * 10.0 / 0.1) < 1e-14);
        assert!(l.b_local([0.05, 0.0, 0.0])[2].is_nan());
    }

    #[test]
    fn pose_conventions() {
        // Euler z = 90°: local x → world y.
        let p = Pose::from_euler_deg([0.0; 3], [0.0, 0.0, 90.0]);
        assert!(vrel(p.vec_to_world([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]) < 1e-15);
        // R = Rx·Ry·Rz: with x = 90, z = 90, local x → Rx(Rz x̂) = Rx ŷ = ẑ.
        let q = Pose::from_euler_deg([0.0; 3], [90.0, 0.0, 90.0]);
        assert!(vrel(q.vec_to_world([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]) < 1e-15);
        // Axis-angle agrees with Euler for a single axis.
        let r = Pose::from_axis_angle([0.0; 3], [0.0, 1.0, 0.0], 0.7);
        let e = Pose::from_euler_deg([0.0; 3], [0.0, 0.7f64.to_degrees(), 0.0]);
        for i in 0..3 {
            for j in 0..3 {
                assert!((r.rotation[i][j] - e.rotation[i][j]).abs() < 1e-14);
            }
        }
        // from_axis_and_x fixes the roll; round-trips.
        let s = Pose::from_axis_and_x([1.0, 2.0, 3.0], [0.0, 0.0, -2.0], [1.0, 1.0, 5.0]).unwrap();
        assert!(vrel(s.axis(), [0.0, 0.0, -1.0]) < 1e-15);
        assert!(vrel(s.vec_to_world([1.0, 0.0, 0.0]), normalize([1.0, 1.0, 0.0])) < 1e-15);
        let w = [0.3, -0.2, 0.9];
        assert!(vrel(s.to_local(s.to_world(w)), w) < 1e-14);
        assert!(Pose::from_axis_and_x([0.0; 3], [0.0, 0.0, 1.0], [0.0, 0.0, 3.0]).is_none());
    }

    fn test_scene() -> FieldScene {
        FieldScene::new()
            .with(
                FieldSource::BlockMagnet {
                    size_m: BLOCK,
                    remanence_t: 1.3,
                },
                Pose::from_euler_deg([0.01, -0.004, 0.002], [20.0, -35.0, 60.0]),
            )
            .with(
                FieldSource::CylinderMagnet {
                    outer_radius_m: 4e-3,
                    inner_radius_m: 1e-3,
                    length_m: 3e-3,
                    remanence_t: 1.2,
                    n_loops: None,
                },
                Pose::from_axis([-0.012, 0.006, 0.001], [1.0, 2.0, -0.5]),
            )
            .with(
                FieldSource::CurrentLoop {
                    radius_m: 0.03,
                    ampere_turns: 200.0,
                    wire_radius_m: 1e-3,
                },
                Pose::from_axis([0.0, 0.0, 0.02], [0.2, 0.0, 1.0]),
            )
    }

    #[test]
    fn rigid_motion_invariance_and_superposition() {
        let scene = test_scene();
        let g = Pose::from_euler_deg([0.05, -0.02, 0.11], [33.0, 71.0, -12.0]);
        let moved = FieldScene {
            sources: scene
                .sources
                .iter()
                .map(|s| PosedSource {
                    pose: g.compose(&s.pose),
                    source: s.source,
                })
                .collect(),
        };
        for p in [[0.02, 0.01, 0.015], [-0.03, 0.004, -0.01], [0.0, 0.0, 0.04]] {
            let b = scene.b(p);
            let bm = moved.b(g.to_world(p));
            assert!(vrel(bm, g.vec_to_world(b)) < 1e-12, "p={p:?}");
            // Superposition: the scene is the sum of its parts.
            let sum = scene
                .sources
                .iter()
                .fold([0.0; 3], |acc, s| add(acc, s.source.b_at(&s.pose, p)));
            assert_eq!(b, sum);
        }
        // A posed source at a local point = rotated local field.
        let s = scene.sources[0];
        let q = [3e-3, -7e-3, 5e-3];
        let bw = s.source.b_at(&s.pose, s.pose.to_world(q));
        assert!(vrel(bw, s.pose.vec_to_world(s.source.b_local(q))) < 1e-13);
    }

    #[test]
    fn jacobian_is_symmetric_and_traceless_in_free_space() {
        let scene = test_scene();
        for p in [[0.02, 0.01, 0.015], [-0.03, 0.004, -0.01]] {
            let j = scene.jacobian(p, 1e-6);
            let scale = j.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
            let tr = j[0][0] + j[1][1] + j[2][2];
            assert!(tr.abs() < 1e-5 * scale, "div B = {tr} vs {scale}");
            for (a, b) in [(0, 1), (0, 2), (1, 2)] {
                assert!((j[a][b] - j[b][a]).abs() < 1e-5 * scale, "curl B");
            }
        }
    }

    #[test]
    fn grad_magnitude_follows_dipole_falloff() {
        // Far on a magnet's axis |B| ∝ z⁻³, so d|B|/dz = −3|B|/z.
        let scene = FieldScene::new().with(
            DiscMagnet::n52_10x5().as_source(),
            Pose::from_axis([0.0; 3], [0.0, 0.0, 1.0]),
        );
        let z = 0.5;
        let g = scene.grad_magnitude([0.0, 0.0, z], 1e-4);
        let expect = -3.0 * scene.b_magnitude([0.0, 0.0, z]) / z;
        assert!(rel(g[2], expect) < 1e-3, "{} vs {expect}", g[2]);
    }

    #[test]
    fn helmholtz_pair_is_uniform() {
        let (r, ni) = (0.05, 100.0);
        let coil = FieldSource::CurrentLoop {
            radius_m: r,
            ampere_turns: ni,
            wire_radius_m: 1e-3,
        };
        // Tilted pair, to exercise the poses as well.
        let axis = normalize([1.0, -1.0, 2.0]);
        let c0 = [0.01, 0.02, -0.03];
        let scene = FieldScene::new()
            .with(coil, Pose::from_axis(add(c0, scale(axis, 0.5 * r)), axis))
            .with(coil, Pose::from_axis(sub(c0, scale(axis, 0.5 * r)), axis));
        let b0 = (0.8f64).powf(1.5) * MU_0 * ni / r;
        let bc = scene.b(c0);
        assert!(vrel(bc, scale(axis, b0)) < 1e-12);
        // Within a sphere of 0.1 R the field stays within ~(144/125)(0.1)⁴.
        let (u, v) = basis(axis);
        for d in [axis, u, v, normalize(add(axis, u)), normalize(sub(v, axis))] {
            let b = scene.b(add(c0, scale(d, 0.1 * r)));
            assert!(vrel(b, bc) < 2e-4, "d={d:?}: {}", vrel(b, bc));
        }
        // …and is visibly non-uniform at 0.4 R (the test has teeth).
        assert!(vrel(scene.b(add(c0, scale(axis, 0.4 * r))), bc) > 1e-2);
    }
}
