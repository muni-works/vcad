# plasmoid explainer — script

90 s, vertical 720×1280, 30 fps, captions only (no voice). The audience knows
nothing about plasma. Every number comes from `docs/plasmoid-sim.md`
(reduced model, #1198/#1229) or `docs/plasmoid-fluid-m0.md` (fluid model,
#1237 at `f67dc3429`, an open draft whose numbers can still move). The video
does not claim more than those docs: the lock in the clip is **not explained
yet**, and the video ends on the experiment that would settle it.

| # | t (s) | picture | caption |
|---|---|---|---|
| 1 | 0–8 | path-traced clip, t = 1→9 s at 1×: racing, then the magnet comes down and the foot locks (5.93 s) | "a glowing thread races around a dish" → "bring a magnet close…" → "…and it locks on" |
| 2 | 8–20 | diagram: gas atoms, an RF field flipping, electrons bouncing, ionization flashes | "what's glowing? (our best guess from the video)" → "thin gas, ~1/760 of an atmosphere" → "a radio-frequency field shakes its electrons" → "fast electrons knock more loose; atoms glow as they settle" |
| 3 | 20–30 | raw clip frames (racing, t = 0→5) with labels: spike, filament, foot | "glowing, electrified gas is a plasma" → "a plasmoid: a blob of plasma that holds its shape and moves as one thing" → "this one's foot races around the dish (we didn't model why it races)" |
| 4 | 30–41 | diagram: current arrow flipping, push arrow flipping, running average at zero | "so why would a magnet grab it?" → "the obvious answer: magnets push on currents" → "but this current flips millions of times a second" → "push left, push right: it averages out to zero" |
| 5 | 41–56 | diagram: two electrons side by side, 1 atm (straight hops) vs 1 Torr (curls); then the β map | "what survives: magnets curl electrons" → "β = how much an electron curls before it hits an atom" → "at normal air pressure: β ≈ 0.004. nothing" → "in thin gas under this magnet: β ≈ 2.9" → "curled electrons stick around, so the glow might prefer the magnet" |
| 6 | 56–68 | path-traced clip, t = 8→19 s at 1× (HUD burned over the whole sequence first, then trimmed) | "we built a small model on that idea and simulated it in vcad" → "we script the magnet. the model decides when the foot locks and lets go" → "lock, drag, flick it off, recapture, release" |
| 7 | 68–80 | chart: three arrows on the foot — the model's assumed pull (inward), the derived magnetic pull (outward), the metal pull (inward, 2.7× the magnetic one) | "then we modelled the glow itself, from first principles" → "result: the magnet pushes the foot *away*" → "but a magnet is nickel-plated metal" → "metal near the electrode pulls the glow in: 2.7× harder, at 1 Torr" |
| 8 | 80–90 | the experiment: magnet vs aluminum slug, side by side; end card | "so the lock is probably electric, not magnetic (or it's where the foot re-forms)" → "test it: swap the magnet for an aluminum slug" → "still locks? it was never magnetism" → "sim + render: vcad · open source" |

## Sources for each claim

- device, gas and pressure are a hypothesis from one frame of the source
  clip: `plasmoid-sim.md` §1. The race speed `v_race` is read off the clip by
  eye; no model explains why the foot races (§2).
- β ≈ 2.9 at 1 Torr, β ≈ 0.004 at 1 atm, 61 mT: `plasmoid-sim.md` §1, §3.
- RF averaging of `I·L×B`: `plasmoid-sim.md` §1. The frequency is not
  stated in either doc; "millions of times a second" assumes MHz RF.
- the reduced model (race + preferred radius + fitted pull + swirl), lock
  times 5.93/14.04/14.47/18.08 s: `plasmoid-sim.md` §2–3. The magnet path is
  the document's keyframed timeline; lock and release are model output.
- derived magnetic pull outward at 1 Torr for every heating drive; floating
  conductor pulls inward; "2.7×" = 22.7 / 8.3 (|ES| / |magnetic| at 1 Torr,
  uniform heating). Fluid m/s are frozen-profile skew rates, so the chart
  shows direction and ratio only, never speeds. Growth is higher under the
  magnet, so "where the foot re-forms" stays open: `plasmoid-fluid-m0.md`
  (result, verdict).
- below ~0.3 Torr the sign of the derived pull depends on the heating model,
  so the video makes no low-pressure claim.
- aluminum-slug control: `plasmoid-sim.md` §1, `plasmoid-fluid-m0.md` verdict.

## Build

Captions are cut to a 120 BPM grid (`beat.py` finds it: onset envelope →
tempo → phase maximising on-grid onset energy, and checks drift per 20 s
window). The soundtrack starts at `TRACK_OFFSET` = 11.04 s so its first drop
lands on the lock (scene 1, 5.0 s) and its third on the flick break-away
(scene 6, 61.0 s). Every scene boundary and caption cue is a whole beat.
Timings above are the first draft; `SCENES` in `make_video.py` is the cut.

Style is the Pilot Light system used by the board videos
(`hardware/pcb-motor-video/comp.py`): `style.py` has the warm palette, IBM
Plex Mono/Sans, the one easing curve, the numbered panel and the ▽cad end
card. Fonts are vendored in `fonts/` (IBM Plex: SIL OFL, `OFL-IBM-Plex.txt`;
Inter: SIL OFL 1.1).

```bash
# on a build box, with the clip frames + trajectory from docs/plasmoid-sim.md
python3 beat.py track.mp3 beat.json
python3 make_video.py --raw <clip frames> --traj <trajectory.csv> \
  --assets ../../plasmoid-sim --beat beat.json --track track.mp3 -o explainer.mp4
```

The soundtrack is a commercial release and is not in the repo; its licence
has to be cleared before the scored cut is published.
