# robot-from-nothing — ep 1 teaser

`teaser.mp4`: 62 s, 1920×1080, 30 fps, silent (VO and music go in the edit).
`poster.png` is the frame at 20 s. Every robot frame is drawn by `vcad-render`;
the cards are PIL in the Pilot Light palette (IBM Plex). The front-door shot is a
labelled placeholder slate for live action.

rebuild (on mew, never the Mac; needs a `vcad-render` with muni-works/vcad#1248):

```bash
# inputs: rose-mini.vcad + rose-mini.joints.json (../poc), trace.csv (gym hold, ../poc/data)
python3 make_shots.py rose-mini.vcad rose-mini.joints.json trace.csv .   # rose-anim.vcad, rose-fall.vcad, shot_*.json
SIZE=2496x1404 sh render_shots.sh                                        # frames_{fall,cut,assemble}
python3 comp.py . trace.csv <plex-fonts-dir> out
ffmpeg -framerate 30 -i out/f_%05d.png -c:v libx264 -pix_fmt yuv420p -crf 18 -movflags +faststart teaser.mp4
```

notes:
- assemble: Visibility tracks bring parts in link by link (feet up), an Explode
  track collapses authored per-instance `explode` offsets (0.6 x link centre minus
  robot centre), and a 150° Turntable camera. cut: Visibility tracks drop the arms
  and head under an Orbit camera. fall: a hidden slider x / slider z / pitch rig
  keyed from the gym trace (timelines drive scalar joints, not a Free base).
- `--animate` frames on the union of every frame's posed bounds, so shots render
  at 1.3x and `comp.py` crops a 1920x1080 window around the drawn pixels.
- the first cut (5f2e3d5ab) predates #1248 and faked the build-up with
  crossfaded stills; #1248 made `--animate` play Visibility / Explode / camera.
