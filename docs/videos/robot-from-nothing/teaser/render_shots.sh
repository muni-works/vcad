#!/bin/sh
cd ~/robotvid/teaser
B=$HOME/mainchk2-target/release/vcad-render
for s in ${SHOTS:-fall cut assemble}; do
  doc=rose-anim.vcad; [ $s = fall ] && doc=rose-fall.vcad
  nice $B $doc --animate shot_$s.json -o frames_$s --theme dark --size ${SIZE:-1920x1080} > ../logs/11-render-$s.log 2>&1
  echo "EXIT $?" >> ../logs/11-render-$s.log
done
