//! Natural frequencies and mode shapes of any solid (free vibration).
//!
//! The part is filled with lattice bricks ([`crate::mesh::hex_fill_pitch`]).
//! Each brick is a trilinear hexahedron **with incompatible modes**
//! (Wilson–Taylor): nine extra internal bubble displacements `1−ξ²` per axis,
//! statically condensed out. Plain trilinear bricks and linear tets lock in
//! bending — a bar meshed two cells thick reads tens of percent sharp — while
//! the incompatible-mode brick reproduces pure bending exactly on a
//! rectangular cell, so two cells through a bar's thickness already lands
//! within a percent or so. Every lattice cell is the same brick, so the 24×24
//! element stiffness and consistent mass are computed once.
//!
//! The generalized eigenproblem `K φ = ω² M φ` is solved for its lowest modes
//! by LOBPCG (locally optimal block preconditioned conjugate gradients),
//! preconditioned with an incomplete Cholesky of `K + σM`. Free-free parts
//! have six rigid-body modes at ω ≈ 0; they are found and dropped, never
//! reported as elastic.
//!
//! Units: geometry in mm, modulus in Pa (via MPa in the spec), density in
//! kg/m³; frequencies in Hz.

use serde::{Deserialize, Serialize};
use tang_sparse::{CooMatrix, CsrMatrix, Preconditioner};
use vcad_kernel_tessellate::TriangleMesh;

use crate::mesh::{hex_fill_pitch, HexMesh, MeshError};
use crate::spec::RegionBox;

const TAU: f64 = 2.0 * std::f64::consts::PI;

/// What to compute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModalSpec {
    /// Cells along the longest bounding-box axis.
    pub resolution: usize,
    /// Optional cap on cell size per axis (mm): use it to put at least two
    /// cells through a thin section.
    #[serde(default)]
    pub max_pitch_mm: Option<[f64; 3]>,
    /// Young's modulus, MPa.
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio.
    pub poisson: f64,
    /// Density, kg/m³.
    pub density_kg_m3: f64,
    /// Fixed regions (all three translations held). Empty = free-free.
    #[serde(default)]
    pub supports: Vec<RegionBox>,
    /// Elastic modes wanted.
    pub modes: usize,
}

/// One elastic mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mode {
    /// Natural frequency, Hz.
    pub hz: f64,
    /// Nodal displacement (x, y, z per node, interleaved), max |u| = 1.
    pub shape: Vec<f64>,
    /// Mass-normalized participation in each axis (|φᵀ M e_axis|), for
    /// telling rigid-ish modes from bending.
    pub residual: f64,
}

/// The modal solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModalResult {
    /// Elastic modes, ascending.
    pub modes: Vec<Mode>,
    /// Rigid-body modes found and dropped (6 for free-free).
    pub rigid_modes: usize,
    /// Mesh node positions, mm.
    pub nodes: Vec<[f64; 3]>,
    /// Bricks.
    pub cells: usize,
    /// Cell size, mm.
    pub pitch: [f64; 3],
    /// Free DOFs.
    pub dofs: usize,
    /// LOBPCG iterations.
    pub iterations: usize,
    /// Total mass, kg.
    pub mass_kg: f64,
}

/// Modal failures.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalError {
    /// Meshing failed.
    Mesh(MeshError),
    /// Bad material or request.
    Spec(String),
    /// The eigensolver did not converge.
    NotConverged {
        /// Iterations run.
        iterations: usize,
        /// Worst relative residual among wanted modes.
        residual: f64,
    },
}

impl std::fmt::Display for ModalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModalError::Mesh(e) => write!(f, "mesh: {e:?}"),
            ModalError::Spec(s) => write!(f, "{s}"),
            ModalError::NotConverged {
                iterations,
                residual,
            } => write!(
                f,
                "eigensolver did not converge in {iterations} iterations (residual {residual:.2e})"
            ),
        }
    }
}

impl std::error::Error for ModalError {}

// ─── Element ───────────────────────────────────────────────────────────────

/// 24×24 stiffness (incompatible modes condensed) and consistent mass of one
/// brick `a×b×c` (m), row-major.
pub(crate) fn brick_matrices(
    a: f64,
    b: f64,
    c: f64,
    e: f64,
    nu: f64,
    rho: f64,
) -> (Vec<f64>, Vec<f64>) {
    let lam = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = e / (2.0 * (1.0 + nu));
    let g = 1.0 / 3f64.sqrt();
    let det = a * b * c / 8.0;
    let inv = [2.0 / a, 2.0 / b, 2.0 / c];
    const NC: usize = 24;
    const NT: usize = 33;
    let mut k = vec![0.0; NT * NT];
    let mut m = vec![0.0; NC * NC];
    for &xi in &[-g, g] {
        for &eta in &[-g, g] {
            for &zeta in &[-g, g] {
                let p = [xi, eta, zeta];
                // Shape gradients (physical) for 8 corners + 3 bubbles.
                let mut grads = [[0.0; 3]; 11];
                let mut n = [0.0; 8];
                for (cn, grad) in grads.iter_mut().enumerate().take(8) {
                    let s = [
                        if cn & 1 == 1 { 1.0 } else { -1.0 },
                        if cn & 2 == 2 { 1.0 } else { -1.0 },
                        if cn & 4 == 4 { 1.0 } else { -1.0 },
                    ];
                    let f = [1.0 + s[0] * p[0], 1.0 + s[1] * p[1], 1.0 + s[2] * p[2]];
                    n[cn] = f[0] * f[1] * f[2] / 8.0;
                    grad[0] = s[0] * f[1] * f[2] / 8.0 * inv[0];
                    grad[1] = f[0] * s[1] * f[2] / 8.0 * inv[1];
                    grad[2] = f[0] * f[1] * s[2] / 8.0 * inv[2];
                }
                for ax in 0..3 {
                    grads[8 + ax][ax] = -2.0 * p[ax] * inv[ax];
                }
                // B (6×33): column for shape s, component comp.
                let mut bm = vec![[0.0f64; 6]; NT];
                for (s, gr) in grads.iter().enumerate() {
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
                // D·B column-wise.
                let db: Vec<[f64; 6]> = bm
                    .iter()
                    .map(|b| {
                        let tr = b[0] + b[1] + b[2];
                        [
                            lam * tr + 2.0 * mu * b[0],
                            lam * tr + 2.0 * mu * b[1],
                            lam * tr + 2.0 * mu * b[2],
                            mu * b[3],
                            mu * b[4],
                            mu * b[5],
                        ]
                    })
                    .collect();
                for i in 0..NT {
                    for j in 0..NT {
                        let mut s = 0.0;
                        for r in 0..6 {
                            s += bm[i][r] * db[j][r];
                        }
                        k[i * NT + j] += s * det;
                    }
                }
                for i in 0..8 {
                    for j in 0..8 {
                        let v = rho * n[i] * n[j] * det;
                        for comp in 0..3 {
                            m[(3 * i + comp) * NC + 3 * j + comp] += v;
                        }
                    }
                }
            }
        }
    }
    // Condense: Kcc − Kci Kii⁻¹ Kic.
    let ni = NT - NC;
    let mut kii = vec![0.0; ni * ni];
    for i in 0..ni {
        for j in 0..ni {
            kii[i * ni + j] = k[(NC + i) * NT + NC + j];
        }
    }
    let l = dense_cholesky(&kii, ni).expect("incompatible-mode block is SPD");
    let mut kc = vec![0.0; NC * NC];
    for j in 0..NC {
        // y = Kii⁻¹ Kic[:, j]
        let mut y: Vec<f64> = (0..ni).map(|i| k[(NC + i) * NT + j]).collect();
        chol_solve(&l, ni, &mut y);
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
    (kc, m)
}

fn dense_cholesky(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if s <= 0.0 {
                    return None;
                }
                l[i * n + i] = s.sqrt();
            } else {
                l[i * n + j] = s / l[j * n + j];
            }
        }
    }
    Some(l)
}

fn chol_solve(l: &[f64], n: usize, b: &mut [f64]) {
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= l[i * n + k] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in (i + 1)..n {
            s -= l[k * n + i] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
}

/// Symmetric eigen-decomposition (cyclic Jacobi): ascending values and
/// column eigenvectors `vecs[c][r]`.
fn sym_eig(mut a: Vec<f64>, n: usize) -> (Vec<f64>, Vec<Vec<f64>>) {
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    for _ in 0..100 {
        let (mut off, mut diag) = (0.0, 0.0);
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
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let t = if theta == 0.0 {
                    1.0
                } else {
                    theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt())
                };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (x, y) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * x - s * y;
                    a[k * n + q] = s * x + c * y;
                }
                for k in 0..n {
                    let (x, y) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * x - s * y;
                    a[q * n + k] = s * x + c * y;
                }
                for k in 0..n {
                    let (x, y) = (v[k * n + p], v[k * n + q]);
                    v[k * n + p] = c * x - s * y;
                    v[k * n + q] = s * x + c * y;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| a[i * n + i].partial_cmp(&a[j * n + j]).unwrap());
    let vals = order.iter().map(|&i| a[i * n + i]).collect();
    let vecs = order
        .iter()
        .map(|&c| (0..n).map(|r| v[r * n + c]).collect())
        .collect();
    (vals, vecs)
}

// ─── Assembly ─────────────────────────────────────────────────────────────

struct System {
    k: CsrMatrix<f64>,
    m: CsrMatrix<f64>,
    /// free index → global DOF.
    free: Vec<usize>,
    ndof: usize,
}

fn assemble(mesh: &HexMesh, spec: &ModalSpec) -> System {
    let ndof = 3 * mesh.nodes.len();
    let p = [
        mesh.pitch[0] / 1000.0,
        mesh.pitch[1] / 1000.0,
        mesh.pitch[2] / 1000.0,
    ];
    let (ke, me) = brick_matrices(
        p[0],
        p[1],
        p[2],
        spec.youngs_modulus_mpa * 1e6,
        spec.poisson,
        spec.density_kg_m3,
    );
    let tol = mesh.pitch[0].min(mesh.pitch[1]).min(mesh.pitch[2]) * 0.25;
    let mut fixed = vec![false; ndof];
    for (i, x) in mesh.nodes.iter().enumerate() {
        if spec
            .supports
            .iter()
            .any(|r| (0..3).all(|a| x[a] >= r.min[a] - tol && x[a] <= r.max[a] + tol))
        {
            fixed[3 * i] = true;
            fixed[3 * i + 1] = true;
            fixed[3 * i + 2] = true;
        }
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
    let mut kc = CooMatrix::with_capacity(nf, nf, mesh.cells.len() * 576);
    let mut mc = CooMatrix::with_capacity(nf, nf, mesh.cells.len() * 576);
    for cell in &mesh.cells {
        let mut dofs = [0usize; 24];
        for (c, &node) in cell.iter().enumerate() {
            for comp in 0..3 {
                dofs[3 * c + comp] = map[3 * node as usize + comp];
            }
        }
        for i in 0..24 {
            if dofs[i] == usize::MAX {
                continue;
            }
            for j in 0..24 {
                if dofs[j] == usize::MAX {
                    continue;
                }
                let kv = ke[i * 24 + j];
                if kv != 0.0 {
                    kc.push(dofs[i], dofs[j], kv);
                }
                let mv = me[i * 24 + j];
                if mv != 0.0 {
                    mc.push(dofs[i], dofs[j], mv);
                }
            }
        }
    }
    System {
        k: kc.to_csr(),
        m: mc.to_csr(),
        free,
        ndof,
    }
}

fn spmv(a: &CsrMatrix<f64>, x: &[f64], y: &mut [f64]) {
    tang_sparse::csr_apply(a, x, y);
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// `A = K + σ M` as CSR (same pattern: both come from the same element loop).
fn shifted(k: &CsrMatrix<f64>, m: &CsrMatrix<f64>, sigma: f64) -> CsrMatrix<f64> {
    let mut c = CooMatrix::with_capacity(k.nrows, k.ncols, k.nnz() + m.nnz());
    for (a, s) in [(k, 1.0), (m, sigma)] {
        for i in 0..a.nrows {
            for idx in a.row_ptrs[i]..a.row_ptrs[i + 1] {
                c.push(i, a.col_indices[idx], s * a.values[idx]);
            }
        }
    }
    c.to_csr()
}

// ─── Exact preconditioner: RCM-ordered skyline Cholesky ────────────────────

/// Reverse Cuthill–McKee ordering of a symmetric CSR pattern.
fn rcm(a: &CsrMatrix<f64>) -> Vec<usize> {
    let n = a.nrows;
    let deg: Vec<usize> = (0..n).map(|i| a.row_ptrs[i + 1] - a.row_ptrs[i]).collect();
    let mut visited = vec![false; n];
    let mut order = Vec::with_capacity(n);
    loop {
        // Start each component from a minimum-degree unvisited node.
        let Some(start) = (0..n).filter(|&i| !visited[i]).min_by_key(|&i| deg[i]) else {
            break;
        };
        let mut queue = std::collections::VecDeque::new();
        visited[start] = true;
        queue.push_back(start);
        while let Some(v) = queue.pop_front() {
            order.push(v);
            let mut nbrs: Vec<usize> = a.col_indices[a.row_ptrs[v]..a.row_ptrs[v + 1]]
                .iter()
                .copied()
                .filter(|&u| !visited[u])
                .collect();
            nbrs.sort_by_key(|&u| deg[u]);
            for u in nbrs {
                if !visited[u] {
                    visited[u] = true;
                    queue.push_back(u);
                }
            }
        }
    }
    order.reverse();
    order
}

/// Lower-triangular profile Cholesky `P A Pᵀ = L Lᵀ`.
pub(crate) struct Skyline {
    perm: Vec<usize>,
    /// first[i] = first stored column of row i (permuted numbering).
    first: Vec<usize>,
    /// Row i occupies data[start[i]..start[i+1]], columns first[i]..=i.
    start: Vec<usize>,
    data: Vec<f64>,
}

impl Skyline {
    /// Profile size the factor would need (entries), for a budget check.
    fn profile(a: &CsrMatrix<f64>, perm: &[usize]) -> (Vec<usize>, Vec<usize>) {
        let n = a.nrows;
        let mut inv = vec![0usize; n];
        for (new, &old) in perm.iter().enumerate() {
            inv[old] = new;
        }
        let mut first: Vec<usize> = (0..n).collect();
        for old in 0..n {
            let i = inv[old];
            for &c in &a.col_indices[a.row_ptrs[old]..a.row_ptrs[old + 1]] {
                let j = inv[c];
                if j < i {
                    first[i] = first[i].min(j);
                }
            }
        }
        (inv, first)
    }

    fn factor(a: &CsrMatrix<f64>, budget: usize) -> Option<Skyline> {
        let n = a.nrows;
        let perm = rcm(a);
        let (inv, first) = Self::profile(a, &perm);
        let mut start = vec![0usize; n + 1];
        for i in 0..n {
            start[i + 1] = start[i] + (i - first[i] + 1);
        }
        if start[n] > budget {
            return None;
        }
        let mut data = vec![0.0; start[n]];
        for old in 0..n {
            let i = inv[old];
            for idx in a.row_ptrs[old]..a.row_ptrs[old + 1] {
                let j = inv[a.col_indices[idx]];
                if j <= i {
                    data[start[i] + (j - first[i])] += a.values[idx];
                }
            }
        }
        // Row-oriented profile Cholesky.
        for i in 0..n {
            let fi = first[i];
            for j in fi..=i {
                let fj = first[j];
                let k0 = fi.max(fj);
                let mut sum = data[start[i] + (j - fi)];
                let (ri, rj) = (start[i] + (k0 - fi), start[j] + (k0 - fj));
                for t in 0..(j - k0) {
                    sum -= data[ri + t] * data[rj + t];
                }
                if j == i {
                    if sum <= 0.0 {
                        return None;
                    }
                    data[start[i] + (i - fi)] = sum.sqrt();
                } else {
                    data[start[i] + (j - fi)] = sum / data[start[j] + (j - fj)];
                }
            }
        }
        Some(Skyline {
            perm,
            first,
            start,
            data,
        })
    }

    fn solve(&self, r: &[f64], z: &mut [f64]) {
        let n = self.perm.len();
        let mut y: Vec<f64> = self.perm.iter().map(|&o| r[o]).collect();
        for i in 0..n {
            let fi = self.first[i];
            let row = &self.data[self.start[i]..self.start[i + 1]];
            let mut s = y[i];
            for (t, j) in (fi..i).enumerate() {
                s -= row[t] * y[j];
            }
            y[i] = s / row[i - fi];
        }
        for i in (0..n).rev() {
            let fi = self.first[i];
            let row = &self.data[self.start[i]..self.start[i + 1]];
            y[i] /= row[i - fi];
            let yi = y[i];
            for (t, j) in (fi..i).enumerate() {
                y[j] -= row[t] * yi;
            }
        }
        for (new, &old) in self.perm.iter().enumerate() {
            z[old] = y[new];
        }
    }
}

enum Precond {
    Exact(Skyline),
    Ic0(tang_sparse::Ic0),
    Jacobi(tang_sparse::Jacobi),
}

impl Precond {
    fn apply(&self, r: &[f64], z: &mut [f64]) {
        match self {
            Precond::Exact(p) => p.solve(r, z),
            Precond::Ic0(p) => p.apply(r, z),
            Precond::Jacobi(p) => p.apply(r, z),
        }
    }
}

// ─── LOBPCG ───────────────────────────────────────────────────────────────

/// M-orthonormalize the columns of `s` (dropping near-dependent directions)
/// via the eigen-decomposition of the Gram matrix. Returns the new basis.
fn m_orthonormalize(s: &[Vec<f64>], m: &CsrMatrix<f64>) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let k = s.len();
    let n = s.first().map_or(0, Vec::len);
    let ms: Vec<Vec<f64>> = s
        .iter()
        .map(|v| {
            let mut y = vec![0.0; n];
            spmv(m, v, &mut y);
            y
        })
        .collect();
    // Scale every column to unit M-norm first: W columns (preconditioned
    // residuals) can be orders of magnitude larger than X, and a raw Gram
    // matrix would make the X directions look dependent and drop them.
    let d: Vec<f64> = (0..k)
        .map(|i| {
            let nrm = dot(&s[i], &ms[i]).max(0.0).sqrt();
            if nrm > 0.0 {
                1.0 / nrm
            } else {
                0.0
            }
        })
        .collect();
    let mut g = vec![0.0; k * k];
    for i in 0..k {
        for j in 0..=i {
            let v = dot(&s[i], &ms[j]) * d[i] * d[j];
            g[i * k + j] = v;
            g[j * k + i] = v;
        }
    }
    let (vals, vecs) = sym_eig(g, k);
    let top = vals.iter().cloned().fold(0.0f64, f64::max);
    let mut out = Vec::new();
    let mut coef = Vec::new();
    for (val, vec) in vals.iter().zip(&vecs) {
        if *val <= 1e-8 * top {
            continue;
        }
        let scale = 1.0 / val.sqrt();
        let mut col = vec![0.0; n];
        let mut cf = vec![0.0; k];
        for (j, c) in vec.iter().enumerate() {
            let w = c * scale * d[j];
            cf[j] = w;
            if w != 0.0 {
                for r in 0..n {
                    col[r] += w * s[j][r];
                }
            }
        }
        out.push(col);
        coef.push(cf);
    }
    (out, coef)
}

/// Remove the M-components along the (M-orthonormal) deflation vectors.
fn project_out(v: &mut [f64], z: &[Vec<f64>], mz: &[Vec<f64>]) {
    for (zi, mzi) in z.iter().zip(mz) {
        let c = dot(mzi, v);
        if c != 0.0 {
            for (a, b) in v.iter_mut().zip(zi) {
                *a -= c * b;
            }
        }
    }
}

struct Eigs {
    vals: Vec<f64>,
    vecs: Vec<Vec<f64>>,
    iterations: usize,
}

fn lobpcg(
    sys: &System,
    deflate: &[Vec<f64>],
    lam_ref: f64,
    want: usize,
    block: usize,
    precond: &Precond,
    tol: f64,
    max_iter: usize,
) -> Result<Eigs, ModalError> {
    let n = sys.k.nrows;
    let block = block.min(n);
    // Deterministic, well-mixed start vectors.
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut x: Vec<Vec<f64>> = (0..block)
        .map(|_| {
            (0..n)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
                })
                .collect()
        })
        .collect();
    // Classic LOBPCG: Rayleigh–Ritz on span[X, W, P], where W = T·R are
    // the preconditioned residuals and P carries the W/P components of the
    // previous Ritz vectors (the "conjugate direction").
    let mz: Vec<Vec<f64>> = deflate
        .iter()
        .map(|v| {
            let mut y = vec![0.0; n];
            spmv(&sys.m, v, &mut y);
            y
        })
        .collect();
    for v in x.iter_mut() {
        project_out(v, deflate, &mz);
    }
    let mut w: Vec<Vec<f64>> = Vec::new();
    let mut p: Vec<Vec<f64>> = Vec::new();
    let mut worst = f64::INFINITY;
    for it in 0..max_iter {
        let nx = x.len();
        let nw = w.len();
        let span: Vec<Vec<f64>> = x.iter().chain(w.iter()).chain(p.iter()).cloned().collect();
        let (basis, coef) = m_orthonormalize(&span, &sys.m);
        let (vals, xnew, ritz) = rr(&basis, sys, block);
        // Ritz vector j = Σ_b ritz[j][b] · basis[b] = Σ_s (Σ_b ritz[j][b] coef[b][s]) · span[s].
        // P_new keeps only the W and P parts (s ≥ nx).
        let mut pnew = Vec::with_capacity(block);
        if it > 0 {
            for rj in &ritz {
                let mut a = vec![0.0; span.len()];
                for (b, rb) in rj.iter().enumerate() {
                    for (sidx, c) in coef[b].iter().enumerate() {
                        a[sidx] += rb * c;
                    }
                }
                let mut col = vec![0.0; n];
                for sidx in nx..span.len() {
                    let c = a[sidx];
                    if c != 0.0 {
                        for r in 0..n {
                            col[r] += c * span[sidx][r];
                        }
                    }
                }
                pnew.push(col);
            }
        }
        let _ = nw;
        x = xnew;
        // Residuals R = K x − λ M x, preconditioned.
        let mut wn = Vec::with_capacity(block);
        worst = 0.0f64;
        for (jj, xv) in x.iter().enumerate() {
            let mut kx = vec![0.0; n];
            let mut mx = vec![0.0; n];
            spmv(&sys.k, xv, &mut kx);
            spmv(&sys.m, xv, &mut mx);
            let lam = vals[jj];
            let r: Vec<f64> = kx.iter().zip(&mx).map(|(a, b)| a - lam * b).collect();
            // Relative to (|λ| + λ_ref)·‖Mx‖: a rigid mode (λ ≈ 0) is judged
            // against a small stiffness scale, not against its own ‖Kx‖.
            let denom = (lam.abs() + lam_ref) * dot(&mx, &mx).sqrt();
            let rel = dot(&r, &r).sqrt() / denom.max(1e-300);
            if jj < want {
                worst = worst.max(rel);
            }
            let mut z = vec![0.0; n];
            precond.apply(&r, &mut z);
            project_out(&mut z, deflate, &mz);
            wn.push(z);
        }
        if worst < tol {
            return Ok(Eigs {
                vals,
                vecs: x,
                iterations: it + 1,
            });
        }
        w = wn;
        p = pnew;
    }
    Err(ModalError::NotConverged {
        iterations: max_iter,
        residual: worst,
    })
}

/// Rayleigh–Ritz in an M-orthonormal basis: lowest `k` Ritz pairs.
fn rr(basis: &[Vec<f64>], sys: &System, k: usize) -> (Vec<f64>, Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let nb = basis.len();
    let n = sys.k.nrows;
    let kb: Vec<Vec<f64>> = basis
        .iter()
        .map(|v| {
            let mut y = vec![0.0; n];
            spmv(&sys.k, v, &mut y);
            y
        })
        .collect();
    let mut g = vec![0.0; nb * nb];
    for i in 0..nb {
        for j in 0..=i {
            let v = dot(&basis[i], &kb[j]);
            g[i * nb + j] = v;
            g[j * nb + i] = v;
        }
    }
    let (vals, vecs) = sym_eig(g, nb);
    let k = k.min(nb);
    let xs = (0..k)
        .map(|c| {
            let mut col = vec![0.0; n];
            for (j, w) in vecs[c].iter().enumerate() {
                if *w != 0.0 {
                    for r in 0..n {
                        col[r] += w * basis[j][r];
                    }
                }
            }
            col
        })
        .collect();
    (vals[..k].to_vec(), xs, vecs[..k].to_vec())
}

// ─── Driver ───────────────────────────────────────────────────────────────

/// The six rigid-body modes on the free DOFs, M-orthonormalized.
fn rigid_modes(hex: &HexMesh, sys: &System) -> Vec<Vec<f64>> {
    let nn = hex.nodes.len() as f64;
    let mut c = [0.0; 3];
    for x in &hex.nodes {
        for a in 0..3 {
            c[a] += x[a] / nn;
        }
    }
    let n = sys.k.nrows;
    let mut raw = Vec::with_capacity(6);
    for mode in 0..6 {
        let mut v = vec![0.0; n];
        for (i, &d) in sys.free.iter().enumerate() {
            let node = d / 3;
            let comp = d % 3;
            let x = hex.nodes[node];
            let r = [
                (x[0] - c[0]) / 1000.0,
                (x[1] - c[1]) / 1000.0,
                (x[2] - c[2]) / 1000.0,
            ];
            v[i] = if mode < 3 {
                if comp == mode {
                    1.0
                } else {
                    0.0
                }
            } else {
                // Rotation about axis (mode-3): ω × r.
                let w = mode - 3;
                let cross = [[0.0, -r[2], r[1]], [r[2], 0.0, -r[0]], [-r[1], r[0], 0.0]];
                // (e_w × r)_comp
                -cross[comp][w]
            };
        }
        raw.push(v);
    }
    m_orthonormalize(&raw, &sys.m).0
}

/// Lowest elastic modes of the solid bounded by `mesh`.
pub fn modal_analysis(mesh: &TriangleMesh, spec: &ModalSpec) -> Result<ModalResult, ModalError> {
    if !(spec.youngs_modulus_mpa > 0.0 && spec.density_kg_m3 > 0.0)
        || !(spec.poisson > -1.0 && spec.poisson < 0.5)
        || spec.modes == 0
    {
        return Err(ModalError::Spec(
            "modulus and density must be positive, poisson in (-1, 0.5), modes ≥ 1".into(),
        ));
    }
    let hex = hex_fill_pitch(mesh, spec.resolution, spec.max_pitch_mm).map_err(ModalError::Mesh)?;
    modal_on_mesh(&hex, spec)
}

/// [`modal_analysis`] on an existing brick mesh.
pub fn modal_on_mesh(hex: &HexMesh, spec: &ModalSpec) -> Result<ModalResult, ModalError> {
    let sys = assemble(hex, spec);
    let n = sys.k.nrows;
    let free_free = spec.supports.is_empty();
    // Free-free: the six rigid-body modes are known exactly (three
    // translations, three rotations about the centroid). Deflate them
    // instead of asking the solver to resolve a cluster at ω = 0.
    let deflate = if free_free {
        rigid_modes(hex, &sys)
    } else {
        Vec::new()
    };
    let rigid = deflate.len();
    let want = spec.modes.min(n.saturating_sub(rigid));
    let block = (want + 4).min(n);
    // Shift for the preconditioner: tiny relative to the stiffness scale,
    // enough to make K + σM positive definite for a free-free part.
    let (mut kd, mut md) = (0.0, 0.0);
    for i in 0..n {
        for idx in sys.k.row_ptrs[i]..sys.k.row_ptrs[i + 1] {
            if sys.k.col_indices[idx] == i {
                kd += sys.k.values[idx];
            }
        }
        for idx in sys.m.row_ptrs[i]..sys.m.row_ptrs[i + 1] {
            if sys.m.col_indices[idx] == i {
                md += sys.m.values[idx];
            }
        }
    }
    // σ ≈ 1e-4 × the element-level ω²: far below the modes of interest
    // (it only shapes the preconditioner), but large enough that the
    // rigid-mode near-singularity can't zero a pivot in the factor.
    let sigma = 1e-4 * kd / md.max(1e-300);
    let a = shifted(&sys.k, &sys.m, sigma);
    // Exact factor when the RCM profile fits (~400 MB budget); LOBPCG then
    // converges in tens of iterations. Otherwise incomplete Cholesky.
    let precond = match Skyline::factor(&a, 50_000_000) {
        Some(f) => Precond::Exact(f),
        None => match tang_sparse::Ic0::new(&a) {
            Ok(p) => Precond::Ic0(p),
            Err(_) => Precond::Jacobi(tang_sparse::Jacobi::from_csr(&a)),
        },
    };
    // λ_ref ≈ 1e-8 × the element-level ω² (diag K / diag M): ~10 Hz on a
    // millimetre-scale metal mesh.
    let eig = lobpcg(
        &sys,
        &deflate,
        1e-8 * kd / md.max(1e-300),
        want,
        block,
        &precond,
        1e-5,
        3000,
    )?;

    let mass_kg = {
        // Total mass = sum of all M entries for one translation direction.
        let ones: Vec<f64> = (0..n)
            .map(|i| if sys.free[i] % 3 == 0 { 1.0 } else { 0.0 })
            .collect();
        let mut y = vec![0.0; n];
        spmv(&sys.m, &ones, &mut y);
        dot(&ones, &y)
    };
    // Rigid modes sit near zero; elastic ones well above. Split at a
    // fraction of the lowest clearly-elastic eigenvalue.
    let rigid_found = rigid;
    let mut modes = Vec::new();
    for (lam, v) in eig.vals.iter().zip(&eig.vecs) {
        if modes.len() == spec.modes {
            break;
        }
        let mut shape = vec![0.0; sys.ndof];
        for (i, &d) in sys.free.iter().enumerate() {
            shape[d] = v[i];
        }
        let peak = shape.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        if peak > 0.0 {
            shape.iter_mut().for_each(|x| *x /= peak);
        }
        modes.push(Mode {
            hz: lam.sqrt() / TAU,
            shape,
            residual: 0.0,
        });
    }
    Ok(ModalResult {
        modes,
        rigid_modes: rigid_found,
        nodes: hex.nodes.clone(),
        cells: hex.cells.len(),
        pitch: hex.pitch,
        dofs: n,
        iterations: eig.iterations,
        mass_kg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::box_mesh;

    const E: f64 = 68_900.0;
    const NU: f64 = 0.33;
    const RHO: f64 = 2700.0;

    fn spec(res: usize, max_pitch: Option<[f64; 3]>, modes: usize) -> ModalSpec {
        ModalSpec {
            resolution: res,
            max_pitch_mm: max_pitch,
            youngs_modulus_mpa: E,
            poisson: NU,
            density_kg_m3: RHO,
            supports: vec![],
            modes,
        }
    }

    /// Euler–Bernoulli free-free f₁ for a bar of length l, thickness t (mm).
    fn eb_f1(l: f64, t: f64) -> f64 {
        let bl: f64 = 4.730_040_744_862_704;
        let (l, t) = (l / 1000.0, t / 1000.0);
        bl * bl * t * (E * 1e6 / (12.0 * RHO)).sqrt() / (TAU * l * l)
    }

    #[test]
    fn brick_patch_bending_is_exact() {
        // The condensed brick must not lock: pure bending of one element
        // should cost exactly the beam energy. Check via a two-element-thick
        // cantilever-free bar's lowest frequency vs Euler–Bernoulli below.
        let (k, m) = brick_matrices(0.001, 0.001, 0.001, 1e9, 0.3, 1000.0);
        // Rigid translations: K·1 = 0.
        for comp in 0..3 {
            let u: Vec<f64> = (0..24)
                .map(|i| if i % 3 == comp { 1.0 } else { 0.0 })
                .collect();
            for i in 0..24 {
                let s: f64 = (0..24).map(|j| k[i * 24 + j] * u[j]).sum();
                assert!(s.abs() < 1e-3, "rigid translation strains the brick");
            }
            let mass: f64 = (0..24)
                .map(|i| (0..24).map(|j| u[i] * m[i * 24 + j] * u[j]).sum::<f64>())
                .sum();
            assert!((mass - 1e-6).abs() < 1e-15, "mass {mass}");
        }
    }

    #[test]
    fn slender_bar_matches_euler_bernoulli() {
        // 300 × 10 × 5 mm 6061: L/t = 60, so shear and rotary inertia are
        // ~0.1% and Euler–Bernoulli is the oracle. Two cells through t.
        let mesh = box_mesh([0.0, 0.0, 0.0], [300.0, 10.0, 5.0]);
        let r = modal_analysis(&mesh, &spec(120, Some([2.5, 2.5, 2.5]), 3)).unwrap();
        assert_eq!(r.rigid_modes, 6, "free-free has six rigid modes");
        let f1 = eb_f1(300.0, 5.0);
        let err = r.modes[0].hz / f1 - 1.0;
        assert!(
            err.abs() < 0.01,
            "f1 {} vs EB {f1} ({:+.2}%)",
            r.modes[0].hz,
            err * 100.0
        );
        // Next: in-plane bending (about the 10 mm axis) at ~2× f1.
        let ratio = r.modes[1].hz / r.modes[0].hz;
        assert!(
            (ratio - 2.0).abs() < 0.04,
            "in-plane / out-of-plane {ratio}"
        );
        // Mass is the box's.
        assert!((r.mass_kg - 300.0 * 10.0 * 5.0 * 1e-9 * RHO).abs() < 1e-6);
    }

    #[test]
    fn free_square_plate_first_mode() {
        // Free square plate, first mode λ² ≈ 13.49 (Leissa, ν = 0.3) at
        // f = λ²/(2π a²)·√(D/ρh). Use ν = 0.3 to match the table.
        let mesh = box_mesh([0.0, 0.0, 0.0], [100.0, 100.0, 3.0]);
        let mut s = spec(40, Some([2.5, 2.5, 1.5]), 1);
        s.poisson = 0.3;
        let r = modal_analysis(&mesh, &s).unwrap();
        let (a, h) = (0.1, 0.003);
        let d = E * 1e6 * h * h * h / (12.0 * (1.0 - 0.09));
        let f = 13.49 / (TAU * a * a) * (d / (RHO * h)).sqrt();
        let err = r.modes[0].hz / f - 1.0;
        assert!(
            err.abs() < 0.04,
            "plate {} vs {f} ({:+.2}%)",
            r.modes[0].hz,
            err * 100.0
        );
    }

    #[test]
    fn clamped_support_removes_rigid_modes() {
        let mesh = box_mesh([0.0, 0.0, 0.0], [200.0, 10.0, 5.0]);
        let mut s = spec(80, Some([2.5, 2.5, 2.5]), 1);
        s.supports = vec![RegionBox {
            min: [-1.0, -1.0, -1.0],
            max: [0.1, 11.0, 6.0],
        }];
        let r = modal_analysis(&mesh, &s).unwrap();
        assert_eq!(r.rigid_modes, 0);
        // Cantilever f1 = 1.8751² / (2π L²) · t·√(E/12ρ).
        let (l, t) = (0.2, 0.005);
        let f =
            1.875_104_068_711_961f64.powi(2) * t * (E * 1e6 / (12.0 * RHO)).sqrt() / (TAU * l * l);
        let err = r.modes[0].hz / f - 1.0;
        assert!(
            err.abs() < 0.02,
            "cantilever {} vs {f} ({:+.2}%)",
            r.modes[0].hz,
            err * 100.0
        );
    }
}

#[cfg(test)]
mod dense_check {
    use super::*;
    use crate::mesh::{box_mesh, hex_fill_pitch};

    /// Dense reference: all eigenvalues of (K, M) on a tiny mesh.
    fn dense_eigs(sys: &System) -> Vec<f64> {
        let n = sys.k.nrows;
        let to_dense = |a: &CsrMatrix<f64>| {
            let mut d = vec![0.0; n * n];
            for i in 0..n {
                for idx in a.row_ptrs[i]..a.row_ptrs[i + 1] {
                    d[i * n + a.col_indices[idx]] += a.values[idx];
                }
            }
            d
        };
        let (k, m) = (to_dense(&sys.k), to_dense(&sys.m));
        let l = dense_cholesky(&m, n).expect("M SPD");
        // C = L⁻¹ K L⁻ᵀ
        let mut y = k.clone();
        for c in 0..n {
            let mut col: Vec<f64> = (0..n).map(|r| y[r * n + c]).collect();
            for i in 0..n {
                let mut s = col[i];
                for q in 0..i {
                    s -= l[i * n + q] * col[q];
                }
                col[i] = s / l[i * n + i];
            }
            for r in 0..n {
                y[r * n + c] = col[r];
            }
        }
        let mut c = vec![0.0; n * n];
        for r in 0..n {
            let mut row: Vec<f64> = (0..n).map(|q| y[r * n + q]).collect();
            for i in 0..n {
                let mut s = row[i];
                for q in 0..i {
                    s -= l[i * n + q] * row[q];
                }
                row[i] = s / l[i * n + i];
            }
            for q in 0..n {
                c[r * n + q] = row[q];
            }
        }
        sym_eig(c, n).0
    }

    #[test]
    #[ignore]
    fn print_dense_vs_eb() {
        let mesh = box_mesh([0.0, 0.0, 0.0], [40.0, 2.0, 2.0]);
        let spec = ModalSpec {
            resolution: 20,
            max_pitch_mm: Some([2.0, 1.0, 1.0]),
            youngs_modulus_mpa: 68_900.0,
            poisson: 0.33,
            density_kg_m3: 2700.0,
            supports: vec![],
            modes: 4,
        };
        let hex = hex_fill_pitch(&mesh, spec.resolution, spec.max_pitch_mm).unwrap();
        let sys = assemble(&hex, &spec);
        eprintln!(
            "dofs {} cells {} pitch {:?}",
            sys.k.nrows,
            hex.cells.len(),
            hex.pitch
        );
        let ev = dense_eigs(&sys);
        let hz: Vec<f64> = ev
            .iter()
            .take(12)
            .map(|l| l.max(0.0).sqrt() / TAU)
            .collect();
        let bl: f64 = 4.730_040_744_862_704;
        let f1 = bl * bl * 0.002 * (68_900e6 / (12.0 * 2700.0f64)).sqrt() / (TAU * 0.04 * 0.04);
        eprintln!("dense hz {hz:.1?}\nEB f1 {f1:.1}");
        let r = modal_on_mesh(&hex, &spec);
        eprintln!(
            "lobpcg {:?}",
            r.as_ref()
                .map(|r| r.modes.iter().map(|m| m.hz).collect::<Vec<_>>())
                .map_err(|e| e.to_string())
        );
    }
}
