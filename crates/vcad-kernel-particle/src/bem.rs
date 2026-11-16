//! Thin-wire boundary-element (charge-simulation) electrostatics for
//! [`Device3d`] — the 3D counterpart of [`crate::poisson`].
//!
//! # Why a boundary method, not a 3D grid
//!
//! A fusor cathode is ~1 mm wire in a ~150 mm chamber. A finite-difference
//! grid that resolves the wire (h ≈ a/4) needs ~10⁸–10⁹ nodes in 3D; even
//! with multigrid that is minutes of solve and gigabytes of field for a
//! problem whose *unknowns* are only the charge on the wires. Here the
//! unknowns are exactly that: a piecewise-constant line charge λⱼ on each
//! straight wire segment (a few hundred for a globe grid), so the solve is
//! a small dense system and the field is analytic everywhere:
//!
//! * **Chamber.** A grounded sphere is handled with Kelvin images — the
//!   exact Dirichlet Green's function `1/|x−y| − (R/|y|)/|x−y*|` — so the
//!   wall costs no unknowns and φ = V_wall on it identically.
//! * **Collocation.** The standard thin-wire *reduced kernel*: charge on
//!   the axis, potential matched to the electrode voltage at each segment
//!   midpoint on the wire surface (`1/√(d² + a²)`, integrated in closed
//!   form). Segments are ≥ 4 wire radii long (the thin-wire validity
//!   limit used by `vcad-kernel-antenna`).
//! * **Linear solve.** Dense LU with partial pivoting from `tang-la`. At
//!   these sizes (N ≲ 4000, O(N³/3)) a direct solve beats an iterative one
//!   and needs no preconditioner; no new solver is needed in `tang`.
//! * **Field.** The exact potential of a uniform straight line segment,
//!   `kλ ln((r₁+r₂+L)/(r₁+r₂−L))`, and its exact gradient, plus image
//!   point charges from Gauss–Legendre quadrature along each segment
//!   (the image term is smooth inside the chamber). E is the analytic
//!   gradient of the sampled potential, so it is **conservative by
//!   construction** — like the bilinear-patch gradient of the 2D path,
//!   integrator energy error is set by the time step alone.
//!
//! # Fail-closed gates
//!
//! After the solve the potential is re-evaluated with the exact field
//! kernel on the wire *surface* (several azimuths at every segment midpoint
//! and joint, skipping points buried inside a crossing wire). The worst
//! deviation from the electrode voltage, relative to the electrode–wall
//! drop, is reported and the solve is refused above
//! [`BemOptions::max_surface_residual`]. This is what catches a junction
//! or a too-coarse discretization the thin-wire model cannot represent.

use crate::constants::{EPSILON_0, MU_0};
use crate::wire3d::{
    add, cross, dot, norm, normalize, plane_basis, point_segment_distance, scale, sub, Chamber3d,
    Device3d, GeometryError,
};

/// Coulomb constant 1/(4πε₀), V·m/C.
const K_E: f64 = 1.0 / (4.0 * std::f64::consts::PI * EPSILON_0);

/// Multiplier on the wire radius inside which the tracer refines its step
/// (matches the axisymmetric tracer's `(d − a)/(6a)` ramp).
pub const NEAR_WIRE_FACTOR: f64 = 7.0;

/// Options for [`solve`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BemOptions {
    /// Target segment length as a multiple of the wire radius. Values
    /// below 4 violate the thin-wire kernel's validity and are refused.
    pub segment_length_factor: f64,
    /// Largest chord sagitta on curved paths, as a fraction of the wire
    /// radius.
    pub max_sag_fraction: f64,
    /// Fail-closed gate on the worst surface-potential residual, relative
    /// to the electrode–wall drop.
    pub max_surface_residual: f64,
    /// Azimuthal sample count around the wire for the residual gate.
    pub residual_azimuths: usize,
    /// Refuse problems with more unknowns than this (dense O(N³) solve).
    pub max_unknowns: usize,
}

impl Default for BemOptions {
    fn default() -> Self {
        Self {
            segment_length_factor: 4.0,
            max_sag_fraction: 0.1,
            max_surface_residual: 0.05,
            residual_azimuths: 8,
            max_unknowns: 4000,
        }
    }
}

/// Failure modes of [`solve`].
#[derive(Debug, Clone, PartialEq)]
pub enum BemError {
    /// The device failed [`Device3d::validate`].
    Geometry(GeometryError),
    /// Invalid solver options.
    InvalidOptions(String),
    /// More unknowns than [`BemOptions::max_unknowns`].
    TooManyUnknowns {
        /// Unknowns the discretization would need.
        unknowns: usize,
        /// The configured cap.
        max: usize,
    },
    /// The collocation matrix is numerically singular.
    Singular,
    /// The surface-potential residual gate failed.
    SurfaceResidual {
        /// Worst relative residual found.
        worst: f64,
        /// Wire on which it occurred.
        wire: usize,
        /// The configured gate.
        threshold: f64,
    },
}

impl std::fmt::Display for BemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BemError::Geometry(e) => write!(f, "invalid geometry: {e}"),
            BemError::InvalidOptions(s) => write!(f, "invalid options: {s}"),
            BemError::TooManyUnknowns { unknowns, max } => write!(
                f,
                "{unknowns} unknowns exceeds the dense-solve cap of {max} (coarsen the wire \
                 discretization or raise max_unknowns)"
            ),
            BemError::Singular => write!(f, "collocation matrix is singular"),
            BemError::SurfaceResidual {
                worst,
                wire,
                threshold,
            } => write!(
                f,
                "surface-potential residual {:.2}% on wire {wire} exceeds the {:.2}% gate — the \
                 thin-wire model does not represent this geometry at this resolution",
                worst * 100.0,
                threshold * 100.0
            ),
        }
    }
}

impl std::error::Error for BemError {}

impl From<GeometryError> for BemError {
    fn from(e: GeometryError) -> Self {
        BemError::Geometry(e)
    }
}

/// One discretized wire.
#[derive(Debug, Clone)]
struct WireRun {
    /// First vertex index in [`WireField::verts`].
    v0: usize,
    /// Segment range in [`WireField::segs`].
    s0: usize,
    s1: usize,
    closed: bool,
    radius: f64,
    potential: f64,
    current: f64,
}

#[derive(Debug, Clone, Copy)]
struct Seg {
    a: usize,
    b: usize,
    wire: usize,
    len: f64,
    /// Line charge, C/m.
    lambda: f64,
}

/// An image point charge (Kelvin), stored in the symmetric form
/// `φ = −k q / √(|x|²·c2 − 2x·y + R²)` with `c2 = |y|²/R²`.
#[derive(Debug, Clone, Copy)]
struct Image {
    y: [f64; 3],
    c2: f64,
    /// Segment index (charge = λ_seg × weight).
    seg: usize,
    weight: f64,
    q: f64,
}

/// Uniform-grid spatial index of segments for hit tests and near-wire
/// distance queries.
#[derive(Debug, Clone)]
struct SegIndex {
    origin: [f64; 3],
    cell: f64,
    dims: [usize; 3],
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl SegIndex {
    fn build(verts: &[[f64; 3]], segs: &[Seg], radii: &[f64], wires: &[WireRun]) -> Self {
        let a_max = radii.iter().copied().fold(0.0, f64::max);
        let reach = NEAR_WIRE_FACTOR * a_max;
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for v in verts {
            for d in 0..3 {
                lo[d] = lo[d].min(v[d]);
                hi[d] = hi[d].max(v[d]);
            }
        }
        for d in 0..3 {
            lo[d] -= reach;
            hi[d] += reach;
        }
        let cell = reach.max(1e-6);
        let dims = [0, 1, 2].map(|d| (((hi[d] - lo[d]) / cell).ceil() as usize).max(1));
        let ncell = dims[0] * dims[1] * dims[2];
        let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); ncell];
        for (si, s) in segs.iter().enumerate() {
            let a = verts[s.a];
            let b = verts[s.b];
            let r = NEAR_WIRE_FACTOR * wires[s.wire].radius;
            let cl = |x: f64, d: usize| {
                (((x - lo[d]) / cell).floor().max(0.0) as usize).min(dims[d] - 1)
            };
            let i_lo = [0, 1, 2].map(|d| cl(a[d].min(b[d]) - r, d));
            let i_hi = [0, 1, 2].map(|d| cl(a[d].max(b[d]) + r, d));
            for i in i_lo[0]..=i_hi[0] {
                for j in i_lo[1]..=i_hi[1] {
                    for k in i_lo[2]..=i_hi[2] {
                        buckets[(i * dims[1] + j) * dims[2] + k].push(si as u32);
                    }
                }
            }
        }
        let mut starts = Vec::with_capacity(ncell + 1);
        let mut items = Vec::new();
        starts.push(0);
        for b in buckets {
            items.extend(b);
            starts.push(items.len() as u32);
        }
        Self {
            origin: lo,
            cell,
            dims,
            starts,
            items,
        }
    }

    #[inline]
    fn candidates(&self, p: [f64; 3]) -> &[u32] {
        let mut idx = [0usize; 3];
        for d in 0..3 {
            let x = (p[d] - self.origin[d]) / self.cell;
            if !(x >= 0.0) || x >= self.dims[d] as f64 {
                return &[];
            }
            idx[d] = x as usize;
        }
        let c = (idx[0] * self.dims[1] + idx[1]) * self.dims[2] + idx[2];
        &self.items[self.starts[c] as usize..self.starts[c + 1] as usize]
    }
}

/// Solved 3D vacuum field of a [`Device3d`]: wire line charges, chamber
/// images, and (for current-carrying loops) Biot–Savart B. Owns all its
/// data; sample it through [`crate::field::FieldSampler`].
#[derive(Debug, Clone)]
pub struct WireField {
    verts: Vec<[f64; 3]>,
    segs: Vec<Seg>,
    wires: Vec<WireRun>,
    images: Vec<Image>,
    chamber_radius_m: Option<f64>,
    wall_potential_v: f64,
    max_drop_v: f64,
    index: SegIndex,
    has_current: bool,
    worst_residual: f64,
    min_wire_radius_m: f64,
    extent_m: f64,
}

/// Solve the vacuum field of `device`.
pub fn solve(device: &Device3d, opts: &BemOptions) -> Result<WireField, BemError> {
    device.validate()?;
    if !(opts.segment_length_factor >= 4.0 && opts.segment_length_factor.is_finite()) {
        return Err(BemError::InvalidOptions(
            "segment_length_factor must be ≥ 4 (thin-wire kernel validity)".into(),
        ));
    }
    if !(opts.max_sag_fraction > 0.0 && opts.max_sag_fraction <= 1.0) {
        return Err(BemError::InvalidOptions(
            "max_sag_fraction must be in (0, 1]".into(),
        ));
    }
    if !(opts.max_surface_residual > 0.0) || opts.residual_azimuths < 3 {
        return Err(BemError::InvalidOptions(
            "max_surface_residual must be > 0 and residual_azimuths ≥ 3".into(),
        ));
    }

    // --- Discretize -----------------------------------------------------
    let mut verts = Vec::new();
    let mut segs = Vec::new();
    let mut wires = Vec::with_capacity(device.wires.len());
    for (wi, w) in device.wires.iter().enumerate() {
        let a = w.wire_radius_mm * 1e-3;
        let pts = w
            .path
            .vertices_m(opts.segment_length_factor * a, opts.max_sag_fraction * a);
        let closed = w.path.is_closed();
        let v0 = verts.len();
        verts.extend_from_slice(&pts);
        let s0 = segs.len();
        let n_seg = if closed { pts.len() } else { pts.len() - 1 };
        for k in 0..n_seg {
            let ia = v0 + k;
            let ib = if closed && k + 1 == pts.len() {
                v0
            } else {
                v0 + k + 1
            };
            segs.push(Seg {
                a: ia,
                b: ib,
                wire: wi,
                len: norm(sub(verts[ib], verts[ia])),
                lambda: 0.0,
            });
        }
        wires.push(WireRun {
            v0,
            s0,
            s1: segs.len(),
            closed,
            radius: a,
            potential: w.potential_v,
            current: w.ampere_turns,
        });
    }
    let n = segs.len();
    if n > opts.max_unknowns {
        return Err(BemError::TooManyUnknowns {
            unknowns: n,
            max: opts.max_unknowns,
        });
    }

    // --- Image quadrature ----------------------------------------------
    let chamber_radius_m = match device.chamber {
        Chamber3d::Sphere { radius_mm } => Some(radius_mm * 1e-3),
        Chamber3d::Unbounded => None,
    };
    let mut images = Vec::new();
    if let Some(rc) = chamber_radius_m {
        // The image integrand 1/√(|x|²|y|²/R² − 2x·y + R²) varies on the
        // scale of (R − |y|); use 2-point Gauss on sub-intervals no longer
        // than a quarter of that.
        let g = 1.0 / 3f64.sqrt();
        for (si, s) in segs.iter().enumerate() {
            let a = verts[s.a];
            let b = verts[s.b];
            let gap = rc - norm(a).max(norm(b));
            let m = ((s.len / (0.25 * gap)).ceil() as usize).max(1);
            let h = 1.0 / m as f64;
            for k in 0..m {
                for xi in [-g, g] {
                    let t = h * (k as f64 + 0.5 + 0.5 * xi);
                    let y = add(a, scale(sub(b, a), t));
                    images.push(Image {
                        y,
                        c2: dot(y, y) / (rc * rc),
                        seg: si,
                        weight: 0.5 * h * s.len,
                        q: 0.0,
                    });
                }
            }
        }
    }

    // --- Collocation matrix (dimensionless; λ = x / k) -------------------
    let mids: Vec<[f64; 3]> = segs
        .iter()
        .map(|s| scale(add(verts[s.a], verts[s.b]), 0.5))
        .collect();
    let mut a_mat = tang_la::DMat::<f64>::zeros(n, n);
    for i in 0..n {
        let x = mids[i];
        let ai = wires[segs[i].wire].radius;
        for (j, s) in segs.iter().enumerate() {
            let v = reduced_kernel(x, verts[s.a], verts[s.b], s.len, ai);
            a_mat.set(i, j, v);
        }
        if let Some(rc) = chamber_radius_m {
            let xx = dot(x, x);
            for im in &images {
                let d = xx * im.c2 - 2.0 * dot(x, im.y) + rc * rc;
                *a_mat.get_mut(i, im.seg) -= im.weight / d.sqrt();
            }
        }
    }
    let rhs = tang_la::DVec::from_fn(n, |i| {
        wires[segs[i].wire].potential - device.wall_potential_v
    });
    let lu = tang_la::Lu::new(&a_mat).ok_or(BemError::Singular)?;
    let x = lu.solve(&rhs);
    for (i, s) in segs.iter_mut().enumerate() {
        s.lambda = x.as_slice()[i] / K_E;
        if !s.lambda.is_finite() {
            return Err(BemError::Singular);
        }
    }
    for im in &mut images {
        im.q = segs[im.seg].lambda * im.weight;
    }

    let radii: Vec<f64> = wires.iter().map(|w| w.radius).collect();
    let index = SegIndex::build(&verts, &segs, &radii, &wires);
    let extent_m = verts.iter().map(|v| norm(*v)).fold(0.0, f64::max)
        + radii.iter().copied().fold(0.0, f64::max);
    let mut field = WireField {
        has_current: wires.iter().any(|w| w.current != 0.0),
        min_wire_radius_m: radii.iter().copied().fold(f64::INFINITY, f64::min),
        verts,
        segs,
        wires,
        images,
        chamber_radius_m,
        wall_potential_v: device.wall_potential_v,
        max_drop_v: device.max_potential_drop_v(),
        index,
        worst_residual: 0.0,
        extent_m,
    };

    // --- Surface residual gate -----------------------------------------
    let (worst, wire) = field.surface_residual(opts.residual_azimuths);
    field.worst_residual = worst;
    if worst > opts.max_surface_residual {
        return Err(BemError::SurfaceResidual {
            worst,
            wire,
            threshold: opts.max_surface_residual,
        });
    }
    Ok(field)
}

/// `∫₀ᴸ ds / √(|x − y(s)|² + a²)` over the straight segment `p→q`.
#[inline]
fn reduced_kernel(x: [f64; 3], p: [f64; 3], q: [f64; 3], len: f64, a: f64) -> f64 {
    let t = scale(sub(q, p), 1.0 / len);
    let rel = sub(x, p);
    let s0 = dot(rel, t);
    let rho2 = (dot(rel, rel) - s0 * s0).max(0.0);
    let re = (rho2 + a * a).sqrt();
    ((len - s0) / re).asinh() + (s0 / re).asinh()
}

impl WireField {
    /// Whether any wire carries current (B can be nonzero).
    pub fn b_field_present(&self) -> bool {
        self.has_current
    }

    /// Number of unknowns (wire segments).
    pub fn unknowns(&self) -> usize {
        self.segs.len()
    }

    /// Worst relative surface-potential residual found by the gate.
    pub fn worst_surface_residual(&self) -> f64 {
        self.worst_residual
    }

    /// Total free charge on wire `k`, coulombs.
    pub fn wire_charge_c(&self, k: usize) -> f64 {
        let w = &self.wires[k];
        self.segs[w.s0..w.s1]
            .iter()
            .map(|s| s.lambda * s.len)
            .sum()
    }

    /// Total free charge on all wires, coulombs.
    pub fn total_charge_c(&self) -> f64 {
        (0..self.wires.len()).map(|k| self.wire_charge_c(k)).sum()
    }

    /// Number of wires.
    pub fn wire_count(&self) -> usize {
        self.wires.len()
    }

    /// Wire radius of wire `k`, m.
    pub fn wire_radius_m(&self, k: usize) -> f64 {
        self.wires[k].radius
    }

    /// Smallest wire radius, m.
    pub fn min_wire_radius_m(&self) -> f64 {
        self.min_wire_radius_m
    }

    /// Chamber radius, m (`None` = unbounded).
    pub fn chamber_radius_m(&self) -> Option<f64> {
        self.chamber_radius_m
    }

    /// Largest distance of any wire surface from the origin, m.
    pub fn extent_m(&self) -> f64 {
        self.extent_m
    }

    /// Deepest electrode–wall potential drop, volts.
    pub fn max_potential_drop_v(&self) -> f64 {
        self.max_drop_v
    }

    /// Potential and field at `x` (meters): `(φ [V], E [V/m])`.
    pub fn evaluate(&self, x: [f64; 3], want_phi: bool) -> (f64, [f64; 3]) {
        let mut phi = 0.0;
        let mut e = [0.0; 3];
        let unit = |v: [f64; 3]| {
            let d = sub(x, v);
            let r = norm(d);
            (r, if r > 0.0 { scale(d, 1.0 / r) } else { [0.0; 3] })
        };
        for w in &self.wires {
            let (r_first, e_first) = unit(self.verts[w.v0]);
            let (mut r0, mut e0) = (r_first, e_first);
            for s in &self.segs[w.s0..w.s1] {
                let (r1, e1) = if w.closed && s.b == w.v0 {
                    (r_first, e_first)
                } else {
                    unit(self.verts[s.b])
                };
                let u = r0 + r1;
                let l = s.len;
                let den = u * u - l * l;
                if den > 1e-30 * l * l {
                    let kl = K_E * s.lambda;
                    let c = kl * 2.0 * l / den;
                    e = add(e, scale(add(e0, e1), c));
                    if want_phi {
                        phi += kl * ((u + l) / (u - l)).ln();
                    }
                }
                r0 = r1;
                e0 = e1;
            }
        }
        if let Some(rc) = self.chamber_radius_m {
            let xx = dot(x, x);
            let r2 = rc * rc;
            for im in &self.images {
                let d = xx * im.c2 - 2.0 * dot(x, im.y) + r2;
                let inv = 1.0 / d.sqrt();
                let kq = K_E * im.q;
                if want_phi {
                    phi -= kq * inv;
                }
                let c = -kq * inv * inv * inv;
                e = add(e, scale(sub(scale(x, im.c2), im.y), c));
            }
        }
        (self.wall_potential_v + phi, e)
    }

    /// Potential at `x` (meters), volts.
    pub fn potential(&self, x: [f64; 3]) -> f64 {
        self.evaluate(x, true).0
    }

    /// Electric field at `x` (meters), V/m.
    pub fn e_field(&self, x: [f64; 3]) -> [f64; 3] {
        self.evaluate(x, false).1
    }

    /// Magnetic field at `x` (meters), tesla — Biot–Savart over every
    /// current-carrying closed loop, regularized inside the conductor
    /// (uniform current density: B ∝ distance from the axis).
    pub fn b_field(&self, x: [f64; 3]) -> [f64; 3] {
        if !self.has_current {
            return [0.0; 3];
        }
        let mut b = [0.0; 3];
        for w in self.wires.iter().filter(|w| w.current != 0.0) {
            let pref = MU_0 * w.current / (4.0 * std::f64::consts::PI);
            for s in &self.segs[w.s0..w.s1] {
                let pa = self.verts[s.a];
                let pb = self.verts[s.b];
                let d = sub(pb, pa);
                let r1 = sub(x, pa);
                let r2 = sub(x, pb);
                let c = cross(d, r1);
                let c2 = dot(c, c);
                let d2 = dot(d, d);
                let denom = c2.max(w.radius * w.radius * d2);
                if denom <= 0.0 {
                    continue;
                }
                let n1 = norm(r1);
                let n2 = norm(r2);
                if n1 == 0.0 || n2 == 0.0 {
                    continue;
                }
                let f = dot(d, r1) / n1 - dot(d, r2) / n2;
                b = add(b, scale(c, pref * f / denom));
            }
        }
        b
    }

    /// Index of the wire whose conductor contains `x`, if any.
    pub fn wire_hit(&self, x: [f64; 3]) -> Option<usize> {
        for &si in self.index.candidates(x) {
            let s = &self.segs[si as usize];
            let a = self.wires[s.wire].radius;
            if point_segment_distance(x, self.verts[s.a], self.verts[s.b]) <= a {
                return Some(s.wire);
            }
        }
        None
    }

    /// Smallest `(d − a) / (6a)` over nearby wires (d = distance to the
    /// wire axis), or 1 when no wire is within [`NEAR_WIRE_FACTOR`] radii —
    /// the tracer's near-wire step-refinement ratio.
    pub fn near_wire_ratio(&self, x: [f64; 3]) -> f64 {
        let mut ratio = 1.0_f64;
        for &si in self.index.candidates(x) {
            let s = &self.segs[si as usize];
            let a = self.wires[s.wire].radius;
            let d = point_segment_distance(x, self.verts[s.a], self.verts[s.b]);
            let r = (d - a) / ((NEAR_WIRE_FACTOR - 1.0) * a);
            if r < ratio {
                ratio = r;
            }
        }
        ratio
    }

    /// Sample the exact-kernel potential on every wire surface and return
    /// the worst relative residual and its wire.
    fn surface_residual(&self, azimuths: usize) -> (f64, usize) {
        let mut worst = 0.0_f64;
        let mut worst_wire = 0;
        for (si, s) in self.segs.iter().enumerate() {
            let w = &self.wires[s.wire];
            let drop = (w.potential - self.wall_potential_v).abs();
            if drop == 0.0 {
                continue;
            }
            let pa = self.verts[s.a];
            let pb = self.verts[s.b];
            let t = normalize(sub(pb, pa));
            let (u, v) = plane_basis(t);
            for frac in [0.0, 0.5] {
                // Skip the free end of an open wire (end caps are not
                // modeled: the thin-wire model has no charge there).
                if frac == 0.0 && !w.closed && s.a == w.v0 {
                    continue;
                }
                let c = add(pa, scale(sub(pb, pa), frac));
                for k in 0..azimuths {
                    let ang = 2.0 * std::f64::consts::PI * k as f64 / azimuths as f64;
                    let p = add(c, scale(add(scale(u, ang.cos()), scale(v, ang.sin())), w.radius));
                    // Skip points buried inside any other segment's
                    // conductor (crossings, inner side of bends).
                    let buried = self.index.candidates(p).iter().any(|&oj| {
                        let oj = oj as usize;
                        if oj == si {
                            return false;
                        }
                        let o = &self.segs[oj];
                        point_segment_distance(p, self.verts[o.a], self.verts[o.b])
                            < 0.98 * self.wires[o.wire].radius
                    });
                    if buried {
                        continue;
                    }
                    let phi = self.potential(p);
                    let r = (phi - w.potential).abs() / drop;
                    if r > worst {
                        worst = r;
                        worst_wire = s.wire;
                        if std::env::var("BEM_DEBUG").is_ok() {
                            eprintln!("worst {r:.4} wire {} seg {si} frac {frac} k {k} p {:?} phi {phi} len {} a {}", s.wire, p, s.len, w.radius);
                        }
                    }
                }
            }
        }
        (worst, worst_wire)
    }

    #[cfg(test)]
    fn segment_count_of(&self, k: usize) -> usize {
        self.wires[k].s1 - self.wires[k].s0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire3d::{GreatCircleLayout, Wire3d, WirePath};

    fn ring_device(chamber: Chamber3d) -> Device3d {
        Device3d {
            chamber,
            wall_potential_v: 0.0,
            wires: vec![Wire3d {
                path: WirePath::Circle {
                    center_mm: [0.0; 3],
                    normal: [0.0, 0.0, 1.0],
                    radius_mm: 30.0,
                },
                wire_radius_mm: 0.5,
                potential_v: -1000.0,
                ampere_turns: 0.0,
            }],
        }
    }

    #[test]
    fn free_ring_charge_matches_thin_torus_capacitance() {
        // Thin ring (a ≪ R): C ≈ 4π²ε₀R / ln(8R/a).
        let f = solve(&ring_device(Chamber3d::Unbounded), &BemOptions::default()).unwrap();
        let (r, a) = (0.030, 0.0005);
        let c = 4.0 * std::f64::consts::PI.powi(2) * EPSILON_0 * r / (8.0 * r / a).ln();
        let q_expect = c * -1000.0;
        let q = f.total_charge_c();
        assert!(
            (q - q_expect).abs() < 0.02 * q_expect.abs(),
            "q={q:e} expect={q_expect:e}"
        );
        assert!(f.worst_surface_residual() < 0.02);
        assert!(f.segment_count_of(0) > 8);
    }

    #[test]
    fn grounded_sphere_wall_is_at_zero_potential() {
        let d = Device3d::great_circle_globe(
            60.0,
            20.0,
            &GreatCircleLayout::Icosidodecahedral6.normals(),
            0.75,
            -5000.0,
        );
        let f = solve(&d, &BemOptions::default()).unwrap();
        for dir in crate::wire3d::fibonacci_sphere(50) {
            let p = scale(dir, 0.060);
            let phi = f.potential(p);
            assert!(phi.abs() < 1e-3 * 5000.0, "wall φ={phi} at {dir:?}");
        }
    }

    #[test]
    fn field_is_the_gradient_of_the_potential() {
        let d = Device3d::lat_long_globe(76.0, 19.0, 3, 4, 1.0, -30e3);
        let f = solve(&d, &BemOptions::default()).unwrap();
        let h = 1e-6;
        for p in [[0.004, 0.003, 0.002], [0.025, -0.01, 0.006], [0.0, 0.0, 0.05]] {
            let e = f.e_field(p);
            for k in 0..3 {
                let mut pp = p;
                let mut pm = p;
                pp[k] += h;
                pm[k] -= h;
                let fd = -(f.potential(pp) - f.potential(pm)) / (2.0 * h);
                assert!(
                    (fd - e[k]).abs() < 1e-5 * e.iter().map(|x| x.abs()).fold(1.0, f64::max),
                    "∂φ mismatch at {p:?} axis {k}: fd={fd} e={}",
                    e[k]
                );
            }
        }
    }

    #[test]
    fn biot_savart_matches_the_elliptic_ring_field() {
        let mut d = ring_device(Chamber3d::Unbounded);
        d.wires[0].ampere_turns = 1000.0;
        let f = solve(&d, &BemOptions::default()).unwrap();
        let coil = crate::field::RingCoil {
            radius_m: 0.030,
            z_m: 0.0,
            ampere_turns: 1000.0,
            wire_radius_m: 0.0005,
        };
        for (r, z) in [(0.0, 0.0), (0.01, 0.02), (0.045, -0.01), (0.02, 0.0)] {
            let b = f.b_field([r, 0.0, z]);
            let (br, bz) = crate::field::b_ring(&coil, r, z);
            let scale_b = (br * br + bz * bz).sqrt();
            assert!(
                (b[0] - br).abs() < 5e-3 * scale_b && (b[2] - bz).abs() < 5e-3 * scale_b,
                "({r},{z}): got {b:?}, want ({br},{bz})"
            );
            assert!(b[1].abs() < 1e-9 * scale_b.max(1e-12) + 1e-12);
        }
    }

    #[test]
    fn hit_and_near_queries() {
        let f = solve(&ring_device(Chamber3d::Unbounded), &BemOptions::default()).unwrap();
        assert_eq!(f.wire_hit([0.030, 0.0, 0.0002]), Some(0));
        assert_eq!(f.wire_hit([0.030, 0.0, 0.0008]), None);
        assert_eq!(f.wire_hit([0.0, 0.0, 0.0]), None);
        assert!(f.near_wire_ratio([0.0, 0.0, 0.0]) >= 1.0);
        let r = f.near_wire_ratio([0.030, 0.0, 0.0015]);
        assert!((r - (0.0015 - 0.0005) / 0.003).abs() < 0.05, "r={r}");
    }

    #[test]
    fn refuses_bad_options_and_oversize_problems() {
        let d = ring_device(Chamber3d::Unbounded);
        let opts = BemOptions {
            segment_length_factor: 2.0,
            ..Default::default()
        };
        assert!(matches!(solve(&d, &opts), Err(BemError::InvalidOptions(_))));
        let opts = BemOptions {
            max_unknowns: 4,
            ..Default::default()
        };
        assert!(matches!(
            solve(&d, &opts),
            Err(BemError::TooManyUnknowns { .. })
        ));
    }
}
