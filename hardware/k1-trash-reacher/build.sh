#!/usr/bin/env bash
# Export every printed part (STL, print-oriented), a one-plate 3MF kit,
# per-part volumes, and the assembly render.
#   cargo build --release -p vcad -p vcad-render   # needs sibling tang/ and phyz/
#   hardware/k1-trash-reacher/build.sh
set -euo pipefail
cd "$(dirname "$0")"
BIN="${CARGO_TARGET_DIR:-../../target}/release"
VCAD="$BIN/vcad"
RENDER="$BIN/vcad-render"
PARTS="servo-mount trigger-shoe pod pod-lid cuff-sleeve cuff-collar"

mkdir -p stl render
for p in $PARTS; do
  # export also prints the floating_floor note: +Z up must be support-free
  "$VCAD" export "$p.loon" "stl/$p.stl"
  "$VCAD" eval-json "$p.loon" | grep '"volume"' | sed "s/^ */$p /"
done
"$VCAD" kit kit.json -o k1-trash-reacher.3mf
# vcad-render does not evaluate .loon; go through the evaluated document.
"$VCAD" export assembly.loon render/assembly.vcad
"$RENDER" render/assembly.vcad --view iso --size 1600 --fill 0.92 --auto-aspect -o render/assembly.png
"$RENDER" render/assembly.vcad --azimuth 20 --elevation 15 --size 1400 --fill 0.92 --auto-aspect -o render/actuation.png
rm -f render/assembly.vcad
