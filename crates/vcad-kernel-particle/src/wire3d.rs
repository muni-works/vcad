//! Non-axisymmetric electrode geometry: thin wires along arbitrary 3D
//! paths inside a grounded chamber.
//!
//! The axisymmetric [`crate::device::Device`] can only hold coaxial
//! latitude rings, so a real fusor cathode — a globe of great circles, or
//! latitude rings plus meridians — has no faithful representation there.
//! [`Device3d`] holds any set of wires (circles, arcs, polylines), each
//! with its own radius, potential and (on closed loops) current, inside a
//! grounded **spherical** chamber or in free space. The existing ring
//! devices are the special case [`Device3d::from_rings`].
//!
//! Geometry is in millimeters (vcad convention); potentials in volts;
//! currents in ampere-turns. The field solve lives in [`crate::bem`].

use crate::device::Device;

/// Centerline path of one wire electrode.
#[derive(Debug, Clone, PartialEq)]
pub enum WirePath {
    /// A full circle of `radius_mm` about `center_mm` in the plane normal
    /// to `normal` (need not be unit). Traversed counter-clockwise about
    /// `normal` — the positive current direction.
    Circle {
        /// Circle center, mm.
        center_mm: [f64; 3],
        /// Plane normal (any nonzero length).
        normal: [f64; 3],
        /// Circle radius, mm.
        radius_mm: f64,
    },
    /// A circular arc about `center_mm` in the plane normal to `normal`,
    /// starting at `start_mm` (which must lie in that plane; its distance
    /// from the center is the radius) and sweeping `sweep_deg`
    /// counter-clockwise about `normal`.
    Arc {
        /// Arc center, mm.
        center_mm: [f64; 3],
        /// Plane normal (any nonzero length).
        normal: [f64; 3],
        /// Start point on the arc, mm.
        start_mm: [f64; 3],
        /// Sweep angle, degrees, in (0, 360].
        sweep_deg: f64,
    },
    /// Straight segments through `points_mm`; `closed` joins the last
    /// point back to the first.
    Polyline {
        /// Vertices, mm (at least two).
        points_mm: Vec<[f64; 3]>,
        /// Whether the path is a closed loop.
        closed: bool,
    },
}

/// One wire electrode.
#[derive(Debug, Clone, PartialEq)]
pub struct Wire3d {
    /// Centerline path.
    pub path: WirePath,
    /// Wire (minor) radius, mm.
    pub wire_radius_mm: f64,
    /// Electrode potential, volts.
    pub potential_v: f64,
    /// Current along the path direction, ampere-turns. Only allowed on
    /// closed paths (an open wire cannot carry a steady current alone).
    pub ampere_turns: f64,
}

/// The grounded boundary enclosing the electrodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chamber3d {
    /// A conducting sphere of `radius_mm` centered on the origin, held at
    /// [`Device3d::wall_potential_v`]. Solved exactly with Kelvin images —
    /// no wall unknowns.
    Sphere {
        /// Inner radius, mm.
        radius_mm: f64,
    },
    /// No chamber: potential → 0 at infinity (validation / far-field
    /// studies). `wall_potential_v` must be 0.
    Unbounded,
}

/// A complete 3D device: chamber + wire electrodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Device3d {
    /// Enclosing chamber.
    pub chamber: Chamber3d,
    /// Chamber wall potential, volts (normally 0 — grounded anode).
    pub wall_potential_v: f64,
    /// Wire electrodes.
    pub wires: Vec<Wire3d>,
}

/// Validation failures for a [`Device3d`] (fail-closed: the solver never
/// silently repairs a bad geometry).
#[derive(Debug, Clone, PartialEq)]
pub enum GeometryError {
    /// The device has no wires.
    NoWires,
    /// A numeric field is not finite or out of its domain.
    InvalidValue {
        /// Wire index (`None` = chamber-level field).
        wire: Option<usize>,
        /// What was wrong.
        what: String,
    },
    /// A wire leaves the chamber or comes within one wire radius of it.
    OutsideChamber {
        /// Wire index.
        wire: usize,
        /// Largest centerline distance from the origin, mm.
        max_radius_mm: f64,
    },
    /// Current on an open path.
    CurrentOnOpenPath {
        /// Wire index.
        wire: usize,
    },
    /// `Unbounded` chamber with a nonzero wall potential.
    UnboundedWallPotential,
}

impl std::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeometryError::NoWires => write!(f, "device has no wire electrodes"),
            GeometryError::InvalidValue { wire, what } => match wire {
                Some(k) => write!(f, "wire {k}: {what}"),
                None => write!(f, "chamber: {what}"),
            },
            GeometryError::OutsideChamber {
                wire,
                max_radius_mm,
            } => write!(
                f,
                "wire {wire} reaches {max_radius_mm:.3} mm from the center: outside the chamber \
                 or within one wire radius of the wall"
            ),
            GeometryError::CurrentOnOpenPath { wire } => write!(
                f,
                "wire {wire} carries current on an open path (only closed loops may carry \
                 ampere-turns)"
            ),
            GeometryError::UnboundedWallPotential => {
                write!(f, "an unbounded chamber requires wall_potential_v = 0")
            }
        }
    }
}

impl std::error::Error for GeometryError {}

/// Named symmetric great-circle layouts for globe cathodes.
///
/// "N great circles" does not define a grid by itself; these are the
/// standard maximally symmetric choices, each given by its plane normals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreatCircleLayout {
    /// 3 mutually orthogonal circles (octahedral: 8 triangular openings).
    Orthogonal3,
    /// 4 circles normal to the cube body diagonals (cuboctahedral:
    /// 8 triangles + 6 squares).
    Cuboctahedral4,
    /// 6 circles normal to the icosahedron's six 5-fold axes — the
    /// decagonal great circles of the icosidodecahedron (12 pentagonal +
    /// 20 triangular openings; every pair of circles crosses at
    /// arccos(1/√5) ≈ 63.4°). The classic "6 great circle" fusor grid.
    Icosidodecahedral6,
}

impl GreatCircleLayout {
    /// Unit plane normals of the layout's circles.
    pub fn normals(self) -> Vec<[f64; 3]> {
        match self {
            GreatCircleLayout::Orthogonal3 => {
                vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            }
            GreatCircleLayout::Cuboctahedral4 => {
                let s = 1.0 / 3f64.sqrt();
                vec![[s, s, s], [s, -s, -s], [-s, s, -s], [-s, -s, s]]
            }
            GreatCircleLayout::Icosidodecahedral6 => {
                // Icosahedron vertices (0, ±1, ±φ) and cyclic permutations;
                // one per antipodal pair gives the six 5-fold axes.
                let phi = 0.5 * (1.0 + 5f64.sqrt());
                let raw = [
                    [0.0, 1.0, phi],
                    [0.0, 1.0, -phi],
                    [1.0, phi, 0.0],
                    [1.0, -phi, 0.0],
                    [phi, 0.0, 1.0],
                    [-phi, 0.0, 1.0],
                ];
                raw.iter().map(|v| normalize(*v)).collect()
            }
        }
    }
}

impl WirePath {
    /// Whether the path is a closed loop.
    pub fn is_closed(&self) -> bool {
        match self {
            WirePath::Circle { .. } => true,
            WirePath::Arc { sweep_deg, .. } => (*sweep_deg - 360.0).abs() < 1e-9,
            WirePath::Polyline { closed, .. } => *closed,
        }
    }

    /// Centerline vertices in **meters**, refined so no chord is longer
    /// than `max_len_m` or sags more than `max_sag_m` off a circular path.
    /// For closed paths the first vertex is *not* repeated at the end.
    pub fn vertices_m(&self, max_len_m: f64, max_sag_m: f64) -> Vec<[f64; 3]> {
        match self {
            WirePath::Circle {
                center_mm,
                normal,
                radius_mm,
            } => {
                let (u, w) = plane_basis(*normal);
                let r = radius_mm * 1e-3;
                let n = arc_divisions(r, 2.0 * std::f64::consts::PI, max_len_m, max_sag_m);
                let c = scale(*center_mm, 1e-3);
                (0..n)
                    .map(|k| {
                        let t = 2.0 * std::f64::consts::PI * k as f64 / n as f64;
                        add(c, add(scale(u, r * t.cos()), scale(w, r * t.sin())))
                    })
                    .collect()
            }
            WirePath::Arc {
                center_mm,
                normal,
                start_mm,
                sweep_deg,
            } => {
                let nhat = normalize(*normal);
                let c = scale(*center_mm, 1e-3);
                let rel = sub(scale(*start_mm, 1e-3), c);
                let r = norm(rel);
                let u = scale(rel, 1.0 / r);
                let w = cross(nhat, u);
                let sweep = sweep_deg.to_radians();
                let n = arc_divisions(r, sweep, max_len_m, max_sag_m);
                let closed = self.is_closed();
                let count = if closed { n } else { n + 1 };
                (0..count)
                    .map(|k| {
                        let t = sweep * k as f64 / n as f64;
                        add(c, add(scale(u, r * t.cos()), scale(w, r * t.sin())))
                    })
                    .collect()
            }
            WirePath::Polyline { points_mm, closed } => {
                let pts: Vec<[f64; 3]> = points_mm.iter().map(|p| scale(*p, 1e-3)).collect();
                let mut out = Vec::new();
                let n_edges = if *closed { pts.len() } else { pts.len() - 1 };
                for e in 0..n_edges {
                    let a = pts[e];
                    let b = pts[(e + 1) % pts.len()];
                    let len = norm(sub(b, a));
                    let m = ((len / max_len_m).ceil() as usize).max(1);
                    for k in 0..m {
                        out.push(add(a, scale(sub(b, a), k as f64 / m as f64)));
                    }
                }
                if !*closed {
                    out.push(*pts.last().expect("validated: ≥ 2 points"));
                }
                out
            }
        }
    }
}

fn arc_divisions(r: f64, sweep: f64, max_len_m: f64, max_sag_m: f64) -> usize {
    let by_len = (r * sweep / max_len_m).ceil();
    // Sagitta of a chord subtending angle θ: r(1 − cos(θ/2)) ≤ max_sag.
    let by_sag = if max_sag_m < r {
        (sweep / (2.0 * (1.0 - max_sag_m / r).acos())).ceil()
    } else {
        1.0
    };
    let min = (8.0 * sweep / (2.0 * std::f64::consts::PI)).ceil();
    by_len.max(by_sag).max(min).max(1.0) as usize
}

impl Device3d {
    /// The axisymmetric special case: every ring of `device` becomes a
    /// circle normal to z, inside `chamber` (the cylinder of `device` is
    /// not carried over — choose the chamber explicitly).
    pub fn from_rings(device: &Device, chamber: Chamber3d) -> Self {
        Self {
            chamber,
            wall_potential_v: device.wall_potential_v,
            wires: device
                .rings
                .iter()
                .map(|r| Wire3d {
                    path: WirePath::Circle {
                        center_mm: [0.0, 0.0, r.z_mm],
                        normal: [0.0, 0.0, 1.0],
                        radius_mm: r.ring_radius_mm,
                    },
                    wire_radius_mm: r.wire_radius_mm,
                    potential_v: r.potential_v,
                    ampere_turns: r.ampere_turns,
                })
                .collect(),
        }
    }

    /// A great-circle globe cathode: one circle of `grid_radius_mm` per
    /// plane normal, all centered on the origin, inside a grounded sphere.
    pub fn great_circle_globe(
        chamber_radius_mm: f64,
        grid_radius_mm: f64,
        normals: &[[f64; 3]],
        wire_radius_mm: f64,
        cathode_v: f64,
    ) -> Self {
        Self {
            chamber: Chamber3d::Sphere {
                radius_mm: chamber_radius_mm,
            },
            wall_potential_v: 0.0,
            wires: normals
                .iter()
                .map(|n| Wire3d {
                    path: WirePath::Circle {
                        center_mm: [0.0; 3],
                        normal: *n,
                        radius_mm: grid_radius_mm,
                    },
                    wire_radius_mm,
                    potential_v: cathode_v,
                    ampere_turns: 0.0,
                })
                .collect(),
        }
    }

    /// A latitude/longitude globe: `n_lat` latitude rings at polar angles
    /// `π(k+1)/(n_lat+1)` (the [`Device::classic_fusor`] placement) plus
    /// `n_long` pole-to-pole meridian half-circles at azimuths `2πj/n_long`.
    pub fn lat_long_globe(
        chamber_radius_mm: f64,
        grid_radius_mm: f64,
        n_lat: usize,
        n_long: usize,
        wire_radius_mm: f64,
        cathode_v: f64,
    ) -> Self {
        let mut wires = Vec::with_capacity(n_lat + n_long);
        for k in 0..n_lat {
            let theta = std::f64::consts::PI * (k as f64 + 1.0) / (n_lat as f64 + 1.0);
            wires.push(Wire3d {
                path: WirePath::Circle {
                    center_mm: [0.0, 0.0, grid_radius_mm * theta.cos()],
                    normal: [0.0, 0.0, 1.0],
                    radius_mm: grid_radius_mm * theta.sin(),
                },
                wire_radius_mm,
                potential_v: cathode_v,
                ampere_turns: 0.0,
            });
        }
        for j in 0..n_long {
            let az = 2.0 * std::f64::consts::PI * j as f64 / n_long as f64;
            // Rotating +z about (−sin az, cos az, 0) sweeps through azimuth az.
            wires.push(Wire3d {
                path: WirePath::Arc {
                    center_mm: [0.0; 3],
                    normal: [-az.sin(), az.cos(), 0.0],
                    start_mm: [0.0, 0.0, grid_radius_mm],
                    sweep_deg: 180.0,
                },
                wire_radius_mm,
                potential_v: cathode_v,
                ampere_turns: 0.0,
            });
        }
        Self {
            chamber: Chamber3d::Sphere {
                radius_mm: chamber_radius_mm,
            },
            wall_potential_v: 0.0,
            wires,
        }
    }

    /// Fail-closed geometry validation (finite values, positive radii,
    /// wires strictly inside the chamber, currents only on closed loops).
    pub fn validate(&self) -> Result<(), GeometryError> {
        if self.wires.is_empty() {
            return Err(GeometryError::NoWires);
        }
        let bad = |wire: Option<usize>, what: &str| GeometryError::InvalidValue {
            wire,
            what: what.to_string(),
        };
        if !self.wall_potential_v.is_finite() {
            return Err(bad(None, "wall_potential_v is not finite"));
        }
        let r_chamber = match self.chamber {
            Chamber3d::Sphere { radius_mm } => {
                if !(radius_mm.is_finite() && radius_mm > 0.0) {
                    return Err(bad(None, "sphere radius must be finite and positive"));
                }
                Some(radius_mm)
            }
            Chamber3d::Unbounded => {
                if self.wall_potential_v != 0.0 {
                    return Err(GeometryError::UnboundedWallPotential);
                }
                None
            }
        };
        for (k, w) in self.wires.iter().enumerate() {
            let some = Some(k);
            if !(w.wire_radius_mm.is_finite() && w.wire_radius_mm > 0.0) {
                return Err(bad(some, "wire_radius_mm must be finite and positive"));
            }
            if !w.potential_v.is_finite() || !w.ampere_turns.is_finite() {
                return Err(bad(some, "potential/current must be finite"));
            }
            if w.ampere_turns != 0.0 && !w.path.is_closed() {
                return Err(GeometryError::CurrentOnOpenPath { wire: k });
            }
            let finite3 = |v: &[f64; 3]| v.iter().all(|x| x.is_finite());
            match &w.path {
                WirePath::Circle {
                    center_mm,
                    normal,
                    radius_mm,
                } => {
                    if !finite3(center_mm) || !finite3(normal) || norm(*normal) < 1e-12 {
                        return Err(bad(some, "circle center/normal invalid"));
                    }
                    if !(radius_mm.is_finite() && *radius_mm > 2.0 * w.wire_radius_mm) {
                        return Err(bad(
                            some,
                            "circle radius must exceed twice the wire radius (thin-wire limit)",
                        ));
                    }
                }
                WirePath::Arc {
                    center_mm,
                    normal,
                    start_mm,
                    sweep_deg,
                } => {
                    if !finite3(center_mm) || !finite3(normal) || !finite3(start_mm) {
                        return Err(bad(some, "arc values must be finite"));
                    }
                    let nn = norm(*normal);
                    if nn < 1e-12 {
                        return Err(bad(some, "arc normal is zero"));
                    }
                    let rel = sub(*start_mm, *center_mm);
                    let r = norm(rel);
                    if r <= 2.0 * w.wire_radius_mm {
                        return Err(bad(
                            some,
                            "arc radius must exceed twice the wire radius (thin-wire limit)",
                        ));
                    }
                    if dot(rel, *normal).abs() > 1e-6 * r * nn {
                        return Err(bad(some, "arc start point is not in the arc plane"));
                    }
                    if !(sweep_deg.is_finite() && *sweep_deg > 0.0 && *sweep_deg <= 360.0) {
                        return Err(bad(some, "arc sweep must be in (0, 360] degrees"));
                    }
                }
                WirePath::Polyline { points_mm, closed } => {
                    let need = if *closed { 3 } else { 2 };
                    if points_mm.len() < need {
                        return Err(bad(some, "polyline has too few points"));
                    }
                    if !points_mm.iter().all(finite3) {
                        return Err(bad(some, "polyline points must be finite"));
                    }
                    let n_edges = if *closed {
                        points_mm.len()
                    } else {
                        points_mm.len() - 1
                    };
                    for e in 0..n_edges {
                        let a = points_mm[e];
                        let b = points_mm[(e + 1) % points_mm.len()];
                        if norm(sub(b, a)) < 1e-9 {
                            return Err(bad(some, "polyline has a zero-length edge"));
                        }
                    }
                }
            }
            if let Some(rc) = r_chamber {
                let max_r = self.max_centerline_radius_mm(k);
                if max_r + w.wire_radius_mm >= rc {
                    return Err(GeometryError::OutsideChamber {
                        wire: k,
                        max_radius_mm: max_r,
                    });
                }
            }
        }
        Ok(())
    }

    /// Largest distance from the origin of wire `k`'s centerline, mm.
    fn max_centerline_radius_mm(&self, k: usize) -> f64 {
        let w = &self.wires[k];
        w.path
            .vertices_m(0.25e-3, 1e-6)
            .iter()
            .map(|p| norm(*p) * 1e3)
            .fold(0.0, f64::max)
    }

    /// Deepest electrode-to-wall potential difference, volts (absolute).
    pub fn max_potential_drop_v(&self) -> f64 {
        self.wires
            .iter()
            .map(|w| (w.potential_v - self.wall_potential_v).abs())
            .fold(0.0, f64::max)
    }

    /// Smallest distance from the origin of any wire centerline vertex, mm
    /// — the 3D analogue of [`Device::min_ring_spherical_radius_mm`], used
    /// to size the tracer's core region.
    pub fn min_wire_spherical_radius_mm(&self) -> f64 {
        self.wires
            .iter()
            .flat_map(|w| w.path.vertices_m(1e-3, 1e-5))
            .map(|p| norm(p) * 1e3)
            .fold(f64::INFINITY, f64::min)
    }

    /// Radial-shadow transparency seen from the origin: the fraction of
    /// rays from the center (deterministic Fibonacci directions, `n_dirs`
    /// of them) that miss every wire. For a globe centered on the origin
    /// this is the purely geometric grid transparency — no ion optics.
    pub fn geometric_transparency(&self, n_dirs: usize) -> f64 {
        let segs: Vec<([f64; 3], [f64; 3], f64)> = self
            .wires
            .iter()
            .flat_map(|w| {
                let v = w.path.vertices_m(0.5e-3, 1e-6);
                let closed = w.path.is_closed();
                let a = w.wire_radius_mm * 1e-3;
                let n = if closed { v.len() } else { v.len() - 1 };
                (0..n)
                    .map(|i| (v[i], v[(i + 1) % v.len()], a))
                    .collect::<Vec<_>>()
            })
            .collect();
        let reach = segs
            .iter()
            .map(|s| norm(s.0).max(norm(s.1)) + s.2)
            .fold(0.0, f64::max)
            * 2.0;
        let dirs = fibonacci_sphere(n_dirs);
        let clear = dirs
            .iter()
            .filter(|d| {
                let far = scale(**d, reach);
                !segs
                    .iter()
                    .any(|(a, b, r)| segment_segment_distance([0.0; 3], far, *a, *b) <= *r)
            })
            .count();
        clear as f64 / dirs.len().max(1) as f64
    }
}

/// `n` near-uniform unit vectors on the sphere (Fibonacci lattice) —
/// deterministic launch / probe directions with no preferred meridian.
pub fn fibonacci_sphere(n: usize) -> Vec<[f64; 3]> {
    let n = n.max(1);
    let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
    (0..n)
        .map(|k| {
            let z = 1.0 - 2.0 * (k as f64 + 0.5) / n as f64;
            let r = (1.0 - z * z).max(0.0).sqrt();
            let t = golden * k as f64;
            [r * t.cos(), r * t.sin(), z]
        })
        .collect()
}

/// Minimum distance between segments `p0–p1` and `q0–q1`.
pub fn segment_segment_distance(p0: [f64; 3], p1: [f64; 3], q0: [f64; 3], q1: [f64; 3]) -> f64 {
    let d1 = sub(p1, p0);
    let d2 = sub(q1, q0);
    let r = sub(p0, q0);
    let a = dot(d1, d1);
    let e = dot(d2, d2);
    let f = dot(d2, r);
    let (s, t);
    if a <= 1e-300 && e <= 1e-300 {
        return norm(r);
    }
    if a <= 1e-300 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = dot(d1, r);
        if e <= 1e-300 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = dot(d1, d2);
            let denom = a * e - b * b;
            let mut ss = if denom > 1e-300 * a * e {
                ((b * f - c * e) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut tt = (b * ss + f) / e;
            if tt < 0.0 {
                tt = 0.0;
                ss = (-c / a).clamp(0.0, 1.0);
            } else if tt > 1.0 {
                tt = 1.0;
                ss = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = ss;
            t = tt;
        }
    }
    norm(sub(add(p0, scale(d1, s)), add(q0, scale(d2, t))))
}

/// Distance from point `p` to segment `a–b`.
pub fn point_segment_distance(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = sub(b, a);
    let l2 = dot(ab, ab);
    let t = if l2 > 0.0 {
        (dot(sub(p, a), ab) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    norm(sub(p, add(a, scale(ab, t))))
}

/// Orthonormal in-plane basis `(u, w)` for a plane with normal `n`, with
/// `u × w = n̂` (so increasing angle is counter-clockwise about `n`).
pub(crate) fn plane_basis(n: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let nh = normalize(n);
    // Prefer x as the reference so z-normal circles start on +x, matching
    // the axisymmetric convention.
    let reference = if nh[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = normalize(sub(reference, scale(nh, dot(reference, nh))));
    let w = cross(nh, u);
    (u, w)
}

#[inline]
pub(crate) fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub(crate) fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
#[inline]
pub(crate) fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
#[inline]
pub(crate) fn normalize(a: [f64; 3]) -> [f64; 3] {
    scale(a, 1.0 / norm(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icosidodecahedral_circles_cross_at_63_degrees() {
        let n = GreatCircleLayout::Icosidodecahedral6.normals();
        assert_eq!(n.len(), 6);
        for i in 0..6 {
            assert!((norm(n[i]) - 1.0).abs() < 1e-12);
            for j in i + 1..6 {
                // Any two icosahedral 5-fold axes have |cos| = 1/√5, so
                // every pair of circles crosses at ≈ 63.4°.
                let c = dot(n[i], n[j]).abs();
                assert!((c - 1.0 / 5f64.sqrt()).abs() < 1e-12, "cos={c}");
            }
        }
    }

    #[test]
    fn lat_long_globe_wires_lie_on_the_sphere() {
        let d = Device3d::lat_long_globe(76.0, 19.0, 3, 4, 1.0, -30e3);
        d.validate().unwrap();
        assert_eq!(d.wires.len(), 7);
        for w in &d.wires {
            for v in w.path.vertices_m(1e-3, 1e-6) {
                assert!((norm(v) * 1e3 - 19.0).abs() < 1e-9);
            }
        }
        // Meridians run pole to pole: first vertex N pole, last S pole.
        let v = d.wires[3].path.vertices_m(1e-3, 1e-6);
        assert!((v[0][2] * 1e3 - 19.0).abs() < 1e-9);
        assert!((v.last().unwrap()[2] * 1e3 + 19.0).abs() < 1e-9);
    }

    #[test]
    fn validation_fails_closed() {
        let mut d = Device3d::great_circle_globe(
            76.0,
            19.0,
            &GreatCircleLayout::Orthogonal3.normals(),
            1.0,
            -30e3,
        );
        d.validate().unwrap();
        d.chamber = Chamber3d::Sphere { radius_mm: 19.5 };
        assert!(matches!(
            d.validate(),
            Err(GeometryError::OutsideChamber { .. })
        ));
        d.chamber = Chamber3d::Sphere { radius_mm: 76.0 };
        d.wires[0].path = WirePath::Polyline {
            points_mm: vec![[0.0; 3], [5.0, 0.0, 0.0]],
            closed: false,
        };
        d.wires[0].ampere_turns = 10.0;
        assert_eq!(
            d.validate(),
            Err(GeometryError::CurrentOnOpenPath { wire: 0 })
        );
        d.wires[0].ampere_turns = 0.0;
        d.wires[0].wire_radius_mm = f64::NAN;
        assert!(d.validate().is_err());
    }

    #[test]
    fn geometric_transparency_of_one_great_circle_is_one_minus_a_over_r() {
        // One great circle shadows a band of half-width asin(a/R): blocked
        // solid-angle fraction exactly a/R.
        let d = Device3d::great_circle_globe(76.0, 20.0, &[[0.0, 0.0, 1.0]], 1.0, -1.0);
        let t = d.geometric_transparency(40_000);
        assert!((t - (1.0 - 1.0 / 20.0)).abs() < 2e-3, "t={t}");
    }

    #[test]
    fn segment_distance_basics() {
        let d = segment_segment_distance([0.0; 3], [1.0, 0.0, 0.0], [0.5, 1.0, -1.0], [0.5, 1.0, 1.0]);
        assert!((d - 1.0).abs() < 1e-12);
        let d = point_segment_distance([2.0, 1.0, 0.0], [0.0; 3], [1.0, 0.0, 0.0]);
        assert!((d - 2f64.sqrt()).abs() < 1e-12);
    }
}
