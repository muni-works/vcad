# robot-from-nothing — ep 1 teaser

`teaser.mp4`: 59 s, 1920×1080, 30 fps, silent (VO and music go in the edit).
`poster.png` is the frame at 20 s. Every robot frame is drawn by `vcad-render`;
the cards are PIL in the Pilot Light palette (IBM Plex). The front-door shot is a
labelled placeholder slate for live action.

rebuild (on mew, never the Mac):

```bash
# inputs: rose-mini.vcad + rose-mini.joints.json (../poc), trace.csv (gym hold, ../poc/data)
python3 make_shots.py rose-mini.vcad rose-mini.joints.json trace.csv .     # fall rig + timeline
python3 make_stills.py rose-mini.vcad rose-mini.joints.json stills         # build-up / cut-down states
vcad-render rose-fall.vcad --animate shot_fall.json -o frames_fall --theme dark --size 2496x1404
cp hold.json stills/ && sh run_states.sh cut_*.vcad assemble_*.vcad        # one 1-frame --animate per state
python3 comp.py . trace.csv <plex-fonts-dir> out
ffmpeg -framerate 30 -i out/f_%05d.png -c:v libx264 -pix_fmt yuv420p -crf 18 -movflags +faststart teaser.mp4
```

renderer limits this works around:
- `vcad-render --animate` poses joints only. Visibility, Explode and camera
  entries in a timeline are accepted and silently ignored, so the build-up and
  cut-down are stills crossfaded in `comp.py`; the fall is a joint rig (slider x,
  slider z, pitch) keyed from the gym trace.
- `--animate` also refuses a timeline without a joint track (`hold.json` holds one).
- a still render of the 142-part assembly without `--animate` ran 6 h without
  finishing; a one-frame `--animate` run takes ~3 min (162 s of it evaluation).
- the fit covers the whole timeline's bounds, so moving shots render at 1.3× and
  `comp.py` crops around the drawn pixels.
