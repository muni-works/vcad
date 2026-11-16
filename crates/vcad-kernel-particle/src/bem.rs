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
//!   the axis, potential matched to the electrode voltage on the wire
//!   (`1/√(d² + a²)`, integrated in closed form). Segments are ≥ 4 wire
//!   radii long (the thin-wire validity limit used by
//!   `vcad-kernel-antenna`). The potential is matched at several stations
//!   per segment **including the joints**, in the least-squares sense:
//!   midpoint-only collocation leaves a ~70% potential bulge at wire
//!   crossings (both wires' charge counted at full density between
//!   collocation points); matching at joints cuts it to a few percent.
//! * **Linear solve.** Dense normal equations + Cholesky (or LU for
//!   square collocation) from `tang-la`. At these sizes (N ≲ 3000) a
//!   direct solve beats an iterative one and needs no preconditioner; no
//!   new solver is needed in `tang`.
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
//! kernel on the wire *surface* (a ring of points at every segment midpoint
//! and joint, skipping points buried inside a crossing wire) and reduced
//! to a [`ResidualReport`]: the ring-mean residual away from junctions,
//! the same inside junction zones, and the azimuthal spread (the missing
//! transverse-dipole response of an on-axis line charge). Each has its own
//! gate in [`BemOptions`]; the solve is refused if any fails. This is what
//! catches a junction or a too-coarse discretization the thin-wire model
//! cannot represent.
//!
//! Known limit (documented, gated): the near-wire field within a few wire
//! radii carries the azimuthal-spread error (≈ 3–4% of ΔV for a 1 mm wire
//! on a 19 mm globe). Higher-order (dipole) thin-wire unknowns would remove
//! it; the far field is unaffected at O((a/d)²).

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
    /// Gate on [`ResidualReport::mean`] (ring-mean surface residual away
    /// from junctions), relative to the electrode–wall drop.
    pub max_surface_residual: f64,
    /// Gate on [`ResidualReport::junction_mean`].
    pub max_junction_residual: f64,
    /// Gate on [`ResidualReport::spread`].
    pub max_azimuthal_spread: f64,
    /// Azimuthal sample count around the wire for the residual gate.
    pub residual_azimuths: usize,
    /// Stations within this many wire radii of a *different* wire lie in
    /// a junction zone (crossing, weld), where the thin-wire model resolves
    /// charge least well; they are gated separately
    /// ([`Self::max_junction_residual`]).
    pub junction_zone_factor: f64,
    /// Collocation stations per segment (≥ 1). One = square midpoint
    /// collocation; more = least squares over stations that include the
    /// segment joints, which tames the thin-wire error at crossings.
    pub stations_per_segment: usize,
    /// Refuse problems with more unknowns than this (dense O(N³) solve).
    pub max_unknowns: usize,
}

impl Default for BemOptions {
    fn default() -> Self {
        Self {
            segment_length_factor: 4.0,
            max_sag_fraction: 0.1,
            max_surface_residual: 0.02,
            max_junction_residual: 0.10,
            max_azimuthal_spread: 0.10,
            residual_azimuths: 8,
            junction_zone_factor: 3.0,
            stations_per_segment: 4,
            max_unknowns: 3000,
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
    /// A surface-potential residual gate failed.
    SurfaceResidual {
        /// Which gate.
        kind: ResidualKind,
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
                kind,
                worst,
                wire,
                threshold,
            } => write!(
                f,
                "{kind:?} surface-potential residual {:.2}% on wire {wire} exceeds the {:.2}% \
                 gate — the thin-wire model does not represent this geometry at this resolution",
                worst * 100.0,
                threshold * 100.0
            ),
        }
    }
}

impl std::error::Error for BemError {}

/// Which surface-residual gate ([`ResidualReport`]) failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidualKind {
    /// Ring-mean residual away from junctions.
    Mean,
    /// Ring-mean residual inside a junction zone.
    Junction,
    /// Azimuthal variation around the wire (missing dipole response).
    AzimuthalSpread,
}

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

/// Owned segment-capsule geometry for hit tests and near-wire distance
/// queries (shared with tracers via `Arc`, so a tracer's collision
/// geometry borrows nothing).
#[derive(Debug, Clone)]
struct WireHits {
    verts: Vec<[f64; 3]>,
    /// (vertex a, vertex b, wire) per segment.
    segs: Vec<(usize, usize, usize)>,
    radii: Vec<f64>,
    index: SegIndex,
}

impl WireHits {
    fn wire_hit(&self, x: [f64; 3]) -> Option<usize> {
        for &si in self.index.candidates(x) {
            let (a, b, w) = self.segs[si as usize];
            if point_segment_distance(x, self.verts[a], self.verts[b]) <= self.radii[w] {
                return Some(w);
            }
        }
        None
    }

    fn near_wire_ratio(&self, x: [f64; 3]) -> f64 {
        let mut ratio = 1.0_f64;
        for &si in self.index.candidates(x) {
            let (a, b, w) = self.segs[si as usize];
            let rad = self.radii[w];
            let d = point_segment_distance(x, self.verts[a], self.verts[b]);
            let r = (d - rad) / ((NEAR_WIRE_FACTOR - 1.0) * rad);
            if r < ratio {
                ratio = r;
            }
        }
        ratio
    }
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
            if x.is_nan() || x < 0.0 || x >= self.dims[d] as f64 {
                return &[];
            }
            idx[d] = x as usize;
        }
        let c = (idx[0] * self.dims[1] + idx[1]) * self.dims[2] + idx[2];
        &self.items[self.starts[c] as usize..self.starts[c + 1] as usize]
    }
}

/// Surface-residual summary, all relative to the electrode–wall drop.
///
/// At every station (segment midpoint and joint) the exact-kernel
/// potential is sampled on a ring of points around the wire surface.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ResidualReport {
    /// Worst |ring-mean − V| away from junctions: the monopole
    /// consistency the thin-wire model claims. Gated by
    /// [`BemOptions::max_surface_residual`].
    pub mean: f64,
    /// Wire on which [`Self::mean`] occurs.
    pub mean_wire: usize,
    /// Worst |ring-mean − V| inside junction zones (crossings). Gated by
    /// [`BemOptions::max_junction_residual`].
    pub junction_mean: f64,
    /// Wire on which [`Self::junction_mean`] occurs.
    pub junction_wire: usize,
    /// Worst half peak-to-peak variation around a ring: the missing
    /// transverse-dipole (proximity) response of an on-axis line charge,
    /// ~ a·|E_ext|/ΔV. Bounds the near-wire field error within a few wire
    /// radii. Gated by [`BemOptions::max_azimuthal_spread`].
    pub spread: f64,
    /// Wire on which [`Self::spread`] occurs.
    pub spread_wire: usize,
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
    hits: std::sync::Arc<WireHits>,
    has_current: bool,
    residual: ResidualReport,
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
    if opts.stations_per_segment == 0 {
        return Err(BemError::InvalidOptions(
            "stations_per_segment must be ≥ 1".into(),
        ));
    }
    let gates = [
        opts.max_surface_residual,
        opts.max_junction_residual,
        opts.max_azimuthal_spread,
        opts.junction_zone_factor,
    ];
    if gates.iter().any(|g| g.is_nan() || *g <= 0.0) || opts.residual_azimuths < 3 {
        return Err(BemError::InvalidOptions(
            "residual gates and junction_zone_factor must be > 0; residual_azimuths ≥ 3".into(),
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

    // --- Collocation (dimensionless; λ = x / k) ------------------------
    //
    // Stations at `stations_per_segment` evenly spaced fractions of every
    // segment, starting at the joint (fraction 0) — so joints, where two
    // piecewise-constant charges meet and where crossings concentrate the
    // thin-wire error, are matched too. With more than one station the
    // system is overdetermined and solved in the least-squares sense
    // (normal equations, Cholesky); one station is classic square
    // midpoint collocation (LU).
    let n_st = opts.stations_per_segment;
    let mut stations: Vec<([f64; 3], usize)> = Vec::with_capacity(n * n_st);
    for (i, s) in segs.iter().enumerate() {
        let w = &wires[s.wire];
        for k in 0..n_st {
            let frac = if n_st == 1 {
                0.5
            } else {
                k as f64 / n_st as f64
            };
            // The free end of an open wire carries no end-cap charge in
            // the thin-wire model; don't ask it to hold the potential.
            if frac == 0.0 && !w.closed && s.a == w.v0 {
                continue;
            }
            stations.push((add(verts[s.a], scale(sub(verts[s.b], verts[s.a]), frac)), i));
        }
    }
    let m = stations.len();
    let mut a_mat = tang_la::DMat::<f64>::zeros(m, n);
    for (row, &(x, i)) in stations.iter().enumerate() {
        let ai = wires[segs[i].wire].radius;
        for (j, s) in segs.iter().enumerate() {
            a_mat.set(row, j, reduced_kernel(x, verts[s.a], verts[s.b], s.len, ai));
        }
        if let Some(rc) = chamber_radius_m {
            let xx = dot(x, x);
            for im in &images {
                let d = xx * im.c2 - 2.0 * dot(x, im.y) + rc * rc;
                *a_mat.get_mut(row, im.seg) -= im.weight / d.sqrt();
            }
        }
    }
    let rhs: Vec<f64> = stations
        .iter()
        .map(|&(_, i)| wires[segs[i].wire].potential - device.wall_potential_v)
        .collect();
    let x = if m == n {
        let lu = tang_la::Lu::new(&a_mat).ok_or(BemError::Singular)?;
        lu.solve(&tang_la::DVec::from_slice(&rhs))
    } else {
        // Normal equations G = AᵀA, b = Aᵀr (A is column-major: column j
        // is the response of every station to segment j).
        let mut g = tang_la::DMat::<f64>::zeros(n, n);
        for i in 0..n {
            let ci = a_mat.col(i);
            for j in 0..=i {
                let cj = a_mat.col(j);
                let v: f64 = ci.iter().zip(cj).map(|(p, q)| p * q).sum();
                g.set(i, j, v);
                g.set(j, i, v);
            }
        }
        let b = tang_la::DVec::from_fn(n, |j| {
            a_mat.col(j).iter().zip(&rhs).map(|(p, q)| p * q).sum()
        });
        match tang_la::Cholesky::new(&g) {
            Some(ch) => ch.solve(&b),
            None => tang_la::Lu::new(&g).ok_or(BemError::Singular)?.solve(&b),
        }
    };
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
    let hits = std::sync::Arc::new(WireHits {
        verts: verts.clone(),
        segs: segs.iter().map(|s| (s.a, s.b, s.wire)).collect(),
        radii: radii.clone(),
        index,
    });
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
        hits,
        residual: ResidualReport::default(),
        extent_m,
    };

    // --- Surface residual gate -----------------------------------------
    let rep = field.surface_residual(opts.residual_azimuths, opts.junction_zone_factor);
    field.residual = rep;
    for (kind, worst, wire, threshold) in [
        (
            ResidualKind::Mean,
            rep.mean,
            rep.mean_wire,
            opts.max_surface_residual,
        ),
        (
            ResidualKind::Junction,
            rep.junction_mean,
            rep.junction_wire,
            opts.max_junction_residual,
        ),
        (
            ResidualKind::AzimuthalSpread,
            rep.spread,
            rep.spread_wire,
            opts.max_azimuthal_spread,
        ),
    ] {
        if worst.is_nan() || worst > threshold {
            return Err(BemError::SurfaceResidual {
                kind,
                worst,
                wire,
                threshold,
            });
        }
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

    /// Worst ring-mean surface residual away from junctions (the gated
    /// monopole-consistency number).
    pub fn worst_surface_residual(&self) -> f64 {
        self.residual.mean
    }

    /// The full surface-residual report.
    pub fn residual_report(&self) -> ResidualReport {
        self.residual
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
        self.hits.wire_hit(x)
    }

    /// Smallest `(d − a) / (6a)` over nearby wires (d = distance to the
    /// wire axis), or 1 when no wire is within [`NEAR_WIRE_FACTOR`] radii —
    /// the tracer's near-wire step-refinement ratio.
    pub fn near_wire_ratio(&self, x: [f64; 3]) -> f64 {
        self.hits.near_wire_ratio(x)
    }

    /// Sample the exact-kernel potential on rings of points around every
    /// wire (at each segment midpoint and joint) and summarize.
    fn surface_residual(&self, azimuths: usize, junction_factor: f64) -> ResidualReport {
        let mut rep = ResidualReport::default();
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
                let mut sum = 0.0;
                let mut count = 0usize;
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                let mut clearance = f64::INFINITY;
                for k in 0..azimuths {
                    let ang = 2.0 * std::f64::consts::PI * k as f64 / azimuths as f64;
                    let p = add(c, scale(add(scale(u, ang.cos()), scale(v, ang.sin())), w.radius));
                    // Skip points buried inside any other segment's
                    // conductor (crossings, inner side of bends); measure
                    // the clearance to other wires for junction zoning.
                    let mut buried = false;
                    for &oj in self.hits.index.candidates(p) {
                        let oj = oj as usize;
                        if oj == si {
                            continue;
                        }
                        let o = &self.segs[oj];
                        let ao = self.wires[o.wire].radius;
                        let d = point_segment_distance(p, self.verts[o.a], self.verts[o.b]);
                        if d < 0.98 * ao {
                            buried = true;
                            break;
                        }
                        if o.wire != s.wire {
                            clearance = clearance.min(d / ao);
                        }
                    }
                    if buried {
                        continue;
                    }
                    let r = (self.potential(p) - w.potential) / drop;
                    sum += r;
                    count += 1;
                    lo = lo.min(r);
                    hi = hi.max(r);
                }
                if count == 0 {
                    continue;
                }
                let mean = (sum / count as f64).abs();
                let spread = 0.5 * (hi - lo);
                if spread > rep.spread {
                    rep.spread = spread;
                    rep.spread_wire = s.wire;
                }
                if clearance < junction_factor {
                    if mean > rep.junction_mean {
                        rep.junction_mean = mean;
                        rep.junction_wire = s.wire;
                    }
                } else if mean > rep.mean {
                    rep.mean = mean;
                    rep.mean_wire = s.wire;
                }
            }
        }
        rep
    }

    #[cfg(test)]
    fn segment_count_of(&self, k: usize) -> usize {
        self.wires[k].s1 - self.wires[k].s0
    }
}

/// Collision geometry of a solved [`WireField`] for the Boris tracer:
/// segment-capsule wire hits, spherical (or far-field) wall.
#[derive(Debug, Clone)]
pub struct WireGeometry {
    hits: std::sync::Arc<WireHits>,
    wall_radius_m: f64,
}

impl crate::trace::TraceGeometry for WireGeometry {
    fn wire_hit(&self, p: [f64; 3]) -> Option<usize> {
        self.hits.wire_hit(p)
    }

    fn at_wall(&self, p: [f64; 3]) -> bool {
        dot(p, p) > self.wall_radius_m * self.wall_radius_m
    }

    fn near_wire_ratio(&self, p: [f64; 3]) -> f64 {
        self.hits.near_wire_ratio(p)
    }

    fn exit_distance(&self, p: [f64; 3], v: [f64; 3]) -> f64 {
        let speed = norm(v);
        if speed < 1e-12 {
            return 0.0;
        }
        let d = scale(v, 1.0 / speed);
        let b = dot(p, d);
        let c = dot(p, p) - self.wall_radius_m * self.wall_radius_m;
        let disc = b * b - c;
        if disc <= 0.0 {
            return 0.0;
        }
        (-b + disc.sqrt()).max(0.0)
    }
}

impl WireField {
    /// Smallest distance of any wire centerline vertex from the origin, m
    /// (sizes the tracer's core region, like the 2D path).
    pub fn min_wire_spherical_radius_m(&self) -> f64 {
        self.verts.iter().map(|v| norm(*v)).fold(f64::INFINITY, f64::min)
    }

    /// The wall radius the tracer uses, m: the chamber sphere less half a
    /// step, or — unbounded — four times the electrode extent.
    pub fn trace_wall_radius_m(&self, step_length_m: f64) -> f64 {
        match self.chamber_radius_m {
            Some(r) => r - 0.5 * step_length_m,
            None => 4.0 * self.extent_m,
        }
    }

    /// Default tracer step length, m: a quarter of the smallest wire
    /// radius, capped at 0.25 mm (the 2D hobby-fusor grid spacing). Wires
    /// are the only fine structure of the field; the tracer also refines
    /// its step within 7 radii of a wire. Mean energy drift on the −30 kV
    /// 6-great-circle grid: 1.5% at 0.5 mm, 0.7% at 0.25 mm.
    pub fn default_step_length_m(&self) -> f64 {
        (0.25 * self.min_wire_radius_m).min(0.25e-3)
    }

    /// A Boris tracer over this field with segment-capsule wire hits and a
    /// spherical wall. `step_length_m` plays the role of the 2D grid
    /// spacing (see [`Self::default_step_length_m`]). Core radius is 0.35×
    /// the innermost wire radius; launch shell is
    /// `opts.launch_shell_fraction` × the chamber radius.
    pub fn tracer(
        &self,
        step_length_m: f64,
        opts: crate::trace::TraceOptions,
    ) -> crate::trace::Tracer<'_> {
        let wall = self.trace_wall_radius_m(step_length_m);
        let reference = self.chamber_radius_m.unwrap_or(wall);
        crate::trace::Tracer::with_geometry(
            self,
            Box::new(WireGeometry {
                hits: self.hits.clone(),
                wall_radius_m: wall,
            }),
            crate::trace::TraceScales {
                step_length_m,
                core_radius_m: 0.35 * self.min_wire_spherical_radius_m(),
                shell_radius_m: opts.launch_shell_fraction * reference,
                potential_drop_v: self.max_drop_v,
            },
            opts,
        )
    }
}

/// Launch `n` particles at rest on the tracer's launch shell along
/// deterministic Fibonacci directions — full-sphere coverage with no
/// preferred meridian, which a non-axisymmetric grid requires (the
/// axisymmetric [`crate::trace::Tracer::launch_ensemble`] samples a single
/// meridian plane).
pub fn launch_fibonacci(
    tracer: &crate::trace::Tracer<'_>,
    species: crate::trace::Species,
    n: usize,
) -> Vec<crate::trace::TraceOutcome> {
    let r = tracer.shell_radius_m();
    crate::wire3d::fibonacci_sphere(n)
        .into_iter()
        .map(|d| tracer.trace_from(species, scale(d, r), [0.0; 3]))
        .collect()
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
