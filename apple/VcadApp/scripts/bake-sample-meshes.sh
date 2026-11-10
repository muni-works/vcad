#!/usr/bin/env bash
# Bake `<sample>.vcadmesh` beside every bundled sample document so that a
# first-time user's first open is a root-cache hit, not a cold kernel solve.
# Measured on an M-series Mac (scripts/launch-perf.sh): Stirling engine 37.8 s
# cold → 0.07 s from the bundle; PCB motor assembly 18.0 s → 0.05 s.
#
#   apple/VcadApp/scripts/bake-sample-meshes.sh [path/to/vcad.app]
#
# Bundles are keyed by the kernel build, so they are baked into the built
# app's Resources/samples (bundle.sh runs this before signing) rather than
# committed: a stale bundle is only a cache miss, never wrong geometry.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
app="${1:-$here/dist/vcad.app}"
samples="$app/Contents/Resources/samples"
[ -d "$samples" ] || { echo "no samples in $app" >&2; exit 1; }
# A private cache, so the bake measures nothing from this machine's history
# and leaves the user's cache alone.
cache="$(mktemp -d)"
trap 'rm -rf "$cache"' EXIT
find "$samples" -name '*.vcad' | while read -r doc; do
  t0=$(date +%s)
  VCAD_BAKE_MESH_BUNDLE=1 VCAD_NO_WELCOME=1 VCAD_CACHE_DIR="$cache" \
    timeout "${BAKE_TIMEOUT:-180}" "$app/Contents/MacOS/vcad" --vcad-open "$doc" 2>&1 | grep '\[vcad\] baked' || echo "  (no bundle for $doc)"
  echo "  $(basename "$doc"): $(( $(date +%s) - t0 )) s"
  # A bundle that costs more to ship than it saves (boards: tens of MB for a
  # 1–2 s solve) is dropped; the document still opens, just solved live.
  mesh="${doc%.vcad}.vcadmesh"
  if [ -f "$mesh" ] && [ "$(stat -f %z "$mesh")" -gt "${MAX_BUNDLE_BYTES:-4000000}" ]; then
    echo "  dropped $(basename "$mesh") ($(( $(stat -f %z "$mesh") / 1000000 )) MB)"; rm -f "$mesh"
  fi
done
