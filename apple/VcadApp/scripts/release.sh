#!/bin/zsh
# Build, sign, notarize and stage a release of vcad.app (Developer ID DMG +
# Sparkle appcast). Modelled on frog's apps/macos/scripts/release.sh.
#
#   apple/VcadApp/scripts/release.sh               # version from packages/app/package.json
#   apple/VcadApp/scripts/release.sh 1.0.0         # explicit version
#   apple/VcadApp/scripts/release.sh --dry-run     # everything except notarization
#
# This script NEVER publishes. It leaves:
#   apple/VcadApp/dist/release/<version>/vcad-<version>.dmg
#   apple/VcadApp/dist/release/<version>/desktop/   <- mirrors packages/app/public/desktop/
#                                                     (appcast.xml + index.html)
# and prints the publish steps (GitHub Release upload + appcast PR). One-time
# setup and the full procedure: docs/launch/mac-release.md.
#
# Knobs (env):
#   VCAD_SIGN_IDENTITY   default "Developer ID Application: Cameron Pedersen (54SNZV8YN8)"
#   VCAD_NOTARY_PROFILE  notarytool keychain profile, default "frog" (shared with frog)
#   VCAD_BUILD           CFBundleVersion override (default: git commit count)
#   VCAD_DOWNLOAD_BASE   where the DMG will be hosted (appcast enclosure prefix),
#                        default https://github.com/muni-works/vcad/releases/download/desktop-v<version>/
#   SPARKLE_PUBLIC_ED_KEY  default: `generate_keys -p` (login Keychain)
#   VCAD_ALLOW_DIRTY=1   allow a dirty tree outside --dry-run (don't)
set -euo pipefail

DRY_RUN=0
VERSION=""
for arg in "$@"; do
  case $arg in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    -*) print -u2 "unknown flag: $arg"; exit 2 ;;
    *) VERSION=$arg ;;
  esac
done

HERE=${0:A:h}
APP_DIR=${HERE:h}
ROOT=$(git -C "$APP_DIR" rev-parse --show-toplevel)
SIGN_ID=${VCAD_SIGN_IDENTITY:-"Developer ID Application: Cameron Pedersen (54SNZV8YN8)"}
NOTARY_PROFILE=${VCAD_NOTARY_PROFILE:-frog}
BUNDLE_ID=io.vcad.m0
SPARKLE_BIN="$APP_DIR/.build/artifacts/sparkle/Sparkle/bin"
PUBLIC_DESKTOP="$ROOT/packages/app/public/desktop"

step() { print -P "%F{green}==>%f $*" }
note() { print -P "%F{yellow}  ·%f $*" }
fail() { print -P "%F{red}error:%f $*" >&2; exit 1 }

[[ -n $VERSION ]] || VERSION=$(node -p "require('$ROOT/packages/app/package.json').version")
[[ $VERSION =~ '^[0-9]+\.[0-9]+\.[0-9]+$' ]] || fail "version must look like 1.2.3 (got '$VERSION')"
TAG="desktop-v$VERSION"
BUILD=${VCAD_BUILD:-$(git -C "$ROOT" rev-list --count HEAD)}
DOWNLOAD_BASE=${VCAD_DOWNLOAD_BASE:-"https://github.com/muni-works/vcad/releases/download/$TAG/"}
[[ $DOWNLOAD_BASE == */ ]] || DOWNLOAD_BASE="$DOWNLOAD_BASE/"
DIST="$APP_DIR/dist/release/$VERSION"
DMG="$DIST/vcad-$VERSION.dmg"
STAGE="$DIST/desktop"

# ---------------------------------------------------------------- preflight
# Everything that can fail late, checked before a long build.
step "Preflight: vcad $VERSION (build $BUILD)$([[ $DRY_RUN == 1 ]] && print ' [dry run]')"
problems=()

if [[ -n $(git -C "$ROOT" status --porcelain) ]]; then
  if [[ $DRY_RUN == 1 || ${VCAD_ALLOW_DIRTY:-0} == 1 ]]; then note "working tree is dirty (allowed)"
  else problems+=("working tree isn't clean: a release must be reproducible from a commit"); fi
fi

if ! security find-identity -v -p codesigning | grep -qF "$SIGN_ID"; then
  problems+=("no \"$SIGN_ID\" codesigning identity in the Keychain.
      Setup: Xcode → Settings → Accounts → Manage Certificates → + → Developer ID Application.
      Check: security find-identity -v -p codesigning")
fi

if xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
  note "notary profile '$NOTARY_PROFILE' works"
elif [[ $DRY_RUN == 1 ]]; then
  note "notary profile '$NOTARY_PROFILE' missing — fine for --dry-run, required for a release"
else
  problems+=("notarytool keychain profile '$NOTARY_PROFILE' is missing or rejected.
      Setup (App Store Connect API key, not an app-specific password):
        xcrun notarytool store-credentials $NOTARY_PROFILE --key ~/keys/AuthKey_<KEYID>.p8 --key-id <KEYID> --issuer <ISSUER-UUID>
      See docs/launch/mac-release.md §2")
fi

# Sparkle's tools come from the SwiftPM artifact; resolve it if absent.
if [[ ! -x $SPARKLE_BIN/sign_update ]]; then
  swift package --package-path "$APP_DIR" resolve >/dev/null 2>&1 || true
fi
[[ -x $SPARKLE_BIN/sign_update ]] || problems+=("Sparkle tools not found at $SPARKLE_BIN (run: swift build --package-path apple/VcadApp)")

# The private EdDSA key lives in the login Keychain (generate_keys). Probe for
# the item without reading the secret.
if ! security find-generic-password -s "https://sparkle-project.org" >/dev/null 2>&1; then
  problems+=("no Sparkle EdDSA private key in the login Keychain.
      Setup: $SPARKLE_BIN/generate_keys   (prints the public key; back the key up: generate_keys -x)")
fi
ED_PUBLIC=${SPARKLE_PUBLIC_ED_KEY:-}
if [[ -z $ED_PUBLIC && -x $SPARKLE_BIN/generate_keys ]]; then
  ED_PUBLIC=$("$SPARKLE_BIN/generate_keys" -p 2>/dev/null | tail -1 | tr -d '[:space:]') || ED_PUBLIC=""
fi
[[ -n $ED_PUBLIC ]] || problems+=("could not read the Sparkle public key (SPARKLE_PUBLIC_ED_KEY or generate_keys -p)")

command -v create-dmg >/dev/null || problems+=("create-dmg missing: brew install create-dmg")
[[ -f $APP_DIR/Libs/libvcad_ffi.a ]] || problems+=("Libs/libvcad_ffi.a missing: run apple/VcadApp/build-ffi.sh")
if [[ -f $APP_DIR/Libs/libvcad_ffi.a ]]; then
  # A stale lib fails at link time 10 minutes in; compare against the header.
  exported=$( { nm -gU "$APP_DIR/Libs/libvcad_ffi.a" 2>/dev/null || true; } | grep -oE '_vcad_[a-z0-9_]+$' | sed 's/^_//' | sort -u || true)
  declared=$(grep -ohE '\bvcad_[a-z0-9_]+[[:space:]]*\(' "$APP_DIR"/Sources/CVcadFFI/*.h | tr -d ' (' | sort -u || true)
  missing=$(comm -23 <(print -r -- "$declared") <(print -r -- "$exported") | grep . || true)
  [[ -z $missing ]] || problems+=("Libs/libvcad_ffi.a is stale (missing: ${${(f)missing}[1,3]}…): run apple/VcadApp/build-ffi.sh")
fi
[[ -f $APP_DIR/Libs/libtang_compress_ffi.a ]] || problems+=("Libs/libtang_compress_ffi.a missing: run apple/VcadApp/build-tc-ffi.sh")

# CFBundleVersion must beat every build already in the feed, or Sparkle
# clients never see this release.
if [[ -f $PUBLIC_DESKTOP/appcast.xml ]]; then
  published=$(grep -o '<sparkle:version>[0-9]*</sparkle:version>' "$PUBLIC_DESKTOP/appcast.xml" \
    | grep -o '[0-9][0-9]*' | sort -n | tail -1 || true)
  if [[ -n $published ]] && (( BUILD <= published )); then
    problems+=("CFBundleVersion $BUILD is not above the published $published; set VCAD_BUILD")
  fi
fi

if (( ${#problems} )); then
  for p in "${problems[@]}"; do print -P "%F{red}  ✗%f $p" >&2; done
  fail "preflight failed (${#problems} problem(s)); see docs/launch/mac-release.md"
fi
note "identity: $SIGN_ID"
note "Sparkle public key: ${ED_PUBLIC[1,8]}…"

# ---------------------------------------------------------------- build
step "Building release bundle"
rm -rf "$DIST"; mkdir -p "$DIST"
CONFIG=release VCAD_RELEASE=1 VCAD_BUNDLE_ID=$BUNDLE_ID \
  VCAD_VERSION=$VERSION VCAD_BUILD=$BUILD SPARKLE_PUBLIC_ED_KEY=$ED_PUBLIC \
  "$APP_DIR/bundle.sh"
APP="$DIST/vcad.app"
ditto "$APP_DIR/dist/vcad.app" "$APP"

plist="$APP/Contents/Info.plist"
pb() { /usr/libexec/PlistBuddy -c "Print :$1" "$plist" 2>/dev/null }
[[ $(pb CFBundleIdentifier) == $BUNDLE_ID ]] || fail "bundle id is $(pb CFBundleIdentifier), expected $BUNDLE_ID"
[[ $(pb CFBundleShortVersionString) == $VERSION ]] || fail "CFBundleShortVersionString not stamped"
[[ $(pb CFBundleVersion) == $BUILD ]] || fail "CFBundleVersion not stamped"
[[ $(pb SUPublicEDKey) == $ED_PUBLIC ]] || fail "SUPublicEDKey missing from Info.plist"
[[ $(pb SUFeedURL) == https://vcad.io/desktop/appcast.xml ]] || fail "SUFeedURL is $(pb SUFeedURL)"
[[ $(pb VCADReleaseBuild) == true ]] || fail "VCADReleaseBuild is not true"

# ---------------------------------------------------------------- sign
# Inside out: every Mach-O gets the hardened runtime and a secure timestamp,
# or notarization refuses the whole app. Never --deep (it mis-signs Sparkle's
# XPC services); --deep is only for verification.
step "Signing with $SIGN_ID"
sign() { codesign --force --timestamp --options runtime --sign "$SIGN_ID" "$@" }
is_macho() { file -b "$1" | grep -q 'Mach-O' }
SPARKLE="$APP/Contents/Frameworks/Sparkle.framework"
MAIN="$APP/Contents/MacOS/vcad"

# Loose Mach-O (irys dylib, any helper) outside Sparkle and the main binary,
# deepest paths first.
loose=()
while IFS= read -r f; do
  [[ $f == $SPARKLE/* || $f == $MAIN ]] && continue
  is_macho "$f" && loose+=("$f")
done < <(find "$APP/Contents" -type f | awk '{ print gsub("/","/"), $0 }' | sort -rn | cut -d' ' -f2-)
for f in "${loose[@]}"; do sign "$f"; note "signed ${f#$APP/}"; done

if [[ -d $SPARKLE ]]; then
  SB="$SPARKLE/Versions/B"
  sign "$SB/XPCServices/Installer.xpc"
  sign --preserve-metadata=entitlements "$SB/XPCServices/Downloader.xpc"
  sign "$SB/Autoupdate"
  sign "$SB/Updater.app"
  sign "$SPARKLE"
  note "signed Sparkle.framework (Installer.xpc, Downloader.xpc, Autoupdate, Updater.app)"
fi
sign --entitlements "$APP_DIR/VcadApp.entitlements" "$APP"
note "signed vcad.app with VcadApp.entitlements"

codesign --verify --deep --strict --verbose=2 "$APP" 2>&1 | sed 's/^/    /'
# Every Mach-O in the bundle must carry our team, the runtime flag and a timestamp.
unsigned=0
while IFS= read -r f; do
  is_macho "$f" || continue
  info=$(codesign -dvv "$f" 2>&1) || { print -u2 "  unsigned: ${f#$APP/}"; unsigned=1; continue }
  [[ $info == *"TeamIdentifier=54SNZV8YN8"* || $SIGN_ID != *54SNZV8YN8* ]] || { print -u2 "  wrong team: ${f#$APP/}"; unsigned=1 }
  [[ $info == *"runtime"* ]] || { print -u2 "  no hardened runtime: ${f#$APP/}"; unsigned=1 }
  [[ $info == *"Timestamp="* ]] || { print -u2 "  no secure timestamp: ${f#$APP/}"; unsigned=1 }
done < <(find "$APP/Contents" -type f)
(( unsigned == 0 )) || fail "some Mach-O files are not release-signed"
note "all Mach-O: team 54SNZV8YN8, hardened runtime, secure timestamp"
codesign -d --entitlements :- "$APP" 2>/dev/null | grep -q 'com.apple.security.device.audio-input' \
  || fail "audio-input entitlement missing from the signed app"

notarize() {
  local log="$DIST/notary-${1:t}.log"
  xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait --timeout 30m | tee "$log"
  grep -q "status: Accepted" "$log" \
    || fail "notarization rejected; run: xcrun notarytool log <id> --keychain-profile $NOTARY_PROFILE"
}

# ---------------------------------------------------------------- notarize app
if [[ $DRY_RUN == 1 ]]; then
  step "Notarizing the app: skipped (--dry-run)"
else
  step "Notarizing the app"
  ditto -c -k --keepParent "$APP" "$DIST/vcad.zip"
  notarize "$DIST/vcad.zip"
  rm "$DIST/vcad.zip"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
fi
if spctl --assess --type execute --verbose=2 "$APP" 2>&1 | sed 's/^/    /'; then
  note "Gatekeeper accepts the app"
elif [[ $DRY_RUN == 1 ]]; then
  note "Gatekeeper rejects the un-notarized app (expected in --dry-run)"
else
  fail "Gatekeeper rejects the notarized app"
fi

# ---------------------------------------------------------------- DMG
step "Making the disk image"
dmgsrc="$DIST/dmgsrc"; rm -rf "$dmgsrc"; mkdir -p "$dmgsrc"; ditto "$APP" "$dmgsrc/vcad.app"
# Drafting-sheet background (1x + 2x in one TIFF); layout lives in dmg-background.swift.
bgdir="$DIST/dmgbg"; rm -rf "$bgdir"; mkdir -p "$bgdir"
swift "$HERE/dmg-background.swift" "$bgdir"
tiffutil -cathidpicheck "$bgdir/dmg-background.png" "$bgdir/dmg-background@2x.png" -out "$bgdir/background.tiff" >/dev/null
create-dmg --volname "vcad" --background "$bgdir/background.tiff" \
  --window-pos 200 160 --window-size 660 420 --icon-size 128 --text-size 12 \
  --icon vcad.app 180 190 --hide-extension vcad.app --app-drop-link 480 190 \
  --no-internet-enable "$DMG" "$dmgsrc" >/dev/null
rm -rf "$bgdir"
rm -rf "$dmgsrc"
codesign --force --timestamp --sign "$SIGN_ID" "$DMG"
codesign --verify --strict "$DMG"
if [[ $DRY_RUN == 1 ]]; then
  step "Notarizing the DMG: skipped (--dry-run)"
else
  step "Notarizing the DMG"
  notarize "$DMG"
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
fi
if spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG" 2>&1 | sed 's/^/    /'; then
  note "Gatekeeper accepts the DMG"
elif [[ $DRY_RUN == 1 ]]; then
  note "Gatekeeper rejects the un-notarized DMG (expected in --dry-run)"
else
  fail "Gatekeeper rejects the notarized DMG"
fi

# ---------------------------------------------------------------- appcast
# Sign the exact bytes being shipped (after stapling: stapling changes the DMG).
step "Writing the appcast"
ED=$("$SPARKLE_BIN/sign_update" "$DMG") || fail "sign_update failed (Keychain access to the Sparkle key?)"
[[ $ED == *'sparkle:edSignature="'* && $ED == *'length="'* ]] || fail "sign_update output unexpected: $ED"
# Verify against the public key baked into the app, so a key mismatch fails here
# instead of on every user's Mac.
sig=${${ED#*sparkle:edSignature=\"}%%\"*}
"$SPARKLE_BIN/sign_update" --verify "$DMG" "$sig" -p "$ED_PUBLIC" >/dev/null 2>&1 \
  || "$SPARKLE_BIN/sign_update" --verify "$DMG" "$sig" >/dev/null \
  || fail "EdDSA signature does not verify"
note "EdDSA signature verifies"

mkdir -p "$STAGE"
# Mirror the served directory, then replace the feed.
ditto "$PUBLIC_DESKTOP" "$STAGE"
min_os=$(pb LSMinimumSystemVersion)
cat > "$STAGE/appcast.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>vcad</title>
    <link>https://vcad.io/desktop/</link>
    <description>vcad for Mac updates</description>
    <language>en</language>
    <item>
      <title>vcad $VERSION</title>
      <pubDate>$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$BUILD</sparkle:version>
      <sparkle:shortVersionString>$VERSION</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>$min_os</sparkle:minimumSystemVersion>
      <sparkle:releaseNotesLink>https://github.com/muni-works/vcad/releases/tag/$TAG</sparkle:releaseNotesLink>
      <enclosure url="${DOWNLOAD_BASE}vcad-$VERSION.dmg" type="application/octet-stream" $ED/>
    </item>
  </channel>
</rss>
EOF
xmllint --noout "$STAGE/appcast.xml" || fail "appcast.xml is not well-formed"
shasum -a 256 "$DMG" | sed "s|$DIST/||" > "$DMG.sha256"

# ---------------------------------------------------------------- summary
step "Done$([[ $DRY_RUN == 1 ]] && print ' (dry run: NOT notarized, do not ship)')"
print "  app:      $APP"
print "  dmg:      $DMG ($(du -h "$DMG" | cut -f1), sha256 $(cut -c1-12 "$DMG.sha256")…)"
print "  appcast:  $STAGE/appcast.xml  (build $BUILD, $VERSION)"
print "  bundle:   $BUNDLE_ID, min macOS $min_os"
if [[ $DRY_RUN == 0 ]]; then
  print "\nPublish (only with an explicit go):"
  print "  gh release create $TAG \"$DMG\" \"$DMG.sha256\" --repo muni-works/vcad --title \"vcad $VERSION\" --generate-notes"
  print "  cp \"$STAGE/appcast.xml\" packages/app/public/desktop/appcast.xml   # then PR + merge; vcad.io redeploys"
fi
