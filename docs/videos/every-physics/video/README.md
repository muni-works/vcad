# puck video

50 s, 1920×1080, 30 fps. Built from the every-physics PoC v3 results (`../poc/results/run-v3.log`).
Every number on screen is predicted; none is measured yet.

- `render.sh`: vcad-render footage. A 300-frame dark turntable plus a 1200-frame `--section y=0` sweep. A section frame costs ~6 s, so run it on mew with J=6 or more.
- `synth.py`: the score, synthesized from the puck's own numbers. Eb3 = 155.0 Hz (the field-solved port tuning). The drone is that tuning an octave down. The bell is the six Fine-mesh shell modes (448–887 Hz) struck together. The optimizer bars glide along the optimizer's f_b history.
- `comp.py`: the compositor. Pilot Light style, IBM Plex (`fonts/`, OFL), 96 BPM bar grid shared with synth.py.
- `sheet.py`: contact sheet at given seconds. `final.sh`: synth + composite + encode.

## Reproduce (mew)
```
# puck.vcad = ../poc/results/puck.vcad with the air root and the PcbBoard node (id 9000) removed
VCAD_RENDER=.../target/release/vcad-render J=6 ./render.sh puck.vcad frames
mkdir res && cp ../poc/results/{port_velocity,antenna_s11}.csv res/
./final.sh            # → puck.mp4, puck_sheet.jpg
```
The mp4 is not committed. It's in the mew work dir, `~/renders/puck-vid/`.
