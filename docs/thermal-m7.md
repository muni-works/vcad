# Heat-conduction FEA M7: surface radiation

The M0–M6 ladder closed with radiation on the follow-up list. This rung
adds it, because the first place vcad needed a temperature that hot was a
fusor cathode: a star-mode grid glows orange in vacuum and sheds most of
its heat by radiation. A conduction-only model can only send that heat
down the support stalk, so it read a melting cathode where radiation
keeps the grid solid (`fusor_codesign` rev-a/b vetoed on that floor).

## The model

`Boundary::Radiation { emissivity, ambient_c, h_w_m2k, area_scale,
enclosure }` goes on any domain face or the exposed (solid↔void) rule:

```text
q″ = ε_eff·σ·(T_s⁴ − T_∞⁴) + h·(T_s − T_∞)        (kelvin inside, °C outside)
```

- **Half-cell in series.** As with convection, the conduction resistance
  r = d/(2k) from the voxel center to the face sits in series with the
  surface. The surface temperature T_s is solved per face by safeguarded
  Newton–bisection: q″ is monotone in T_s, so the root is bracketed by
  T_∞ and the voxel temperature, and the solve always converges.
- **Parallel film.** `h_w_m2k` (default 0, vacuum) adds convection on the
  same face, so a board in still air can carry its ~6 W/m²K of radiation
  explicitly instead of folding it into an "h_eff".
- **Two-surface gray enclosure.** `enclosure: {emissivity, area_m2}` puts
  the slot inside a wall (view factor 1, wall temperature = `ambient_c`):
  ε_eff = 1/(1/ε + (A_s/A_w)(1/ε_w − 1)), with A_s the slot's radiating
  area as solved. An enclosure no larger than what it encloses fails
  closed. Without one, the surface sees large black surroundings
  (ε_eff = ε).
- **Staircase area.** A voxelized curved surface has more face area than
  the real one at every resolution (×4/π on a cylinder wall, ×1.5 on a
  sphere). Over-counted area over-cools, so the part reads cold, which is
  the unsafe direction. `area_scale` multiplies the slot's faces;
  `solve::radiation_areas(model)` returns the voxel area per slot
  (geometry only, no solve) so `area_scale = true_area / voxel_area` is
  one call. Every solution also reports `voxel_area_m2` and `area_m2`.
- **Fail-closed ranges.** ε ∈ (0, 1], h ≥ 0, `area_scale` > 0, ambient
  above absolute zero, enclosure ε_w ∈ (0, 1] and A_w > A_s. The spec
  requires `emissivity`: a defaulted ε would be a guessed answer.

Radiation needs absolute temperature, so radiating models must use °C.
Conduction-only models are still affine-invariant and are unaffected.

## The solver

R(T) = A·T − b + Q(T) = 0 is solved by **Newton**. The Jacobian is
A + diag(∂Q/∂T_p), with ∂Q/∂T_p = A_f·h_s/(1 + h_s·r) and
h_s = 4ε_effσT_s³ + h. It stays SPD, so each Newton step is one more
matrix-free Jacobi-PCG solve with a larger diagonal and no new code path.

- **Start from above.** The steady solve starts from a uniform field at
  the radiation ambient. Its first Newton step is the problem linearized
  at ambient, which under-counts cooling (T⁴ is convex), so it lands on a
  supersolution. From above, Newton on a convex monotone loss descends
  monotonically. Later steps are backtracked on the absolute ‖R‖ (the
  relative residual is not monotone along the path, because far above
  the root ‖Q‖ dominates both numerator and denominator).
- **Inexact inner solves.** Inner PCG tolerances follow Eisenstat–Walker,
  η = min(0.1, ‖R‖_rel), and are never tighter than the outer tolerance
  needs.
- **Stopping.** ‖R‖ ≤ tol·max(‖b‖, ‖Q‖). This is the same scale-invariant
  test as M0, with the radiated flux standing in when a part radiates and
  has no source of its own. A tolerance below the roundoff floor
  (R cancels terms of size ~diag·|T|) is clamped to the floor and
  reported, not chased forever.
- **Transient.** Every backward-Euler step is a Newton solve warm-started
  from Tⁿ. The energy audit stays an identity of the scheme, to Newton
  tolerance, because the outflow is evaluated at Tⁿ⁺¹.
  `TransientSolution::newton_iterations_total` counts the work.
- **Graph paths.** The tang tensor-graph assembly is linear-only, so
  `device_assembly` and `smooth_max_gradient_graph` refuse radiating
  models (`RadiationUnsupported`) rather than drop the loss silently. The
  steady solve routes radiating models to Newton automatically.

## The adjoint

The adjoint linearizes at the converged state. Q is a per-voxel diagonal,
so the Newton Jacobian is still symmetric, and the gradient of the
smoothed peak still costs one extra PCG solve, now with the Jacobian's
diagonal. The per-face implicit surface balance gives the parameter
partials (D = 1 + h_s·r):

```text
∂q/∂ε_eff = A·σ(T_s⁴ − T_∞⁴)/D      ∂q/∂h = A·(T_s − T_∞)/D
∂q/∂T_∞   = −A·(4ε_effσT_∞³ + h)/D   ∂q/∂k = h_s·q·r/(D·k)
```

plus the enclosure chain dε_eff/dε = ε_eff²/ε². `SmoothMaxGradient`
gains `d_emissivity` per slot. A radiation slot's parallel film and
ambient use the existing `d_film` / `d_ambient` slots, and the half-cell
term adds to `d_conductivity`. `ParamRole`: emissivity and the parallel
film are `Adjoint`; `area_scale` and the enclosure's ε_w and A_w are
finite-difference (not wired).

**FD validation** (`adjoint::tests::radiating_adjoint_matches_finite_differences`):
the two-material board at 40 W with +z radiating (ε = 0.7, h = 6, a gray
enclosure) and −z convecting, where radiation carries a first-order share
of the heat. Central differences at tol = 1e-12 agree with the adjoint on
source power (rel < 1e-5), both regions' k on mixed axes (< 2e-4),
emissivity, the radiation film, the radiation ambient, and the convection
film alongside (all < 1e-5).

## The validation ladder (`tests/radiation.rs`)

| rung | closed form | result |
|---|---|---|
| slab → radiating face | (T₀ − T_s)k/L = εσ(T_s⁴ − T_∞⁴) [+ h(T_s − T_∞)] | exact at voxel centers, < 1e-8 K, for ε ∈ {0.3, 0.8, 1.0}, with and without a film |
| radiating sphere, uniform generation | T_s = (P/(εσA) + T_∞⁴)^¼; interior +q‴R²/(6k) | R = 10 mm, 50 W, ε = 0.9, 40³ voxels: staircase area ×1.499; with `area_scale` the mean surface is 668.89 °C vs 668.89 °C closed form (|err| < 1 mK, 13 Newton iterations); uncorrected it reads 579 °C, 90 K cold, which is the unsafe direction and is pinned by the test |
| lumped radiative cooling (Bi ≈ 4e-4) | t = C/(εσA)·[F(T₀) − F(T)], F = (ln((T−a)/(T+a)) − 2 atan(T/a))/(4a³) | 4 mm Cu cube, 800 → ~300 °C over 50 s, ε = 0.8: worst error 0.19 K at Δt = 0.02 s (first order in Δt); energy audit 4.4e-13; 2 Newton iterations per step |
| transient → steady | long run relaxes to the Newton steady state | T_max within 1e-4 K; audit < 1e-7 |
| enclosure | boxed slab ≡ open slab at ε_eff | field identical to 1e-9 K; black or huge wall → ε |
| radiation-only part in vacuum | radiation alone grounds the system | surface on the closed form within 0.05 K |
| fail-closed | ε ∉ (0,1], NaN, T_amb ≤ 0 K, h < 0, scale ≤ 0, A_w ≤ A_s, ε_w = 0 | `InvalidRadiation`; graph assembly → `RadiationUnsupported` |

Every steady rung also checks the energy balance (source = conduction +
convection + reservoir + radiation outflow) to ≤ 1e-7 relative.

## Receipt

When any face radiates, the claim set changes in four ways:

- `provenance.radiation_model` names the model: gray-diffuse, half-cell
  in series, Newton, no surface-to-surface exchange, supplied ε,
  staircase × `area_scale`. The unified-receipt `details` string carries
  it too.
- Every temperature claim's caveat swaps "radiation not modeled" for the
  supplied ε values.
- `radiated_w` per slot gives faces, area, voxel area, ε → ε_eff, and
  mean and max surface temperature.
- `radiation_fraction` is the share of source power that left through
  radiation faces.

MCP `solve_thermal` accepts the `Radiation` boundary (schema and
description updated) and returns `newton_iterations`, per-slot
`radiation` reports, and `energy.radiation_out_w`.

## The fusor cathode, re-priced (`fusor_codesign` rev-c)

`cargo run --release -p vcad-kernel-particle --example fusor_codesign`

The thermal leg now models the grid as it sits in the experiment: two
copper rings and a stainless stalk in vacuum, at 96³ voxels (1.25 mm).
The rings radiate to the chamber wall through a two-surface gray
enclosure (R = 150 mm sphere, stainless ε_w = 0.3, 25 °C). The stalk
still conducts to the cooled flange. The radiating area is
staircase-corrected against the analytic ring and stalk area
(`area_scale` 0.987; on these thin rings the voxel staircase happens to
nearly cancel). The heat load is particle's interception: 0.59 × 10 mA ×
30 kV = **178 W**.

| model | T_max |
|---|---:|
| conduction only (rev-a/b physics, same 96³ grid) | 19 152 °C |
| radiating, ε = 0.05 (sputter-clean copper, the hot bound) | 1 175 °C |
| **radiating, ε = 0.1 (lightly oxidized; the receipt's `t_max_c`)** | **949 °C** |
| radiating, ε = 0.2 | 760 °C |
| radiating, ε = 0.3 | 665 °C |
| radiating, ε = 0.6 (oxidized) | 525 °C |
| radiating, ε = 0.1, 2 % duty (solved at average power) | 171 °C |

Radiation carries 90–96 % of the 178 W across the band. Newton converges
in 10–13 iterations.

**The verdict changes.** The conduction-only number was never a
temperature. A 19 000 °C cathode is a model with no path for the heat,
and rev-a vetoed on it. With radiation the steady cathode sits at
**~950 °C, about 135 K under copper's 1085 °C melt**. That is not a
comfortable margin: a sputter-cleaned surface (ε ≈ 0.05) reads 1175 °C,
past melt. So continuous operation at 10 mA is still marginal, decided
by surface emissivity, and the receipt now says that directly:
`t_max_c` = 949 °C, `t_max_c_clean_copper` = 1175 °C, and
`radiation_fraction` = 0.92 are all on it. The 2 % commissioning duty
cycle remains comfortable at 171 °C (7.3× melt margin on the rise). It
is now priced by a nonlinear solve at average power, because rev-b's
linear rescale stops holding once radiation is in the model.

Caveat on the number: the rings partly see each other and their own
inner walls, and this model lets every face see the wall. That
over-counts cooling, and true T_max is somewhat higher. That favors
blackening the grid (oxidizing it, or using stainless or tungsten
wire) over trusting clean copper.

## Honesty

- **ε is supplied, not derived.** It is now the dominant uncertainty, the
  way h always was. Copper runs from ~0.03 polished to ~0.6–0.8 oxidized,
  and a fusor grid's surface changes under sputtering. Quote the band, as
  the fusor example does.
- **No surface-to-surface exchange.** Each face sees only its ambient or
  its enclosure wall. Faces that see each other (a fin array, the inside
  of a cage, the two rings of a fusor grid) are treated as if each saw the
  wall. That over-counts cooling where the parts shadow each other.
  Gray-body view-factor exchange between model surfaces is the next rung.
- **Gray and diffuse.** No spectral emissivity, no specular surfaces, no
  participating media.
- **Area is staircase × a supplied scale.** The correction is exact when
  the true area is known (analytic or from the BRep). Automatic normal
  estimation from the voxel mask fails exactly where it matters, on thin
  1–3 voxel features like a wire grid, so it is not attempted.
- **One emissivity per BC slot.** A two-material part on the exposed rule
  (copper rings plus a stainless stalk) shares one ε. Per-material
  emissivity is a natural extension.
