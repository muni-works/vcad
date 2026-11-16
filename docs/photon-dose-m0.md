# Photon dose M0: bremsstrahlung x-rays and capture gammas

`crates/vcad-kernel-photon-dose`. This closes blocker #1 of the open-fusor
audit (`docs/videos/open-fusor.md`, §1 and §5): vcad had no photon transport,
so the dominant hazard of a 30–40 kV fusor (bremsstrahlung through the glass
viewport, where the camera wants to sit) and the 2.22 MeV capture gammas from
the HDPE shield were both unmodeled. The safety case was neutron-only.

It's a new crate rather than a module in `vcad-kernel-neutronics`. The physics,
data and estimators are different. Another session is reworking neutronics
geometry in parallel, so the two stay apart. The dependency runs one way:
photon-dose reads neutronics `RunResult`/`ShieldSpec`, and neutronics is
unchanged. Nothing went into `tang`: the numerics are a few bisections and
a 4-point Gauss–Legendre, which isn't general enough to earn a home there.

## What it computes

| piece | model | validity / band |
|---|---|---|
| bremsstrahlung source | Kramers thick target: dN/dE = (2η/T₀)(T₀−E)/E per electron, η = kZV, k = 1.1×10⁻⁹ V⁻¹, Z = electron-weighted Z of the target, isotropic over 4π | 10–150 kV enforced, outside fails closed. Yield/shape band ×/÷1.5 (published k 0.9–1.4×10⁻⁹, Kramers shape ±20–30%) |
| interaction data | NIST SRD 126 (Hubbell & Seltzer) elemental μ/ρ, μ_en/ρ for H B C N O Na Al Si K Cr Mn Fe Ni Pb, 5 keV–3 MeV, edges included, transcribed verbatim, log-log interpolation | outside the window fails closed. Per-material μ uncertainty 5% (glass 8%, nominal lead glass 15%) |
| materials | ss304, aluminum, lead, borosilicate, lead-glass (nominal 65 wt% PbO, 4.8 g/cc), hdpe, borated-hdpe-5, paraffin, water, air | unknown names fail closed |
| transport | infinite slab stack, point source on the inner face, detectors on axis past an attenuate-only air gap (≥10 mm). Exact uncollided term (per bin, GL4 in energy) plus a next-event Monte Carlo scatter term (Klein–Nishina, implicit capture, roulette, importance-sampled source energies). Buildup is computed, not tabulated | coherent/photo/pair are removal (no fluorescence, no 511 keV), free-electron KN, no room return. Scatter below 10 keV is killed |
| dose | ICRP-74 photon H*(10)/Φ and K_air/Φ, 10 keV–10 MeV, **transcribed at design-estimate precision** (not machine-read). The kerma column is cross-checked against NIST μ_en; the H*(10) column at 10–40 keV has no independent check yet and should be verified against ICRP 74 / ICRU 57 | H*(10) is deep dose; air kerma rides along as the shallow-dose proxy (H′(0.07)/K_a ≈ 0.95–1.6) |
| chamber model | uniform electron loading of a spherical wall. The wall path integrates the uniform-shell source exactly over emission angle, using each ray's true oblique path through the wall (L = √((a+t)² − a²sin²α) − a\|cos α\|), times the slab MC's buildup. The viewport path is two equivalent point sources at the window: glass-struck electrons (× `beaming_factor`) and the far wall seen through the aperture (radiance invariance → f·S_steel, f = r²/4a²) | geometry band ×/÷2 (star-mode jets can concentrate loading) |
| capture gammas | per-region capture rates from the neutronics flux tally × the library's own σ_a × restated number densities (checked against the neutronics Σ_a at runtime; drift fails closed). H 2223 keV, ¹⁰B 478 keV transported. C/O/Pb lines above 3 MeV get an unattenuated one-photon-at-Q bound. Captures are placed at each region's outer radius | library band ×/÷1.3. Concrete or fiction materials fail closed. Air (n,γ) is a caveat |

Every dose is a `tally::Estimate` (mean ± MC relative standard error) **and**
a named systematic band composed in log-quadrature from the source, geometry,
per-slab data and spectral-tip terms:

- the data term is u·τ per slab, so thick shields widen their own band;
- the tip term (×/÷1.5) switches on when more than half the transmitted
  H*(10) comes from the top 10% of the spectrum, which is exactly when the
  Kramers endpoint shape carries the answer.

Running more histories shrinks the error, never the band. Only a survey does.

## Fail-closed rules

- Voltage outside 10–150 kV, current ≤ 0, electron fraction outside (0, 1]:
  error.
- A detector inside the stack or within 10 mm of it: error. This keeps the
  next-event estimator's variance bounded.
- **Soft leak:** a continuous spectrum whose 10 keV end is transmitted
  above 10⁻³ is refused. The dose table's 10 keV floor would otherwise hide
  soft dose behind a thin window.
- Claims (`vcad.photon-claims/1`) refuse truncated histories, a scattering
  run that scored nothing (buildup unresolved), or an infinite-RSE dose.
- Unified-receipt claims are always `Predicted`, so a receipt rolls up
  **Provisional, never Pass**.
- `compare()`: a measurement matching no claim is an error, and an unmeasured
  claim never passes.

## Validation (tests on mew, all green)

Analytic:

| check | result |
|---|---|
| uncollided = S·e^{−μt−μ_air·gap}/4πR² · h(E) | exact to 1e-12, rse = 0 |
| shell integral: μ→0 gives S/(8πaR)·ln((R+a)/(R−a)); far field gives S·E₂(μt)/4πR² | 1e-4 / 2% |
| T(2t) = T(t)² | 1e-9 |
| inverse square ×4·e^{μ_air Δr} at 0.5/1/2 m | 1e-9 |
| Klein–Nishina: ∫dσ/dΩ = σ; Thomson limit; ×(Z/A)N_A = NIST hydrogen above 200 keV | 1e-5 / 1e-4 / 1% |
| water compound from elements vs NIST water table (30 keV, 100 keV, 1 MeV) | < 1% |
| ICRP K/Φ vs E·(μ_en/ρ)_air from the bundled NIST data, 100 keV–3 MeV | < 6%. Below 100 keV the bundled air (Ar folded into N) reads 10–17% low, which is the conservative direction, pinned by a test |
| Kramers ∫E dN = ηT₀; binned spectrum = closed form | 1e-3 / 1e-12 |
| 120 vs 480 spectrum bins | < 1% |
| same seed bit-identical; different seeds agree within 4σ | ✓ |

Published values (reference numbers are quoted from the standard tables
at design-estimate precision, not machine-read):

| check | vcad | reference |
|---|---|---|
| Pb buildup, 1 MeV, 1 mfp (slab MC) | **1.367** | ANS-6.4.3 point-isotropic, infinite medium: 1.37 |
| iron/SS304 buildup, 1 MeV, 1 mfp | 1.793 | ANS-6.4.3 iron: 1.87 (a finite slab with void behind sits below infinite medium) |
| water buildup, 1 MeV, 2 mfp | 2.88 | ANS-6.4.3: 3.71 (infinite medium, upper bound) |
| Pb HVL, Cs-137, narrow / broad (MC) | 5.50 / 7.36 mm | handbook 6.5 mm |
| Pb first broad HVL, Co-60 | 15.6 mm | ANS buildup B(1 mfp) ≈ 1.36 ⇒ ≈ 1.0 mfp ≈ 15 mm. The handbook's "12 mm" is the deep-penetration convention (TVL 40 mm / 3.32) |
| Pb hardened HVL, 100 kVp + 2.5 mm Al | **0.245 mm** | NCRP-49/147 barrier tables: 0.25–0.27 mm. Set by the transmission window just below the Pb K edge, so it's a sharp test of the edge data plus Kramers shape |
| Pb hardened HVL, 50 kVp | 0.051 mm | tables ≈ 0.06 mm |
| Al first HVL, 100 kVp + 2.5 mm Al | 2.60 mm | tube data 2.5–4 mm Al |
| 8 mm lead glass, Pb equivalence at 40 kV | 2.09 mm Pb | grade-dependent. Use the vendor sheet |
| HDPE capture γ / neutron dose, 15 cm, 1 m | 19% | literature 5–30% |
| captures ≤ neutronics absorbed fraction × S | ✓ | books balance across crates |

Two first drafts of the "published HVL" tests compared different conventions:
first narrow-beam HVL against barrier-table hardened HVLs, and first broad
HVL against deep-penetration handbook HVLs. The tests now compare like with
like, and the derivations sit in the test comments. The physics didn't change.

## The example: 6″ SS fusor, −20…−40 kV, 10 mA

`cargo run --release -p vcad-kernel-photon-dose --example fusor_xray` (run on
mew, seed 20261116, 20 × 20 000 histories per run).

Setup:

- 6″ SS304 sphere, inner radius 76 mm, 1.65 mm wall (0.065″ hemispheres);
- 2.75″ CF viewport, 19 mm clear radius, 4.8 mm borosilicate;
- 10 mA supply, **electron fraction 1.0**: every milliamp reaches the wall
  as electrons at full energy. That's the bound, and dose is linear in it;
- uniform wall loading, `beaming_factor` 1.

H*(10) µSv/h, mean (MC rse < 0.3% everywhere), with the **band upper edge**
in brackets. The upper edge is the number a safety case uses.

**On the viewport axis**, distance from the window's outer face:

| window | kV | 0.5 m | 1 m | 2 m |
|---|---:|---:|---:|---:|
| bare glass (4.8 mm borosilicate) | 20 | 128 [301] | 30.5 [71.7] | 6.8 [16.0] |
| | 30 | 4 580 [10 400] | 1 120 [2 530] | 262 [593] |
| | 40 | **19 500 [43 800]** | **4 790 [10 800]** | **1 140 [2 560]** |
| glass + 8 mm lead glass | 30 | 0.011 [0.035] | 0.0033 [0.010] | 0.00085 [0.0026] |
| | 40 | 32.8 [78.9] | 9.44 [22.7] | 2.49 [5.97] |
| glass + 3 mm SS304 shutter | 30 | 0.012 [0.035] | 0.0033 [0.010] | 0.00085 [0.0026] |
| | 40 | 33.4 [80.3] | 9.54 [22.9] | 2.50 [6.00] |

At 20 kV, both shielded windows sit around 10⁻¹⁴ µSv/h. The shielded-window
totals are the flange/wall term; the window itself passes 10⁻¹⁴ (lead
glass) or 10⁻² (shutter) µSv/h at 40 kV.

**Through the 1.65 mm wall**, distance from its outer face:

| kV | 0.5 m | 1 m | 2 m |
|---:|---:|---:|---:|
| 20 | 6×10⁻¹⁴ | 2×10⁻¹⁴ | 4×10⁻¹⁵ |
| 30 | 0.012 [0.036] | 0.0033 [0.010] | 0.00086 [0.0026] |
| 40 | 34.1 [82.0] | 9.64 [23.2] | 2.51 [6.03] |

**Wall thickness sweep**, −40 kV, 1 m from the wall:

| SS304 wall | H*(10) µSv/h [band] |
|---:|---|
| 1.65 mm | 9.64 [4.01 – 23.2] |
| 2 mm | 2.29 [0.85 – 6.13] |
| 3 mm | 0.050 [0.017 – 0.144] |
| 4 mm | 0.0014 [0.0004 – 0.0044] |
| 6 mm | 2×10⁻⁶ |

### What the numbers say

1. **The bare viewport is the hazard, as the audit guessed.**
   - At 40 kV, 10 mA (bound) it's ~5 mSv/h at 1 m on axis, upper edge
     ~11 mSv/h. Someone standing 0.5 m from the glass takes ~0.3 mSv a
     minute at the bound (upper edge ~0.7).
   - Even at 20 kV it's ~30 µSv/h at 1 m, ten times the shielded-grid
     plan's 2.5 µSv/h design budget.
   - The audit's rule "no camera at a bare glass viewport above ~15 kV"
     stands, and now has a number behind it.
2. **8 mm lead glass or a 3 mm steel shutter kills the window path.** At
   40 kV the window term drops to 10⁻¹⁴ (lead glass) or 10⁻² (shutter)
   µSv/h. What's left on axis is the flange/wall term.
3. **At 40 kV the thin wall itself is marginal.** 1.65 mm of SS304 passes
   the top few keV of a 40 kV spectrum: ~10 µSv/h at 1 m at the bound
   (upper edge 23). It falls ~4× by 2 mm and ~200× by 3 mm. The audit
   didn't have this:
   - at −40 kV with 1.65 mm walls and electron fraction 1, the wall alone
     is above the 2.5 µSv/h budget out to ~2 m. Thicker walls (3 mm gives
     0.05), a thin lead skirt, or a measured electron fraction well below 1
     bring it down;
   - at −30 kV the same wall gives ~0.003 µSv/h at 1 m and is fine.

   This term is tip-dominated (the ×/÷1.5 tip band is on), so it's the most
   model-sensitive number in the table. Survey it first.

   An earlier draft priced this path with normal-incidence attenuation for
   the whole shell. That overstated it ~8× at τ ≈ 5–6, because the shell
   integral goes as E₂(τ), not e^{−τ}. The exact oblique-ray integral
   replaced it, and a test pins the far-field E₂ limit and the
   unattenuated shell formula.
4. **Capture gammas are a fifth of the neutron dose behind HDPE.** At the
   5×10⁶ n/s upper bound with 15 cm HDPE (neutronics seed 20260717, 20 × 50k):

   | detector (radius from source) | neutron | capture γ (2.22 MeV H + C bound) | total [band] |
   |---|---:|---:|---:|
   | 0.5 m | 28.1 ± 0.4% | 7.04 ± 0.1% | 35.2 [27.0 – 45.7] |
   | 1 m | 6.20 ± 0.4% | 1.16 ± 0.1% | 7.35 [5.66 – 9.56] |
   | 2 m | 1.51 ± 0.4% | 0.27 ± 0.1% | 1.79 [1.37 – 2.32] |

   The HDPE captures 4.05×10⁶ n/s on hydrogen, 81% of the source. Behind
   15 cm at 2 m the total is 1.79 µSv/h, upper edge 2.32 (edges summed
   edge-wise: high = N·b_n + P·b_p). That's still
   under the 2.5 µSv/h budget, but the margin the neutron-only case showed
   (1.52) mostly goes away. Borated poly swaps some 2.22 MeV for 478 keV.

## Caveats carried on every claim set

- H*(10) is deep dose. For soft x-rays through thin windows the eye-lens
  and skin dose can exceed it; air kerma bounds H′(0.07) within ~×1.6.
- Infinite lateral slabs at normal incidence overcount scatter through a
  small window, which is conservative. No room return or skyshine.
- No fluorescence x-rays, no annihilation photons. Coherent scatter counts
  as removal.
- Priced at peak voltage and full-energy electrons. Ripple and partial-
  energy electrons lower the real dose. Star-mode jets aimed at the port
  raise it (`beaming_factor`).
- **Predicted, not measured.** The filming gate is still an energy-
  compensated survey at every voltage step (`receipt::compare` against
  these claims). A Violated claim means stop.

## Follow-ups (not in this PR)

- Register `vcad.photon-claims/1` in `crates/vcad-receipt` and wire
  `simulate_photon_dose` into the MCP server. This is the same staging the
  neutronics and particle families used.
- An electron-fraction model (secondary-electron yield of D⁺ on the cathode
  wires, ~1–2 at 30–40 keV), so the bound can come down by physics instead
  of by measurement.
- Oblique incidence and a finite window aperture for the scatter term. The
  geometry from the 2D neutronics work could feed this.
- A Pb fluorescence and annihilation line source, needed only for
  lead-shielded MeV gamma problems.
