#!/bin/sh
# one --animate run per still state (1 frame); args = .vcad files
cd ~/robotvid/teaser/stills
for f in "$@"; do
  s=${f%.vcad}
  nice $HOME/mainchk2-target/release/vcad-render $f --animate hold.json --frames 1 -o r_$s --theme dark --size 2496x1404 >> ../../logs/12-states.log 2>&1 \
    && cp r_$s/frame_0000.png $s.png || echo "FAIL $f" >> ../../logs/12-states.log
done
echo DONE >> ../../logs/12-states.log
