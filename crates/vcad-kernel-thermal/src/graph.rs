//! Steady conduction as a tang tensor graph: Assemble → LinearSolve → Reduce.
//!
//! The parameters the adjoint in [`crate::adjoint`] differentiates —
//! source powers, per-material per-axis conductivity, per-slot film
//! coefficients and ambients — are graph inputs. Everything from them to
//! the smoothed peak temperature is one `tang_compile` tensor graph:
//!
//! 1. **Assemble.** Every conductance link (internal voxel pair, Dirichlet
//!    face, convection face) gets `G = area / (½d/k_a + ½d/k_b + 1/h)`, its
//!    conductivities and film gathered from the parameter vectors. The
//!    free-voxel CSR values are `S·G` for the ±1 link incidence `S`, and
//!    the right-hand side is the per-voxel source share plus `G·T_ref` of
//!    every link into a reference, likewise incidence products. Gathers
//!    (`IndexedGather`) and incidence products (`SparseMatVec`) are
//!    operator nodes whose adjoints are a scatter-add and `Sᵀ·ȳ`: linear
//!    time.
//! 2. **LinearSolve.** One operator node: Jacobi PCG forward, and the
//!    implicit-function adjoint (a transposed solve plus `−λ⊗T` on the
//!    pattern) backward.
//! 3. **Reduce.** `J = T_ref + (Σ_v (T_v − T_ref)₊ᵖ)^(1/p)`.
//!
//! `TensorGraph::backward` derives the whole gradient; nothing in this
//! file is a hand chain rule. [`solve_steady`](crate::solve::solve_steady)
//! runs the forward half, and the tests check the derived gradient against
//! the hand adjoint.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tang::Scalar;
use tang_compile::tensor::sparse::{
    spmv, IndexedGather, LinearSolve, Pattern, Precond, SolveRecord, SparseMatVec,
};
use tang_compile::tensor::{Monoid, TensorGraph, TensorId};
use tang_expr::ExprId;
use tang_sparse::{CooMatrix, PcgOptions, PcgStatus};

use crate::model::{Boundary, ThermalModel};
use crate::solve::{LinkKind, SolveError, SolveOptions, System, EXPOSED_SLOT};

/// The derived gradient of the smoothed peak temperature, in the same
/// parameter layout as [`crate::adjoint::SmoothMaxGradient`].
#[derive(Debug, Clone, PartialEq)]
pub struct GraphGradient {
    /// Smoothed peak temperature J, °C.
    pub value_c: f64,
    /// dJ/d(power) per source, K/W.
    pub d_source_power: Vec<f64>,
    /// dJ/d(k_axis) per material region.
    pub d_conductivity: Vec<[f64; 3]>,
    /// dJ/dh per BC slot (`None` where the slot is not convection).
    pub d_film: [Option<f64>; 7],
    /// dJ/d(ambient) per BC slot (`None` where the slot is not convection).
    pub d_ambient: [Option<f64>; 7],
}

/// Where the time of one graph evaluation went.
#[derive(Debug, Clone)]
pub struct GraphTiming {
    /// Wall time of the whole evaluation (graph build excluded).
    pub total: Duration,
    /// Every linear solve the evaluation ran (forward, then adjoint).
    pub solves: Vec<SolveRecord>,
}

impl GraphTiming {
    /// Total time inside the solves.
    pub fn solve_time(&self) -> Duration {
        self.solves.iter().map(|r| r.time).sum()
    }

    /// Everything that is not a solve: assembly, reductions, the adjoint
    /// comprehensions and the interpreter itself.
    pub fn overhead(&self) -> Duration {
        self.total.saturating_sub(self.solve_time())
    }
}

/// Graph inputs, pushed alongside their data.
struct Inputs {
    g: TensorGraph,
    data: Vec<Vec<f64>>,
    lowering: Lowering,
}

/// How the assembly's gathers and incidence products are expressed.
///
/// Both lowerings build the same values in the same summation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lowering {
    /// `IndexedGather` / `SparseMatVec` operator nodes: native forward,
    /// linear-time adjoints (scatter-add, `Sᵀ·ȳ`). The gradient path.
    Operator,
    /// Gather-view comprehensions (ELL `spmv`: a gather view feeding a sum
    /// monoid). Lowers to every tensor backend, so the assembly runs on
    /// the GPU; its adjoint through the gather is a dense gated sum, so
    /// it is the forward/device path only.
    Comprehension,
}

impl Inputs {
    fn add(&mut self, shape: &[usize], data: Vec<f64>) -> TensorId {
        debug_assert_eq!(shape.iter().product::<usize>(), data.len());
        self.data.push(data);
        self.g.input(shape)
    }

    /// `src[index]` over a flat `index` (−1 reads 0).
    fn gather(&mut self, src: TensorId, index: &[i64]) -> TensorId {
        let m = index.len();
        match self.lowering {
            Lowering::Operator => {
                let ix = self.add(&[m], index.iter().map(|&i| i as f64).collect());
                IndexedGather::apply(&mut self.g, src, ix).expect("gather shapes")
            }
            Lowering::Comprehension => {
                // one-wide ELL: weight 0 (and a harmless index 0) for −1
                let w = self.add(
                    &[m, 1],
                    index.iter().map(|&i| (i >= 0) as u8 as f64).collect(),
                );
                let ix = self.add(&[m, 1], index.iter().map(|&i| i.max(0) as f64).collect());
                spmv(&mut self.g, w, ix, src).expect("gather shapes")
            }
        }
    }

    /// `S·x` for the `n × m` matrix `S` given as `(row, col, value)`
    /// entries (summed in entry order within a row).
    fn incidence(
        &mut self,
        n: usize,
        m: usize,
        entries: &mut dyn Iterator<Item = (usize, usize, f64)>,
        x: TensorId,
    ) -> TensorId {
        match self.lowering {
            Lowering::Operator => {
                let mut c = CooMatrix::new(n, m);
                for (r, k, v) in entries {
                    c.push(r, k, v);
                }
                SparseMatVec::new(c.to_csr())
                    .apply(&mut self.g, x)
                    .expect("incidence shapes")
            }
            Lowering::Comprehension => {
                let mut rows: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
                for (r, k, v) in entries {
                    rows[r].push((k, v));
                }
                let width = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
                let mut w = vec![0.0; n * width];
                let mut ix = vec![0.0; n * width];
                for (r, row) in rows.iter().enumerate() {
                    for (c, &(k, v)) in row.iter().enumerate() {
                        w[r * width + c] = v;
                        ix[r * width + c] = k as f64;
                    }
                }
                let w = self.add(&[n, width], w);
                let ix = self.add(&[n, width], ix);
                spmv(&mut self.g, w, ix, x).expect("incidence shapes")
            }
        }
    }
}

/// One CSR entry under assembly: its column and the `(link, ±1)` terms
/// that sum to its value.
type RowEntry = (usize, Vec<(usize, f64)>);

/// One conductance link.
struct Link {
    /// Free-voxel (compact) endpoints; `None` for a reference side.
    a: Option<usize>,
    b: Option<usize>,
    /// Indices into the flattened `3·material + axis` conductivity vector.
    ka: i64,
    kb: i64,
    /// Convection slot, or −1.
    slot: i64,
    half: f64,
    area: f64,
    /// Fixed reference temperature (Dirichlet faces, reservoir pairs).
    t_const: f64,
}

/// A built steady graph and the handles to evaluate it.
pub(crate) struct SteadyGraph {
    ins: Inputs,
    /// Compact free index → voxel.
    pub(crate) free_voxels: Vec<usize>,
    t: TensorId,
    j: Option<TensorId>,
    grads: Option<[TensorId; 4]>,
    solve: Arc<LinearSolve>,
}

fn conv_slots(model: &ThermalModel) -> [Option<(f64, f64)>; 7] {
    let mut out = [None; 7];
    for (slot, o) in out.iter_mut().enumerate() {
        let bc = if slot == EXPOSED_SLOT {
            model.exposed
        } else {
            model.domain_faces[slot]
        };
        if let Boundary::Convection { h_w_m2k, ambient_c } = bc {
            *o = Some((h_w_m2k, ambient_c));
        }
    }
    out
}

/// Links, parameters and the Assemble stage.
fn assemble(model: &ThermalModel, sys: &System, lowering: Lowering) -> Assembled {
    let [nx, ny, nz] = sys.n;
    let nvox = nx * ny * nz;
    let idx = |i: usize, j: usize, k: usize| (k * ny + j) * nx + i;
    let mut compact = vec![usize::MAX; nvox];
    let free_voxels: Vec<usize> = (0..nvox).filter(|&p| sys.free[p]).collect();
    for (f, &p) in free_voxels.iter().enumerate() {
        compact[p] = f;
    }
    let nf = free_voxels.len();
    let cf = |p: usize| sys.free[p].then(|| compact[p]);
    let kidx = |p: usize, axis: usize| (3 * sys.mat_id[p] + axis) as i64;

    // -- links ------------------------------------------------------------
    let mut links: Vec<Link> = Vec::new();
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let p = idx(i, j, k);
                if !sys.solid[p] {
                    continue;
                }
                for axis in 0..3 {
                    let (ii, jj, kk) = match axis {
                        0 => (i + 1, j, k),
                        1 => (i, j + 1, k),
                        _ => (i, j, k + 1),
                    };
                    if ii >= nx || jj >= ny || kk >= nz {
                        continue;
                    }
                    let q = idx(ii, jj, kk);
                    if !sys.solid[q] || (!sys.free[p] && !sys.free[q]) {
                        continue;
                    }
                    let t_const = if sys.free[p] {
                        sys.tfix[q]
                    } else {
                        sys.tfix[p]
                    };
                    links.push(Link {
                        a: cf(p),
                        b: cf(q),
                        ka: kidx(p, axis),
                        kb: kidx(q, axis),
                        slot: -1,
                        half: 0.5 * sys.d_m[axis],
                        area: sys.area[axis],
                        t_const: if sys.free[p] && sys.free[q] {
                            0.0
                        } else {
                            t_const
                        },
                    });
                }
            }
        }
    }
    for l in &sys.links {
        let (slot, t_const) = match l.kind {
            LinkKind::FixedRegion(_) => continue, // a voxel pair, above
            LinkKind::FixedFace => (-1, l.t_ref),
            LinkKind::Convection(slot) => (slot as i64, 0.0),
        };
        links.push(Link {
            a: cf(l.voxel),
            b: None,
            ka: kidx(l.voxel, l.axis),
            kb: -1,
            slot,
            half: 0.5 * sys.d_m[l.axis],
            area: sys.area[l.axis],
            t_const,
        });
    }
    let ne = links.len();

    // -- parameters ---------------------------------------------------------
    let slots = conv_slots(model);
    let mut ins = Inputs {
        g: TensorGraph::new(),
        data: Vec::new(),
        lowering,
    };
    let ns = model.sources.len();
    let nk = 3 * model.materials.len();
    let p_in = ins.add(&[ns.max(1)], {
        let mut v: Vec<f64> = model.sources.iter().map(|s| s.power_w).collect();
        v.resize(ns.max(1), 0.0);
        v
    });
    let k_in = ins.add(
        &[nk],
        model.materials.iter().flat_map(|m| m.k_w_mk).collect(),
    );
    let h_in = ins.add(&[7], slots.iter().map(|s| s.map_or(0.0, |s| s.0)).collect());
    let ta_in = ins.add(&[7], slots.iter().map(|s| s.map_or(0.0, |s| s.1)).collect());

    // -- Assemble: link conductances ----------------------------------------
    let ka = ins.gather(k_in, &links.iter().map(|l| l.ka).collect::<Vec<_>>());
    let kb = ins.gather(k_in, &links.iter().map(|l| l.kb).collect::<Vec<_>>());
    let sl: Vec<i64> = links.iter().map(|l| l.slot).collect();
    let hh = ins.gather(h_in, &sl);
    let ta = ins.gather(ta_in, &sl);
    let half = ins.add(&[ne], links.iter().map(|l| l.half).collect());
    let has_b = ins.add(
        &[ne],
        links.iter().map(|l| (l.kb >= 0) as u8 as f64).collect(),
    );
    let has_h = ins.add(
        &[ne],
        links.iter().map(|l| (l.slot >= 0) as u8 as f64).collect(),
    );
    let area = ins.add(&[ne], links.iter().map(|l| l.area).collect());
    let g_link = ins
        .g
        .map(&[ka, kb, hh, half, has_b, has_h, area], |v| {
            let zero = ExprId::from_f64(0.0);
            let one = ExprId::from_f64(1.0);
            let ra = v[3] / v[0];
            let rb = <ExprId as Scalar>::select(v[4], v[3] / v[1], zero);
            let rh = <ExprId as Scalar>::select(v[5], one / v[2], zero);
            v[6] / (ra + rb + rh)
        })
        .expect("conductance map");
    // G·T_ref for every link into a reference (0 for free–free pairs)
    let tconst = ins.add(&[ne], links.iter().map(|l| l.t_const).collect());
    let to_ref = ins.add(
        &[ne],
        links
            .iter()
            .map(|l| (l.a.is_none() || l.b.is_none()) as u8 as f64)
            .collect(),
    );
    let q_link = ins
        .g
        .map(&[g_link, ta, tconst, to_ref], |v| {
            v[0] * (v[1] + v[2]) * v[3]
        })
        .expect("reference flux map");

    // -- Assemble: CSR values ------------------------------------------------
    // per free row: (col, [(link, ±1)])
    let mut rows: Vec<Vec<RowEntry>> = vec![Vec::new(); nf];
    let mut add = |r: usize, c: usize, e: usize, s: f64| {
        let row = &mut rows[r];
        match row.iter_mut().find(|(cc, _)| *cc == c) {
            Some((_, l)) => l.push((e, s)),
            None => row.push((c, vec![(e, s)])),
        }
    };
    let mut b_links: Vec<Vec<usize>> = vec![Vec::new(); nf];
    for (e, l) in links.iter().enumerate() {
        match (l.a, l.b) {
            (Some(a), Some(b)) => {
                add(a, a, e, 1.0);
                add(b, b, e, 1.0);
                add(a, b, e, -1.0);
                add(b, a, e, -1.0);
            }
            (Some(a), None) | (None, Some(a)) => {
                add(a, a, e, 1.0);
                b_links[a].push(e);
            }
            (None, None) => unreachable!("link with no free end"),
        }
    }
    for r in &mut rows {
        r.sort_by_key(|(c, _)| *c);
    }
    let mut row_ptrs = vec![0usize];
    let mut cols = Vec::new();
    for r in &rows {
        cols.extend(r.iter().map(|(c, _)| *c));
        row_ptrs.push(cols.len());
    }
    let nnz = cols.len();
    // values = S·G, S the (nnz × links) incidence with ±1 entries
    let vals = ins.incidence(
        nnz,
        ne,
        &mut rows
            .iter()
            .flatten()
            .enumerate()
            .flat_map(|(s, (_, l))| l.iter().map(move |&(e, c)| (s, e, c))),
        g_link,
    );

    // -- Assemble: right-hand side ------------------------------------------
    // b = Σ_links-into-a-reference G·T_ref + Σ_sources P/N over covered voxels
    let b_ref = ins.incidence(
        nf,
        ne,
        &mut b_links
            .iter()
            .enumerate()
            .flat_map(|(f, l)| l.iter().map(move |&e| (f, e, 1.0))),
        q_link,
    );
    let cref = &compact;
    let b_src = ins.incidence(
        nf,
        ns.max(1),
        &mut sys.sources.iter().enumerate().flat_map(|(s, (_, _, ids))| {
            let share = 1.0 / ids.len() as f64;
            ids.iter().map(move |&p| (cref[p], s, share))
        }),
        p_in,
    );
    let b = ins.g.map(&[b_src, b_ref], |v| v[0] + v[1]).expect("rhs");

    Assembled {
        ins,
        free_voxels,
        pattern: Pattern {
            n: nf,
            row_ptrs,
            cols,
        },
        vals,
        b,
        params: [p_in, k_in, h_in, ta_in],
    }
}

/// The Assemble stage: CSR values and right-hand side as graph nodes over
/// the parameter inputs.
struct Assembled {
    ins: Inputs,
    free_voxels: Vec<usize>,
    pattern: Pattern,
    vals: TensorId,
    b: TensorId,
    /// Power, conductivity, film and ambient inputs.
    params: [TensorId; 4],
}

/// Build the graph. With `objective = Some((reference_c, p))` it also
/// carries J and its gradient with respect to the four parameter vectors.
pub(crate) fn build_graph(
    model: &ThermalModel,
    sys: &System,
    opts: &SolveOptions,
    objective: Option<(f64, f64)>,
) -> SteadyGraph {
    build_graph_with(model, sys, opts, objective, Lowering::Operator)
}

pub(crate) fn build_graph_with(
    model: &ThermalModel,
    sys: &System,
    opts: &SolveOptions,
    objective: Option<(f64, f64)>,
    lowering: Lowering,
) -> SteadyGraph {
    let Assembled {
        mut ins,
        free_voxels,
        pattern,
        vals,
        b,
        params: [p_in, k_in, h_in, ta_in],
    } = assemble(model, sys, lowering);
    let nnz = pattern.cols.len();

    // -- LinearSolve ---------------------------------------------------------
    let pattern = Arc::new(pattern);
    let rows_in = ins.add(&[nnz], pattern.rows().iter().map(|&r| r as f64).collect());
    let cols_in = ins.add(&[nnz], pattern.cols.iter().map(|&c| c as f64).collect());
    let solve = Arc::new(LinearSolve::new(
        pattern,
        Precond::Jacobi,
        PcgOptions {
            tol: opts.tol,
            max_iters: opts.max_iters,
        },
    ));
    let t = solve
        .apply(&mut ins.g, vals, b, rows_in, cols_in)
        .expect("solve shapes");

    // -- Reduce --------------------------------------------------------------
    let (j, grads) = match objective {
        None => (None, None),
        Some((reference_c, p)) => {
            let pw = ins
                .g
                .map(&[t], |v| {
                    let x = v[0] - ExprId::from_f64(reference_c);
                    let safe = <ExprId as Scalar>::select(x, x, ExprId::from_f64(1.0));
                    let xp = (ExprId::from_f64(p) * safe.ln()).exp();
                    <ExprId as Scalar>::select(x, xp, ExprId::from_f64(0.0))
                })
                .expect("excess power");
            let s = ins.g.reduce(pw, 0, Monoid::sum()).expect("sum");
            let j = ins
                .g
                .map(&[s], |v| {
                    ExprId::from_f64(reference_c) + (v[0].ln() * ExprId::from_f64(1.0 / p)).exp()
                })
                .expect("root");
            let gs = ins
                .g
                .backward(j, &[p_in, k_in, h_in, ta_in])
                .expect("backward");
            (Some(j), Some([gs[0], gs[1], gs[2], gs[3]]))
        }
    };

    // Fuse: every single-use map is inlined into its readers, so the
    // interpreter makes one pass where the unfused graph made several.
    let mut outs = vec![t];
    outs.extend(j);
    if let Some(gs) = grads {
        outs.extend(gs);
    }
    let (fused, outs) = ins.g.substitute_maps(&outs);
    ins.g = fused;
    let t = outs[0];
    let j = j.map(|_| outs[1]);
    let grads = grads.map(|_| [outs[2], outs[3], outs[4], outs[5]]);

    SteadyGraph {
        ins,
        free_voxels,
        t,
        j,
        grads,
        solve,
    }
}

/// What one evaluation produced.
pub(crate) struct Evaluated {
    /// Temperature per free voxel (compact order).
    pub(crate) t_free: Vec<f64>,
    pub(crate) j: Option<f64>,
    pub(crate) grads: Option<[Vec<f64>; 4]>,
    pub(crate) timing: GraphTiming,
}

impl SteadyGraph {
    /// Evaluate on the interpreter; fails closed when a solve did not
    /// converge.
    pub(crate) fn eval(&self) -> Result<Evaluated, SolveError> {
        let before = self.solve.records().len();
        let mut outs = vec![self.t];
        outs.extend(self.j);
        if let Some(g) = self.grads {
            outs.extend(g);
        }
        let t0 = Instant::now();
        let mut o = self.ins.g.eval(&self.ins.data, &outs);
        let total = t0.elapsed();
        let solves: Vec<SolveRecord> = self.solve.records()[before..].to_vec();
        if let Some(bad) = solves.iter().find(|r| r.status != PcgStatus::Converged) {
            return Err(SolveError::NotConverged {
                residual_rel: bad.residual_rel,
                iterations: bad.iterations,
            });
        }
        let grads = self.grads.map(|_| {
            let g3 = o.pop().unwrap();
            let g2 = o.pop().unwrap();
            let g1 = o.pop().unwrap();
            let g0 = o.pop().unwrap();
            [g0, g1, g2, g3]
        });
        let j = self.j.map(|_| o.pop().unwrap()[0]);
        Ok(Evaluated {
            t_free: o.pop().unwrap(),
            j,
            grads,
            timing: GraphTiming { total, solves },
        })
    }

    /// The forward solve's (iterations, residual) from the last [`eval`].
    pub(crate) fn last_forward(&self) -> (usize, f64) {
        let r = self.solve.records();
        let f = r
            .iter()
            .rev()
            .find(|r| !r.transpose)
            .expect("a forward solve");
        (f.iterations, f.residual_rel)
    }
}

/// The Assemble stage alone, as gather-view comprehensions: a forward
/// graph that lowers to every tang tensor backend (CUDA, Metal, WGSL), so
/// the assembly can run on a GPU while the solve stays on the CPU in f64.
pub struct DeviceAssembly {
    /// Parameters → (CSR values, right-hand side). No operator nodes.
    pub graph: TensorGraph,
    /// One buffer per graph input, in input order.
    pub inputs: Vec<Vec<f64>>,
    /// `[values (nnz), rhs (free voxels)]`.
    pub outputs: [TensorId; 2],
    /// Free-voxel CSR row pointers (`n + 1`).
    pub row_ptrs: Vec<usize>,
    /// Free-voxel CSR columns (`nnz`).
    pub cols: Vec<usize>,
    /// Compact free index → voxel.
    pub free_voxels: Vec<usize>,
}

impl DeviceAssembly {
    /// Evaluate on the reference interpreter (f64).
    pub fn eval_cpu(&self) -> (Vec<f64>, Vec<f64>) {
        let mut o = self.graph.eval(&self.inputs, &self.outputs);
        let b = o.pop().unwrap();
        (o.pop().unwrap(), b)
    }

    /// Jacobi-PCG on assembled values, as the steady solver runs it:
    /// temperature per free voxel (compact order).
    pub fn solve(
        &self,
        vals: &[f64],
        b: &[f64],
        opts: &SolveOptions,
    ) -> Result<Vec<f64>, SolveError> {
        let n = self.free_voxels.len();
        let row = |r: usize| self.row_ptrs[r]..self.row_ptrs[r + 1];
        let csr = |x: &[f64], y: &mut [f64]| {
            for (r, yr) in y.iter_mut().enumerate() {
                *yr = row(r).map(|k| vals[k] * x[self.cols[k]]).sum();
            }
        };
        let diag: Vec<f64> = (0..n)
            .map(|r| row(r).find(|&k| self.cols[k] == r).map_or(1.0, |k| vals[k]))
            .collect();
        let sol = tang_sparse::pcg(
            csr,
            &tang_sparse::Jacobi::new(diag),
            b,
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

/// Build the device assembly graph for a model.
pub fn device_assembly(model: &ThermalModel) -> Result<DeviceAssembly, SolveError> {
    let sys = crate::solve::build(model)?;
    let a = assemble(model, &sys, Lowering::Comprehension);
    Ok(DeviceAssembly {
        graph: a.ins.g,
        inputs: a.ins.data,
        outputs: [a.vals, a.b],
        row_ptrs: a.pattern.row_ptrs,
        cols: a.pattern.cols,
        free_voxels: a.free_voxels,
    })
}

/// The smoothed-peak gradient derived by the graph, plus where the time
/// went. Same objective, reference resolution and parameter layout as
/// [`crate::adjoint::smooth_max_gradient`].
pub fn smooth_max_gradient_graph(
    model: &ThermalModel,
    opts: &SolveOptions,
    oopts: &crate::adjoint::ObjectiveOptions,
) -> Result<(GraphGradient, GraphTiming), SolveError> {
    if !oopts.p.is_finite() || oopts.p <= 1.0 {
        return Err(SolveError::InvalidSmoothingExponent);
    }
    let sys = crate::solve::build(model)?;
    let model_ref = crate::solve::resolve_reference(&sys, model)?;
    let reference_c = oopts
        .reference_c
        .or(model_ref)
        .ok_or(SolveError::AmbiguousReference)?;
    let g = build_graph(model, &sys, opts, Some((reference_c, oopts.p)));
    let e = g.eval()?;
    let [dp, dk, dh, dta] = e.grads.unwrap();
    let slots = conv_slots(model);
    let per_slot = |v: &[f64]| {
        let mut out = [None; 7];
        for (s, o) in out.iter_mut().enumerate() {
            if slots[s].is_some() {
                *o = Some(v[s]);
            }
        }
        out
    };
    Ok((
        GraphGradient {
            value_c: e.j.unwrap(),
            d_source_power: dp[..model.sources.len()].to_vec(),
            d_conductivity: dk.chunks(3).map(|c| [c[0], c[1], c[2]]).collect(),
            d_film: per_slot(&dh),
            d_ambient: per_slot(&dta),
        },
        e.timing,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjoint::{smooth_max_gradient, ObjectiveOptions};
    use crate::model::{FixedTemperature, MaterialRegion, PowerSource, Shape};

    /// The adjoint tests' two-material board (spreader in a plate, one
    /// source, two convection faces, a cold reservoir strip), at a chosen
    /// resolution.
    fn board(div: [usize; 3]) -> ThermalModel {
        let mut m = ThermalModel::new([0.0, 0.0, 0.0], [40.0, 40.0, 4.0], div);
        m.materials.push(MaterialRegion::anisotropic(
            Shape::Box {
                min_mm: [0.0, 0.0, 0.0],
                size_mm: [40.0, 40.0, 4.0],
            },
            [2.0, 2.0, 0.8],
        ));
        m.materials.push(MaterialRegion::isotropic(
            Shape::Box {
                min_mm: [12.0, 12.0, 0.0],
                size_mm: [16.0, 16.0, 4.0],
            },
            160.0,
        ));
        m.sources.push(PowerSource {
            name: "die".into(),
            shape: Shape::Box {
                min_mm: [16.0, 16.0, 0.0],
                size_mm: [8.0, 8.0, 4.0],
            },
            power_w: 3.0,
        });
        m.sources.push(PowerSource {
            name: "regulator".into(),
            shape: Shape::Box {
                min_mm: [30.0, 4.0, 0.0],
                size_mm: [6.0, 6.0, 4.0],
            },
            power_w: 0.7,
        });
        m.fixed.push(FixedTemperature {
            shape: Shape::Box {
                min_mm: [0.0, 0.0, 0.0],
                size_mm: [4.0, 40.0, 4.0],
            },
            temperature_c: 20.0,
        });
        m.domain_faces[1] = Boundary::FixedTemperature {
            temperature_c: 30.0,
        };
        m.domain_faces[4] = Boundary::Convection {
            h_w_m2k: 12.0,
            ambient_c: 25.0,
        };
        m.domain_faces[5] = Boundary::Convection {
            h_w_m2k: 8.0,
            ambient_c: 24.0,
        };
        m.reference_c = Some(25.0);
        m
    }

    fn tight() -> SolveOptions {
        SolveOptions {
            tol: 1e-13,
            max_iters: 200_000,
        }
    }

    /// Largest relative difference over a parameter vector, measured
    /// against that vector's largest entry.
    fn rel(a: &[f64], b: &[f64]) -> f64 {
        let scale = b.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max)
            / scale
    }

    /// The T1 gate: the derived gradient equals the hand adjoint to
    /// ≤ 1e-12 relative per parameter group, in f64.
    #[test]
    fn graph_gradient_matches_hand_adjoint() {
        let m = board([20, 20, 2]);
        let oo = ObjectiveOptions::default();
        let (_, hand) = smooth_max_gradient(&m, &tight(), &oo).unwrap();
        let (graph, timing) = smooth_max_gradient_graph(&m, &tight(), &oo).unwrap();
        let flat = |v: &[[f64; 3]]| v.iter().flatten().copied().collect::<Vec<_>>();
        let slot = |v: &[Option<f64>; 7]| v.iter().flatten().copied().collect::<Vec<_>>();
        let r = [
            ("value", rel(&[graph.value_c], &[hand.value_c])),
            ("power", rel(&graph.d_source_power, &hand.d_source_power)),
            (
                "conductivity",
                rel(&flat(&graph.d_conductivity), &flat(&hand.d_conductivity)),
            ),
            ("film", rel(&slot(&graph.d_film), &slot(&hand.d_film))),
            (
                "ambient",
                rel(&slot(&graph.d_ambient), &slot(&hand.d_ambient)),
            ),
        ];
        println!("graph vs hand adjoint (max rel per group): {r:?}");
        println!(
            "solves {:?}, total {:?}, overhead {:?}",
            timing.solve_time(),
            timing.total,
            timing.overhead()
        );
        for (name, e) in r {
            assert!(e <= 1e-12, "{name}: {e:.3e}");
        }
        assert_eq!(graph.d_film[0], None);
    }

    /// The forward half is the steady solver: same field as the hand path.
    #[test]
    fn graph_forward_matches_matrix_free_pcg() {
        let m = board([16, 16, 2]);
        let sys = crate::solve::build(&m).unwrap();
        let g = build_graph(&m, &sys, &tight(), None);
        let e = g.eval().unwrap();
        let zeros = vec![0.0; sys.b.len()];
        let (t, _, _) = crate::solve::pcg(&sys, &sys.diag, &sys.b, &zeros, &tight()).unwrap();
        let hand: Vec<f64> = g.free_voxels.iter().map(|&p| t[p]).collect();
        assert!(
            rel(&e.t_free, &hand) < 1e-12,
            "{:.3e}",
            rel(&e.t_free, &hand)
        );
    }

    /// T2: the comprehension lowering (what runs on a GPU) assembles the
    /// same system as the operator lowering, and solving it gives the
    /// steady solver's field.
    #[test]
    fn comprehension_assembly_matches_operator_assembly() {
        let m = board([16, 16, 2]);
        let sys = crate::solve::build(&m).unwrap();
        let op = assemble(&m, &sys, Lowering::Operator);
        let mut o = op.ins.g.eval(&op.ins.data, &[op.vals, op.b]);
        let (op_b, op_vals) = (o.pop().unwrap(), o.pop().unwrap());
        let dev = device_assembly(&m).unwrap();
        let (vals, b) = dev.eval_cpu();
        assert_eq!(dev.cols, op.pattern.cols);
        assert!(
            rel(&vals, &op_vals) <= 1e-15,
            "{:.3e}",
            rel(&vals, &op_vals)
        );
        assert!(rel(&b, &op_b) <= 1e-15, "{:.3e}", rel(&b, &op_b));

        let t = dev.solve(&vals, &b, &tight()).unwrap();
        let zeros = vec![0.0; sys.b.len()];
        let (hand, _, _) = crate::solve::pcg(&sys, &sys.diag, &sys.b, &zeros, &tight()).unwrap();
        let hand: Vec<f64> = dev.free_voxels.iter().map(|&p| hand[p]).collect();
        assert!(rel(&t, &hand) < 1e-12, "{:.3e}", rel(&t, &hand));

        // and the whole forward graph under the comprehension lowering
        let g = build_graph_with(&m, &sys, &tight(), None, Lowering::Comprehension);
        let e = g.eval().unwrap();
        assert!(
            rel(&e.t_free, &hand) < 1e-12,
            "{:.3e}",
            rel(&e.t_free, &hand)
        );
    }

    /// Overhead measurement on a larger board (run with --ignored
    /// --nocapture in release): graph time outside the two solves, as a
    /// fraction of the solve time.
    #[test]
    #[ignore]
    fn graph_overhead() {
        let oo = ObjectiveOptions::default();
        let opts = SolveOptions::default();
        for div in [[40, 40, 4], [80, 80, 4], [64, 64, 16]] {
            let m = board(div);
            let t0 = Instant::now();
            let (_, hand) = smooth_max_gradient(&m, &opts, &oo).unwrap();
            let hand_t = t0.elapsed();
            let (graph, timing) = smooth_max_gradient_graph(&m, &opts, &oo).unwrap();
            let solve = timing.solve_time();
            println!(
                "{div:?}: hand {hand_t:?} | graph total {:?} = solves {solve:?} + overhead {:?} ({:.1}% of solve) | dP rel {:.2e}",
                timing.total,
                timing.overhead(),
                100.0 * timing.overhead().as_secs_f64() / solve.as_secs_f64(),
                rel(&graph.d_source_power, &hand.d_source_power)
            );
        }
    }
}
