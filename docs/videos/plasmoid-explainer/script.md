# plasmoid explainer — script

**Current cut (narrated):** the spoken script is `narration.py`. `voice.py`
voices each line with [ribt](https://ribt.dev) (the released af_heart voice)
and times every word with Whisper (`timing.json`); `make_video.py` gives each
line a shot, pops the captions in on the words as they are said, fires
diagram cues on named words, and ducks the soundtrack under the voice. It
opens on "What the fuck is the sun?" over the SDO footage; the lock lands on
the track's 32 s jump; the payoff lines are pinned to the clip's own events
(drag, flick, release). The second half explains the physics: magnets curl
electrons (exact paths), only in thin gas (pressure sweep), a plasma lives
where gain beats loss, and a magnet tips that balance two ways (it cuts the
loss; as metal it crowds the electric field). `scene_live`, `scene_lever1`,
`scene_lever2` and `scene_foot` are illustrations, not simulation output;
the 2.7× ratio is the glow-fluid model's (#1237). The sections below
describe the earlier captions-only cuts.

```bash
~/venvs/voice-eval/bin/python voice.py <ribt-tts> <weights dir> vo
python3 make_video.py --raw <clip frames> --traj <trajectory.csv> \
  --assets <dir> --vo vo --track track.mp3 -o explainer.mp4
```

100.6 s, vertical (720×1280, or 1080×1920 with `EXPLAINER_SCALE=1.5`), 30 fps, captions only (no voice). The audience knows
nothing about plasma. Every number comes from `docs/plasmoid-sim.md`
(reduced model, #1198/#1229) or `docs/plasmoid-fluid-m0.md` (fluid model,
#1237 at `f67dc3429`, an open draft whose numbers can still move). The video
does not claim more than those docs: the lock in the clip is **not explained
yet**, and the video ends on the experiment that would settle it.

The cut is `SCENES` in `make_video.py`; [`storyboard.png`](storyboard.png)
(`storyboard.py`) shows it. Structure: show the phenomenon and what it can do
before asking what it is, build the pieces, let the natural guess fail, land
the key idea, replay the opening explained, then the twist and the open test.
Captions state the physics; no "we".

| # | t (s) | picture | beat |
|---|---|---|---|
| 01 | 0–11 | real footage: a xenon filament in a flask, the filament gathering to a hand, a solar eruption | wonder first, nothing explained |
| 01 | 11–16 | clip, slowed so one lap = two beats; the magnet comes down in the track's near-silence | the dish; the lock lands on the 16 s drop |
| 02 | 16–30 | clip at 1×: drag, flick (24 s), recapture, release | what it can do |
| — | 30–34 | title card over the dimmed plasmoid | "but what is a plasmoid?" / "and why would a magnet grab it?" (32 s jump) |
| 03 | 34–46 | labelled footage, then atoms / RF field / ionization | what a plasmoid and a plasma are |
| 04 | 46–56 | current × field flipping, average = 0 | the obvious answer fails; "so what's left?" in the 54 s dip |
| 05 | 56–68 | one electron's exact path with and without the magnet; pressure sweep 760 → 1 Torr; β map | the key idea, on the 56 s drop |
| 06 | 68–78 | clip at 1× with the model's HUD; flick on the 72 s drop | the opening, explained |
| 07 | 78–88 | the same magnet move under both lock models (`lock_compare.csv`) | the twist |
| 08 | 88–96 | magnet vs aluminum slug | the open test |
| — | 96–100.6 | ▽cad end card | |

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
window). The track plays from its start; its drops at 16, 56 and 72 s carry
the lock, the key idea and the flick. Every scene boundary and caption cue
is a whole beat.

Data, not cartoons: scene 05's electron paths are exact gyro-arcs with the
kernel's collision frequency (β = 2.88 at 1 Torr, to scale); scene 07's two
foot trajectories come from `plasmoid_lock_compare.rs`, an example run on
the glow-fluid PR (#1237 at `f67dc3429`): the fitted lock holds the foot
3.0 mm from the magnet, the derived one lets it race on (7.9 laps in 5 s).

Style is the Pilot Light system used by the board videos
(`hardware/pcb-motor-video/comp.py`): `style.py` has the warm palette, IBM
Plex Mono/Sans, the one easing curve, the numbered panel and the ▽cad end
card. Fonts are vendored in `fonts/` (IBM Plex: SIL OFL, `OFL-IBM-Plex.txt`;
Inter: SIL OFL 1.1).

```bash
# on a build box, with the clip frames + trajectory from docs/plasmoid-sim.md
python3 beat.py track.mp3 beat.json
python3 make_video.py --raw <clip frames> --traj <trajectory.csv> \
  --assets <dir with beta-map.png + lock_compare.csv> --beat beat.json --track track.mp3 -o explainer.mp4
```

The soundtrack is a commercial release and is not in the repo; its licence
has to be cleared before the scored cut is published.

## Real footage

The opening uses two clips, not in the repo (extract with ffmpeg into
`<assets>/<name>/%04d.jpg` at 30 fps):

| name | source | licence | seconds |
|---|---|---|---|
| `xe_thread`, `xe_hand` | Carl Willis, "Decorative xenon plasmas, 20-80 torr" (Wikimedia Commons) | CC BY 3.0: credited on screen and on the end card | 458–462.3, 22.5–25.8 |
| `sdo` | NASA SVS 14126, SDO Video Toolkit "Magnificent Eruption Mk II" (Wikimedia Commons) | public domain | 0–8.4, played 2× |

The captions call these "a thread of light" and "arcs of the same stuff"
(plasma); neither source calls its subject a plasmoid.
