# how a fusor works — explainer script

A narrated vertical explainer (720×1280 logical, 1080×1920 with
`EXPLAINER_SCALE=1.5`, 30 fps) about the machine in
[`../open-fusor/fusor.loon`](../open-fusor/fusor.loon). It explains the
physics. It is not footage of a running machine: nothing has been built.

The spoken script is [`narration.py`](narration.py). `voice.py` voices each
line with [ribt](https://ribt.dev) and times every word with Whisper;
`make_video.py` gives each line a shot, pops the captions in on the words as
they are said, and fires scene cues on named words. `scenes.py` draws the
shots. The pipeline, `style.py` and the fonts are copied from the plasmoid
explainer (#1229), which took them from the board videos.

Structure: the payoff first (atoms fuse in a steel ball), then the question
(how does a wire cage do that?), then the answer built in order: why nuclei
resist, the vacuum, the voltage hill, one ion falling, the cage it misses,
the trips back and forth, what happens at the centre, how often fusion wins,
and what that costs. The last line names the three parts again over the
opening shot. Captions state the physics; no "we".

## What is simulation and what is drawn

| shot | lines | source |
|---|---|---|
| hero, close | open1, open2, ask, start of so | `vcad-render --photoreal` of `fusor.loon`. The glow is added geometry ([`../open-fusor/star-mode.loon`](../open-fusor/star-mode.loon)), not simulated. On-screen tag: "render · path-traced in vcad · glow added" |
| fuel | fuel, repel | illustration (tagged on screen) |
| machine | vac | chamber, cage and rod drawn from the CAD's dimensions. The dots are an illustration: the real gas has 1.6×10²⁰ molecules per m³ |
| well | well | φ on the y = 0 plane from the 3D thin-wire BEM (`potential.csv`), as a map and then as a surface. The rolling ball is the hero ion's traced path, projected onto that plane |
| ions | fall, speed, miss, back, core | 96 deuteron flights from the same solver (`paths.csv`), played at true relative speed. The time, speed and energy readout is the traced ion's own. The electron leaving the atom before release is drawn |
| hit, fuse | hit, fuse | illustration (tagged on screen) |
| count | count | the number is the prediction below; the streaks are drawn |
| power | power | arithmetic below |
| why | why, end of so | the same 96 flights; each ring marks a traced wire strike. `so` opens on the hero shot again, then names the hill, the cage and the ions over their own shots |

Data: `cargo run --release -p vcad-kernel-particle --example
fusor_explainer_paths -- <dir> 96` at main `f0e85d84`. It solves the six
great circles (19 mm, 2 mm wire, −30 kV) in a 76 mm grounded sphere and
prints `96 ions: 84 hit a wire, mean core passes 6.23, phi(center) =
-28.91 kV`. The flights are vacuum orbits: charge exchange is not applied
to the trajectory, so an ion flies until a wire or the 40-pass cap stops it. The solver's cathode is the six circles alone: it has no stalk, so traced ions pass through the rod the scenes draw.

## Sources for each spoken number

- **thirty thousand volts, ten milliamps, 5 mTorr**: the operating point of
  [`../open-fusor.md`](../open-fusor.md).
- **a hundred and fifty thousand times thinner than air**: 760 Torr /
  0.005 Torr = 152,000.
- **ninety billionths of a second, seventeen hundred kilometers a second**:
  the traced hero ion reaches the cage radius 91 ns after release at
  28.7 keV; the fastest sample in `paths.csv` is 29.5 keV, which for a
  deuteron is 1,680 km/s. The ions a real discharge makes are mostly D₂⁺
  (`docs/particle-optics-m0.md` M7), which are twice as heavy and so 1/√2 as
  fast at the same energy. The line describes a bare deuteron.
- **seventy percent empty space**: geometric transparency 0.714 for the six
  great circles (`docs/particle-optics-3d.md`).
- **with nothing in the way, about seven trips**: 7.1 mean core passes for
  the globe, 400 ions, same doc, in vacuum. The 96-ion run here gives 6.2.
  In the 5 mTorr gas an ion rarely gets that far, which is the next line.
- **the fast ions crowd together** and the on-screen "≈ 60× denser": dwell
  time per unit volume in `paths.csv` (samples are uniform in time) inside
  r = 5 mm, against the 45–60 mm shell, is 63× counting only each ion's
  first transit (t ≤ 200 ns, which is what the gas allows), 70× over whole
  flights, 54× without the 12 capped survivors. Vacuum orbits: geometric
  convergence alone, no space charge.
- **almost always, charge exchange**: at 5 mTorr the charge-exchange mean
  free path is shorter than one pass (`docs/particle-optics-3d.md`).
- **helium and a neutron at twenty thousand kilometers a second**: D + D →
  ³He + n, neutron 2.45 MeV = 21,600 km/s. The other branch, about half the
  time, gives tritium and a proton; the screen says so.
- **about two thousand neutrons a second**: 1.9×10³ n/s, band 87 to
  1.8×10⁴, at −30 kV and 10 mA supply current
  (`docs/particle-optics-m0.md` M7). That prediction is for a ring-stack
  cathode, not this globe; the two-sided chain has not been run on the
  globe. The screen says "ring-stack model".
- **three hundred watts, a few billionths of one**: 30 kV × 10 mA = 300 W.
  1.9×10³ n/s with the two branches about equal is 3.8×10³ fusions per
  second at 3.27 MeV (neutron branch) and 4.03 MeV (proton branch), which
  is 2.2×10⁻⁹ W. Across the band it is 0.1 to 21 nW. The ratio on screen,
  10¹¹, is 300 / 2.2×10⁻⁹ rounded to the decade.
- **nearly every ion ends on a wire or a wall**: 84 of the 96 traced ions
  hit a wire inside 40 passes; in the gas the rest charge-exchange and
  their neutrals reach the wall.

## Build

On a build box (not the Mac), from the repo root:

```bash
cargo run --release -p vcad-kernel-particle --example fusor_explainer_paths -- assets 96
```

```bash
D=docs/videos/open-fusor
grep -v "viewport-front" $D/fusor.loon > open.loon && cat open.loon $D/star-mode.loon > shot.loon
cargo run --release -p vcad-loon --example loon2vcad -- shot.loon > shot.vcad
L="--photoreal --look $D/look.json --lighting warm-studio --backdrop cyclorama --exposure 0.75 --bloom 0.06 --tonemap exp --size 1080x1920 --spp 160"
cargo run --release -p vcad-render -- shot.vcad $L --focus chamber --azimuth 274 --elevation 4 --fill 0.62 --fov 22 -o assets/hero.jpg
cargo run --release -p vcad-render -- shot.vcad $L --focus chamber --azimuth 282 --elevation 10 --fill 0.9 --fov 22 -o assets/close.jpg
```

```bash
cd docs/videos/fusor-explainer
python voice.py <ribt-tts> <weights dir> vo          # needs openai-whisper
python3 make_video.py --assets ../../../assets --sheet storyboard.png
EXPLAINER_SCALE=1.5 python3 make_video.py --assets ../../../assets --vo vo -o fusor-explainer.mp4
```

The cut has no music. Fonts are vendored in `fonts/` (IBM Plex and Inter,
SIL OFL).
