#!/bin/bash
# Render the puck footage with vcad-render. Run on mew.
# usage: render.sh <puck.vcad (air root + PcbBoard stripped)> <outdir>
# turn/: 300-frame dark turntable (10 s at 30 fps). sect/: 1200-frame section (y=0) sweep.
set -e
V=${VCAD_RENDER:-vcad-render}; DOC=$1; OUT=$2; J=${J:-6}
mkdir -p "$OUT/turn" "$OUT/sect"
seq 0 299 | xargs -P "$J" -I{} sh -c '
  k={}; az=$(python3 -c "print(20 + 360*$k/300)")
  '"$V"' '"$DOC"' --azimuth $az --elevation 22 --style tinted --theme dark \
    --jpeg '"$OUT"'/turn/$(printf %04d $k).jpg --size 1920x1080 --quality 92 >/dev/null 2>&1'
seq 0 1199 | xargs -P "$J" -I{} sh -c '
  k={}; az=$(python3 -c "import math; print(15 + 60*$k/1200)"); el=$(python3 -c "import math; print(24 + 6*math.sin(2*math.pi*$k/1200))")
  '"$V"' '"$DOC"' --azimuth $az --elevation $el --section y=0 --style tinted --theme dark \
    --jpeg '"$OUT"'/sect/$(printf %04d $k).jpg --size 1920x1080 --quality 92 >/dev/null 2>&1'
ls "$OUT/turn" | wc -l; ls "$OUT/sect" | wc -l
