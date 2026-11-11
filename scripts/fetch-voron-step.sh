#!/usr/bin/env bash
# Fetch the Voron 2.4r2 STEP assembly used by docs/assembly-stress.md.
# Source: https://github.com/VoronDesign/Voron-2 (CAD/Voron_2.4r2_Assembly_STEP.zip)
# Licence: GPL-3.0 (VoronDesign). Kept OUT of git; lands in target/stress/.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/stress/voron"
mkdir -p "$out"
zip="$out/Voron_2.4r2_Assembly_STEP.zip"
[ -f "$zip" ] || curl -fL -o "$zip" \
  "https://raw.githubusercontent.com/VoronDesign/Voron-2/Voron2.4/CAD/Voron_2.4r2_Assembly_STEP.zip"
unzip -o -q "$zip" -d "$out"
find "$out" \( -iname '*.step' -o -iname '*.stp' \) -print
