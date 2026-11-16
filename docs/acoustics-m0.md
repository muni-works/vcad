# Air-side acoustics M0: Helmholtz field solver, lumped oracles, baffled-piston radiation

`vcad-kernel-acoustics` makes vcad a design tool for the **air** in an acoustic
device — the pressure field inside cavities, ports, horns and boxes. It is the
missing half of the acoustics loop the workspace already proved once with the
glockenspiel: `simulate_strike` models how a *solid bar* vibrates (Euler–
Bernoulli / Hermite beam modes) and was verified against a microphone to
−5 cents. That is the *structural* side. This crate is the *air* side, and the
two meet at one boundary condition —

> structural mode shape → surface normal velocity `v_n(x)` → Neumann datum for
> the air-side Helmholtz solve → radiated pressure field

— **surface velocity in, pressure field out**. Wiring the two together (a
vibrating cone or bar radiating into a modelled room) is M2; M0 states the seam
and keeps the solvers independent.

The domain is one where the tooling is a mess: loudspeaker enclosure design,
Helmholtz resonators, ducts and vents. Hobbyists stitch together scattered
one-off calculators; the "serious" options are 1990s freeware (Hornresp,
BassBox) or five-figure FEA. The ground truth here is cheap and unarguable:
**closed-form resonances and a $20 measurement microphone**.

## M0 scope (and honesty)

**In scope:** linear, lossless, time-harmonic acoustics.

- **Helmholtz field solve.** `(∇²+k²)p = −jωρ·s` on an axisymmetric (r, z)
  grid, `k = ω/c`. Vertex-centred **finite volume**: integrating the operator
  over each node's control volume makes the assembled matrix conservative,
  symmetric, and exact on the axis (the radial cell area vanishes as `r → 0`,
  reproducing the `2·∂²p/∂r²` limit with no special-casing — the same
  r-weighted stencil discipline as `vcad-kernel-particle`'s Poisson solve).
  Rigid (Neumann), pressure-release (Dirichlet), and impedance boundary
  conditions; interior monopole and driven-piston sources.
- **Direct solve, not relaxation.** The Helmholtz operator is **indefinite**
  (its spectrum straddles zero; it is exactly singular at every resonance), so
  SOR — the electrostatic-Poisson workhorse next door — would diverge. The
  block-tridiagonal system is solved by block-Thomas with dense complex LU per
  slab. A resonance sitting exactly on the sampled frequency reports
  `Singular` rather than a garbage field; sweeps nudge off the pole.
- **Lumped oracles.** Duct acoustic mass `M_A = ρL_eff/S`, cavity compliance
  `C_A = V/(ρc²)`, Helmholtz / bass-reflex tuning
  `f_b = (c/2π)√(S/(V·L_eff))` with end corrections (Beranek/Kinsler). Both a
  feature (the number a designer wants) and the field solver's validation
  target.
- **Radiation.** The baffled circular piston — on-axis pressure
  `|p| = 2ρc·U·|sin((k/2)(√(z²+a²)−z))|`, far-field directivity
  `|2J₁(ka·sinθ)/(ka·sinθ)|`, and a numerical Rayleigh integral checked
  against both.
- **Figures of merit + optimizer.** Resonance frequencies (from sweeps), axial
  mode shapes, port volume velocity, on-axis response; a box-constrained
  finite-difference maximizer (the `vcad-kernel-particle` pattern) that sizes a
  port for a target tuning.
- **The `.vcad` seam + receipt.** A serde `CavitySpec` with named parameters
  (fail-closed resolution) and the `vcad.acoustics-claims/1` predicted claim
  family.

**Out of scope at M0** (each a milestone below): thermoviscous and radiation
losses (so Q is optimistic); a radiation-impedance / PML mouth (the open end is
a crude pressure-release plane); structural↔air coupling; non-axisymmetric
geometry; absorption materials and room modes; the field adjoint.

**Regime of validity:** below the first cross-mode, where the lumped picture
and the axisymmetric field agree — exactly the regime of enclosure and
resonator design. Do not read broadband room acoustics or high-frequency horn
directivity out of these solves yet.

## Validation ladder (all in `cargo test -p vcad-kernel-acoustics`)

- **Air properties** against the textbook relations (`c ≈ 343.2 m/s`,
  `ρ ≈ 1.204`, `ρc ≈ 413` rayl at 20 °C; Kinsler & Frey §5).
- **Lumped self-consistency**: the compact Helmholtz form equals mass ×
  compliance composed; `∝ 1/√V`, `∝ √S` scaling exact.
- **Baffled piston** (`radiation`): the numeric Rayleigh integral recovers the
  analytic on-axis pressure (< 2%), tracks the `J₁` directivity off-axis
  (< 5%), and nulls at `ka·sinθ = 3.8317` (first zero of `J₁`); `J₁` itself
  against Abramowitz & Stegun tabulated values to 10⁻⁶.
- **Rigid closed cylinder axial modes** (`tests/analytic.rs`): the field-solved
  resonances land on `fₙ = n·c/2L` — **mode 1 to 0.10%, mode 2 to 0.04%** at
  a 9×137 grid.
- **Helmholtz resonator**: the field-solved fundamental falls in the
  end-correction band around the lumped formula (see the honesty note below).
- **Reciprocity**: swapping source and receiver leaves the transfer function
  unchanged to **4.5×10⁻¹⁶** — the payoff of the symmetric FV assembly, and the
  discretisation's conscience.
- **Impedance absorption**: a matched termination (`β = ρc/Z = 1`) cuts the
  resonant response a nearly-rigid end (`β = 0.02`) sustains by **~50×**, and
  every solve stays finite — the imaginary diagonal term regularizes the pole.
  This is the boundary M1's absorptive materials and radiating mouth ride on.
- **Grid convergence**: the axial-mode error falls **second order** — 0.104% →
  0.025% → 0.005% over `dz` = 17 → 8.5 → 4.25 mm (19× over 4× refinement),
  with the **0.005% floor named**.

## The flagship: the ported box a thousand builders design in freeware

`cargo run --release -p vcad-kernel-acoustics --example ported_box`

A bass-reflex loudspeaker enclosure — driver piston, sealed box, vent — all
axisymmetric. A 9.4 L box with a 25 mm-radius port:

1. **Lumped tuning.** At a 120 mm port the Thiele–Small-style formula gives
   `f_b` = 61.9 / 63.0 / 66.3 Hz (min/nominal/max across the end-correction
   band).
2. **Field confirmation.** The driven field sweep puts the port (Helmholtz)
   resonance at **72.4 Hz** — the pressure-release mouth omits the exterior
   radiation mass, so it reads ~15% high (see honesty). The port volume
   velocity peaks sharply at `f_b`, the defining bass-reflex signature.
3. **The optimizer sizes the port.** Targeting a 45 Hz tuning, the
   finite-difference optimizer lengthens the port **120 → 339 mm** in 35
   evaluations, retuning the field solve **72.4 → 45.0 Hz** (residual 0.14 Hz)
   — the design loop closed against the sim, not a formula.
4. **The receipt.** `vcad.acoustics-claims/1` with tuning, mode frequency, and
   port-mouth response, basis `predicted` → the unified receipt rolls up
   **Provisional, never Pass**. The measurement schema names a calibrated
   measurement microphone + swept sine as the closing instruments.

## Honesty: where the numbers are optimistic, and by how much

- **Lossless ⇒ Q is fiction.** No thermoviscous or radiation damping. The
  undamped operator is *singular* at resonance (infinite response); real ports
  have finite, often modest Q. Every tuning claim is trustworthy; every Q or
  peak-height is an upper bound, and the claim note says so.
- **The pressure-release mouth reads ~15% high.** Pinning `p = 0` at the
  geometric mouth omits the exterior radiation mass (the `0.6–0.85·a` end
  correction) and, at M0 neck resolution, under-resolves the interior added
  mass at the neck–cavity junction — so the field tuning lands above the fully
  end-corrected lumped value (measured: resonator +18% on nominal / +11% above
  the interior-only bound; ported box +15%). This is a *known BC gap*, not a
  bug: a radiation-impedance mouth (M1) closes it. The lumped band is reported
  alongside every field tuning so the gap is always visible.
- **Axisymmetric only.** Rectangular boxes, offset ports, and multi-driver
  layouts are not representable; the first non-axisymmetric mode is
  invisible. Hand-built specs can still *approximate* a box as a body of
  revolution, but the document extractor below refuses to: it fails closed
  on any non-axisymmetric face rather than folding a box into a cylinder.

## From a document: `simulate_cavity` (internal ports, no hand mapping)

`vcad_eval::cavity_extract::extract_cavity` + `vcad_kernel_acoustics::ported::analyze`,
over MCP as `simulate_cavity` (WASM `simulateCavity`). Point it at two parts
of a session document — the **enclosure** walls and the enclosed **air** — and
it measures their analytic BRep faces, reconstructs the exact axisymmetric
profile, and solves it. The worked case is the "every physics" puck
(`crates/vcad-eval/tests/fixtures/puck.loon`, a copy of
`docs/videos/every-physics/puck.loon`): a 104 mm-bore round box, 3" driver
firing down through a 35 mm cutout, and a 12 mm × 60 mm port **hanging inside**
the box from the top wall.

**Internal ports are modelled, not mapped.** `Cavity` grew rigid annular
**obstacles** (`Obstacle`, `CavitySpec.obstacles`): the port tube's wall is a
solid annulus `12 < r < 14` hanging 57 mm into the box, so the bore's air
column couples to the annular box air only at its open hanging end — the real
topology. (Before this, the solver could only put a port *outside* the box,
and the every-physics PoC hand-mapped the puck to an equivalent-volume
cylinder with an external port.) The lumped band is mount-aware
(`PortMount::Internal`: interior end unflanged, mouth flanged by the face);
only `f_max` moves, since the nominal is one flanged + one unflanged end
either way.

**Puck numbers** (20 °C, 1 mm grid, 53 × 118 nodes):

| quantity | value |
|---|---|
| `V_box` (extracted = air solid, exact profile) | 933,312 mm³ = 0.933 L (π(52²·114 − 14²·57)); mesh volume agrees to < 1% |
| port | r = 12 mm, L = 60 mm, internal |
| lumped band (min / nominal / max) | 134.1 / **136.6** / 146.5 Hz — nominal identical to the PoC's 136.6 Hz |
| field `f_b`, internal port from the document | **151.0 Hz** (1.105 × nominal) |
| PoC equivalent-volume external-port mapping, same 1 mm grid | 150.5 Hz |
| PoC mapping on its own 2 mm radial grid | 155.5 Hz |

So for this geometry the equivalent-volume mapping was *not* the error that
mattered (0.3% from the exact internal-port model on the same grid); the
PoC's 155.5 Hz was mostly its coarse radial grid inflating the 12 mm bore.
The field reads 10.5% above the lumped nominal and 3% above the internal
band top — the known pressure-release-mouth gap (honesty, above).

**Honest limits of the extraction:**

- **What is accepted.** Every face of both parts must be a plane
  perpendicular to the axis or a cylinder coaxial with it (axis = the
  enclosure's largest cylinder). Faces like that can only bound full annuli
  and full cylinder bands, so a part that passes is exactly axisymmetric and
  its (r, z) cell profile is exact — classified by ray parity against the
  planar faces' annuli, cross-checked against the tessellated mesh volume
  (3% gate). Cones, spheres, tori (fillets, flared ports, domed caps) are
  refused today even when coaxial; a sloped profile would need a sloped
  segment the solver doesn't have.
- **Fail-closed, named.** A non-axisymmetric face errors with the part,
  face index, surface kind and a point on it. Air that doesn't fill the
  enclosure (a sideways gap), a side vent or annular slot, a second port, a
  sealed air part, enclosure/air overlap, a port mouth recessed inside the
  box's axial extent, or driver and port on the same end each error naming
  where.
- **The driver plugs its cutout.** The cutout cells are not fluid; the
  piston sits flush with the inner face of the bottom wall. Piston radius is
  a document parameter (kept named, so overrides re-price) or a literal;
  default is the cutout radius, stated in the assumptions. The driver itself
  (cone mass, suspension, voice-coil — Thiele–Small) is not modelled: this is
  the box + port tuning `f_b`, not the loaded system response.
- **Openings.** One port passage (a disk on the axis, walled by enclosure,
  from the air to the outside) and at most one driver cutout on the opposite
  end face. If the port vents downward the profile is mirrored so the mouth
  is the top of the stack.
- **Grid.** Default spacing is half the smallest feature (here the 2 mm port
  wall → 1 mm), so every obstacle holds a node; a grid that would drop a wall
  is refused (`SolveError::ObstacleUnresolved`), never solved leaky. Obstacles
  use the vertex grid's ±dr/2 face convention (strict radial test, inclusive
  axial), so wall thickness and bore radius carry an O(dr) error.
- **Everything in the M0 honesty list still applies**: lossless (Q and the
  port-velocity peak height are upper bounds), rigid walls, pressure-release
  mouth (field `f_b` reads high), no radiation into the room.

## Milestone ladder

- **M0 — this PR.** FV Helmholtz solve (rigid/pressure-release/impedance),
  block-Thomas direct solver, lumped oracles, baffled-piston radiation,
  figures of merit, port-sizing optimizer, `CavitySpec` seam,
  `vcad.acoustics-claims/1`. Validation: cylinder axial modes, Helmholtz band,
  reciprocity, grid convergence, piston directivity. Flagship: `ported_box`.
- **M1 — losses + absorption + room modes.** A radiation-impedance / PML mouth
  (closing the ~15% BC gap and letting the interior field radiate); a
  boundary-admittance model for absorptive materials with a real port Q; a
  rectangular-room mode example (`fₙₗₘ = (c/2)√((n/Lₓ)²+(l/L_y)²+(m/L_z)²)` as
  the oracle). Register `vcad.acoustics-claims/1` in `crates/vcad-receipt` and
  expose `simulate_acoustics` / `size_port` MCP tools.
- **M2 — structural↔air coupling.** Consume `simulate_strike`'s mode shapes as
  surface velocity BCs; radiate a struck bar / driven cone into the modelled
  field. The seam this M0 states, wired.
- **M3 — the field adjoint + measurement pack.** Reverse-mode gradient of a
  response objective through the block solve (the `optimize::maximize` FD
  stand-in is API-shaped for the swap); a COTS bench pack (calibrated mic,
  swept-sine, a printed resonator) binding predictions to measurements — the
  glockenspiel loop, closed on the air side.

## Non-goals

This crate does not claim broadband room-acoustics or auralization fidelity. It
prices, per geometry, the resonances and tuning that enclosure and resonator
design turn on — and it says on a receipt exactly which of its numbers are
trustworthy (tuning) and which are optimistic (Q), and by how much.
