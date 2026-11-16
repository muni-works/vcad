# particle optics in 3D: real globe grids

`vcad-kernel-particle` used to describe a device as a grounded cylinder plus
coaxial **latitude rings**, solved by an axisymmetric Laplace SOR. A real
fusor cathode is a globe: great circles, or latitude rings plus meridians.
That can't be written as rings. So either you built an odd ring stack to
match the sim, or the sim stood in for the machine you built (blocker #3a in
`docs/videos/open-fusor.md`).

This adds non-axisymmetric electrodes end to end: geometry, a 3D field solve,
tracing, a serde schema, and the wasm/MCP tool.

## what's new

| piece | where | what |
|---|---|---|
| geometry | `src/wire3d.rs` | `Device3d`: wires along circles, arcs and polylines, each with its own radius, potential and (closed loops only) current. Chamber is a grounded **sphere** (or unbounded). Constructors: `great_circle_globe` (explicit normals or `GreatCircleLayout::{Orthogonal3, Cuboctahedral4, Icosidodecahedral6}`), `lat_long_globe`, `from_rings` (the old devices). Fail-closed `validate()`. `geometric_transparency()` counts radial shadows on a Fibonacci sphere |
| field solve | `src/bem.rs` | thin-wire boundary-element (charge-simulation) method. `solve(&Device3d, &BemOptions) -> WireField` |
| tracing | `src/field.rs`, `src/trace.rs` | `trait FieldSampler` (implemented by the 2D `FieldMap` and the 3D `WireField`), `trait TraceGeometry` (hit test, wall, near-wire step ratio, CX exit distance). `Tracer::with_geometry` builds a tracer over any pair. `Tracer::new` and the Boris/CX/yield loop are unchanged. `WireField::tracer` plus `bem::launch_fibonacci` give a full-sphere launch |
| 2D twin | `src/poisson.rs` | `solve_with_chamber(.., ChamberShape::Sphere, ..)`: the axisymmetric solver in a spherical staircase chamber, for apples-to-apples ring checks. `solve()` is unchanged |
| schema | `src/spec3d.rs` | `DeviceSpec3d` (serde, `deny_unknown_fields`, named params, fail-closed). Electrode kinds `great_circle_globe`, `lat_long_globe`, `circle`, `arc`, `polyline`, `ring`. `from_axisym` lifts a 2D `DeviceSpec` and keeps its parameter names |
| wasm / MCP | `vcad-kernel-wasm` `particleSimulate3d`, engine `particleSimulate3d`, MCP `simulate_charged_particles` | a spec with `electrodes` dispatches to the 3D path. It returns stats, first-pass and geometric transparency, the center potential, the neutron-rate floor and ceiling, and the residual report. **No receipt claims yet**: `vcad.particle-claims/1` is bound to the 2D grid's convergence basis |

## why a boundary method, not a 3D grid

The wire is 1 mm in a 150 mm chamber. A grid that resolves the wire (h ≈ a/4)
needs ~10⁸–10⁹ nodes in 3D. Even with multigrid that's minutes of solve and
gigabytes of field, for a problem whose real unknowns are only the charge on
the wires.

The BEM's unknowns are exactly that: a piecewise-constant line charge on each
straight wire segment. For the hobby globe that's **186 unknowns, solved in
30 ms**.

- **chamber**: Kelvin images (the exact grounded-sphere Green's function).
  The wall costs no unknowns, and φ = V_wall on it identically. The image term
  is smooth inside the chamber, so it is integrated with 2-point Gauss on
  sub-intervals sized to the wall gap.
- **collocation**: reduced thin-wire kernel `1/√(d²+a²)` in closed form.
  Segments are ≥ 4a long (the `vcad-kernel-antenna` validity rule). Sag is
  ≤ 0.1a on arcs.
- **least squares at joints**: matching the potential only at segment
  midpoints leaves a **~74% potential bulge** at wire crossings. Both wires'
  charge is counted at full density between the collocation points. Matching
  at 4 stations per segment, including the joints, and solving the
  overdetermined system by least squares cuts that to **2–4%**. Without this,
  great-circle grids are wrong exactly where ions hit them.
- **linear algebra**: dense normal equations + Cholesky from `tang-la`
  (LU for square collocation). At N ≲ 3000 a direct solve beats an iterative
  one and needs no preconditioner. `tang-la` already has LU, Cholesky and QR,
  and `tang-sparse` has PCG with Jacobi/IC0. No reusable solver was missing,
  so there's no `tang` PR.
- **field**: the exact potential of a uniform line segment,
  `kλ ln((r₁+r₂+L)/(r₁+r₂−L))`, plus image point charges. E is its analytic
  gradient, so the sampled field is **conservative by construction** (like the
  bilinear-patch gradient in 2D). Energy error comes from the time step alone.
  Endpoint distances are shared between adjacent segments, so a field
  evaluation costs one sqrt per vertex.

### fail-closed gates (`ResidualReport`)

After the solve, the exact-kernel potential is sampled on a ring of 8 points
around the wire at every segment midpoint and joint. Points buried inside a
crossing wire are skipped. Three numbers, each relative to ΔV and each gated:

| gate | default | meaning | hobby globe |
|---|---:|---|---:|
| `mean` | 2% | ring-mean residual away from junctions: the monopole consistency the model claims | 1.1% |
| `junction_mean` | 10% | the same within 3 wire radii of another wire (crossings) | 3.9% |
| `spread` | 10% | half peak-to-peak around the ring: the missing transverse-dipole (proximity) response of an on-axis line charge, ~a·E_ext/ΔV | 4.2% |

`spread` is the known limit. Within a few wire radii, the near-wire field
carries a ~4% error for 1 mm wire on a 19 mm globe. Dipole unknowns per
segment would remove it. The far field is unaffected at O((a/d)²).

## validation (`cargo test -p vcad-kernel-particle`)

`tests/globe3d.rs`, plus unit tests in `bem.rs`, `wire3d.rs` and `spec3d.rs`:

- **ring stack vs the 2D solver** (4 rings in a 60 mm sphere, both solvers):
  3D is axisymmetric to 10⁻³. The 2D solver's gap to 3D shrinks under
  refinement (h = 0.5 → 0.25 → 0.125 mm: 3.9% → 2.0% → 1.2% of |V| at the
  center; 2D is first order from the wire mask and staircase wall).
  Richardson-extrapolated 2D agrees with 3D to ≤ 1% at every probe (0.14% at
  the center).
- **point charge in a grounded sphere**: a 1 mm ring reproduces
  `q(1/|x−y| − (R/|y|)/|x−y*|)` to 0.2%.
- **concentric spheres**: lat/long globes of growing density
  ((3,4) → (7,10) → (11,16)) converge monotonically to the concentric-sphere
  potential outside the grid (< 3%) and to the concentric capacitor's charge
  (< 6%).
- **thin ring capacitance**: 4π²ε₀R / ln(8R/a) to 2%.
- **Biot–Savart** on a current loop matches the elliptic-integral `b_ring`
  to 0.5%.
- **E = −∇φ** to 10⁻⁵ by central differences.
- **6-great-circle transparency**: Fibonacci ray count = closed form
  (1 − 6a/R + 30 rhombic overlaps at 63.4° crossings = 0.714) to 1%.
  Ballistic tracing (100 keV ions, 1 V grid) gives the same transparency to
  3%, which exercises the tracer's capsule hit test end to end.
- **tracing** at −30 kV conserves energy (mean drift < 1% at the default
  step over a 6-pass cap; drift grows with trace length, so see the
  convergence table for 40 passes) and ions recirculate.
- `Tracer::new` behavior is unchanged: the 66 pre-existing lib tests and the
  `analytic` / `codesign` / `shielding` integration tests pass as before.

## hobby fusor: 6-great-circle globe vs the ring stack

`cargo run --release -p vcad-kernel-particle --example globe_vs_rings 400`

6" chamber (76 mm sphere), 1.5" grid (19 mm), 2 mm wire, −30 kV, 10 mA,
5 mTorr D₂, σ_cx = 1×10⁻¹⁹ m². 400 D⁺ start at rest on a Fibonacci sphere at
0.85 R, with a 40-pass cap. Every row uses the same solver, chamber, launch
and tracer, so the differences come from the grid alone (mew, step 0.25 mm):

| grid | unknowns | φ(center) kV | T geometric | T first pass | interception | survivors | mean passes | floor n/s | ceiling n/s |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| ring stack (4 latitude rings) | 108 | −27.40 | 0.838 | 0.815 | 0.600 | 0.400 | 20.7 | 2.69×10⁴ | ≥1.01×10⁶ |
| ring stack (8 rings, transparency-matched) | 206 | −29.91 | 0.702 | 0.680 | 0.642 | 0.357 | 19.6 | 3.19×10⁴ | ≥1.24×10⁶ |
| **6 great circles** (icosidodecahedral) | 186 | −28.91 | 0.714 | 0.682 | 0.848 | 0.152 | 7.1 | 2.79×10⁴ | ≥4.17×10⁵ |
| lat/long (3 lat + 4 meridians) | 147 | −28.22 | 0.780 | 0.765 | 0.978 | 0.022 | 3.2 | 2.82×10⁴ | ≥2.09×10⁵ |

Survivors / mean passes by launch latitude, with |cos θ| bands at the
4-ring stack's ring latitudes:

| grid | equator band (< 0.31) | mid band (0.31–0.81) | polar caps (> 0.81) |
|---|---|---|---|
| ring stack (4) | 0.56 / 25.2 | 0.37 / 21.8 | 0.21 / 10.3 |
| ring stack (8) | 0.37 / 19.8 | 0.39 / 20.3 | 0.25 / 17.3 |
| 6 great circles | 0.16 / 7.4 | 0.14 / 6.6 | 0.17 / 7.7 |
| lat/long (3+4) | 0.00 / 2.7 | 0.01 / 2.7 | 0.09 / 5.3 |

**What it says**

- **A ring stack overstates recirculation ~3×** (20.7 vs 7.1 mean passes for
  the globe), and the D-D ceiling by **2.4×**. This is the geometry error
  blocker #3a was about. A ring-stack stand-in is not a conservative proxy
  for a globe: it is optimistic.
- **It isn't the polar holes, and it isn't transparency.** Launches into the
  4-ring stack's polar caps recirculate *least* (10 passes vs 25 near the
  equator). A ring stack with 8 rings, matched to the globe's 0.70 geometric
  transparency, still makes 19.6 passes. The data point to the symmetry
  itself. In an axisymmetric field, an ion launched from rest never leaves
  its meridian plane. Each annular gap between rings is then a slit lens
  extended over the whole azimuth, and it re-focuses the same orbit pass
  after pass. Per-pass survival after the first crossing is ~0.95 for both
  ring stacks, far above their geometric 0.70–0.84. The globe's discrete
  openings confine less stably (per-pass ~0.88).
- **The floor barely moves** (2.7–3.2×10⁴ n/s). At 5 mTorr the CX mean free
  path is shorter than a pass, so the CX-on floor depends on the first few
  passes and on well depth: the 8-ring stack has the deepest well
  (−29.9 kV) and the highest floor.
- **First crossing ≈ geometric.** Traced first-pass transparency sits 0.01–0.03
  below geometric for every grid. That is within the ±0.02–0.03 sampling
  noise of these ensembles, so call it consistent with geometric. Lensing
  matters on later passes, not on the first crossing.
- **2D cross-check** (ring stack): the 2D sphere solve at h = 0.25 mm sits
  1.9% of |V| shallower at the center than 3D, its known first-order error,
  and agrees to < 0.1% away from the grid. The old 2D pipeline (cylinder,
  meridian-plane launch, h = 0.25 mm) gives 10.8 mean passes and a
  4.6×10⁵ n/s ceiling. The POC already showed that ceiling still climbing
  under grid refinement (1.8 → 3.3 → 4.4×10⁵). The 3D ring stack
  (1.0×10⁶) is consistent with its h → 0 limit.
- **Ensemble size**: N = 192 → 400 moves the globe's passes 6.4 → 7.1 and its
  ceiling 3.8 → 4.2×10⁵ (~9%). The ring stack moves < 1%. The ring/globe
  ceiling ratio is 2.4–2.6× across both N and step size.

**Convergence (tracer step, mew):**

N = 192:

| step | ring stack: passes / ceiling / mean drift | 6 great circles: passes / ceiling / mean drift |
|---|---|---|
| 0.5 mm | 16.2 / 6.7×10⁵ / 11.9% | 6.78 / 3.30×10⁵ / 4.8% |
| 0.25 mm (default) | 20.8 / 1.01×10⁶ / 7.0% | 6.44 / 3.84×10⁵ / 2.0% |
| 0.125 mm | 20.9 / 1.16×10⁶ / 3.3% | 6.42 / 4.25×10⁵ / 1.0% |

Passes, interception and the floor are converged at the default step. The
ceiling still rises ~10–15% per halving for both grids, because it weights
the highest-energy, longest-lived ions (and survivors at the pass cap make it
a censored "≥" anyway). The **ratio (2.6–2.7× at N = 192) is stable**.

## limits and next steps

- **cylindrical chambers** aren't supported in 3D (the spec fails closed).
  The 2D path keeps them. A charge-simulation wall (fictitious charges
  outside, collocated on the wall) would add them cheaply. The CF spherical
  cube is closer to a sphere anyway.
- **dipole thin-wire unknowns** would remove the ~4% near-wire azimuthal
  error (the `spread` gate).
- **3D adjoint**: `DeviceSpec3d::parameter_roles` reports every parameter as
  finite-difference. `adjoint.rs` is 2D-grid-bound.
- **3D receipt claims**: `receipt::predicted_claims` takes a 2D `Solution`.
  3D needs a residual-based convergence basis before it can issue
  `vcad.particle-claims/1`.
- **space charge in 3D**: `Tracer::launch_ensemble_dwell` returns an empty
  dwell vector for 3D tracers (no r–z grid). The space-charge gauge stays 2D.
- **speed**: a 40-pass ion costs ~70 ms at the default step (186 segments).
  A cell-local near/far field cache would cut that if MCP ensembles need to
  be large.
