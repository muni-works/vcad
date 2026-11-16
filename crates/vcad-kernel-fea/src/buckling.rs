//! Linear eigenvalue buckling: `(K + λ(K_G + K_L))·φ = 0`.
//!
//! A static solve under a *reference* load gives the prestress; the
//! geometric stiffness `K_G` built from it scales linearly with the load,
//! so the lowest positive `λ` with a singular tangent stiffness is the
//! bifurcation load factor — `λ × reference load` is the critical load —
//! and `φ` is the buckled shape.
//!
//! **Element.** An 8-node isoparametric brick with Wilson–Taylor
//! incompatible modes (the [`crate::modal`] element, generalized to
//! distorted geometry with Taylor's centroid-Jacobian modification so it
//! still passes the patch test). Plain trilinear bricks lock in bending,
//! and buckling *is* bending; this brick reproduces pure bending exactly on
//! a rectangle, so one or two cells through a wall already bend correctly.
//! The prestress is recovered at the 2×2×2 Gauss points *including* the
//! condensed bubble strains, then `K_G = ∫ ∇Nᵀ σ ∇N dV` on the compatible
//! shape functions.
//!
//! **Pressure follows the surface.** A pressure load stays normal to the
//! deforming wall, which adds a load-stiffness `K_L = −∂f/∂u` to the
//! tangent. Leaving it out computes buckling under a *dead* load of the
//! same initial direction: for a ring under external pressure that is the
//! `n²` vs `n² − 1` difference — a third too high at `n = 2`, the wrong
//! side for a vacuum vessel. Pressure loads are follower loads by default
//! (`follower: false` for a cap load transmitted as a dead end force).
//! `K_L` is symmetric for a closed surface; on an open one we keep its
//! symmetric part and report the dropped asymmetry.
//!
//! **Eigensolver.** The pencil is solved as `(K_G + K_L)·x = μ·K·x` with
//! the [`crate::modal`] LOBPCG — `K` (SPD once supported) plays the mass,
//! its exact skyline Cholesky (or incomplete Cholesky) preconditions — and
//! `λ = −1/μ` for the most negative `μ`. No negative `μ` means the load
//! stiffens the structure everywhere: [`BucklingError::NoBuckling`].
//!
//! **Supports.** Box-region and explicit node-set DOF fixes, plus an
//! automatic 3-2-1 kinematic constraint for self-equilibrated loads (a
//! closed vessel under uniform pressure needs no supports — but `K` must be
//! nonsingular). The 3-2-1 set is statically determinate, so a balanced
//! load leaves its reactions at zero and the prestress untouched; the
//! result reports the reaction residual so a not-quite-balanced load is
//! visible.
//!
//! **Honesty.** Linear bifurcation of a *perfect* structure. For columns and
//! plates that is the design number; for shells it is **unconservative** —
//! real cylinders and spheres collapse well below it because of
//! imperfections. Every claim says so, and the closed-form screen
//! ([`crate::shell`]) carries the cited knockdowns. Curved thin walls also
//! need a body-fitted mesh: the lattice fill staircases them, so for
//! cylinders and spheres use [`cylinder_shell`] / [`sphere_shell`], which
//! is how the validation tests below check this solver against Bresse,
//! Batdorf and Zoelly.
//!
//! Units: the crate-wide mm-N-MPa system.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tang_sparse::{CooMatrix, CsrMatrix};
use vcad_kernel_tessellate::TriangleMesh;

use crate::convergence::ConvergenceVerdict;
use crate::mesh::{hex_fill_conforming, HexMesh, MeshError};
use crate::modal::{chol_solve, dense_cholesky, lobpcg, Precond, Skyline};
use crate::spec::{RegionBox, Support};

// ─── Mesh ────────────────────────────────────────────────────────────────

/// A general 8-node hexahedral mesh. Corner `c` of a cell sits at
/// parametric `(±1, ±1, ±1)` with the sign of axis `a` given by bit `a` of
/// `c` (the [`HexMesh`] convention); cells must have positive Jacobian
/// (the generators here and [`Hex8Mesh::orient`] ensure it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hex8Mesh {
    /// Node positions, mm.
    pub nodes: Vec<[f64; 3]>,
    /// Cells, 8 node indices each.
    pub cells: Vec<[u32; 8]>,
}

impl From<&HexMesh> for Hex8Mesh {
    fn from(h: &HexMesh) -> Self {
        Hex8Mesh {
            nodes: h.nodes.clone(),
            cells: h.cells.clone(),
        }
    }
}

/// A quadrilateral boundary face, ordered so `x_ξ × x_η` points **out of**
/// the solid (corners in loop order).
pub type Face = [u32; 4];

/// Parametric sign of corner `c` along axis `a`.
fn sgn(c: usize, a: usize) -> f64 {
    if (c >> a) & 1 == 1 {
        1.0
    } else {
        -1.0
    }
}

impl Hex8Mesh {
    /// Flip any inside-out cell (negative centroid Jacobian) by mirroring
    /// its first parametric axis.
    pub fn orient(&mut self) {
        for ci in 0..self.cells.len() {
            let x = self.cell_coords(ci);
            if det3(jacobian(&x, [0.0; 3])) < 0.0 {
                let c = self.cells[ci];
                self.cells[ci] = [c[1], c[0], c[3], c[2], c[5], c[4], c[7], c[6]];
            }
        }
    }

    fn cell_coords(&self, ci: usize) -> [[f64; 3]; 8] {
        self.cells[ci].map(|n| self.nodes[n as usize])
    }

    /// Total volume, mm³ (2×2×2 Gauss).
    pub fn volume(&self) -> f64 {
        let g = 1.0 / 3f64.sqrt();
        (0..self.cells.len())
            .map(|ci| {
                let x = self.cell_coords(ci);
                let mut v = 0.0;
                for c in 0..8 {
                    v += det3(jacobian(&x, [sgn(c, 0) * g, sgn(c, 1) * g, sgn(c, 2) * g]));
                }
                v
            })
            .sum()
    }

    /// Every boundary face (shared by exactly one cell), outward-ordered.
    pub fn boundary_faces(&self) -> Vec<Face> {
        let mut seen: HashMap<[u32; 4], (usize, Face)> = HashMap::new();
        for (ci, cell) in self.cells.iter().enumerate() {
            let x = self.cell_coords(ci);
            let centroid = mean(&x);
            for axis in 0..3 {
                let (b, c) = match axis {
                    0 => (1, 2),
                    1 => (0, 2),
                    _ => (0, 1),
                };
                for side in 0..2usize {
                    let corner = |ib: usize, ic: usize| (side << axis) | (ib << b) | (ic << c);
                    let mut f = [
                        cell[corner(0, 0)],
                        cell[corner(1, 0)],
                        cell[corner(1, 1)],
                        cell[corner(0, 1)],
                    ];
                    let q = f.map(|n| self.nodes[n as usize]);
                    let n = cross(sub(q[2], q[0]), sub(q[3], q[1]));
                    if dot3(n, sub(mean(&q), centroid)) < 0.0 {
                        f.swap(1, 3);
                    }
                    let mut key = f;
                    key.sort_unstable();
                    seen.entry(key).and_modify(|e| e.0 += 1).or_insert((1, f));
                }
            }
        }
        let mut out: Vec<Face> = seen
            .into_values()
            .filter(|(count, _)| *count == 1)
            .map(|(_, f)| f)
            .collect();
        out.sort_unstable();
        out
    }
}

/// Boundary faces visible from outside: a ray from the face centroid along
/// its outward normal escapes without crossing another boundary face. On a
/// hollow vessel this is the outer skin, not the cavity wall. O(F²) — fine
/// for the tens of thousands of faces a lattice part has.
pub fn exterior_faces(mesh: &Hex8Mesh, faces: &[Face]) -> Vec<Face> {
    let tris: Vec<[[f64; 3]; 3]> = faces
        .iter()
        .flat_map(|f| {
            let q = f.map(|n| mesh.nodes[n as usize]);
            [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]
        })
        .collect();
    let bbox: Vec<([f64; 3], [f64; 3])> = tris
        .iter()
        .map(|t| {
            let mut lo = t[0];
            let mut hi = t[0];
            for p in &t[1..] {
                for a in 0..3 {
                    lo[a] = lo[a].min(p[a]);
                    hi[a] = hi[a].max(p[a]);
                }
            }
            (lo, hi)
        })
        .collect();
    faces
        .iter()
        .enumerate()
        .filter(|(fi, f)| {
            let q = f.map(|n| mesh.nodes[n as usize]);
            let n = normalize(cross(sub(q[2], q[0]), sub(q[3], q[1])));
            let size = norm3(sub(q[2], q[0])).max(1e-12);
            let o = add(mean(&q), scale(n, 1e-3 * size));
            !tris.iter().enumerate().any(|(ti, t)| {
                if ti / 2 == *fi {
                    return false;
                }
                let (lo, hi) = bbox[ti];
                if !ray_box(o, n, lo, hi) {
                    return false;
                }
                ray_tri(o, n, t).is_some_and(|s| s > 0.0)
            })
        })
        .map(|(_, f)| *f)
        .collect()
}

fn ray_box(o: [f64; 3], d: [f64; 3], lo: [f64; 3], hi: [f64; 3]) -> bool {
    let (mut t0, mut t1) = (0.0f64, f64::INFINITY);
    for a in 0..3 {
        if d[a].abs() < 1e-300 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return false;
            }
            continue;
        }
        let (mut ta, mut tb) = ((lo[a] - o[a]) / d[a], (hi[a] - o[a]) / d[a]);
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        t0 = t0.max(ta);
        t1 = t1.min(tb);
        if t0 > t1 {
            return false;
        }
    }
    true
}

/// Möller–Trumbore.
fn ray_tri(o: [f64; 3], d: [f64; 3], t: &[[f64; 3]; 3]) -> Option<f64> {
    let e1 = sub(t[1], t[0]);
    let e2 = sub(t[2], t[0]);
    let p = cross(d, e2);
    let det = dot3(e1, p);
    if det.abs() < 1e-300 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, t[0]);
    let u = dot3(s, p) * inv;
    // Inclusive by a hair: a ray through a quad's centroid lands on the
    // split diagonal, and exact bounds let it slip between both halves.
    const EPS: f64 = 1e-9;
    if !(-EPS..=1.0 + EPS).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot3(d, q) * inv;
    if v < -EPS || u + v > 1.0 + EPS {
        return None;
    }
    Some(dot3(e2, q) * inv)
}

// ─── Small vector helpers ──────────────────────────────────────────────

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm3(a: [f64; 3]) -> f64 {
    dot3(a, a).sqrt()
}
fn normalize(a: [f64; 3]) -> [f64; 3] {
    let n = norm3(a).max(1e-300);
    scale(a, 1.0 / n)
}
fn mean<const N: usize>(p: &[[f64; 3]; N]) -> [f64; 3] {
    let mut c = [0.0; 3];
    for q in p {
        for a in 0..3 {
            c[a] += q[a] / N as f64;
        }
    }
    c
}
fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}
#[allow(clippy::needless_range_loop)]
fn inv3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let d = det3(m);
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (i1, i2) = ((j + 1) % 3, (j + 2) % 3);
            let (j1, j2) = ((i + 1) % 3, (i + 2) % 3);
            r[i][j] = (m[i1][j1] * m[i2][j2] - m[i1][j2] * m[i2][j1]) / d;
        }
    }
    r
}

/// `J[i][j] = ∂x_i/∂ξ_j` at parametric point `p`.
fn jacobian(x: &[[f64; 3]; 8], p: [f64; 3]) -> [[f64; 3]; 3] {
    let mut j = [[0.0; 3]; 3];
    for (c, xc) in x.iter().enumerate() {
        let d = shape_dxi(c, p);
        for i in 0..3 {
            for k in 0..3 {
                j[i][k] += xc[i] * d[k];
            }
        }
    }
    j
}

fn shape_dxi(c: usize, p: [f64; 3]) -> [f64; 3] {
    let s = [sgn(c, 0), sgn(c, 1), sgn(c, 2)];
    let f = [1.0 + s[0] * p[0], 1.0 + s[1] * p[1], 1.0 + s[2] * p[2]];
    [
        s[0] * f[1] * f[2] / 8.0,
        f[0] * s[1] * f[2] / 8.0,
        f[0] * f[1] * s[2] / 8.0,
    ]
}

/// Physical gradient `J⁻ᵀ·g`.
fn to_phys(jinv: &[[f64; 3]; 3], g: [f64; 3]) -> [f64; 3] {
    [
        jinv[0][0] * g[0] + jinv[1][0] * g[1] + jinv[2][0] * g[2],
        jinv[0][1] * g[0] + jinv[1][1] * g[1] + jinv[2][1] * g[2],
        jinv[0][2] * g[0] + jinv[1][2] * g[1] + jinv[2][2] * g[2],
    ]
}

// ─── Element ─────────────────────────────────────────────────────────────

const NC: usize = 24;
const NT: usize = 33;

/// Per-Gauss-point gradients: 8 corner + 3 bubble shape functions, and the
/// integration weight `det J`.
struct GaussData {
    grads: [[f64; 3]; 11],
    wdet: f64,
}

fn gauss_data(x: &[[f64; 3]; 8]) -> Option<Vec<GaussData>> {
    let g = 1.0 / 3f64.sqrt();
    let j0 = jacobian(x, [0.0; 3]);
    let det0 = det3(j0);
    if det0 <= 0.0 {
        return None;
    }
    let j0inv = inv3(j0);
    let mut out = Vec::with_capacity(8);
    for gp in 0..8 {
        let p = [sgn(gp, 0) * g, sgn(gp, 1) * g, sgn(gp, 2) * g];
        let j = jacobian(x, p);
        let det = det3(j);
        if det <= 0.0 {
            return None;
        }
        let jinv = inv3(j);
        let mut grads = [[0.0; 3]; 11];
        for (c, gr) in grads.iter_mut().enumerate().take(8) {
            *gr = to_phys(&jinv, shape_dxi(c, p));
        }
        // Taylor's modification: bubble gradients through the centroid
        // Jacobian, scaled by det J0 / det J — the patch test then holds on
        // distorted bricks.
        for a in 0..3 {
            let mut d = [0.0; 3];
            d[a] = -2.0 * p[a];
            grads[8 + a] = scale(to_phys(&j0inv, d), det0 / det);
        }
        out.push(GaussData { grads, wdet: det });
    }
    Some(out)
}

/// Voigt strain columns (xx, yy, zz, xy, yz, zx) of the 33 element DOFs.
fn b_columns(gd: &GaussData) -> [[f64; 6]; NT] {
    let mut bm = [[0.0f64; 6]; NT];
    for (s, gr) in gd.grads.iter().enumerate() {
        for comp in 0..3 {
            let col = if s < 8 {
                3 * s + comp
            } else {
                24 + 3 * (s - 8) + comp
            };
            let r = &mut bm[col];
            match comp {
                0 => {
                    r[0] = gr[0];
                    r[3] = gr[1];
                    r[5] = gr[2];
                }
                1 => {
                    r[1] = gr[1];
                    r[3] = gr[0];
                    r[4] = gr[2];
                }
                _ => {
                    r[2] = gr[2];
                    r[4] = gr[1];
                    r[5] = gr[0];
                }
            }
        }
    }
    bm
}

fn d_times(b: &[f64; 6], lam: f64, mu: f64) -> [f64; 6] {
    let tr = b[0] + b[1] + b[2];
    [
        lam * tr + 2.0 * mu * b[0],
        lam * tr + 2.0 * mu * b[1],
        lam * tr + 2.0 * mu * b[2],
        mu * b[3],
        mu * b[4],
        mu * b[5],
    ]
}

/// Condensed 24×24 stiffness and the recovery operator `T = Kii⁻¹·Kic`
/// (9×24, bubble amplitudes `α = −T·u`).
struct Element {
    kc: Vec<f64>,
    t: Vec<f64>,
}

fn element(x: &[[f64; 3]; 8], lam: f64, mu: f64) -> Option<Element> {
    let gds = gauss_data(x)?;
    let mut k = vec![0.0; NT * NT];
    for gd in &gds {
        let bm = b_columns(gd);
        let db: Vec<[f64; 6]> = bm.iter().map(|b| d_times(b, lam, mu)).collect();
        for i in 0..NT {
            for j in 0..NT {
                let mut s = 0.0;
                for r in 0..6 {
                    s += bm[i][r] * db[j][r];
                }
                k[i * NT + j] += s * gd.wdet;
            }
        }
    }
    let ni = NT - NC;
    let mut kii = vec![0.0; ni * ni];
    for i in 0..ni {
        for j in 0..ni {
            kii[i * ni + j] = k[(NC + i) * NT + NC + j];
        }
    }
    let l = dense_cholesky(&kii, ni)?;
    let mut t = vec![0.0; ni * NC];
    let mut kc = vec![0.0; NC * NC];
    for j in 0..NC {
        let mut y: Vec<f64> = (0..ni).map(|i| k[(NC + i) * NT + j]).collect();
        chol_solve(&l, ni, &mut y);
        for i in 0..ni {
            t[i * NC + j] = y[i];
        }
        for i in 0..NC {
            let mut s = k[i * NT + j];
            for q in 0..ni {
                s -= k[i * NT + NC + q] * y[q];
            }
            kc[i * NC + j] = s;
        }
    }
    for i in 0..NC {
        for j in 0..i {
            let v = 0.5 * (kc[i * NC + j] + kc[j * NC + i]);
            kc[i * NC + j] = v;
            kc[j * NC + i] = v;
        }
    }
    Some(Element { kc, t })
}

/// Gauss-point stresses (Voigt) of one element under displacement `ue`.
fn element_stresses(
    x: &[[f64; 3]; 8],
    el: &Element,
    ue: &[f64; 24],
    lam: f64,
    mu: f64,
) -> Vec<(GaussData, [f64; 6])> {
    let gds = gauss_data(x).expect("element already built");
    let ni = NT - NC;
    let mut alpha = [0.0; 9];
    for (i, a) in alpha.iter_mut().enumerate().take(ni) {
        *a = -(0..NC).map(|j| el.t[i * NC + j] * ue[j]).sum::<f64>();
    }
    gds.into_iter()
        .map(|gd| {
            let bm = b_columns(&gd);
            let mut eps = [0.0; 6];
            for (col, b) in bm.iter().enumerate() {
                let v = if col < NC { ue[col] } else { alpha[col - NC] };
                if v != 0.0 {
                    for r in 0..6 {
                        eps[r] += b[r] * v;
                    }
                }
            }
            let s = d_times(&eps, lam, mu);
            (gd, s)
        })
        .collect()
}

fn von_mises(s: &[f64; 6]) -> f64 {
    let (a, b, c) = (s[0] - s[1], s[1] - s[2], s[2] - s[0]);
    (0.5 * (a * a + b * b + c * c) + 3.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5])).sqrt()
}

// ─── Spec ────────────────────────────────────────────────────────────────

/// Which boundary faces a pressure acts on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FaceSelection {
    /// Faces visible from outside ([`exterior_faces`]) — the outer skin of a
    /// vacuum vessel.
    Exterior,
    /// Every boundary face (a solid part fully immersed).
    AllBoundary,
    /// Boundary faces whose centroid lies in the box.
    Region {
        /// The box.
        region: RegionBox,
    },
    /// Explicit outward-ordered faces (from a generator).
    Faces {
        /// The faces.
        faces: Vec<Face>,
    },
}

/// A uniform pressure. Positive pushes **into** the solid (compressive on
/// the face) — an external pressure on a vessel's outer skin is positive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PressureLoad {
    /// Reference pressure, MPa (scaled by the load factor). Defaults to
    /// one atmosphere, so `λ` reads directly as atmospheres of vacuum.
    #[serde(default = "atm")]
    pub pressure_mpa: f64,
    /// Faces it acts on.
    pub faces: FaceSelection,
    /// Stays normal to the deforming surface (true for a fluid). Default.
    #[serde(default = "yes")]
    pub follower: bool,
}

fn yes() -> bool {
    true
}

fn atm() -> f64 {
    crate::shell::ATM_MPA
}

/// A total force spread equally over the nodes in a box (dead load).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForceLoad {
    /// Node selection.
    pub region: RegionBox,
    /// Total force, N.
    pub force: [f64; 3],
}

/// DOFs held on an explicit node set (generator-labelled rings, symmetry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSupport {
    /// Node indices.
    pub nodes: Vec<u32>,
    /// Which translations are held.
    pub fix: [bool; 3],
}

/// What to compute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BucklingSpec {
    /// Young's modulus, MPa.
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio.
    pub poisson: f64,
    /// Reference pressure loads.
    #[serde(default)]
    pub pressures: Vec<PressureLoad>,
    /// Reference force loads.
    #[serde(default)]
    pub forces: Vec<ForceLoad>,
    /// Box-region supports.
    #[serde(default)]
    pub supports: Vec<Support>,
    /// Node-set supports.
    #[serde(default)]
    pub node_supports: Vec<NodeSupport>,
    /// Add an automatic 3-2-1 kinematic constraint (for self-equilibrated
    /// loads on an otherwise free body).
    #[serde(default)]
    pub kinematic: bool,
    /// Buckling modes wanted (≥ 1).
    #[serde(default = "one")]
    pub modes: usize,
    /// Optional imperfection knockdown to report alongside the bifurcation
    /// factor (e.g. from [`crate::shell::check_shell`]). The bifurcation
    /// factor itself is never knocked down silently.
    #[serde(default)]
    pub knockdown: Option<f64>,
}

fn one() -> usize {
    1
}

/// One buckling mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucklingMode {
    /// Load factor `λ` (critical load = λ × reference load).
    pub load_factor: f64,
    /// Nodal shape (x, y, z per node, interleaved), max |φ| = 1.
    pub shape: Vec<f64>,
}

/// The buckling solution on one mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucklingResult {
    /// Modes, ascending load factor.
    pub modes: Vec<BucklingMode>,
    /// Peak reference-load displacement, mm.
    pub max_displacement_mm: f64,
    /// Peak reference-load Gauss-point von Mises, MPa.
    pub max_von_mises_mpa: f64,
    /// Cells.
    pub cells: usize,
    /// Free DOFs.
    pub dofs: usize,
    /// LOBPCG iterations.
    pub iterations: usize,
    /// `‖K_L − K_Lᵀ‖_F / ‖K_L‖_F` before symmetrization (0 for a closed
    /// pressurized surface; large means an open-edge pressure patch).
    pub load_stiffness_asymmetry: f64,
    /// `‖reaction at the 3-2-1 constraints‖ / ‖applied load‖` (0 when the
    /// load is self-equilibrated; `None` without a kinematic constraint).
    pub kinematic_reaction_rel: Option<f64>,
    /// Total reference force magnitude on pressure faces, N (Σ|f|).
    pub pressure_force_n: f64,
}

/// Failures.
#[derive(Debug, Clone, PartialEq)]
pub enum BucklingError {
    /// Meshing failed.
    Mesh(MeshError),
    /// Bad input.
    Spec(String),
    /// A cell is inverted or degenerate.
    BadCell(usize),
    /// `K` is singular: the supports leave rigid-body motion.
    Unsupported(String),
    /// The load stiffens the structure in every direction — no positive
    /// buckling factor exists for it (reverse the load to check the other
    /// sense).
    NoBuckling,
    /// The eigensolver did not converge.
    NotConverged {
        /// Iterations run.
        iterations: usize,
        /// Worst relative residual.
        residual: f64,
    },
}

impl std::fmt::Display for BucklingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BucklingError::Mesh(e) => write!(f, "mesh: {e:?}"),
            BucklingError::Spec(s) => write!(f, "{s}"),
            BucklingError::BadCell(c) => write!(f, "cell {c} is inverted or degenerate"),
            BucklingError::Unsupported(s) => write!(f, "{s}"),
            BucklingError::NoBuckling => write!(
                f,
                "no positive buckling load factor: the load stiffens the structure (tension); \
                 reverse it to check the opposite sense"
            ),
            BucklingError::NotConverged {
                iterations,
                residual,
            } => write!(
                f,
                "buckling eigensolver did not converge in {iterations} iterations (residual \
                 {residual:.2e})"
            ),
        }
    }
}

impl std::error::Error for BucklingError {}

impl BucklingSpec {
    fn validate(&self) -> Result<(), BucklingError> {
        let bad = |m: &str| Err(BucklingError::Spec(m.into()));
        let (e, nu) = (self.youngs_modulus_mpa, self.poisson);
        if e.is_nan() || e <= 0.0 || nu.is_nan() || nu <= -1.0 || nu >= 0.5 {
            return bad("modulus must be positive and poisson in (-1, 0.5)");
        }
        if self.pressures.is_empty() && self.forces.is_empty() {
            return bad("at least one pressure or force load is required");
        }
        if self.modes == 0 {
            return bad("modes must be >= 1");
        }
        if self
            .pressures
            .iter()
            .any(|p| !p.pressure_mpa.is_finite() || p.pressure_mpa == 0.0)
        {
            return bad("every pressure must be finite and nonzero");
        }
        if let Some(k) = self.knockdown {
            if !(k > 0.0 && k <= 1.0) {
                return bad("knockdown must be in (0, 1]");
            }
        }
        if self.supports.is_empty() && self.node_supports.is_empty() && !self.kinematic {
            return bad(
                "no supports: add supports or set kinematic = true for a self-equilibrated load",
            );
        }
        Ok(())
    }
}

// ─── Assembly and solve ─────────────────────────────────────────────────

impl tang_sparse::Preconditioner for PrecondRef<'_> {
    fn apply(&self, r: &[f64], z: &mut [f64]) {
        self.0.apply(r, z);
    }
}

struct PrecondRef<'a>(&'a Precond);

fn factor(a: &CsrMatrix<f64>) -> Precond {
    match Skyline::factor(a, 60_000_000) {
        Some(f) => Precond::Exact(f),
        None => match tang_sparse::Ic0::new(a) {
            Ok(p) => Precond::Ic0(p),
            Err(_) => Precond::Jacobi(tang_sparse::Jacobi::from_csr(a)),
        },
    }
}

/// 3-2-1 kinematic constraint DOFs (global indices) on a mesh.
fn kinematic_dofs(nodes: &[[f64; 3]]) -> Vec<usize> {
    let a = (0..nodes.len())
        .min_by(|&i, &j| nodes[i][0].total_cmp(&nodes[j][0]))
        .unwrap_or(0);
    let b = (0..nodes.len())
        .max_by(|&i, &j| norm3(sub(nodes[i], nodes[a])).total_cmp(&norm3(sub(nodes[j], nodes[a]))))
        .unwrap_or(0);
    let ab = normalize(sub(nodes[b], nodes[a]));
    let dist = |i: usize| {
        let v = sub(nodes[i], nodes[a]);
        norm3(sub(v, scale(ab, dot3(v, ab))))
    };
    let c = (0..nodes.len())
        .max_by(|&i, &j| dist(i).total_cmp(&dist(j)))
        .unwrap_or(0);
    let mut dofs = vec![3 * a, 3 * a + 1, 3 * a + 2];
    // At B hold the two components most perpendicular to AB.
    let mut axes = [0usize, 1, 2];
    axes.sort_by(|&p, &q| ab[p].abs().total_cmp(&ab[q].abs()));
    dofs.push(3 * b + axes[0]);
    dofs.push(3 * b + axes[1]);
    // At C hold the component most aligned with the ABC normal.
    let nrm = cross(ab, sub(nodes[c], nodes[a]));
    let k = (0..3)
        .max_by(|&p, &q| nrm[p].abs().total_cmp(&nrm[q].abs()))
        .unwrap_or(2);
    dofs.push(3 * c + k);
    dofs
}

/// Buckling on a given hex mesh.
#[allow(clippy::needless_range_loop)]
pub fn buckling_on_mesh(
    mesh: &Hex8Mesh,
    spec: &BucklingSpec,
) -> Result<BucklingResult, BucklingError> {
    spec.validate()?;
    let e = spec.youngs_modulus_mpa;
    let nu = spec.poisson;
    let lam = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = e / (2.0 * (1.0 + nu));
    let nn = mesh.nodes.len();
    let ndof = 3 * nn;
    if mesh.cells.is_empty() {
        return Err(BucklingError::Spec("empty mesh".into()));
    }

    // Supports.
    let mut fixed = vec![false; ndof];
    let lo_cell = mesh
        .cells
        .iter()
        .take(64)
        .map(|c| norm3(sub(mesh.nodes[c[7] as usize], mesh.nodes[c[0] as usize])))
        .fold(f64::INFINITY, f64::min);
    let tol = 0.1 * lo_cell.max(1e-9);
    for s in &spec.supports {
        let mut any = false;
        for (i, x) in mesh.nodes.iter().enumerate() {
            if s.region.contains(*x, tol) {
                any = true;
                for a in 0..3 {
                    fixed[3 * i + a] |= s.fix[a];
                }
            }
        }
        if !any {
            return Err(BucklingError::Spec(format!(
                "support region {:?}..{:?} selects no nodes",
                s.region.min, s.region.max
            )));
        }
    }
    for s in &spec.node_supports {
        for &n in &s.nodes {
            let n = n as usize;
            if n >= nn {
                return Err(BucklingError::Spec(format!(
                    "support node {n} out of range"
                )));
            }
            for a in 0..3 {
                fixed[3 * n + a] |= s.fix[a];
            }
        }
    }
    // Only the 3-2-1 DOFs no other support already holds carry the
    // "should be zero" reaction.
    let kin: Vec<usize> = if spec.kinematic {
        kinematic_dofs(&mesh.nodes)
            .into_iter()
            .filter(|&d| !fixed[d])
            .collect()
    } else {
        Vec::new()
    };
    for &d in &kin {
        fixed[d] = true;
    }
    let mut map = vec![usize::MAX; ndof];
    let mut free = Vec::new();
    for d in 0..ndof {
        if !fixed[d] {
            map[d] = free.len();
            free.push(d);
        }
    }
    let nf = free.len();

    // Elements (cached by shape: lattice meshes repeat a few bricks).
    let key_of = |x: &[[f64; 3]; 8]| -> [i64; 21] {
        let mut k = [0i64; 21];
        for c in 1..8 {
            let d = sub(x[c], x[0]);
            for a in 0..3 {
                k[3 * (c - 1) + a] = (d[a] * 1e7).round() as i64;
            }
        }
        k
    };
    let mut cache: HashMap<[i64; 21], usize> = HashMap::new();
    let mut elems: Vec<Element> = Vec::new();
    let mut elem_of = Vec::with_capacity(mesh.cells.len());
    for ci in 0..mesh.cells.len() {
        let x = mesh.cell_coords(ci);
        let key = key_of(&x);
        let idx = match cache.get(&key) {
            Some(&i) => i,
            None => {
                let el = element(&x, lam, mu).ok_or(BucklingError::BadCell(ci))?;
                elems.push(el);
                cache.insert(key, elems.len() - 1);
                elems.len() - 1
            }
        };
        elem_of.push(idx);
    }
    let dofs_of = |ci: usize| -> [usize; 24] {
        let mut d = [0usize; 24];
        for (c, &node) in mesh.cells[ci].iter().enumerate() {
            for comp in 0..3 {
                d[3 * c + comp] = 3 * node as usize + comp;
            }
        }
        d
    };
    let mut kc = CooMatrix::with_capacity(nf, nf, mesh.cells.len() * 576);
    for ci in 0..mesh.cells.len() {
        let ke = &elems[elem_of[ci]].kc;
        let gd = dofs_of(ci);
        for i in 0..24 {
            let fi = map[gd[i]];
            if fi == usize::MAX {
                continue;
            }
            for j in 0..24 {
                let fj = map[gd[j]];
                if fj != usize::MAX {
                    kc.push(fi, fj, ke[i * 24 + j]);
                }
            }
        }
    }
    let k = kc.to_csr();

    // Loads.
    let bfaces = if spec.pressures.is_empty() {
        Vec::new()
    } else {
        mesh.boundary_faces()
    };
    let mut f = vec![0.0; ndof];
    let mut kl_entries: HashMap<(usize, usize), f64> = HashMap::new();
    let mut pressure_force = 0.0;
    let mut exterior: Option<Vec<Face>> = None;
    for pl in &spec.pressures {
        let faces: Vec<Face> = match &pl.faces {
            FaceSelection::AllBoundary => bfaces.clone(),
            FaceSelection::Exterior => exterior
                .get_or_insert_with(|| exterior_faces(mesh, &bfaces))
                .clone(),
            FaceSelection::Region { region } => bfaces
                .iter()
                .copied()
                .filter(|fc| region.contains(mean(&fc.map(|n| mesh.nodes[n as usize])), tol))
                .collect(),
            FaceSelection::Faces { faces } => faces.clone(),
        };
        if faces.is_empty() {
            return Err(BucklingError::Spec(format!(
                "pressure {:?} selects no faces",
                pl.faces
            )));
        }
        for fc in &faces {
            if fc.iter().any(|&n| n as usize >= nn) {
                return Err(BucklingError::Spec(
                    "pressure face node out of range".into(),
                ));
            }
            pressure_face(
                mesh,
                fc,
                pl.pressure_mpa,
                pl.follower,
                &mut f,
                &mut kl_entries,
                &mut pressure_force,
            );
        }
    }
    for fl in &spec.forces {
        let sel: Vec<usize> = (0..nn)
            .filter(|&i| fl.region.contains(mesh.nodes[i], tol))
            .collect();
        if sel.is_empty() {
            return Err(BucklingError::Spec(format!(
                "force region {:?}..{:?} selects no nodes",
                fl.region.min, fl.region.max
            )));
        }
        for &i in &sel {
            for a in 0..3 {
                f[3 * i + a] += fl.force[a] / sel.len() as f64;
            }
        }
    }

    // Static solve on the free DOFs.
    let rhs: Vec<f64> = free.iter().map(|&d| f[d]).collect();
    let pre = factor(&k);
    let uf = match &pre {
        Precond::Exact(sky) => {
            let mut x = vec![0.0; nf];
            sky.solve(&rhs, &mut x);
            x
        }
        _ => {
            let sol = tang_sparse::pcg(
                |x, y| tang_sparse::csr_apply(&k, x, y),
                &PrecondRef(&pre),
                &rhs,
                &vec![0.0; nf],
                &tang_sparse::PcgOptions {
                    tol: 1e-10,
                    max_iters: 20_000,
                },
            );
            if !sol.converged() {
                return Err(BucklingError::Unsupported(format!(
                    "static prestress solve did not converge ({:?}, residual {:.2e}) — the \
                     supports may leave rigid-body motion",
                    sol.status, sol.residual_rel
                )));
            }
            sol.x
        }
    };
    if uf.iter().any(|v| !v.is_finite()) {
        return Err(BucklingError::Unsupported(
            "stiffness is singular: the supports leave rigid-body motion".into(),
        ));
    }
    let mut u = vec![0.0; ndof];
    for (i, &d) in free.iter().enumerate() {
        u[d] = uf[i];
    }
    // Reaction at the kinematic constraints: K_full·u − f on those DOFs.
    let kinematic_reaction_rel = if !spec.kinematic {
        None
    } else if kin.is_empty() {
        Some(0.0)
    } else {
        Some(kinematic_reaction(mesh, &elems, &elem_of, &u, &f, &kin))
    };

    // Geometric stiffness from the prestress.
    let mut kg = CooMatrix::with_capacity(nf, nf, mesh.cells.len() * 576);
    let mut max_vm = 0.0f64;
    for ci in 0..mesh.cells.len() {
        let x = mesh.cell_coords(ci);
        let gd = dofs_of(ci);
        let ue: [f64; 24] = std::array::from_fn(|i| u[gd[i]]);
        let el = &elems[elem_of[ci]];
        let mut kge = [[0.0f64; 8]; 8];
        for (g, s) in element_stresses(&x, el, &ue, lam, mu) {
            max_vm = max_vm.max(von_mises(&s));
            let sig = [[s[0], s[3], s[5]], [s[3], s[1], s[4]], [s[5], s[4], s[2]]];
            for a in 0..8 {
                let sa = [
                    dot3(sig[0], g.grads[a]),
                    dot3(sig[1], g.grads[a]),
                    dot3(sig[2], g.grads[a]),
                ];
                for b in 0..8 {
                    kge[a][b] += dot3(sa, g.grads[b]) * g.wdet;
                }
            }
        }
        for a in 0..8 {
            for b in 0..8 {
                let v = kge[a][b];
                if v == 0.0 {
                    continue;
                }
                for comp in 0..3 {
                    let (fi, fj) = (map[gd[3 * a + comp]], map[gd[3 * b + comp]]);
                    if fi != usize::MAX && fj != usize::MAX {
                        kg.push(fi, fj, v);
                    }
                }
            }
        }
    }
    // Load stiffness, symmetrized.
    let (mut asym, mut total) = (0.0, 0.0);
    for (&(i, j), &v) in &kl_entries {
        let (fi, fj) = (map[i], map[j]);
        if fi == usize::MAX || fj == usize::MAX {
            continue;
        }
        let vt = kl_entries.get(&(j, i)).copied().unwrap_or(0.0);
        asym += (v - vt) * (v - vt);
        total += v * v;
        kg.push(fi, fj, 0.5 * (v + vt));
    }
    let load_stiffness_asymmetry = if total > 0.0 {
        (asym / total).sqrt() / 2f64.sqrt()
    } else {
        0.0
    };
    let a = kg.to_csr();

    // Eigen: A x = μ K x, lowest μ; λ = −1/μ.
    let want = spec.modes.min(nf.saturating_sub(1)).max(1);
    let block = (want + 4).min(nf);
    let (mut ad, mut kd) = (0.0, 0.0);
    for i in 0..nf {
        for idx in a.row_ptrs[i]..a.row_ptrs[i + 1] {
            if a.col_indices[idx] == i {
                ad += a.values[idx].abs();
            }
        }
        for idx in k.row_ptrs[i]..k.row_ptrs[i + 1] {
            if k.col_indices[idx] == i {
                kd += k.values[idx];
            }
        }
    }
    if ad == 0.0 {
        return Err(BucklingError::NoBuckling);
    }
    let eig =
        lobpcg(&a, &k, &[], 1e-6 * ad / kd, want, block, &pre, 1e-6, 4000).map_err(
            |e| match e {
                crate::modal::ModalError::NotConverged {
                    iterations,
                    residual,
                } => BucklingError::NotConverged {
                    iterations,
                    residual,
                },
                other => BucklingError::Spec(other.to_string()),
            },
        )?;
    let mut modes = Vec::new();
    for (mu_i, v) in eig.vals.iter().zip(&eig.vecs) {
        if modes.len() == spec.modes {
            break;
        }
        if *mu_i >= 0.0 {
            break;
        }
        let mut shape = vec![0.0; ndof];
        for (i, &d) in free.iter().enumerate() {
            shape[d] = v[i];
        }
        let peak = shape.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        if peak > 0.0 {
            shape.iter_mut().for_each(|x| *x /= peak);
        }
        modes.push(BucklingMode {
            load_factor: -1.0 / mu_i,
            shape,
        });
    }
    if modes.is_empty() {
        return Err(BucklingError::NoBuckling);
    }
    let max_displacement_mm = (0..nn)
        .map(|i| norm3([u[3 * i], u[3 * i + 1], u[3 * i + 2]]))
        .fold(0.0, f64::max);
    Ok(BucklingResult {
        modes,
        max_displacement_mm,
        max_von_mises_mpa: max_vm,
        cells: mesh.cells.len(),
        dofs: nf,
        iterations: eig.iterations,
        load_stiffness_asymmetry,
        kinematic_reaction_rel,
        pressure_force_n: pressure_force,
    })
}

/// `‖K·u − f‖` on the 3-2-1 DOFs over `‖f‖`.
fn kinematic_reaction(
    mesh: &Hex8Mesh,
    elems: &[Element],
    elem_of: &[usize],
    u: &[f64],
    f: &[f64],
    kin: &[usize],
) -> f64 {
    let mut r = vec![0.0; u.len()];
    for (ci, cell) in mesh.cells.iter().enumerate() {
        let ke = &elems[elem_of[ci]].kc;
        let gd: [usize; 24] = std::array::from_fn(|i| 3 * cell[i / 3] as usize + i % 3);
        for i in 0..24 {
            if !kin.contains(&gd[i]) {
                continue;
            }
            for j in 0..24 {
                r[gd[i]] += ke[i * 24 + j] * u[gd[j]];
            }
        }
    }
    let react: f64 = kin
        .iter()
        .map(|&d| (r[d] - f[d]).powi(2))
        .sum::<f64>()
        .sqrt();
    let fnorm = f.iter().map(|v| v * v).sum::<f64>().sqrt().max(1e-300);
    react / fnorm
}

/// Consistent pressure load and (follower) load stiffness of one face.
fn pressure_face(
    mesh: &Hex8Mesh,
    fc: &Face,
    p: f64,
    follower: bool,
    f: &mut [f64],
    kl: &mut HashMap<(usize, usize), f64>,
    total: &mut f64,
) {
    let q = fc.map(|n| mesh.nodes[n as usize]);
    let g = 1.0 / 3f64.sqrt();
    // Loop order q0(−,−) q1(+,−) q2(+,+) q3(−,+).
    let si = [-1.0, 1.0, 1.0, -1.0];
    let ti = [-1.0, -1.0, 1.0, 1.0];
    let mut fsum = [[0.0; 3]; 4];
    for (xi, eta) in [(-g, -g), (g, -g), (g, g), (-g, g)] {
        let n: [f64; 4] = std::array::from_fn(|a| 0.25 * (1.0 + si[a] * xi) * (1.0 + ti[a] * eta));
        let dxi: [f64; 4] = std::array::from_fn(|a| 0.25 * si[a] * (1.0 + ti[a] * eta));
        let deta: [f64; 4] = std::array::from_fn(|a| 0.25 * ti[a] * (1.0 + si[a] * xi));
        let mut x_xi = [0.0; 3];
        let mut x_eta = [0.0; 3];
        for a in 0..4 {
            x_xi = add(x_xi, scale(q[a], dxi[a]));
            x_eta = add(x_eta, scale(q[a], deta[a]));
        }
        let c = cross(x_xi, x_eta);
        for a in 0..4 {
            fsum[a] = add(fsum[a], scale(c, -p * n[a]));
        }
        if !follower {
            continue;
        }
        // K_L[a][b] = p·N_a·(∂N_b/∂η·[x_ξ]× − ∂N_b/∂ξ·[x_η]×).
        let skew = |v: [f64; 3]| [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]];
        let (sx, se) = (skew(x_xi), skew(x_eta));
        for a in 0..4 {
            for b in 0..4 {
                for i in 0..3 {
                    for j in 0..3 {
                        let v = p * n[a] * (deta[b] * sx[i][j] - dxi[b] * se[i][j]);
                        if v != 0.0 {
                            *kl.entry((3 * fc[a] as usize + i, 3 * fc[b] as usize + j))
                                .or_insert(0.0) += v;
                        }
                    }
                }
            }
        }
    }
    for a in 0..4 {
        for i in 0..3 {
            f[3 * fc[a] as usize + i] += fsum[a][i];
        }
        *total += norm3(fsum[a]);
    }
}

// ─── Structured shells ──────────────────────────────────────────────────

/// A body-fitted shell mesh with its labelled surfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellMesh {
    /// The mesh.
    pub mesh: Hex8Mesh,
    /// Outer skin faces (outward).
    pub outer_faces: Vec<Face>,
    /// Inner skin faces (outward from the solid, i.e. into the cavity).
    pub inner_faces: Vec<Face>,
    /// End annulus faces at z = 0 and z = L (cylinders only).
    pub end_faces: [Vec<Face>; 2],
    /// Mid-surface nodes of the two end rings (cylinders, even `n_t`).
    pub end_mid_nodes: [Vec<u32>; 2],
}

fn label_faces(
    mesh: &Hex8Mesh,
    r_in: f64,
    r_out: f64,
    radial: impl Fn([f64; 3]) -> f64,
) -> (Vec<Face>, Vec<Face>, Vec<Face>) {
    let (mut outer, mut inner, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    let tol = 0.25 * (r_out - r_in);
    for fc in mesh.boundary_faces() {
        // Mean of the corner radii, not the centroid's radius: on a coarse
        // curved mesh the flat face's centroid sags well inside the skin.
        let r = fc
            .iter()
            .map(|&n| radial(mesh.nodes[n as usize]))
            .sum::<f64>()
            / 4.0;
        if (r - r_out).abs() < tol {
            outer.push(fc);
        } else if (r - r_in).abs() < tol {
            inner.push(fc);
        } else {
            rest.push(fc);
        }
    }
    (outer, inner, rest)
}

/// Circular cylindrical shell along +Z: mean radius `r`, wall `t`,
/// length `l`; `n_theta` cells around, `n_z` along, `n_t` through the wall.
pub fn cylinder_shell(r: f64, t: f64, l: f64, n_theta: usize, n_z: usize, n_t: usize) -> ShellMesh {
    let (n_theta, n_z, n_t) = (n_theta.max(8), n_z.max(1), n_t.max(1));
    let id = |i: usize, j: usize, k: usize| ((k * (n_t + 1) + j) * n_theta + i % n_theta) as u32;
    let mut nodes = Vec::with_capacity(n_theta * (n_t + 1) * (n_z + 1));
    for k in 0..=n_z {
        for j in 0..=n_t {
            for i in 0..n_theta {
                let th = std::f64::consts::TAU * i as f64 / n_theta as f64;
                let rr = r - 0.5 * t + t * j as f64 / n_t as f64;
                nodes.push([rr * th.cos(), rr * th.sin(), l * k as f64 / n_z as f64]);
            }
        }
    }
    let mut cells = Vec::new();
    for k in 0..n_z {
        for j in 0..n_t {
            for i in 0..n_theta {
                cells.push(std::array::from_fn(|c| {
                    id(i + (c & 1), j + ((c >> 1) & 1), k + ((c >> 2) & 1))
                }));
            }
        }
    }
    let mut mesh = Hex8Mesh { nodes, cells };
    mesh.orient();
    let (outer, inner, rest) = label_faces(&mesh, r - 0.5 * t, r + 0.5 * t, |p| {
        (p[0] * p[0] + p[1] * p[1]).sqrt()
    });
    let mut ends = [Vec::new(), Vec::new()];
    for fc in rest {
        let z = mean(&fc.map(|n| mesh.nodes[n as usize]))[2];
        ends[usize::from(z > 0.5 * l)].push(fc);
    }
    let mid = |k: usize| -> Vec<u32> {
        if n_t % 2 == 0 {
            (0..n_theta).map(|i| id(i, n_t / 2, k)).collect()
        } else {
            Vec::new()
        }
    };
    ShellMesh {
        mesh,
        outer_faces: outer,
        inner_faces: inner,
        end_faces: ends,
        end_mid_nodes: [mid(0), mid(n_z)],
    }
}

/// Complete spherical shell (cube-sphere: six equiangular patches of
/// `m × m` cells), mean radius `r`, wall `t`, `n_t` cells through it.
pub fn sphere_shell(r: f64, t: f64, m: usize, n_t: usize) -> ShellMesh {
    let (m, n_t) = (m.max(2), n_t.max(1));
    let mut dirs: Vec<[f64; 3]> = Vec::new();
    let mut index: HashMap<[i64; 3], u32> = HashMap::new();
    let mut patch_ids = Vec::new();
    for axis in 0..3 {
        for side in [-1.0, 1.0] {
            let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
            let mut ids = vec![0u32; (m + 1) * (m + 1)];
            for j in 0..=m {
                for i in 0..=m {
                    let u = (std::f64::consts::FRAC_PI_4 * (2.0 * i as f64 / m as f64 - 1.0)).tan();
                    let v = (std::f64::consts::FRAC_PI_4 * (2.0 * j as f64 / m as f64 - 1.0)).tan();
                    let mut p = [0.0; 3];
                    p[axis] = side;
                    p[b] = u;
                    p[c] = v;
                    let d = normalize(p);
                    let key = d.map(|x| (x * 1e9).round() as i64);
                    let id = *index.entry(key).or_insert_with(|| {
                        dirs.push(d);
                        (dirs.len() - 1) as u32
                    });
                    ids[j * (m + 1) + i] = id;
                }
            }
            patch_ids.push(ids);
        }
    }
    let nd = dirs.len();
    let mut nodes = Vec::with_capacity(nd * (n_t + 1));
    for j in 0..=n_t {
        let rr = r - 0.5 * t + t * j as f64 / n_t as f64;
        nodes.extend(dirs.iter().map(|d| scale(*d, rr)));
    }
    let mut cells = Vec::new();
    for ids in &patch_ids {
        for jj in 0..m {
            for ii in 0..m {
                for layer in 0..n_t {
                    cells.push(std::array::from_fn(|c| {
                        let (di, dj, dl) = (c & 1, (c >> 1) & 1, (c >> 2) & 1);
                        ids[(jj + dj) * (m + 1) + ii + di] + ((layer + dl) * nd) as u32
                    }));
                }
            }
        }
    }
    let mut mesh = Hex8Mesh { nodes, cells };
    mesh.orient();
    let (outer, inner, _) = label_faces(&mesh, r - 0.5 * t, r + 0.5 * t, norm3);
    ShellMesh {
        mesh,
        outer_faces: outer,
        inner_faces: inner,
        end_faces: [Vec::new(), Vec::new()],
        end_mid_nodes: [Vec::new(), Vec::new()],
    }
}

/// The spec SP-8007's simply-supported cylinder implies, on a
/// [`cylinder_shell`] mesh with even `n_t`: external pressure `p` on the
/// outer skin (follower), the two end rings held circular at mid-surface
/// (x, y fixed) but free to move axially (one node pinned in z), and — for
/// hydrostatic loading — the cap force as a dead pressure on both end
/// annuli. Holding the whole end ring axially would raise the buckling
/// pressure (by up to 50%, SP-8007 notes); this is the axially-free case.
pub fn capped_cylinder_spec(
    sh: &ShellMesh,
    r: f64,
    t: f64,
    youngs_modulus_mpa: f64,
    poisson: f64,
    p: f64,
    loading: crate::shell::CylinderLoading,
) -> BucklingSpec {
    let (ri, ro) = (r - 0.5 * t, r + 0.5 * t);
    let mut pressures = vec![PressureLoad {
        pressure_mpa: p,
        faces: FaceSelection::Faces {
            faces: sh.outer_faces.clone(),
        },
        follower: true,
    }];
    if loading == crate::shell::CylinderLoading::Hydrostatic {
        for end in &sh.end_faces {
            pressures.push(PressureLoad {
                pressure_mpa: p * ro * ro / (ro * ro - ri * ri),
                faces: FaceSelection::Faces { faces: end.clone() },
                follower: false,
            });
        }
    }
    let mut node_supports: Vec<NodeSupport> = sh
        .end_mid_nodes
        .iter()
        .map(|ring| NodeSupport {
            nodes: ring.clone(),
            fix: [true, true, false],
        })
        .collect();
    if let Some(&pin) = sh.end_mid_nodes[0].first() {
        node_supports.push(NodeSupport {
            nodes: vec![pin],
            fix: [false, false, true],
        });
    }
    BucklingSpec {
        youngs_modulus_mpa,
        poisson,
        pressures,
        forces: Vec::new(),
        supports: Vec::new(),
        node_supports,
        kinematic: false,
        modes: 1,
        knockdown: None,
    }
}

// ─── Refinement study ────────────────────────────────────────────────────

/// One level of a refinement study.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucklingLevel {
    /// Cells.
    pub cells: usize,
    /// Free DOFs.
    pub dofs: usize,
    /// Lowest load factor.
    pub load_factor: f64,
    /// LOBPCG iterations.
    pub iterations: usize,
}

/// A refinement study of the lowest load factor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucklingStudy {
    /// Levels, coarsest first.
    pub levels: Vec<BucklingLevel>,
    /// The finest level's full result (the reported answer).
    pub finest: BucklingResult,
    /// Relative change of the lowest load factor over the finest step.
    pub load_factor_change_rel: f64,
    /// Gate verdict.
    pub verdict: ConvergenceVerdict,
    /// Reference pressure, MPa, when the load is a single pressure — the
    /// critical pressure is `λ × reference`.
    pub reference_pressure_mpa: Option<f64>,
    /// Knockdown carried from the spec, if any.
    pub knockdown: Option<f64>,
}

impl BucklingStudy {
    /// Lowest bifurcation load factor (finest level).
    pub fn load_factor(&self) -> f64 {
        self.finest.modes[0].load_factor
    }

    /// True when the refinement gate passed.
    pub fn converged(&self) -> bool {
        self.verdict == ConvergenceVerdict::Converged
    }
}

/// Run each `(mesh, spec)` level (coarsest first) and gate the lowest
/// load factor on its finest-step change ≤ `tol`. Fail-closed: fewer than
/// two levels is Unverifiable, as is a level that loses or reverses
/// monotone convergence badly.
pub fn buckling_study(
    levels: &[(Hex8Mesh, BucklingSpec)],
    tol: f64,
) -> Result<BucklingStudy, BucklingError> {
    if levels.is_empty() {
        return Err(BucklingError::Spec(
            "a study needs at least one level".into(),
        ));
    }
    let mut rows = Vec::new();
    let mut last = None;
    for (mesh, spec) in levels {
        let r = buckling_on_mesh(mesh, spec)?;
        rows.push(BucklingLevel {
            cells: r.cells,
            dofs: r.dofs,
            load_factor: r.modes[0].load_factor,
            iterations: r.iterations,
        });
        last = Some(r);
    }
    let finest = last.expect("non-empty");
    let mut reasons = Vec::new();
    let change = if rows.len() >= 2 {
        let (a, b) = (
            rows[rows.len() - 2].load_factor,
            rows[rows.len() - 1].load_factor,
        );
        (b - a).abs() / b.abs().max(1e-300)
    } else {
        reasons.push("a single mesh level demonstrates no convergence".into());
        f64::INFINITY
    };
    if change > tol {
        reasons.push(format!(
            "lowest load factor changed {:.2}% over the finest refinement step (gate {:.2}%) — \
             refine further",
            100.0 * change,
            100.0 * tol
        ));
    }
    let spec = &levels[levels.len() - 1].1;
    let reference_pressure_mpa = match (spec.pressures.as_slice(), spec.forces.is_empty()) {
        ([p], true) => Some(p.pressure_mpa),
        _ => None,
    };
    Ok(BucklingStudy {
        levels: rows,
        finest,
        load_factor_change_rel: change,
        verdict: if reasons.is_empty() {
            ConvergenceVerdict::Converged
        } else {
            ConvergenceVerdict::Unverifiable { reasons }
        },
        reference_pressure_mpa,
        knockdown: spec.knockdown,
    })
}

/// Lattice-path study on a tessellated part: bricks on a grid conforming
/// to every axis-aligned face ([`hex_fill_conforming`]) at each of
/// `pitches` (coarsest first). Region-based loads and supports carry
/// across levels; node-set supports do not (they are mesh-specific) and
/// are rejected. Curved thin walls are staircased — use the shell
/// generators for cylinders and spheres.
pub fn lattice_buckling_study(
    mesh: &TriangleMesh,
    spec: &BucklingSpec,
    pitches: &[[f64; 3]],
    tol: f64,
) -> Result<BucklingStudy, BucklingError> {
    if !spec.node_supports.is_empty()
        || spec
            .pressures
            .iter()
            .any(|p| matches!(p.faces, FaceSelection::Faces { .. }))
    {
        return Err(BucklingError::Spec(
            "node-set supports and explicit faces are mesh-specific; use regions on the lattice \
             path"
                .into(),
        ));
    }
    let levels = pitches
        .iter()
        .map(|p| {
            hex_fill_conforming(mesh, *p)
                .map(|h| (Hex8Mesh::from(&h), spec.clone()))
                .map_err(BucklingError::Mesh)
        })
        .collect::<Result<Vec<_>, _>>()?;
    buckling_study(&levels, tol)
}

// ─── Claims ──────────────────────────────────────────────────────────────

fn caveat() -> &'static str {
    "linear (eigenvalue) bifurcation of the PERFECT geometry under small-displacement \
     prestress: no imperfections, no plasticity, no pre-buckling nonlinearity; for thin SHELLS \
     this is UNCONSERVATIVE — real cylinders and spheres collapse well below it, apply an \
     imperfection knockdown (NASA/SP-8007 for cylinders, ASME UG-28(d) for spheres; see the \
     shell screen); for columns and plates it is the classical design number"
}

/// Predicted claims for a **converged** study (`vcad.fea-claims/1`);
/// refuses when Unverifiable.
pub fn predicted_claims(
    study: &BucklingStudy,
) -> Result<crate::receipt::ClaimSet, crate::receipt::ClaimError> {
    if let ConvergenceVerdict::Unverifiable { reasons } = &study.verdict {
        return Err(crate::receipt::ClaimError::Unverifiable(reasons.clone()));
    }
    let cav = caveat();
    let mk = |name: &str, value: f64, unit: &str, note: String| crate::receipt::Claim {
        name: name.into(),
        value,
        unit: unit.into(),
        basis: "predicted".into(),
        note,
    };
    let lf = study.load_factor();
    let mut claims = vec![
        mk(
            "buckling_load_factor",
            lf,
            "1",
            format!(
                "lowest lambda with K + lambda·(K_G + K_L) singular, finest level; inter-level \
                 change {:.2}%; follower pressure load stiffness included; {cav}",
                100.0 * study.load_factor_change_rel
            ),
        ),
        mk(
            "discretization_error_buckling_rel",
            study.load_factor_change_rel,
            "1",
            "relative change of the lowest load factor over the finest refinement step — the \
             error estimate the gate judged"
                .into(),
        ),
    ];
    if let Some(p) = study.reference_pressure_mpa {
        claims.push(mk(
            "bifurcation_pressure_mpa",
            lf * p,
            "MPa",
            format!("lambda × reference pressure {p} MPa; {cav}"),
        ));
    }
    match study.knockdown {
        Some(k) => {
            claims.push(mk(
                "knocked_down_load_factor",
                k * lf,
                "1",
                format!(
                    "bifurcation factor × imperfection knockdown {k} supplied with the spec — \
                     the knockdown is an input, not something this solver measured; {cav}"
                ),
            ));
            if let Some(p) = study.reference_pressure_mpa {
                claims.push(mk(
                    "knocked_down_pressure_mpa",
                    k * lf * p,
                    "MPa",
                    format!("knockdown {k} × bifurcation pressure; {cav}"),
                ));
            }
        }
        None => claims.push(mk(
            "imperfection_knockdown",
            1.0,
            "1",
            "NO knockdown applied — the bifurcation factor above is the perfect-structure \
             number; for a shell supply a knockdown or use the shell screen before designing"
                .into(),
        )),
    }
    let f = &study.finest;
    let mut levels: Vec<String> = study
        .levels
        .iter()
        .map(|l| {
            format!(
                "{} cells / {} dofs: lambda {:.5}, {} lobpcg iters",
                l.cells, l.dofs, l.load_factor, l.iterations
            )
        })
        .collect();
    levels.push(format!(
        "prestress peak von Mises {:.3} MPa, peak displacement {:.4e} mm at the reference load; \
         load-stiffness asymmetry {:.2e}{}",
        f.max_von_mises_mpa,
        f.max_displacement_mm,
        f.load_stiffness_asymmetry,
        f.kinematic_reaction_rel
            .map(|r| format!("; 3-2-1 reaction residual {r:.2e}"))
            .unwrap_or_default()
    ));
    Ok(crate::receipt::ClaimSet {
        schema: crate::receipt::CLAIM_SCHEMA.to_string(),
        provenance: crate::receipt::SolverProvenance {
            levels,
            material: String::new(),
            loads: study
                .reference_pressure_mpa
                .map(|p| vec![format!("reference pressure {p} MPa")])
                .unwrap_or_default(),
            supports: Vec::new(),
            displacement_change_rel: 0.0,
            stress_change_rel: study.load_factor_change_rel,
        },
        claims,
    })
}

/// Oracle reference for the eigenvalue route.
pub fn oracle() -> vcad_receipt::OracleRef {
    vcad_receipt::OracleRef::new("vcad-kernel-fea/buckling", env!("CARGO_PKG_VERSION"))
}

/// Unified-receipt claims under `structure.buckling.*`, basis `Predicted`.
pub fn design_claims(set: &crate::receipt::ClaimSet) -> Vec<vcad_receipt::ReceiptClaim> {
    let oracle = oracle();
    let provenance = set.provenance.levels.join(" | ");
    set.claims
        .iter()
        .map(|c| {
            vcad_receipt::ReceiptClaim::pass(
                format!("structure.buckling.{}", c.name),
                crate::receipt::RECEIPT_DOMAIN,
                c.note.clone(),
                oracle.clone(),
            )
            .with_basis(vcad_receipt::ClaimBasis::Predicted)
            .with_measured(if c.unit == "1" {
                vcad_receipt::ClaimQuantity::bare(c.value)
            } else {
                vcad_receipt::ClaimQuantity::new(c.value, &c.unit)
            })
            .with_details(provenance.clone())
        })
        .collect()
}

/// One Unverifiable claim for an unconverged study.
pub fn design_claims_unverifiable(reasons: &[String]) -> Vec<vcad_receipt::ReceiptClaim> {
    vec![vcad_receipt::ReceiptClaim::unverifiable(
        "structure.buckling.convergence",
        crate::receipt::RECEIPT_DOMAIN,
        "eigenvalue buckling refinement gate",
        oracle(),
        format!(
            "refinement study did not converge — no buckling load is claimed: {}",
            reasons.join("; ")
        ),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::box_mesh;
    use crate::shell::{
        classical_cylinder_pressure, long_cylinder_pressure, von_mises_cylinder_pressure,
        zoelly_pressure, CylinderLoading,
    };

    const E: f64 = 200_000.0;
    const NU: f64 = 0.3;

    fn base_spec() -> BucklingSpec {
        BucklingSpec {
            youngs_modulus_mpa: E,
            poisson: NU,
            pressures: vec![],
            forces: vec![],
            supports: vec![],
            node_supports: vec![],
            kinematic: false,
            modes: 1,
            knockdown: None,
        }
    }

    fn rel(a: f64, b: f64) -> f64 {
        (a - b) / b
    }

    #[test]
    fn patch_test_on_distorted_brick() {
        // Linear displacement field u = G·x on a skewed brick: the condensed
        // element must give the exact constant stress (no bubble pollution).
        let mut x: [[f64; 3]; 8] = std::array::from_fn(|c| {
            [
                (c & 1) as f64 * 2.0,
                ((c >> 1) & 1) as f64 * 1.5,
                ((c >> 2) & 1) as f64,
            ]
        });
        x[7] = add(x[7], [0.3, -0.2, 0.25]);
        x[2] = add(x[2], [-0.15, 0.1, 0.05]);
        let (lam, mu) = (1.0, 0.7);
        let el = element(&x, lam, mu).unwrap();
        let grad = [[1e-3, 2e-4, -3e-4], [5e-4, -2e-3, 1e-4], [0.0, 3e-4, 7e-4]];
        let ue: [f64; 24] = std::array::from_fn(|i| {
            let (c, comp) = (i / 3, i % 3);
            dot3(grad[comp], x[c])
        });
        let eps = [
            grad[0][0],
            grad[1][1],
            grad[2][2],
            grad[0][1] + grad[1][0],
            grad[1][2] + grad[2][1],
            grad[2][0] + grad[0][2],
        ];
        let want = d_times(&eps, lam, mu);
        for (_, s) in element_stresses(&x, &el, &ue, lam, mu) {
            for r in 0..6 {
                assert!((s[r] - want[r]).abs() < 1e-9, "stress {s:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn load_stiffness_matches_finite_difference() {
        // f(x) on a warped face; K_L = −∂f/∂u.
        let mesh = Hex8Mesh {
            nodes: vec![
                [0.0, 0.0, 0.0],
                [2.0, 0.1, 0.2],
                [2.2, 1.9, -0.1],
                [-0.1, 2.0, 0.3],
            ],
            cells: vec![],
        };
        let fc: Face = [0, 1, 2, 3];
        let p = 0.7;
        let mut kl = HashMap::new();
        let mut f0 = vec![0.0; 12];
        let mut tot = 0.0;
        pressure_face(&mesh, &fc, p, true, &mut f0, &mut kl, &mut tot);
        let h = 1e-6;
        for col in 0..12 {
            let mut m2 = mesh.clone();
            m2.nodes[col / 3][col % 3] += h;
            let mut f1 = vec![0.0; 12];
            pressure_face(&m2, &fc, p, false, &mut f1, &mut HashMap::new(), &mut tot);
            for row in 0..12 {
                let fd = -(f1[row] - f0[row]) / h;
                let an = kl.get(&(row, col)).copied().unwrap_or(0.0);
                assert!((fd - an).abs() < 1e-5, "K_L[{row}][{col}] {an} vs fd {fd}");
            }
        }
    }

    #[test]
    fn euler_cantilever_column() {
        // 200 × 10 × 10 mm, fixed at x = 0, unit compression at the tip:
        // P_cr = π²EI/(4L²).
        let surf = box_mesh([0.0; 3], [200.0, 10.0, 10.0]);
        let hex = hex_fill_conforming(&surf, [5.0, 2.5, 2.5]).unwrap();
        let mut spec = base_spec();
        spec.supports = vec![Support {
            region: RegionBox {
                min: [-0.1, -0.1, -0.1],
                max: [0.1, 10.1, 10.1],
            },
            fix: [true, true, true],
        }];
        spec.forces = vec![ForceLoad {
            region: RegionBox {
                min: [199.9, -0.1, -0.1],
                max: [200.1, 10.1, 10.1],
            },
            force: [-1.0, 0.0, 0.0],
        }];
        let r = buckling_on_mesh(&Hex8Mesh::from(&hex), &spec).unwrap();
        let i = 10.0f64.powi(4) / 12.0;
        let pcr = std::f64::consts::PI.powi(2) * E * i / (4.0 * 200.0 * 200.0);
        let err = rel(r.modes[0].load_factor, pcr);
        eprintln!("Euler FE {} vs {pcr} ({:+.2}%)", r.modes[0].load_factor, 100.0 * err);
        assert!(
            err.abs() < 0.03,
            "Euler {} vs {pcr} ({:+.2}%)",
            r.modes[0].load_factor,
            100.0 * err
        );
        // Tension: nothing within orders of magnitude of the compressive
        // load (a local Poisson-compression mode at the clamp may exist at
        // an absurd factor; "no buckling" is also acceptable).
        spec.forces[0].force = [1.0, 0.0, 0.0];
        match buckling_on_mesh(&Hex8Mesh::from(&hex), &spec) {
            Err(BucklingError::NoBuckling) => {}
            Ok(t) => assert!(
                t.modes[0].load_factor > 1e3 * pcr,
                "{}",
                t.modes[0].load_factor
            ),
            Err(e) => panic!("{e}"),
        }
    }

    fn ring_spec(sh: &ShellMesh, follower: bool) -> BucklingSpec {
        let mut spec = base_spec();
        spec.pressures = vec![PressureLoad {
            pressure_mpa: 1.0,
            faces: FaceSelection::Faces {
                faces: sh.outer_faces.clone(),
            },
            follower,
        }];
        // Plane strain: hold z everywhere; 3-2-1 removes in-plane rigid motion.
        spec.node_supports = vec![NodeSupport {
            nodes: (0..sh.mesh.nodes.len() as u32).collect(),
            fix: [false, false, true],
        }];
        spec.kinematic = true;
        spec
    }

    #[test]
    fn ring_under_external_pressure_is_bresse_with_follower_load() {
        // Plane-strain ring = long cylinder: p = E/(4(1−ν²))·(t/r)³ (n = 2).
        let (r, t) = (50.0, 2.0);
        let sh = cylinder_shell(r, t, 2.0, 72, 1, 2);
        let fol = buckling_on_mesh(&sh.mesh, &ring_spec(&sh, true)).unwrap();
        let want = long_cylinder_pressure(E, NU, r, t);
        let err = rel(fol.modes[0].load_factor, want);
        assert!(
            err.abs() < 0.03,
            "ring {} vs Bresse {want} ({:+.2}%)",
            fol.modes[0].load_factor,
            100.0 * err
        );
        // Closed ring: K_L symmetric, load self-equilibrated.
        assert!(
            fol.kinematic_reaction_rel.unwrap() < 1e-8,
            "{:?}",
            fol.kinematic_reaction_rel
        );
        // A dead (fixed-direction) pressure reads high: dropping the
        // follower term is unconservative for a vacuum vessel.
        let dead = buckling_on_mesh(&sh.mesh, &ring_spec(&sh, false)).unwrap();
        let ratio = dead.modes[0].load_factor / fol.modes[0].load_factor;
        eprintln!(
            "ring follower {} dead/follower {ratio}",
            fol.modes[0].load_factor
        );
        assert!(ratio > 1.03, "dead/follower {ratio}");
    }

    #[test]
    fn capped_cylinder_matches_von_mises_and_screen() {
        // r = 50, t = 1, L = 300, hydrostatic, simply supported ends held
        // circular and free to move axially: n = 3 lobes.
        let (r, t, l) = (50.0, 1.0, 300.0);
        let sh = cylinder_shell(r, t, l, 64, 40, 2);
        let spec = capped_cylinder_spec(&sh, r, t, E, NU, 1.0, CylinderLoading::Hydrostatic);
        let fe = buckling_on_mesh(&sh.mesh, &spec).unwrap().modes[0].load_factor;
        let (vm, n) = von_mises_cylinder_pressure(E, NU, r, t, l, CylinderLoading::Hydrostatic);
        let (screen, _) = classical_cylinder_pressure(E, NU, r, t, l, CylinderLoading::Hydrostatic);
        eprintln!("cylinder FE {fe} vs von Mises {vm} (n {n}) vs screen {screen}");
        assert_eq!(n, 3);
        assert!(
            rel(fe, vm).abs() < 0.05,
            "FE {fe} vs von Mises {vm} ({:+.2}%)",
            100.0 * rel(fe, vm)
        );
        // The screen's classical number is the conservative side of both.
        assert!(screen <= fe, "screen {screen} above FE {fe}");
    }

    #[test]
    fn sphere_matches_zoelly() {
        let (r, t) = (50.0, 2.0);
        let sh = sphere_shell(r, t, 16, 1);
        let mut spec = base_spec();
        spec.pressures = vec![PressureLoad {
            pressure_mpa: 1.0,
            faces: FaceSelection::Faces {
                faces: sh.outer_faces.clone(),
            },
            follower: true,
        }];
        spec.kinematic = true;
        let fe = buckling_on_mesh(&sh.mesh, &spec).unwrap();
        let want = zoelly_pressure(E, NU, r, t);
        let err = rel(fe.modes[0].load_factor, want);
        eprintln!(
            "sphere FE {} vs Zoelly {want} ({:+.2}%)",
            fe.modes[0].load_factor,
            100.0 * err
        );
        assert!(
            err.abs() < 0.10,
            "sphere FE {} vs Zoelly {want} ({:+.2}%)",
            fe.modes[0].load_factor,
            100.0 * err
        );
        eprintln!(
            "sphere K_L asymmetry {:.2e}, 3-2-1 reaction {:?}",
            fe.load_stiffness_asymmetry, fe.kinematic_reaction_rel
        );
        assert!(
            fe.load_stiffness_asymmetry < 1e-9,
            "closed surface K_L is symmetric"
        );
        assert!(fe.kinematic_reaction_rel.unwrap() < 1e-8);
    }

    #[test]
    fn exterior_faces_skip_the_cavity() {
        let sh = sphere_shell(50.0, 2.0, 4, 1);
        let all = sh.mesh.boundary_faces();
        let ext = exterior_faces(&sh.mesh, &all);
        assert_eq!(all.len(), sh.outer_faces.len() + sh.inner_faces.len());
        let mut a = ext.clone();
        let mut b = sh.outer_faces.clone();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "exterior = outer skin");
        // Volume sanity on a finer cube-sphere (the m = 4 one is a visibly
        // faceted polyhedron, ~6% under).
        let v = sphere_shell(50.0, 2.0, 24, 1).mesh.volume();
        let want = 4.0 / 3.0 * std::f64::consts::PI * (51f64.powi(3) - 49f64.powi(3));
        assert!(rel(v, want).abs() < 0.01, "volume {v} vs {want}");
    }

    #[test]
    fn study_gate_is_fail_closed_and_claims_ride_provisional() {
        let (r, t) = (50.0, 2.0);
        let levels: Vec<(Hex8Mesh, BucklingSpec)> = [48, 72]
            .iter()
            .map(|&n| {
                let sh = cylinder_shell(r, t, 2.0, n, 1, 2);
                let s = ring_spec(&sh, true);
                (sh.mesh, s)
            })
            .collect();
        let ok = buckling_study(&levels, 0.02).unwrap();
        assert!(ok.converged(), "{:?}", ok.verdict);
        let set = predicted_claims(&ok).unwrap();
        let names: Vec<&str> = set.claims.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"bifurcation_pressure_mpa"));
        assert!(names.contains(&"imperfection_knockdown"));
        assert!(set.claims[0].note.contains("UNCONSERVATIVE"));
        let receipt = vcad_receipt::DesignReceipt::with_claims(design_claims(&set));
        assert_eq!(receipt.verdict(), vcad_receipt::ReceiptVerdict::Provisional);

        let strict = buckling_study(&levels, 1e-9).unwrap();
        assert!(!strict.converged());
        assert!(predicted_claims(&strict).is_err());
        let single = buckling_study(&levels[1..], 0.5).unwrap();
        assert!(!single.converged(), "one level proves nothing");
    }
}

#[cfg(test)]
mod sweep {
    use super::*;
    use crate::shell::{classical_cylinder_pressure, CylinderLoading};

    /// FE vs the SP-8007 screen across lengths and loadings (diagnostic).
    #[test]
    #[ignore]
    fn cylinder_sweep() {
        let (r, t) = (50.0, 1.0);
        for (l, nth, nz) in [
            (100.0, 64, 24),
            (100.0, 96, 32),
            (300.0, 64, 40),
            (1000.0, 48, 40),
        ] {
            for lo in [CylinderLoading::Hydrostatic, CylinderLoading::Lateral] {
                let sh = cylinder_shell(r, t, l, nth, nz, 2);
                let spec = capped_cylinder_spec(&sh, r, t, 200_000.0, 0.3, 1.0, lo);
                let fe = buckling_on_mesh(&sh.mesh, &spec).unwrap();
                let (sc, n) = classical_cylinder_pressure(200_000.0, 0.3, r, t, l, lo);
                eprintln!(
                    "L {l} {lo:?} nth {nth} nz {nz}: fe {:.4} screen {sc:.4} (n {n}) {:+.2}% asym {:.2e}",
                    fe.modes[0].load_factor,
                    100.0 * (fe.modes[0].load_factor / sc - 1.0),
                    fe.load_stiffness_asymmetry
                );
            }
        }
    }
}
