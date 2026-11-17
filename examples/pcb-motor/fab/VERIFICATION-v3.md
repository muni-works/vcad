# Motor v3 — Verification Ledger (post-fix toolchain)

Third pass, 2026-07-08, after PRs #470–#473 merged. Run by a single scripted pipeline
(`v3_run.py` against the local MCP build); one manual repair batch.

## Fix regressions (Phase A)

| Fix | Result |
|---|---|
| #470 watertight flange-with-holes | **PASS** — rotor iron `verify_spec` 9/9 (volume 6800–6950 window, watertight, bbox) — was red in v2 |
| #471 apply_edits @N refs + root consumption | **PASS** — probe: 1 part, exact volume; full assembly: **11 parts / 68,027 mm³** (v2: 68 parts / 307k) |
| #472 sheet-metal cost alignment | **PASS** — quote $20.96 vs laser $17.64 (×1.19, was ×3.0) |
| #473 DFM tie-aware min_clearance | **PARTIAL** — positions now reported, but 4 star-junction contacts still flagged despite being provably inside the tie region (dist ≤1.09mm vs radius 2.95mm). Follow-up chip filed with exact coordinates. Waived: net-aware DRC = 0 on the same board. |

## Board (`stator-v3.vcad`)

- Realizer + 2 feed stitches (same PHB/PHC island repair as v2, scripted this time).
- **DRC 0**, receipt `31161edd73a2ca11` (now includes unified DesignReceipt claims).
- Gerbers `fab/stator-v3-gerbers/`, JLCPCB $9.55/ea ×5.
- Note: `@N` symbolic refs in apply_edits are batch-local by design — split batches must
  renumber (my v3 script bug, not a tool bug).

## Assembly (`motor-assembly-v3.vcad`)

Clean build via `solid_from_board` + two `@N` batches. All six clearance assertions PASS
(air-gap 1.000mm, magnet-vs-heads 3.15, shaft-vs-stator 0.995, hub-vs-heads 5.05,
rotor-vs-board 4.00, shaft-vs-bearing 0.0498).

## BOM (`fab/BOM-v3.md` / `.csv`)

15 lines, all manufactured lines quote-linked with the aligned cost model.
**Grand total $269.32 landed** (incl. optional $42 induction-demo boards + spares).

## Scorecard across rounds

| | v1 | v2 | v3 |
|---|---|---|---|
| Winding interconnect | hand JSON surgery | realizer + 2 stitches | same, scripted |
| EM verdicts | bash model | 2 tool calls | scripted |
| Clearances | hand math + pairwise booleans | 6 named assertions | scripted, re-run green |
| Assembly integrity | phantom-volume kernel bug | ghost roots (68 parts) | **11 parts, exact volume** |
| BOM | hand-written markdown | bom tools, $271.94 | bom tools, **$269.32** |
| New bugs found | 3 | 4 | 1 (tie-exemption follow-up) |

## Addendum 2026-11-16 — 2 of 9 coils were open (fixed)

**What was wrong.** `add_motor_winding` could not route the PHB and PHC feeds
(J1 sits at r≈32 mm, inside the feed escape band, and every escape arc was
forced onto B.Cu, where PHA's arc would have to cross the PHB/PHC radials), yet
it returned `success: true`. The "2 feed stitches" above then tied J1.2/J1.3 to
the PHB/PHC series ring at 97.8°/102.2° — *after* each phase's first coil. Those
two spirals were left as dead-end stubs: their centre vias had nothing on B.Cu.
Net-aware DRC was 0 because every net was still one island; DRC does not check
that current flows through the coils. The committed gerbers carried the same
copper.

**Fix.** The realizer's feed escape now searches ring × arc layer × direction by
backtracking (a feed may change to F.Cu at the top of its radial, outside the
coils), checks staged feeds against each other on both layers incl. vias, and
fails (`success: false`, `errors`) when any feed stays unrouted. The pipeline no
longer stitches; it fails instead. New tool `check_winding` solves the board as a
resistor network (1 A into each phase pad, the neutral net at 0 V) and reads each
coil's signed ampere-turns; the pipeline and `scripts/stator-board.mjs` gate on it,
and `add_motor_winding` runs it on its own output.

**Per-coil current, 1 A into each phase pad** (independent solver: copper as a
resistor network, neutral pad J1.4 at 0 V; AT/A from `check_winding`):

| coil centre | phase | before: current | after: current | after: AT/A |
|---|---|---|---|---|
| (57.50, 35.00) | PHA | 1.000 A | 1.000 A | +10.00 |
| (52.24, 49.46) | PHB | **0 (open)** | 1.000 A | +10.00 |
| (38.91, 57.16) | PHC | **0 (open)** | 1.000 A | +10.00 |
| (23.75, 54.49) | PHA | 1.000 A | 1.000 A | +10.00 |
| (13.86, 42.70) | PHB | 1.000 A | 1.000 A | +10.00 |
| (13.86, 27.30) | PHC | 1.000 A | 1.000 A | +10.00 |
| (23.75, 15.51) | PHA | 1.000 A | 1.000 A | +10.00 |
| (38.91, 12.84) | PHB | 1.000 A | 1.000 A | +10.00 |
| (52.24, 20.54) | PHC | 1.000 A | 1.000 A | +10.00 |
| **working coils A/B/C** | | **3 / 2 / 2** | **3 / 3 / 3** | |
| pad→J1.4 resistance A/B/C | | 1.265 / 0.866 / 0.865 Ω | 1.273 / 1.243 / 1.218 Ω | |

Every coil carries current centre→outer and all nine have plan polarity +1, so
they circulate the same way. That is correct for 9s/6p, where each phase's
three coils sit at the same electrical angle. Phase resistance now agrees to
within 4.5 %. The remaining spread is feed length: PHA's feed is the longest.

(An earlier reading of 3 / 2 / 1 held the whole 2.95 mm net-tie disc at 0 V,
which swallows the last PHC coil's series hop via at (44.90, 26.69). That coil
was never open on the copper.)

**Regenerated:** `stator-v3.vcad` (receipt `0ae94635c8a7cdb4`, DRC 0, DFM
JLCPCB warnings only), `stator-v3-gerbers/`, `stator-v3-board.png`.
**Not re-run:** quote, BOM, assembly and clearance steps. The board outline
and connector position are unchanged, so those numbers stand, but they were
not recomputed. Nothing here has been built or measured on hardware. The
README's Kt / torque figures come from the winding plan, not from this copper.
