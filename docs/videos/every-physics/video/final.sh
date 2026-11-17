#!/bin/bash
# Composite every frame and encode h264 + aac. Run on mew in the work dir
# (frames/ from render.sh, res/ = poc/results CSVs, fonts/, comp.py, synth.py).
set -e
python3 synth.py track.wav
rm -rf out && NPROC=${NPROC:-6} nice python3 comp.py frames res out
nice ffmpeg -loglevel error -y -framerate 30 -i out/%04d.png -i track.wav \
  -map 0:v -map 1:a -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p \
  -c:a aac -b:a 192k -shortest -movflags +faststart -metadata title="puck: every physics in one object" puck.mp4
ffprobe -v error -show_entries stream=codec_name,width,height,r_frame_rate,duration -of compact puck.mp4
nice ffmpeg -loglevel error -y -i puck.mp4 -vf "fps=1/2.5,scale=480:-1,tile=4x5" -frames:v 1 puck_sheet.jpg
