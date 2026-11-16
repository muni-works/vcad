# Charged-particle optics M0–M1: vacuum fields, Boris tracing, fusion-yield figures of merit

`vcad-kernel-particle` makes vcad a design tool for the electrode-geometry
family of devices: fusors and magnetically shielded IEC machines, ion traps,
ion-thruster grids, mass-spec optics, electron guns, X-ray tubes, ion
implanters. All of them reduce to the same loop —

> electrode geometry → fields → charged-particle trajectories → figure of merit

— and the incumbent tool for that loop (SIMION lineage) is decades old,
non-differentiable, and disconnected from CAD/fabrication. This crate is the
M0 of a differentiable, receipt-native replacement that lives inside the
kernel next to the geometry it scores.

## M0 scope (and honesty)

**In scope:** vacuum-field single-particle optics.

- Axisymmetric electrostatics: finite-difference Laplace on the (r, z)
  chamber cross-section, SOR with the Chebyshev-estimate relaxation factor,
  Dirichlet electrodes (wire rings + grounded chamber), symmetry stencil on
  the axis. No space charge.
- Magnetics: exact off-axis field of circular current loops via complete
  elliptic integrals (AGM), superposed per ring; linear regularization
  inside the conductor.
- Tracing: Boris pusher (the plasma-standard geometric integrator), adaptive
  step from grid spacing and speed, automatic substepping where gyration is
  fast, termination on wire/wall impact, core-pass counting, per-trace
  energy-drift diagnostic.
- Figures of merit: interception fraction (the cathode ammeter),
  mean core passes (recirculation), effective transparency, wall losses,
  plus thin-wire geometric transparency as the no-optics reference.
- Optimization: box-constrained finite-difference gradient ascent
  (`optimize::maximize`) — the M0 stand-in for the adjoint, deliberately
  API-shaped so the adjoint can replace the gradient without changing
  callers.

**Out of scope at M0** (each is a milestone below): space charge,
collisions/charge exchange with neutrals, fusion-rate weighting along
trajectories, non-axisymmetric electrodes, the discrete adjoint.

**Regime of validity:** low-current, low-pressure devices where geometry
dominates — exactly the regime of electrode design tools. Do not read
plasma-physics conclusions (thermalization, virtual cathodes, space-charge
limits) out of these traces.

## Validation ladder (all in `cargo test -p vcad-kernel-particle`)

- Elliptic integrals against Abramowitz & Stegun values.
- Loop field against the textbook on-axis formula; odd/even midplane
  symmetry; finite and continuous through the conductor surface.
- Poisson: maximum principle, axis symmetry, grid-refinement consistency,
  well depth.
- Tracing: ions launched at rest fall inward and recirculate; energy
  conservation in the far field; wires dominate the fate table in a classic
  fusor.
- Headline integration test (`tests/shielding.rs`): at fixed geometry and
  bias, cathode ring current **reduces interception and increases
  recirculation** — the magnetically-shielded-grid effect
  (Hedditch/Bowden-Reid/Khachan 2015, arXiv:1510.01788), reproduced from
  first principles.

## Benchmark: the simulated ammeter and neutron counter

`cargo run --release -p vcad-kernel-particle --example fusor_baseline`

96 deuterons/config launched at rest from an 85% shell, 40-pass cap,
121×241 grid, conservative field sampler (see below). Classic 5-ring fusor
control at −30 kV: 9.45 mean passes, 100% eventual wire interception,
effective transparency 0.904 vs 0.925 geometric (the lensing penalty), and
a **predicted beam-on-background neutron rate of 1.9×10⁵ n/s at the 10 mA /
2 mTorr reference point** — a first-principles floor sitting ~25× under the
5×10⁶ n/s amateur DIY record (real fusors add fast-neutral and beam–beam
channels on top; landing under the record by one order with the
conservative channel only is the physically correct place to land).

Two-ring shielded cathode (45 mm rings at z = ±25 mm, 3 mm wire, opposed
currents), interception fraction and D-D yield vs ampere-turns:

| A·turns | intercept −3 kV | intercept −30 kV | yield −30 kV (σv, m³) |
|---:|---:|---:|---:|
| 0 | 1.000 | 1.000 | 1.04e−32 |
| 5 k | 1.000 | 1.000 | 1.04e−32 |
| 10 k | 0.979 | 1.000 | 1.07e−32 |
| 20 k | 0.833 | 1.000 | 1.80e−32 |
| 40 k | 0.521 | 0.917 | 1.70e−32 |
| 80 k | 0.594 | 0.938 | 5.79e−32 |
| 160 k | 0.115 | 0.740 | 6.20e−32 |

(−3 kV yields are ~10⁻⁴¹ — nine orders below −30 kV: low-voltage
commissioning genuinely produces zero neutrons, and now the sim says so
quantitatively.)

Findings:

1. **Shielding works and is enormous** — interception falls 100% → 12% at
   −3 kV / 160 kA·t, and at −30 kV the shield buys **6× fusion yield**
   (5×10⁵ n/s predicted at the reference point).
2. **The √V law falls out.** The −30 kV interception curve is the −3 kV
   curve shifted ~3–4× right in current, matching r_L ∝ √V. Commissioning
   real hardware at low voltage with pulsed copper, then climbing, is
   quantitatively supported.
3. **There is an optimal shield configuration, and passes ≠ yield.**
   Recirculation (mean passes) peaks and then falls as the cusp starts
   reflecting ions off the core (magnetic aperture), while the σ(E)-weighted
   yield keeps different books — it rewards core passages at full energy.
   Optimizing the right objective is exactly what `optimize_shield` does
   (below). Interception wiggles non-monotonically at intermediate currents
   (deterministic launch grid through a chaotic lens), which is itself
   physical.

**M1 field-fidelity fix:** `Solution::e_at` now returns the exact gradient
of the bilinear potential patch (not an interpolation of node-difference
fields), making the sampled E conservative — its line integral between any
two points equals the interpolant's potential difference — so integrator
energy error is set by the time step alone. Near-wire time-step refinement
(dt shrinks within ~6 wire radii) bounds that. Worst-single-trace drift in
the ensemble max column is dominated by extreme 40-pass wire-grazers;
typical traces sit far below the 8% acceptance test.

## The optimizer designs the cathode

`cargo run --release -p vcad-kernel-particle --example optimize_shield` —
multi-start FD ascent over (ampere-turns × ring spacing) at −30 kV, 64-ion
ensembles on a 101×201 mesh. Findings:

- **The yield landscape is multimodal.** A low-current recirculation hill
  (local optimum ≈26 kA·t, 2.0× unshielded yield) is separated from a
  high-current energy-quality hill that single-start gradient ascent never
  reaches. This also exposed an optimizer bug worth remembering: an
  absolute gradient-norm epsilon read a 1e-32-scale objective as
  "converged" after 5 evals — stopping criteria must be scale-invariant
  (regression-tested now).
- **Multi-start finds the real basin:** ≈165 kA·t with ring spacing driven
  to the ±15 mm box bound — **6.2× unshielded yield**, still rising slowly
  when stopped. The bound binding is a design lesson: the M3 geometry seam
  should widen the parameterization (ring radius, asymmetric pairs) rather
  than trust hand-chosen boxes.
- **Perf scaling:** objective evaluations at ≥150 kA·t cost ~100× the
  low-current ones — the gyration substepper resolves ~0.5 T across most
  of the chamber. An adaptive substep budget (coarsen gyration resolution
  away from wires, where only drift matters) is queued in M1.5.

## Milestone ladder

- **M1 — fusion yield + field fidelity. DONE.** Bosch–Hale D-D cross
  sections (both branches, `xsection`), per-trace ∫σv dt accumulation and
  `neutron_rate_per_s` (beam-on-background floor with stated caveats),
  conservative bilinear-patch field sampling, near-wire dt refinement,
  scale-invariant optimizer stopping (objectives at 1e-32 must not read as
  converged — regression-tested). `optimize_shield` example: the optimizer
  designs the cathode (ampere-turns + ring spacing) against predicted
  yield.
- **M1.5 — loss-budget honesty + perf. DONE.** Censoring-aware statistics
  (uncensored means, mean drift); coil B cached on the Poisson grid
  (analytic within 8 wire radii, bilinear beyond — kills the per-substep
  elliptic cost; consistency-tested against the analytic sum);
  charge-exchange model (`CxModel`, constant-σ approximation): traces
  report expected neutrons/ion in a survival-weighted ion channel + a
  fast-neutral straight-line channel. Reality check at 2 mTorr: the ion
  channel collapses (CX mean free path < one pass) and single-generation
  totals land at 1.9×10⁴ n/s vs 1.9×10⁵ no-CX — the ~30× gap to measured
  fusor rates is the **CX chain** (every event births a cold ion that
  re-accelerates) + volume ionization, both explicitly not yet modeled.
  Corollary the model hands us: with CX on, yield is nearly
  pressure-independent (each ion fuses over ~one CX mean free path of
  track regardless of density) — voltage and current set the rate, which
  matches fusor lore.
- **M2 — discrete adjoint. DONE** (`adjoint::yield_gradient`). Reverse-mode
  gradient of the ensemble yield w.r.t. every ring potential, the wall
  potential, and every coil's ampere-turns: backprop through the Boris
  loop (fixed-step self-consistent forward, full trajectory storage),
  PIC-style adjoint deposits into the potential grid through the exact
  bilinear-patch weights, then one **adjoint Poisson solve** using the
  radial-weight symmetrization of the axisymmetric operator (`w_i = r_i`,
  axis row `Δr/8` — the adjoint system reuses the forward SOR stencil with
  a RHS). Coil gradients via ⟨λ_B, B(I=1)⟩ (loop field linear in current).
  FD-validated end-to-end: dJ/dI to 0.1%, dJ/dV to 0.8%, plus a gauge test
  (Σ over all conductors of dJ/dV ≈ 0, since only potential differences
  matter). Two validation lessons preserved in the tests: (1) freeze the
  time discretization against a reference drop when comparing across
  parameter perturbations, or the integration window becomes a hidden
  parameter; (2) J is genuinely rough on long horizons (cusp-lens chaos) —
  FD at practical h under-reads by 3–4× there, which is precisely the
  regime where the adjoint is the only trustworthy gradient.
  `optimize::maximize_with_gradient` consumes it (same line search, no FD
  probes). Shape parameters (ring position/radius) remain FD/hybrid — the
  Dirichlet mask is discrete; smooth shape adjoints are the M3+ seam
  (boundary-value differentiation on a frozen mask).
- **M3 — parameter seam. DONE** (`spec::DeviceSpec`). Serde schema in
  which every numeric field is a literal **or a named document parameter**;
  fail-closed resolution (unbound name = error, never a default);
  `parameter_roles()` classifies each name by gradient path (potentials +
  ampere-turns → adjoint; geometry → FD, since it moves the discrete
  Dirichlet mask). JSON round-trip, fail-closed, and role-classification
  tests. BRep extraction (revolved sketch sections → `RingSpec`s) lands on
  the vcad side of the seam, emitting this schema — same division of labor
  as `document_parameter_gradient`.
- **M4 — receipt claims. DONE (kernel side)** (`receipt::predicted_claims`).
  Serializable claim set (`vcad.particle-claims/1`): interception,
  recirculation, transparency, neutron rate, fusion power (both D-D
  branches — the trace now integrates D(d,p)T alongside D(d,n)³He), input
  power, `q_estimate`, and **`distance_to_lawson` = log10(1/Q)** — with
  full provenance (grid, SOR tolerance + sweeps, ensemble, censoring,
  physics channels included) and spelled-out caveats on every claim.
  Fail-closed: a no-fusion device reads 99 orders, never a divide-by-zero
  or a silent omission. The classic fusor's card: Q = 4.1×10⁻¹⁰, distance
  9.39 orders — "microwatts, fully audited," now machine-readable.
  **Follow-up PR (flagged, not started):** register the family in
  `crates/vcad-receipt` (`ir:gen` exports two crates; names must be
  unique), and expose `simulate_charged_particles` /
  `optimize_electrodes` MCP tools — cross-crate schema + TS codegen +
  fixture regen, deliberately not done from this worktree.
- **M5 — analytic benchmarks + convergence + paper draft. DONE (except
  the external SIMION runs).** `tests/analytic.rs`: magnetic-mirror loss
  cone honored end-to-end (with the instructive non-adiabatic failure mode
  when r_L approaches the field scale), axial oscillation period vs the
  solved well curvature (±20%, anharmonicity-limited; long traces expose a
  slow amplitude leak from cell-crossing kicks — measured, commented, and
  a future symplectic-refinement item). `examples/convergence.rs`: the
  interception FoM tracks the 0.75·h mask floor until h ≲ wire radius —
  thin-wire absolute yields are trends until then; 3 mm-wire headline
  sweeps are inside the converged regime. Paper draft with all current
  numbers: `docs/particle-optics-paper-draft.md`. SIMION cross-validation
  needs SIMION — external, flagged.
- **M6 — experiment pack. DONE.** `docs/shielded-grid-experiment.md`:
  full COTS BOM (~$10–25k, Phase A alone $2–5k), the floating-pulser
  isolation problem named as the hard engineering item, staged
  commissioning bound to the simulated curves (Phase A: the −3 kV ammeter
  curve with a kA·t pulser and no neutron gear; Phase B: neutrons at
  −30…−40 kV), and the safety envelope. `receipt::compare` binds bench
  measurements to predicted claims with Holds / Violated / Unmeasured
  verdicts — fail-closed: an unmeasured receipt never passes, a
  measurement matching no claim is an error, and Violated is treated as a
  publishable result about the model.
- **M7 — a yield prediction that can lose. DONE (kernel + example).**
  See [the section below](#m7-the-two-sided-fusor-yield). Integrator
  fixed, cited collision data, CX chain, ionization source, supply-current
  partition, self-consistent space charge, and one banded
  `ddn_neutron_rate` claim that `receipt::compare` can judge Violated on
  either side.

## M7: the two-sided fusor yield

**The problem (open-fusor blocker #2).** Before M7 the crate priced a gas
fusor as a bracket: CX off (unphysical at mTorr), and CX on with a single
generation (an ion dies at its first charge exchange). Both edges were
floors on their own physics. The *ion* current was also a free input
that no meter reads. For the open-fusor hobby machine (6" chamber, 1.5"
grid, −30 kV, 10 mA, 5 mTorr) that gave 2.5×10⁴ to ≥4.4×10⁵ n/s, and
no measurement could violate it.

**What changed** (`src/chain.rs`, `src/xsection.rs`, `src/trace.rs`,
`src/receipt.rs`, `examples/open_fusor_band.rs`):

1. **Integrator.** The staggered Boris leapfrog is first order once the
   adaptive `dt` changes from step to step, and its drift diagnostic mixed
   half-step velocities with integer-step positions. At h = 0.5 mm the max
   drift halved with each halving of `step_fraction` (26% → 19% → 9%).
   Most of the old ceiling's "grid non-convergence" (1.8 → 3.3 →
   4.4×10⁵) was this step error. At h = 0.25 mm, ⟨σv⟩ is converged at
   `step_fraction` ≤ 0.125. The new default, `Integrator::Synchronized`,
   is kick(½)–rotate(½)–drift–rotate(½)–kick(½) with a speed-change cap.
   Max drift in the chain at production resolution is ≤ 0.3% (was 19%).
   The change applies to every caller. Benchmark numbers earlier in this
   document predate it.
2. **Cited collision data, applied at equal velocity.**
   - σ_cx(D⁺+D₂): Janev–Reiter–Samm, Jül-4105 Eq. 145. It peaks at
     1.2×10⁻¹⁹ m² near 5 keV/amu and is 7.4×10⁻²⁰ at the 30 keV D⁺ point.
   - Ion-impact ionization, D₂⁺ CX and ionization, and D₃⁺ formation:
     HYDHEL H.1 3.2.5, 4.3.1–4.3.3.
   - Breakup of fast D₂⁺/D₃⁺: Jül-4105 Eqs. 209 and 243.
   - e⁻ + D₂ ionization: Kim–Rudd BEB.
   - The CX target is the D₂ *molecule*. The legacy `CxModel` uses the
     deuteron density, which double-counts attrition. It is kept as is
     for the wasm/MCP surface and documented.
3. **CX chain.** Each charge exchange relaunches a cold D₂⁺ from where it
   happened. Waves are summed *unnormalized* until the relaunch weight per
   source ion falls below 2×10⁻³ (the remainder is reported as
   `truncated`). The fast neutral marches straight. It fuses in the gas,
   ionizes, and either strikes a cathode wire (secondary electrons) or
   buries itself in the wall (wall-embedded fusion, priced against
   `beam_target`'s Ti-drive-in calibration at a stated relative loading).
4. **Ionization source.** Ion-, fast-neutral- and electron-impact
   ionization (cathode secondaries and ionization electrons are traced in
   the field) births the next *generation*. The source is iterated to its
   fixed point. Renormalizing CX births together with ionization births
   converges to a spurious cold, core-weighted mode, so the two are kept
   apart. The model reports **sustainment M** = ionization births per ion
   lost. A self-sustained discharge has M = 1.
5. **Species.** In a pure E field a singly charged ion launched at rest
   follows a mass-independent trajectory, so one trace carries D⁺/D₂⁺/D₃⁺
   weights and applies every rate per unit path. Born ions are D₂⁺ (plus
   a stated D⁺ fraction). D₂⁺ → D₃⁺ conversion is modeled. Fast molecular
   breakup spawns a D⁺ fragment on its own trajectory. Devices with coil
   current are refused (`ChainError::Magnetics`).
6. **Supply-current partition.** `I_supply = e·S·(c_ion + γ₁₅·u_se)`. The
   cathode collects ion arrivals plus the secondary electrons they (and
   fast neutrals on the wires) knock out, with γ_SE ∝ √(E/amu) and
   molecular additivity. The meter reading fixes the birth rate S, and
   the cathode ion current is a *derived* claim.
7. **Space charge.** The ion dwell from all waves, scaled by S, feeds
   `space_charge::grounded_potential`. The chain is re-run in applied +
   beam fields, with under-relaxed iterations. The central value carries
   the correction.
8. **The band** (`chain::predict` → `receipt::yield_band_claims`). The
   result is one rate, a lower edge and an upper edge. The rule is stated,
   not tuned:
   - Numerical terms (sampler scatter from a shifted stratified sampler,
     grid, step) and input terms (meter current ±2%, voltage ±2%, pressure
     ±15%, every cited σ ±25%) add in log-quadrature within their bucket.
   - Model-form terms are a linear envelope: γ_SE 0.6…2.4 at 15 keV/amu,
     born D⁺ fraction 0…0.3, D₃⁺ formation, D₃⁺ breakup-fit frame, wall
     loading 0…0.02, space charge vs. electron-neutralized, and **source
     shape**. The source-shape term weights the unmodeled |1 − M| of ion
     production between uniform-volume and edge-born sources.
   - Knob reruns are warm-started from the central fixed point and share
     its sample points. A rerun excursion below the measured sampling
     scatter is noise and is removed in quadrature.
   - `Claim.band` makes `compare` judge the claim against the prediction's
     own band, widened only by the measurement's uncertainty. The
     measurer's `band_factor` is ignored for banded claims.

**The prediction** (`cargo run --release -p vcad-kernel-particle
--example open_fusor_band`, mew; 10 mA *supply* current, 5 mTorr,
4-ring 2 mm-wire grid, 305×609 grid at h = 0.25 mm, 128 ions per CX wave):

Raw output: [`particle-optics-m7-open-fusor-band.csv`](particle-optics-m7-open-fusor-band.csv)
(commit `9f71fb60f`, 52 min on mew under load ~50).

| −kV | lower n/s | **central n/s** | upper n/s | upper/central | cathode ion mA | M | space charge | ion-gas / fast-neutral / wall |
|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 20 | 5.5 | **2.4×10²** | 4.0×10³ | ×17 | 6.2 | 0.055 | 0.044 | 0.43 / 0.56 / 0.01 |
| 25 | 30 | **8.1×10²** | 1.2×10⁴ | ×15 | 6.0 | 0.071 | 0.030 | 0.44 / 0.55 / 0.01 |
| 30 | 78 | **1.9×10³** | 2.2×10⁴ | ×11 | 5.8 | 0.090 | 0.022 | 0.46 / 0.53 / 0.01 |
| 35 | 1.9×10² | **3.7×10³** | 5.0×10⁴ | ×14 | 5.6 | 0.101 | 0.018 | 0.48 / 0.50 / 0.02 |
| 40 | 3.4×10² | **6.0×10³** | 6.5×10⁴ | ×11 | 5.4 | 0.108 | 0.016 | 0.50 / 0.49 / 0.02 |

Every point converged (one at −25 kV hit the 6-generation cap at 5%
spread). Max per-trace energy drift is ≤ 0.3%, and censored plus
truncated weight is < 0.3%.

Band components at −30 kV, as factors on the central at the low and high
setting of each knob (from the CSV):

| term | kind | low setting | high setting |
|---|---|---:|---:|
| sampling (shifted-sampler RMS 0.081) | numerical | ×0.92 | ×1.09 |
| grid (153×305 vs 305×609) | numerical | ×0.99 | ×1.01 |
| integrator step (½) | numerical | ×0.93 | ×1.08 |
| supply current ±2% | input | ×0.98 | ×1.02 |
| pressure ±15% | input | ×0.87 | ×1.15 |
| voltage ±2% (d ln R/d ln V = 4.4) | input | ×0.92 | ×1.09 |
| σ_cx ±25% | input | ×1.13 | ×0.90 |
| σ_ionization ±25% | input | ×1.01 | ×0.98 |
| σ_breakup ±25% | input | ×0.90 | ×1.07 |
| γ_SE 0.6…2.4 | model form | ×1.26 | ×0.71 |
| wall loading 0…0.02 | model form | ×0.99 | ×1.08 |
| born D⁺ fraction 0…0.3 | model form | ×0.77 | ×2.08 |
| D₃⁺ formation toggled | model form | — | ×1.08 |
| D₃⁺ breakup-fit frame | model form | — | ×1.07 |
| **source shape** (|1−M| = 0.91: cold / uniform / edge) | model form | **×0.095** | **×3.2** |
| space charge vs neutralized | model form | — | ×1.08 |

**Reading it.**

- **The model does not explain how the discharge sustains itself, and
  that sets the width.** Modeled ionization (ion, fast-neutral and
  electron impact) creates only **M ≈ 0.06–0.11** of the ions the supply
  current says are lost. The other ~90% come from something the model
  does not carry, and where they are born decides how much energy they
  fall through. Ions born in the cathode fall (cold) barely fuse
  (×0.095). Ions born uniformly in the volume, or at the wall (hot), fuse
  about 3× more. That single term dominates both edges. Born-species
  composition is second (×2 up), then γ_SE (±25–30%). Every
  measured-input and numerical term is ≤ 15%. The band is wider than 10×,
  and that is the honest state of the physics, not a tuning choice.
- **It can lose, mostly from above.** At −30 kV the upper edge is
  2.2×10⁴ n/s. A hobby machine reading 10⁵ n/s at 30 kV / 10 mA, which
  is a commonly reported amateur level, returns **Violated**, and that
  would be a publishable statement that a channel is missing. The lower
  edge (78 n/s) sits below practical detection, so in practice the
  falsifiable side is the top.
- **The central is not comparable to the old 2.5×10⁴ "floor".** That
  number took 10 mA as the *ion* current, born atomic D⁺ at the wall with
  the full drop, and applied σ_cx to the deuteron density. Now 10 mA on
  the meter is ~5.8 mA of ions at the cathode (γ_SE partition). Born ions
  are molecular. Most cathode arrivals are cold CX relaunches that cost
  supply current and barely fuse (mean cathode-impact energy ≈ 5 keV at
  −30 kV).
- **What would shrink it.** A measurement of where the discharge is
  ionized (an emission profile, or the current–voltage–pressure
  characteristic that sustainment must reproduce) pins the source shape.
  A mass-resolved ion species measurement pins the born D⁺ fraction. A
  γ_SE value for the actual grid metal tightens the partition. Each
  measurement would remove one row of the table above.

**Known limits, all carried in the band or stated here:**
- γ_SE magnitude: no primary table was read; the range is wide on purpose.
- Fast-neutral impact ionization uses the ion fit at equal velocity.
- D₃⁺ collision data borrow from D₂⁺.
- Neutral stripping and electron-of-electron multiplication are not
  followed (each is ≲ 10⁻² per particle).
- Cold ions trapped inside the cathode are tallied, not traced, and their
  dwell is not deposited.
- The cathode is assumed hot, so it retains no deuterium. Beam–beam
  fusion is neglected.
- The geometry is still the ring stack. The other open-fusor session owns
  3D grids.

## Non-goals

This crate does not claim a path to net energy gain. It quantifies,
per-geometry, the loss channels that electrode design can and cannot
close — and at M4 it will say so on a receipt.
