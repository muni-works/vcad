#!/usr/bin/env bash
# Fetch the public STEP interop corpus (manifest.tsv) into $STEP_CORPUS_DIR
# and verify every file against checksums.sha256. The files are NOT in git:
# several are tens of MB and their licences (GPL, CC-BY-NC-SA, CERN-OHL)
# are compatible with fetching for testing, not with vendoring.
#
#   scripts/step-corpus/fetch.sh            # fetch missing, verify all
#   scripts/step-corpus/fetch.sh --update   # rewrite checksums.sha256
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dir="${STEP_CORPUS_DIR:-$HOME/.cache/vcad-step-corpus}"
mkdir -p "$dir"
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

grep -v '^#' "$here/manifest.tsv" | while IFS=$'\t' read -r id _origin _lic url; do
  [ -n "$id" ] || continue
  ext="${url##*.}"; out="$dir/$id.$(echo "$ext" | tr 'A-Z' 'a-z')"
  if [ ! -s "$out" ]; then
    echo "fetch $id" >&2
    curl -fsSL --retry 3 -o "$out.part" "$url" && mv "$out.part" "$out"
  fi
done

if [ "${1:-}" = "--update" ]; then
  (cd "$dir" && for f in *.step *.stp; do [ -e "$f" ] && echo "$(sha "$f")  $f"; done) > "$here/checksums.sha256"
  echo "wrote $here/checksums.sha256" >&2
  exit 0
fi

bad=0
while read -r want f; do
  got="$(sha "$dir/$f" 2>/dev/null || echo missing)"
  [ "$got" = "$want" ] || { echo "CHECKSUM MISMATCH $f ($got)" >&2; bad=1; }
done < "$here/checksums.sha256"
[ $bad = 0 ] && echo "corpus ok: $dir" >&2
exit $bad
