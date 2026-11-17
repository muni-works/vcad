# Plasmoid fluid M0: deriving the magnet lock

The reduced plasmoid model ([`plasmoid.rs`](../crates/vcad-kernel-particle/src/plasmoid.rs), [`plasmoid-sim.md`](plasmoid-sim.md)) pins a racing RF filament under a hand-held magnet with a fitted pull, `μ ∇Φ` where `Φ = ln(1+β²)`, plus a fitted swirl `v_spin`. This milestone replaces that fit with a model of the glow itself: a 2D magnetized drift-diffusion fluid on the dish plane, in [`plasmoid_fluid.rs`](../crates/vcad-kernel-particle/src/plasmoid_fluid.rs). It closes gap #4 of `plasmoid-sim.md`.

## Result: the magnetic channel repels; a conducting magnet attracts

Demo operating point: argon, 1 Torr, N52 Ø10×5 mm disc magnet 10 mm above the dish, north down, 3 mm foot. Output of `cargo run --release -p vcad-kernel-particle --example plasmoid_fluid` (96 cells for the profile, 72 for the sweeps).

**Derived vs fitted pull** (radial foot velocity, inward positive):

| | peak | at ρ | shape vs `μ∇Φ` |
|---|---|---|---|
| fitted `μ∇Φ` | +1.07 m/s | 7 mm | 1 |
| derived, magnetized glow | **−8.3 m/s** (outward) | 5 mm | correlation **−0.89** (mirror image) |
| floating conducting magnet (electrostatic) | +22.7 m/s | 0 | n/a |

- **Channel.** The energy channel gives −8.31 m/s; the particle channel gives −0.0018 m/s. Under the magnet `T_e` is trapped by `κ⊥ ∝ 1/(1+β²)`, but the asymmetry that moves the foot is in its far wing: at ρ = 5 mm, `T_e` at −2σ…+2σ (magnet side first) is 1.73, 1.90, 1.96, 1.89, 1.80 V. The far side is warmer, ionizes faster, and the centroid skews away.
- **Growth disagrees with drift.** Net growth is +1208 /s under the magnet and +1060 /s at 5 mm (zero far away, by calibration). A re-forming foot would prefer the magnet; a drifting one is pushed off. M1 decides.
- **Drive.** Peak pull is −8.3 m/s for uniform, −17.5 for field-driven and −76.3 for current-driven RF heating.
- **Robustness of the sign.** The heated-core width 0.5–1.5σ and background 0–60 % all give an outward pull of 2.3–9.5 m/s at ρ = 3, 6, 9 mm. The foot width 1.5/3/4.5/6 mm gives −4.4/−8.3/−8.2/−6.9 m/s.

**Pressure sweep** (10 mm gap; peak radial velocity in m/s):

| p (Torr) | β under magnet | derived | field-driven | current-driven | fitted μ∇Φ | floating ES | magnetic / ES |
|---|---|---|---|---|---|---|---|
| 0.1 | 28 | −699 | +66 | −28 900 | 1.69 | 136 | −5.1 |
| 0.3 | 9.5 | −98.6 | +61.6 | −1930 | 1.57 | 66.0 | −1.5 |
| 1 | 2.9 | −8.29 | −17.5 | −76.3 | 1.07 | 22.7 | −0.37 |
| 3 | 0.95 | −0.46 | −2.07 | −2.96 | 0.39 | 7.38 | −0.06 |
| 10 | 0.28 | −0.007 | −0.072 | −0.072 | 0.051 | 2.13 | −0.003 |
| 30 | 0.09 | −1e-4 | −0.003 | +0.003 | 0.006 | 0.79 | ~0 |

**Gap sweep** (1 Torr):

| gap (mm) | derived | fitted | floating ES | grounded RF-heating multiplier |
|---|---|---|---|---|
| 5 | −14.6 | 2.86 | 269 | 966× |
| 10 | −8.29 | 1.07 | 22.7 | 109× |
| 20 | −0.64 | 0.070 | 1.95 | 14× |
| 30 | −0.052 | 0.005 | 0.44 | 5.3× |

The derived and fitted pulls fall off with gap at the same rate (ratio ≈ −5 to −10 throughout), because both are set by β(gap). The electrostatic pull falls faster, as `(a/h)³`.

**Swirl.** The derived azimuthal pattern speed is +2.6 m/s (CCW seen from above, north down). It comes from the in-plane Hall terms of κ and `D_a`. The fitted `v_spin` (−0.04 m/s) is the sheath `E_z×B_r` sense, which a 2D sheet cannot contain, so the two are not comparable.

**Feed-back run.** The magnet is held 10 mm over the orbit for 4 s, with the derived map rescaled to the fitted peak (1.07 m/s). Under `LockModel::Fitted` the foot is LOCKED, 3.0 mm from the footprint. Under `LockModel::Derived` it keeps racing, 40.9 mm away.

### Derived vs still fitted

| quantity | status after M0 |
|---|---|
| sign of the magnetic pull | **derived**: outward (repulsive) for uniform and current-driven heating at all pressures, and field-driven at ≥ 1 Torr |
| radial shape of the pull, peak offset (5 mm) | **derived** (the `LockMap` table) |
| pressure and gap dependence | **derived**, as ratios |
| magnetic vs electrostatic ratio | **derived** within the sphere image model: ES wins at ≥ 1 Torr, magnetic below ~0.3 Torr |
| β, ν, `D_a`, `T_e`, rates | derived / literature |
| pull amplitude (m/s) | **still fitted**. The fluid's m/s are frozen-profile skew rates, not foot speeds |
| foot width σ, glow thickness, heating shape | assumed |
| race speed, preferred radius, `v_spin` | still fitted (outside the 2D sheet) |

### Verdict

The fitted lock `μ∇Φ` is not what magnetized transport in the glow does. The magnet makes the glow hotter and more self-sustaining underneath it, but the foot's ionization skew pushes it away. A Ni-plated magnet acting as a conductor gives an attracting pull 2.7× larger at 1 Torr. The visual lock in the source clip is therefore most plausibly electrostatic (or set by where the foot re-forms, which is the growth criterion), not the `D⊥` pull the reduced model assumes. The aluminum-slug and sleeved-magnet controls of `plasmoid-sim.md` §1 decide it; the claims in `vcad.plasmoid-claims/1` are written to be bound to exactly those measurements (M3).


## Model

The glow is a quasi-neutral argon sheet of thickness `L_z` lying on the dish. It is solved on a Cartesian grid centred on the filament's foot (72–96 cells across ±5σ). The magnet enters through the full posed field `B(x, y)` from `field3d::FieldScene`, sampled at the glow height (2 mm).

| piece | equation | status |
|---|---|---|
| electron drift | `v + μ_e v×B = −μ_e(E + ∇p_e/en)`, so `M_e = μ_e(I + β² b̂b̂ᵀ + β[b̂]×)/(1+β²)` | derived (3D, full B vector) |
| ν, β | `collision_frequency(p, 300 K, 1e-19 m², 3 eV)`; `β = μ_e|B|` | same closure as the reduced model, so the two models' β are identical |
| ions | Ar⁺ in Ar, `K₀ = 1.53 cm²/V/s`, unmagnetized, `T_i = 0.026 eV` | literature |
| particle transport | local zero-current ambipolar in 3D: `Γ = −D_a∇n − W n∇T_e`, `D_a = D_i(I − P) + T_e Q`, `P = μ_i A⁻¹`, `Q = μ_i A⁻¹M_e`, `A = μ_i I + M_e` | derived |
| out-of-plane loss | `ν_z = D_a,zz/Λ_z²`, `Λ_z = L_z/π`, from the *same* tensor (under the magnet B ∥ ẑ, so this loss is not magnetically reduced) | derived; `L_z` assumed (5 mm) |
| electron energy | `∇·(κ∇T_e) + n[H − n_g K(T_e) − ν_z ε_w(T_e)] = 0`, `κ = (5/2) n T_e M_e` (in-plane block) | derived; steady, because it relaxes in µs versus the ms density response |
| argon rates | `k_iz`, `k_ex`, `k_el` Maxwellian fits (Lieberman & Lichtenberg 2nd ed., Table 3.3, valid 1–7 eV, ±30 %); `ε_w = 2T_e + T_e/2 + V_s` | literature |
| RF heating per electron | `H = H₀[f_bg + (1 − f_bg) exp(−r²/2σ_h²)]`; ohmic heating from a vertical RF field is B-independent for E ∥ B | shape assumed (σ_h = 3 mm, f_bg = 0.25) |
| `H₀` | Schottky condition: with no magnet the foot is exactly self-sustaining (net growth 0) | derived |
| foot density | Gaussian, rms radius σ = 3 mm | **assumed**. A linear fluid cannot make a filament: constriction physics such as gas heating and stepwise ionization is absent |

**What the solver computes.** Given the magnet pose, the energy equation fixes `T_e(x, y)` (Picard on κ, Newton on the losses, BiCGSTAB on the nonsymmetric 9-point stencil, rows scaled by `1/n`). Continuity `∂n/∂t = −∇·Γ + g n`, with `g = n_g k_iz(T_e) − ν_z`, then fixes the foot's centroid velocity exactly, as a discrete identity:

```text
dx̄/dt = Σ (x − x̄) g n dA / N   −   Σ (x − x̄) ∇·Γ dA / N
        ionization skew            transport drift (incl. Hall)
```

The radial part of `dx̄/dt` is what the reduced model writes as `μ∇Φ`. The azimuthal part is the derived replacement for `v_spin`: the Hall terms of `D_a` and of κ advect the pattern. It is a pattern speed, not the electron E×B fluid speed, which the solver also reports and which is ~10⁴ m/s.

Neglected, both O(μ_i/μ_e⊥): the electron convective energy flux `(5/2)T_eΓ_e` and the Γ·E work.

## The two channels

Magnetizing the electrons barely changes *particle* loss. Ions limit the loss until `μ_e/(1+β²) ≈ μ_i`, which is `β ≳ β_c = √(μ_e/μ_i) ≈ 19` for argon at any pressure (both mobilities scale as 1/p). At 1 Torr the magnet reaches β ≈ 3, which moves `D_a` by a few percent (`ambipolar_closed_form_and_ion_limit`). Electron *heat* conduction is not ambipolar-limited: `κ⊥ ∝ 1/(1+β²)` directly. That is the channel through which the magnet acts. `FluidParams::{magnetize_energy, magnetize_particles}` switch each channel off independently.

## Validation ladder (`cargo test -p vcad-kernel-particle plasmoid_fluid`)

| test | checks |
|---|---|
| `mobility_tensor_limits` | `M_e = μ_e I` at B = 0; E×B/B² at β → ∞; parallel mobility untouched |
| `ambipolar_closed_form_and_ion_limit` | unmagnetized `D_a = (μ_e D_i + μ_i μ_e T_e)/(μ_i + μ_e)`; β = 3 moves in-plane `D_a` < 5 %; β_c ≈ 19 and pressure-independent |
| `calibration_sustains_and_stays_in_fit_range` | Schottky root has net growth < 1 s⁻¹; `T_e` inside the rate fits' 1–7 eV |
| `unmagnetized_foot_does_not_move_and_conserves` | no field → no drift (isotropic); `Σ∇·Γ dA` = boundary outflux to 1e-9 |
| `uniform_field_gives_no_drift_but_more_growth` | a uniform vertical B: no drift (symmetry), higher growth (less heat loss across B) |
| `uniform_heating_zeroes_the_conduction_channel` | with spatially uniform heating (`f_bg = 1`) and unmagnetized particles, a magnet gives no drift: T_e is uniform, so the conduction channel needs a heated core to act on |
| `heating_tilt_moves_the_foot_toward_the_heat` | a tilted heating profile drives the foot up-gradient (sign of the ionization skew) |
| `magnet_pulls_and_conserves` | a magnet moves the foot (sign not asserted: it is the result); discrete conservation holds with B |
| `polarity_flips_swirl_not_pull` | flipping the magnet keeps the radial pull and reverses the swirl (Hall sign) |
| `atmosphere_kills_the_pull` | at 760 Torr the dimensionless effect (ΔT_e/T_e, pull/σν_iz) is < 1e-3 of 1 Torr |
| `pull_converges_under_refinement` | 24 → 48 → 96 cells: the error shrinks ≥ 2.5× per halving and is < 5 % at 96 |
| `es_field_ratios` | image-model closed forms: `1 + 4(a/h)³` floating, `1 + 2aD/h²` grounded; 1 far away |
| `lock_map_feeds_the_reduced_model` | `LockMap` nodes, polarity, cut-off; `LockModel::Derived` = race + radial + table; `LockModel::Fitted` = existing model bit for bit |
| `report_claims_roll_up_provisional` | `vcad.plasmoid-claims/1` rides `vcad_receipt` as Predicted → Provisional, never Pass |
| `plasmoid::fitted_lock_option_is_simulate_scene_bit_for_bit` | the new option leaves the default path untouched |

## Milestone ladder

| milestone | content | status |
|---|---|---|
| M0 | magnetized foot fluid, two channels, derived pull/swirl tables, feed-back option, electrostatic comparison, claims family | **this doc** |
| M1 | self-consistent foot density (eigenmode with co-moving heating) instead of the prescribed Gaussian; a time-dependent foot to test the quasi-static drift | open |
| M2 | conductor field from the crate's Poisson solver (the actual disc, the envelope, the dish) instead of the sphere image model; floating-potential self-consistency | open |
| M3 | bench: `vcad.plasmoid-claims/1` bound to measurements (capture gap, release speed, pressure sweep, aluminium-slug and sleeved-magnet controls) | open |

## Non-goals

- Filament formation and constriction. The foot width is an input.
- Kinetic/PIC sheath physics. The rectified-sheath E×B of `plasmoid-sim.md` §1 is not in the 2D sheet; only the in-plane Hall terms are.
