//! Linear-tet stiffness assembly as a tang tensor graph.
//!
//! The solver in [`crate::solve`] is matrix-free: every PCG iteration
//! recomputes each element's stress from its shape gradients. This module
//! builds the same operator as an assembled CSR matrix, entirely from
//! tang-compile comprehensions, so it lowers to every tensor backend
//! (CUDA, Metal, WGSL) and the assembly can run on a GPU:
//!
//! 1. **Gather.** Element coordinates `X[e, 3i+c] = nodes[3·tet[e,i] + c]`:
//!    connectivity is a gather view, element → node.
//! 2. **Element kernel.** Shape gradients `∇φ_i` and volume from the edge
//!    matrix's cofactors (comprehension bodies), then the unit-E element
//!    stiffness `K_e[(i,a),(j,b)] = V·(λ g_ia g_jb + μ g_ib g_ja + μ δ_ab g_i·g_j)`.
//! 3. **Scatter-add.** CSR value `k` sums the element entries that land on
//!    it: an ELL gather over the flattened `K_e` feeding a sum monoid
//!    (`tang_compile::tensor::sparse::spmv`).
//!
//! Supports are applied by dropping fixed rows and columns and putting 1
//! on their diagonal, so the matrix is SPD and its solution is the
//! matrix-free solver's (whose fixed rows are zeroed, with zero load).
//! The solve itself stays on the CPU in f64.

use tang::Scalar;
use tang_compile::tensor::sparse::spmv;
use tang_compile::tensor::{Arg, Form, Monoid, TensorGraph, TensorId, View};
use tang_expr::ExprId;
use tang_sparse::{PcgOptions, PcgStatus};

use crate::mesh::TetMesh;
use crate::solve::{assemble_bc, build_elements, lame, SolveError, SolveOptions};
use crate::spec::FeaSpec;

/// The unit-E stiffness assembly graph for one mesh and support set.
pub struct DeviceAssembly {
    /// Node coordinates → CSR values. No operator nodes.
    pub graph: TensorGraph,
    /// One buffer per graph input, in input order.
    pub inputs: Vec<Vec<f64>>,
    /// The CSR values (`nnz`).
    pub output: TensorId,
    /// CSR row pointers over all `3·nodes` dofs (`ndof + 1`).
    pub row_ptrs: Vec<usize>,
    /// CSR columns (`nnz`).
    pub cols: Vec<usize>,
    /// Fixed-dof mask (rows and columns dropped, unit diagonal).
    pub fixed: Vec<bool>,
    /// Nodal load vector (zero on fixed dofs).
    pub rhs: Vec<f64>,
}

impl DeviceAssembly {
    /// Evaluate on the reference interpreter (f64).
    pub fn eval_cpu(&self) -> Vec<f64> {
        self.graph
            .eval(&self.inputs, &[self.output])
            .pop()
            .expect("one output")
    }

    fn row(&self, r: usize) -> std::ops::Range<usize> {
        self.row_ptrs[r]..self.row_ptrs[r + 1]
    }

    /// `y = K·x` with assembled values.
    pub fn apply(&self, vals: &[f64], x: &[f64], y: &mut [f64]) {
        for (r, yr) in y.iter_mut().enumerate() {
            *yr = self.row(r).map(|k| vals[k] * x[self.cols[k]]).sum();
        }
    }

    /// Jacobi-PCG on assembled values for the stored load: displacement
    /// per dof at unit Young's modulus, as [`crate::solve`] solves it.
    pub fn solve(&self, vals: &[f64], opts: &SolveOptions) -> Result<Vec<f64>, SolveError> {
        let n = self.rhs.len();
        let diag: Vec<f64> = (0..n)
            .map(|r| {
                self.row(r)
                    .find(|&k| self.cols[k] == r)
                    .map_or(1.0, |k| vals[k])
            })
            .collect();
        let sol = tang_sparse::pcg(
            |x, y| self.apply(vals, x, y),
            &tang_sparse::Jacobi::new(diag),
            &self.rhs,
            &vec![0.0; n],
            &PcgOptions {
                tol: opts.tol,
                max_iters: opts.max_iters,
            },
        );
        if sol.status != PcgStatus::Converged {
            return Err(SolveError::NotConverged {
                residual_rel: sol.residual_rel,
                iterations: sol.iterations,
            });
        }
        Ok(sol.x)
    }
}

/// `Σ_d coefs[d]·u_d + offset` over a rank-`rank` iteration space.
fn form(rank: usize, terms: &[(usize, i64)], offset: i64) -> Form {
    let mut coefs = vec![0; rank];
    for &(d, c) in terms {
        coefs[d] += c;
    }
    Form { coefs, offset }
}

fn view(forms: Vec<Form>) -> View {
    View {
        forms,
        guard: false,
        gather: None,
    }
}

/// The 3×3 edge-matrix inverse entry `inv[j][i]` from the 9 edge
/// components (`m[r][c]` = `e[3r + c]`) and `1/det`, in the matrix-free
/// solver's operation order.
fn inv_entry(m: &[ExprId], inv_det: ExprId, j: usize, i: usize) -> ExprId {
    let at = |r: usize, c: usize| m[3 * r + c];
    let (a, b) = ((i + 1) % 3, (i + 2) % 3);
    let (c, d) = ((j + 1) % 3, (j + 2) % 3);
    (at(a, c) * at(b, d) - at(a, d) * at(b, c)) * inv_det
}

fn det3(m: &[ExprId]) -> ExprId {
    let at = |r: usize, c: usize| m[3 * r + c];
    at(0, 0) * (at(1, 1) * at(2, 2) - at(1, 2) * at(2, 1))
        - at(0, 1) * (at(1, 0) * at(2, 2) - at(1, 2) * at(2, 0))
        + at(0, 2) * (at(1, 0) * at(2, 1) - at(1, 1) * at(2, 0))
}

/// Build the assembly graph. Validates the spec and the mesh (degenerate
/// tets fail closed, as in the solver) and resolves the supports and loads.
pub fn device_assembly(mesh: &TetMesh, spec: &FeaSpec) -> Result<DeviceAssembly, SolveError> {
    spec.validate()?;
    build_elements(mesh)?; // same degenerate-tet check as the solver
    let bc = assemble_bc(mesh, spec)?;
    let (lam, mu) = lame(spec.poisson);
    let ne = mesh.tets.len();
    let ndof = 3 * mesh.nodes.len();

    let mut g = TensorGraph::new();
    let mut inputs: Vec<Vec<f64>> = Vec::new();
    let mut input = |g: &mut TensorGraph, shape: &[usize], data: Vec<f64>| {
        debug_assert_eq!(shape.iter().product::<usize>(), data.len());
        inputs.push(data);
        g.input(shape)
    };

    // -- Gather: element coordinates ----------------------------------------
    let nodes = input(
        &mut g,
        &[ndof],
        mesh.nodes.iter().flatten().copied().collect(),
    );
    let cidx = input(
        &mut g,
        &[ne, 12],
        mesh.tets
            .iter()
            .flat_map(|t| {
                t.iter()
                    .flat_map(|&n| (0..3).map(move |c| (3 * n as usize + c) as f64))
            })
            .collect(),
    );
    let x = g
        .comp(
            &[ne, 12],
            &[],
            vec![
                Arg {
                    src: cidx,
                    view: View::identity(2, 2),
                },
                Arg {
                    src: nodes,
                    view: View::pick(2, &[None]).gather(0, 0, 1),
                },
            ],
            None,
            |v| vec![v[1]],
        )
        .expect("coordinate gather");

    // -- Element kernel: shape gradients and volume -------------------------
    // Edge component (r, c) = X[e, 3(r+1)+c] − X[e, c]; the 12 coordinate
    // reads below are X[e, k] for k = 0..12 at iteration rank `rank`.
    let coords = |rank: usize| -> Vec<Arg> {
        (0..12)
            .map(|k| Arg {
                src: x,
                view: view(vec![form(rank, &[(0, 1)], 0), form(rank, &[], k)]),
            })
            .collect()
    };
    let edges = |v: &[ExprId]| -> Vec<ExprId> { (0..9).map(|k| v[3 + k] - v[k % 3]).collect() };
    // grad[e, 3i+a]: node i ≥ 1 reads inv[a][i−1]; node 0 is −Σ of the
    // other three. A [12, 9] selector picks the combination per entry.
    let mut sel = vec![0.0; 12 * 9];
    for a in 0..3 {
        for i in 1..4 {
            sel[(3 * i + a) * 9 + 3 * a + (i - 1)] = 1.0;
        }
        for j in 0..3 {
            sel[a * 9 + 3 * a + j] = -1.0;
        }
    }
    let sel = input(&mut g, &[12, 9], sel);
    let mut args = coords(2);
    args.extend((0..9).map(|k| Arg {
        src: sel,
        view: view(vec![form(2, &[(1, 1)], 0), form(2, &[], k as i64)]),
    }));
    let grad = g
        .comp(&[ne, 12], &[], args, None, |v| {
            let m = edges(&v[..12]);
            let inv_det = ExprId::from_f64(1.0) / det3(&m);
            // selector column 3a + j ↔ inv[a][j]
            let acc = (0..9).fold(ExprId::from_f64(0.0), |acc, k| {
                acc + v[12 + k] * inv_entry(&m, inv_det, k / 3, k % 3)
            });
            vec![acc]
        })
        .expect("shape gradients");
    let vol = g
        .comp(
            &[ne],
            &[],
            {
                (0..12)
                    .map(|k| Arg {
                        src: x,
                        view: view(vec![form(1, &[(0, 1)], 0), form(1, &[], k)]),
                    })
                    .collect()
            },
            None,
            |v| vec![det3(&edges(v)) / ExprId::from_f64(6.0)],
        )
        .expect("volumes");

    // -- Element kernel: stiffness K_e[e, i, a, j, b] ------------------------
    // Iteration (e, i, a, j, b | c); grad read as G[e, 3·node + comp].
    let gv = |node: usize, comp: usize| Arg {
        src: grad,
        view: view(vec![
            form(6, &[(0, 1)], 0),
            form(6, &[(node, 3), (comp, 1)], 0),
        ]),
    };
    let delta = input(
        &mut g,
        &[3, 3],
        (0..9).map(|k| (k / 3 == k % 3) as u8 as f64).collect(),
    );
    let first = input(&mut g, &[3], vec![1.0, 0.0, 0.0]);
    let ke = g
        .comp(
            &[ne, 4, 3, 4, 3],
            &[3],
            vec![
                gv(1, 2), // g_ia
                gv(3, 4), // g_jb
                gv(1, 4), // g_ib
                gv(3, 2), // g_ja
                gv(1, 5), // g_ic
                gv(3, 5), // g_jc
                Arg {
                    src: delta,
                    view: view(vec![form(6, &[(2, 1)], 0), form(6, &[(4, 1)], 0)]),
                },
                Arg {
                    src: first,
                    view: view(vec![form(6, &[(5, 1)], 0)]),
                },
                Arg {
                    src: vol,
                    view: view(vec![form(6, &[(0, 1)], 0)]),
                },
            ],
            Some(Monoid::sum()),
            move |v| {
                let lam = ExprId::from_f64(lam);
                let mu = ExprId::from_f64(mu);
                // the λ and μ·g_ib·g_ja terms once (c = 0), the μ·δ·g_i·g_j
                // dot product summed over c
                vec![
                    v[8] * (v[7] * (lam * v[0] * v[1] + mu * v[2] * v[3])
                        + mu * v[6] * v[4] * v[5]),
                ]
            },
        )
        .expect("element stiffness");
    let ke_flat = g.reshape(ke, &[ne * 144]).expect("flatten");

    // -- Scatter-add into CSR -------------------------------------------------
    let fixed = &bc.fixed;
    let mut rows: Vec<Vec<(usize, Vec<usize>)>> = vec![Vec::new(); ndof];
    for (e, t) in mesh.tets.iter().enumerate() {
        for (i, &ni) in t.iter().enumerate() {
            for a in 0..3 {
                let r = 3 * ni as usize + a;
                if fixed[r] {
                    continue;
                }
                for (j, &nj) in t.iter().enumerate() {
                    for b in 0..3 {
                        let c = 3 * nj as usize + b;
                        if fixed[c] {
                            continue;
                        }
                        let k = e * 144 + (3 * i + a) * 12 + 3 * j + b;
                        match rows[r].iter_mut().find(|(cc, _)| *cc == c) {
                            Some((_, l)) => l.push(k),
                            None => rows[r].push((c, vec![k])),
                        }
                    }
                }
            }
        }
    }
    for (r, row) in rows.iter_mut().enumerate() {
        if fixed[r] || row.is_empty() {
            row.push((r, Vec::new())); // unit diagonal, added below
        }
        row.sort_by_key(|(c, _)| *c);
    }
    let mut row_ptrs = vec![0usize];
    let mut cols = Vec::new();
    for row in &rows {
        cols.extend(row.iter().map(|(c, _)| *c));
        row_ptrs.push(cols.len());
    }
    let nnz = cols.len();
    let width = rows
        .iter()
        .flatten()
        .map(|(_, l)| l.len())
        .max()
        .unwrap_or(0)
        .max(1);
    let mut w = vec![0.0; nnz * width];
    let mut ix = vec![0.0; nnz * width];
    let mut unit = vec![0.0; nnz];
    for (s, (_, l)) in rows.iter().flatten().enumerate() {
        if l.is_empty() {
            unit[s] = 1.0;
        }
        for (c, &k) in l.iter().enumerate() {
            w[s * width + c] = 1.0;
            ix[s * width + c] = k as f64;
        }
    }
    let w = input(&mut g, &[nnz, width], w);
    let ix = input(&mut g, &[nnz, width], ix);
    let unit = input(&mut g, &[nnz], unit);
    let summed = spmv(&mut g, w, ix, ke_flat).expect("scatter-add");
    let vals = g
        .map(&[summed, unit], |v| v[0] + v[1])
        .expect("unit diagonal");

    Ok(DeviceAssembly {
        graph: g,
        inputs,
        output: vals,
        row_ptrs,
        cols,
        fixed: bc.fixed,
        rhs: bc.f,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{box_mesh, tet_fill};
    use crate::solve::{matvec, pcg};
    use crate::spec::{Load, RegionBox, Support};

    fn bar(res: usize) -> (TetMesh, FeaSpec) {
        let spec = FeaSpec {
            resolution: res,
            youngs_modulus_mpa: 69_000.0,
            poisson: 0.33,
            yield_strength_mpa: None,
            loads: vec![Load {
                region: RegionBox {
                    min: [80.0, 0.0, 0.0],
                    max: [80.0, 10.0, 10.0],
                },
                force: [200.0, 0.0, -1000.0],
            }],
            supports: vec![Support {
                region: RegionBox {
                    min: [0.0, 0.0, 0.0],
                    max: [0.0, 10.0, 10.0],
                },
                fix: [true, true, true],
            }],
        };
        let mesh = tet_fill(&box_mesh([0.0; 3], [80.0, 10.0, 10.0]), res).unwrap();
        (mesh, spec)
    }

    fn rel(a: &[f64], b: &[f64]) -> f64 {
        let scale = b.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max)
            / scale
    }

    /// T2 gate (FEA): the assembled matrix is the matrix-free operator,
    /// and its solution is the solver's displacement.
    #[test]
    fn assembled_matches_matrix_free() {
        let (mesh, spec) = bar(16);
        let dev = device_assembly(&mesh, &spec).unwrap();
        let vals = dev.eval_cpu();

        // K·x against the matrix-free product, x zero on fixed dofs
        let n = dev.rhs.len();
        let x: Vec<f64> = (0..n)
            .map(|i| {
                if dev.fixed[i] {
                    0.0
                } else {
                    ((i * 7919) % 101) as f64 / 50.0 - 1.0
                }
            })
            .collect();
        let mut y = vec![0.0; n];
        dev.apply(&vals, &x, &mut y);
        let elems = build_elements(&mesh).unwrap();
        let (lam, mu) = lame(spec.poisson);
        let mut y_ref = vec![0.0; n];
        matvec(&elems, lam, mu, &dev.fixed, &x, &mut y_ref);
        // fixed rows: unit diagonal × 0 = 0, matrix-free zeroes them
        let e = rel(&y, &y_ref);
        println!("K·x vs matrix-free: {e:.3e}");
        assert!(e < 1e-13, "{e:.3e}");

        let tight = SolveOptions {
            tol: 1e-13,
            max_iters: 200_000,
        };
        let u = dev.solve(&vals, &tight).unwrap();
        let (u_ref, _, _) = pcg(&elems, lam, mu, &dev.fixed, &dev.rhs, &tight).unwrap();
        let e = rel(&u, &u_ref);
        println!("u vs matrix-free solve: {e:.3e}");
        assert!(e < 1e-10, "{e:.3e}");
    }
}
