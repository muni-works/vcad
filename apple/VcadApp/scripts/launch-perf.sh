#!/usr/bin/env bash
# Launch speed pass: cold-launch → first full scene, and orbit FPS, per document.
#   apple/VcadApp/scripts/launch-perf.sh [app] doc1.vcad doc2.vcad ...
# Needs a built app (bundle.sh). Each run is a fresh process; marks come from
# LaunchPerf.swift (VCAD_PERF=1) and land in /tmp/vcad_perf.log.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
app="${APP:-$here/dist/vcad.app}"
orbit="${ORBIT:-6}"
runs="${RUNS:-3}"
for doc in "$@"; do
  # Run 1 starts from an EMPTY kernel cache (a first-time user); later runs
  # reuse it (a second open). A `<doc>.vcadmesh` beside the doc is imported.
  cache="$(mktemp -d)"
  for i in $(seq 1 "$runs"); do
    rm -f /tmp/vcad_perf.log
    VCAD_PERF=1 VCAD_PERF_ORBIT="$orbit" VCAD_PERF_EXIT=1 VCAD_NO_WELCOME=1 VCAD_CACHE_DIR="$cache" \
      timeout 180 "$app/Contents/MacOS/vcad" --vcad-open "$(cd "$(dirname "$doc")" && pwd)/$(basename "$doc")" >/dev/null 2>&1 || true
    echo "== $(basename "$doc") run $i"
    grep -E "since launch|orbit" /tmp/vcad_perf.log || echo "(no marks)"
  done
  rm -rf "$cache"
done
