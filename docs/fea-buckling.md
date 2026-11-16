# Structural FEA: buckling and external pressure

`crates/vcad-kernel-fea` used to stop at linear static analysis. That misses
the failure mode that governs a vacuum chamber, a viewport cover or a custom
welded fusor shell. A wall far from yield will still snap through under one
atmosphere pushing inward, and a static solve can't see that happen. This
milestone adds two routes, a third route's honesty notes, and an MCP tool.

| route | module | answers | cost |
|---|---|---|---|
| eigenvalue buckling | `buckling.rs` | lowest bifurcation load factor + mode shape, any brick mesh | FE solve + LOBPCG |
| shell screen | `shell.rs` | external-pressure collapse of a thin cylinder / sphere / dished head, with the cited knockdown | microseconds |
| MCP | `shell_buckling_check` | the shell screen, geometry by description | microseconds |

## Why two routes

A linear bifurcation load is the design number for a column or a plate. **For
a shell it is unconservative.** Real cylinders and spheres collapse well below
their bifurcation load because of geometric imperfections: dents, flat spots,
weld peak-out. The FE route reports the perfect-structure number, and every
claim it emits says so. The shell screen is the number to design to: classical
bifurcation × a cited empirical knockdown, capped at membrane yield, divided by
a design factor.

## Eigenvalue buckling (`buckling.rs`)

`(K + λ(K_G + K_L))·φ = 0`:

1. **Element.** An 8-node isoparametric brick with Wilson–Taylor incompatible
   modes. It is the `modal.rs` element, generalized to distorted geometry with
   Taylor's centroid-Jacobian modification, so it passes the patch test on
   skewed bricks (`patch_test_on_distorted_brick`). Plain trilinear bricks and
   CST tets lock in bending, and buckling *is* bending.
2. **Prestress.** A static solve under the reference load. Stress is recovered
   at the 2×2×2 Gauss points, *including* the condensed bubble strains. The
   solve uses an exact RCM-skyline Cholesky when the profile fits, and
   IC0-PCG otherwise.
3. **Geometric stiffness.** `K_G = ∫ ∇Nᵀ σ ∇N dV` on the compatible shape
   functions.
4. **Follower pressure.** Pressure stays normal to the deforming wall, which
   adds `K_L = −∂f/∂u` (checked against finite differences,
   `load_stiffness_matches_finite_difference`). Without it, a ring under
   external pressure computes the dead-load problem, which reads high: the
   wrong side for a vacuum vessel. `K_L` is symmetric on a closed surface. On
   an open one (a tube with open ends) we keep the symmetric part and report
   the dropped asymmetry as `load_stiffness_asymmetry`.
5. **Eigensolver.** `(K_G + K_L)·x = μ·K·x` goes through the `modal.rs`
   LOBPCG, now generic over the `(A, B)` pencil. `K` (SPD once supported)
   plays the mass matrix, and its factor is the preconditioner. Then
   `λ = −1/μ` for the most negative `μ`. If no `μ` is negative, the result is
   `BucklingError::NoBuckling`: the load stiffens the structure.
6. **Supports.** Box regions, explicit node sets, and an automatic **3-2-1
   kinematic constraint** for self-equilibrated loads. A closed vessel under
   uniform pressure needs no supports, but `K` must be nonsingular. The 3-2-1
   set is statically determinate. Its reaction residual is reported
   (`kinematic_reaction_rel`, ~1e-12 on a closed sphere), so a load that is
   not quite balanced shows up.
7. **Refinement gate.** `buckling_study` runs two or more meshes and fails
   closed (`Unverifiable`, no claims) when the lowest `λ` moves more than the
   tolerance over the finest step. A single level is always Unverifiable.

**Body-fitted shells.** The lattice fill staircases curved walls, which is
fine for chunky parts and wrong for a 1 mm wall on a 150 mm shell. Use
`cylinder_shell` (structured r-θ-z bricks) and `sphere_shell` (an equiangular
cube-sphere, so there is no pole singularity) for vessels.
`capped_cylinder_spec` builds SP-8007's simply-supported case: ends held
circular at mid-surface, **free to move axially**, cap force as dead pressure
on both annuli. Holding the whole end ring axially raises the pressure.
SP-8007 says by up to 50%, and our first cylinder run read +29% for exactly
that reason.

`lattice_buckling_study` runs the same solver on a tessellated part with
region loads and supports (e.g. the Euler column).

### Validation

| case | reference | FE |
|---|---|---|
| fixed-free column 200×10×10 mm, axial tip load | Euler `π²EI/(4L²)` | within 3% |
| plane-strain ring r=50 t=2, follower pressure | Bresse `E/(4(1−ν²))·(t/r)³` | within 3% |
| same ring, dead pressure | — | reads higher (unconservative), as expected |
| complete sphere r=50 t=2 (cube-sphere 6×16², 1 layer) | Zoelly `2E/√(3(1−ν²))·(t/r)²` | −2.1% |
| capped cylinder r=50 t=1 L=100, hydrostatic, SS axially free | SP-8007 Batdorf screen (n = 5) | +8.8% (64×24×2 bricks; decreasing with θ refinement) |
| closed-surface `K_L` symmetry, 3-2-1 reaction | exact | ~1e-12 |

The cylinder's remaining gap is partly the screen itself. Batdorf/Donnell
shallow-shell theory and von Mises' formula differ by a few percent at n = 5,
and the FE converges from above as the circumferential mesh refines. The test
gates at 10%.

## Shell screen (`shell.rs`)

**Cylinders.** NASA/SP-8007-2020/REV 2, *Buckling of Thin-Walled Circular
Cylinders*, §External Pressure:

- Eq. 19–22, Batdorf form: `N_y = p·r = k·π²D/L²`,
  `k_y = [(1+β²)² + 12γ²Z²/(π⁴(1+β²)²)]/β²` (lateral). For hydrostatic
  loading the leading `β²` becomes `β² + ½`. `Z = L²√(1−ν²)/(rt)`,
  `β = nL/(πr)`. We minimize over integer `n ≥ 2`.
- Knockdown `√γ = 0.75` (Eq. 28), applied *inside* the bracket as the
  monograph does.
- Long ovalizing cylinders (`Z > 4000`, Eq. 26 regime): Eq. 27
  `p = γE/(4(1−ν²))·(t/r)³` with `γ = 0.90` (Eq. 29).
- Shallow-shell theory drops the ring term's `−1` (`n²` where
  Flügge/von Mises have `n²−1`). The screen scales by `(n²−1)/n²` and
  states it in a caution when `n ≤ 3`.
- Design factor 3 by default, the margin UG-28 builds into its cylinder
  curves. This is **not** a UG-28 calculation: the code reads A off Fig. G
  and B off a material chart.

**Spheres and heads.** Classical Zoelly bifurcation, plus ASME VIII-1
UG-28(d)'s elastic branch: `A = 0.125/(Ro/t)`, `B = AE/2`,
`Pa = B/(Ro/t) = 0.0625E/(Ro/t)²`. That allowable is a collapse estimate of
`0.25E(t/Ro)²` (≈ 0.21 × classical at ν = 0.3: the knockdown) with a factor
of 4. Heads follow UG-33: hemispherical at `Do/2`, 2:1 ellipsoidal as a sphere
of `0.9·Do`, torispherical at the crown radius. The knuckle and the
head-to-shell junction are not checked, and the screen says so.

**Yield.** Collapse is capped at membrane yield (`σy·t/r`, or `2σy·t/r` for a
sphere). When the elastic collapse exceeds half the yield pressure, a caution
says elastic-plastic interaction makes the real collapse lower than either.
SP-8007's plasticity factors need `E_sec`/`E_tan`, which the screen doesn't
take.

**Fail-closed.** `D/t < 10` or `R/t < 10` → `Unverifiable` and no claims.

### Claims

`vcad.fea-claims/1`, basis `predicted` (receipts roll up Provisional, never
Pass):

- `structure.shell.*`: `classical_buckling_pressure_mpa` (marked
  UNCONSERVATIVE), `imperfection_knockdown` (with its citation),
  `collapse_pressure_mpa`, `allowable_external_pressure_mpa`,
  `external_pressure_margin`.
- `structure.buckling.*`: `buckling_load_factor`,
  `discretization_error_buckling_rel`, `bifurcation_pressure_mpa`, and either
  `knocked_down_*` (knockdown supplied in the spec, labelled as an input) or
  `imperfection_knockdown = 1` with a note that NO knockdown was applied.

## The fusor example: 6" OD 304 SS, one atmosphere outside

`shell_buckling_check`, E = 193 GPa, ν = 0.29, σy = 205 MPa, cylinder span
300 mm between CF flanges, hydrostatic:

- **0.5 mm wall:** fails the screen. The knocked-down collapse is near
  1 atm, and with the design factor of 3 the margin is < 1.
- **3 mm wall:** passes with a wide margin; yield, not buckling, caps the
  collapse estimate.
- **Sphere, 1 mm wall:** passes. The UG-28(d) elastic collapse (~8 MPa) is
  above membrane yield, so the screen caps at yield and cautions about
  elastic-plastic interaction.

(The unit tests `six_inch_tube_wall_sweep_is_monotone_and_thin_fails` and
`sphere_matches_ug28d_elastic_branch` pin these.)

## Not done (honest list)

- Wiring into vcad.app's Stress panel (Swift/FFI). MCP carries the screen
  today.
- An MCP tool for FE buckling of an arbitrary document part. The lattice
  staircases curved walls, so the useful FE path is the structured-shell one,
  and that is a Rust API for now.
- Plasticity factors, nonlinear (imperfect-geometry) collapse, knuckle and
  junction stresses, stiffened shells.
- The LOBPCG lives in `modal.rs`. A reusable sparse generalized eigensolver
  belongs in `tang-sparse`, but vcad consumes tang from crates.io releases, so
  moving it needs a tang release first.
