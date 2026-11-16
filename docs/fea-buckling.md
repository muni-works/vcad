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

Every number below is printed by the test that asserts it (`cargo test -p
vcad-kernel-fea --release -- buckling --nocapture`), apart from the sweep
rows, which come from the ignored `buckling::sweep::cylinder_sweep` test.

| case | reference | FE |
|---|---|---|
| fixed-free column 200×10×10 mm, axial tip load | Euler `π²EI/(4L²)` | gated at 3% |
| plane-strain ring r=50 t=2, follower pressure | Bresse `E/(4(1−ν²))·(t/r)³` = 3.516 MPa | 3.536 (+0.6%) |
| same ring, dead (fixed-direction) pressure | — | +7.9% over follower: unconservative, as expected |
| complete sphere r=50 t=2 (cube-sphere 6×16², 1 layer) | Zoelly `2E/√(3(1−ν²))·(t/r)²` = 387.3 MPa | 379.1 (−2.1%) |
| capped cylinder r=50 t=1 L=300, hydrostatic, SS axially free (64×40×2) | von Mises (n = 3) 1.662 MPa | 1.685 (+1.4%) |
| closed sphere: `K_L` asymmetry / 3-2-1 reaction residual | 0 | 5.4e-16 / 3.2e-13 |
| sweep, r=50 t=1 L=100 (n = 5), 64×24×2 → 96×32×2 | von Mises | converging from above, about +3% at the finer mesh |

`von_mises_cylinder_pressure` uses von Mises' thin-shell formula with the
full `n²−1` ring term, attributed to Windenburg & Trilling (1934) and
Timoshenko & Gere §11.5. The formula is as commonly published. Its form is
corroborated independently: its long limit is Bresse exactly (unit test), and
the eigenvalue FE reproduces it within ~2% at n = 2, 3 and 5. The test gates
the FE against it at 5%.

## Shell screen (`shell.rs`)

**Cylinders.** Classical value = von Mises (above). The knockdown comes from
NASA/SP-8007-2020/REV 2, *Buckling of Thin-Walled Circular Cylinders*,
§External Pressure, checked against the monograph's text:

- Eq. 19–22, Batdorf form: `N_y = p·r = k·π²D/L²`,
  `k_y = [(1+β²)² + 12γ²Z²/(π⁴(1+β²)²)]/β²` (lateral). For hydrostatic
  loading the leading `β²` becomes `β² + ½`. `Z = L²√(1−ν²)/(rt)`,
  `β = nL/(πr)`.
- Eq. 28: `√γ = 0.75` *inside* the bracket. It multiplies only the membrane
  term, so the effective knockdown depends on the mode. The screen applies
  SP-8007's own ratio for the geometry, Batdorf at `√γ = 0.75` over Batdorf at
  `γ = 1` (both minimized over a continuous β, as the monograph does), to the
  von Mises classical value. Reported knockdowns run 0.72–0.84 for the 6" tube.
- Eq. 27/29: when the governing mode ovalizes (`n = 2`), `γ = 0.90`, capped
  by the Eq. 28 ratio so a longer tube never reads stronger. The screen is
  monotone in span to 1e-6 with no cliffs (unit test). Batdorf's
  shallow-shell form, unlike von Mises, drops the ring term's `−1` and reads
  up to a third high at `n = 2`, which is why it supplies only the ratio.
- Design factor 3 by default (the margin commonly cited for UG-28's cylinder
  curves). This is **not** a UG-28 calculation: the code reads A off Fig. G
  and B off a material chart.

**Spheres and heads.** Classical Zoelly bifurcation, plus ASME VIII-1
UG-28(d)'s elastic branch, as commonly published (the ASME numbers in this
section have **not** been checked against the code text in this crate): `A = 0.125/(Ro/t)`, `B = AE/2`,
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
- A screen whose margin is below 1 emits its `external_pressure_margin`
  claim as **Fail**, so the receipt rolls up Fail. The other claims stay
  predicted.

## The fusor example: 6" OD 304 SS, one atmosphere outside

`cargo run --release -p vcad-kernel-fea --example vacuum_shell`: E = 193 GPa,
ν = 0.29, σy = 205 MPa, 1 atm = 0.101325 MPa, cylinder span 300 mm between CF
flanges, hydrostatic. Pressures are in MPa. "FE" is the perfect-shell
eigenvalue on a body-fitted mesh, an unconservative number to compare against
"classical", not to design to.

| shell | t mm | classical | knockdown | collapse | allowable | margin | FE bifurcation |
|---|---|---|---|---|---|---|---|
| cylinder | 0.5 | 0.157 | 0.75 | 0.118 | 0.039 | **0.39 ✗** | 0.161 |
| cylinder | 1.0 | 0.883 | 0.83 | 0.735 | 0.245 | 2.42 | 0.894 |
| cylinder | 1.5 | 2.553 | 0.72 | 1.837 | 0.613 | 6.04 | 2.580 |
| cylinder | 2.0 | 5.521 | 0.72 | 3.970 | 1.323 | 13.1 | 5.523 |
| cylinder | 3.0 | 13.94 | 0.84 | 8.233 (yield) | 2.744 | 27.1 | 13.94 |
| sphere | 0.5 | 10.09 | 0.21 | 2.077 | 0.519 | 5.13 | — |
| sphere | 1.0 | 40.64 | 0.20 | 5.416 (yield) | 1.354 | 13.4 | — |
| sphere | 1.5 | 92.04 | 0.20 | 8.151 (yield) | 2.038 | 20.1 | 92.53 |
| sphere | 3.0 | 375.6 | 0.20 | 16.47 (yield) | 4.117 | 40.6 | 362.5 |

The cylinder FE column (96×48×2 body-fitted bricks) lands within 0–2.5% of
the classical column at every wall: two independent routes agreeing on the
perfect-shell number. The knockdown and design factor are what turn it into
a design answer.

A 0.5 mm, 6" tube between flanges 300 mm apart is the one that fails: its
perfect-shell number is 1.5 atm, and real tubes collapse below that. The
margin claim then lands as a **Fail** on the receipt, not a provisional pass. Every
sphere passes; spheres are strong in compression and limited by yield, though
they are also the most imperfection-sensitive shape, and the screen says so.
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
