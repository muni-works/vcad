#!/usr/bin/env bash
# Build AppIcon.icns and DocumentIcon.icns from a chosen 1024 px icon PNG.
# Not wired into bundle.sh yet (bundle.sh still draws the old icon via
# make-icon.swift when dist/AppIcon.icns is missing). To adopt, run this
# with the chosen variant; bundle.sh copies dist/*.icns into the app.
#
#   apple/VcadApp/Branding/make-icns.sh icon-variants/vcad-icon-v1-1024.png [out-dir]
#
# macOS only (sips, iconutil). DocumentIcon = the app icon on a white page
# with a folded corner, rendered with Python/Pillow if present, else the
# app icon is reused as-is.
set -euo pipefail
src="${1:?usage: make-icns.sh <icon-1024.png> [out-dir]}"
here="$(cd "$(dirname "$0")" && pwd)"
out="${2:-$here/../dist}"
mkdir -p "$out"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

iconset() {  # <png-1024> <name>
  local png="$1" set="$tmp/$2.iconset"
  mkdir -p "$set"
  for spec in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 \
              256:128x128@2x 256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
    sips -z "${spec%%:*}" "${spec%%:*}" "$png" --out "$set/icon_${spec#*:}.png" >/dev/null
  done
  iconutil -c icns "$set" -o "$out/$2.icns"
  echo "wrote $out/$2.icns"
}

iconset "$src" AppIcon

doc="$tmp/document-1024.png"
if python3 -c 'import PIL' 2>/dev/null; then
  python3 - "$src" "$doc" <<'PY'
import sys
from PIL import Image, ImageDraw, ImageFilter
S = 1024
icon = Image.open(sys.argv[1]).convert("RGBA")
im = Image.new("RGBA", (S, S), (0, 0, 0, 0))
l, t, r, b, fold = 176, 64, 848, 960, 200
page = [(l, t), (r - fold, t), (r, t + fold), (r, b), (l, b)]
sh = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(sh).polygon([(x, y + 10) for x, y in page], fill=(0, 0, 0, 70))
im.alpha_composite(sh.filter(ImageFilter.GaussianBlur(14)))
d = ImageDraw.Draw(im)
d.polygon(page, fill=(250, 250, 251, 255), outline=(214, 216, 220, 255))
d.polygon([(r - fold, t), (r - fold, t + fold), (r, t + fold)], fill=(226, 228, 232, 255))
small = icon.resize((480, 480), Image.LANCZOS)
im.alpha_composite(small, ((S - 480) // 2, 330))
im.save(sys.argv[2])
PY
else
  cp "$src" "$doc"
fi
iconset "$doc" DocumentIcon
