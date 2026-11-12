#!/usr/bin/env bash
# Build fuzz corpora from real files checked into the repo (files < 256 KB).
set -euo pipefail
cd "$(dirname "$0")/.."
seed() { # target, find -iname patterns...
  local t=$1; shift; mkdir -p "fuzz/corpus/$t"
  local args=(); for p in "$@"; do args+=(-o -iname "$p"); done
  git ls-files -z | xargs -0 -I{} find {} -maxdepth 0 -size -256k \( -false "${args[@]}" \) -print 2>/dev/null \
    | while read -r f; do cp "$f" "fuzz/corpus/$t/$(echo "$f" | tr / _)"; done
}
seed step_parse '*.step' '*.stp'
seed step_read '*.step' '*.stp'
seed stl '*.stl'
seed loon '*.loon'
seed vcad_json '*.vcad'
seed kicad_pcb '*.kicad_pcb'
seed kicad_sch '*.kicad_sch'
seed eagle_brd '*.brd'
seed altium_ascii '*.PcbDoc'
seed altium_pcbdoc '*.PcbDoc'
seed urdf '*.urdf'
mkdir -p fuzz/corpus/vcad_snapshot
for t in fuzz/corpus/*; do echo "$t: $(ls "$t" | wc -l)"; done
