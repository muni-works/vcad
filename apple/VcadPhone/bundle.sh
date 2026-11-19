#!/usr/bin/env bash
# Build + bundle vcad for iPhone and install it on a simulator.
# SwiftPM has no iOS app product, so this drives xcodebuild against the
# package and assembles the .app by hand (the VcadVision trick, pointed at
# iphonesimulator). Run build-ffi.sh first.
#
#   ./bundle.sh                 build + install + launch on the booted iPhone sim
#   SIM="iPhone 17 Pro" ./bundle.sh
#   VCAD_OPEN=/path/doc.vcad ./bundle.sh    also open a document on launch
#   NO_LAUNCH=1 ./bundle.sh     build + assemble only
#
# Device builds (TestFlight/App Store) need an iOS distribution certificate
# and provisioning profile — not wired up yet; see docs/iphone-port.md.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
sim="${SIM:-iPhone 17 Pro}"
config="${CONFIG:-Debug}"

[ -d "$here/Libs/VcadFFI.xcframework" ] || { echo "missing Libs/VcadFFI.xcframework — run ./build-ffi.sh" >&2; exit 1; }

echo "xcodebuild (iOS Simulator, $config)…"
cd "$here"
xcodebuild -scheme VcadPhone -configuration "$config" \
  -destination "generic/platform=iOS Simulator" \
  -derivedDataPath "$here/.build/xcode" ARCHS=arm64 build -quiet
bin="$here/.build/xcode/Build/Products/$config-iphonesimulator/VcadPhone"

app="$here/dist/vcad.app"
rm -rf "$app"
mkdir -p "$app"
cp "$bin" "$app/vcad"

# Viewport shaders (screen-space edge lines). Optional: without the Metal
# toolchain the viewport falls back to ribbon edges.
if ls "$here"/../VcadApp/Shaders/*.metal >/dev/null 2>&1 && xcrun -sdk iphonesimulator metal --version >/dev/null 2>&1; then
  air_dir="$(mktemp -d)"
  for f in "$here"/../VcadApp/Shaders/*.metal; do
    xcrun -sdk iphonesimulator metal -c "$f" -o "$air_dir/$(basename "$f" .metal).air"
  done
  xcrun -sdk iphonesimulator metallib "$air_dir"/*.air -o "$app/vcad.metallib"
  rm -rf "$air_dir"
else
  echo "note: Metal toolchain unavailable — skipping vcad.metallib (ribbon edges)"
fi

# Sample documents (the Mac app's Samples/), opened copy-on-open.
mkdir -p "$app/samples"
cp -RL "$here/../VcadApp/Samples/." "$app/samples/"

cat > "$app/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>vcad</string>
  <key>CFBundleDisplayName</key><string>vcad</string>
  <key>CFBundleIdentifier</key><string>io.vcad.phone</string>
  <key>CFBundleExecutable</key><string>vcad</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleShortVersionString</key><string>0.1</string>
  <key>MinimumOSVersion</key><string>18.0</string>
  <key>UIDeviceFamily</key><array><integer>1</integer><integer>2</integer></array>
  <key>UIRequiredDeviceCapabilities</key><array><string>arm64</string></array>
  <key>UILaunchScreen</key><dict/>
  <key>UIApplicationSceneManifest</key>
  <dict><key>UIApplicationSupportsMultipleScenes</key><false/></dict>
  <key>UISupportedInterfaceOrientations</key>
  <array>
    <string>UIInterfaceOrientationPortrait</string>
    <string>UIInterfaceOrientationLandscapeLeft</string>
    <string>UIInterfaceOrientationLandscapeRight</string>
  </array>
  <!-- Documents folder shows in Files as "On My iPhone › vcad"; documents
       there open in place. -->
  <key>UIFileSharingEnabled</key><true/>
  <key>LSSupportsOpeningDocumentsInPlace</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>vcad Document</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Owner</string>
      <key>LSItemContentTypes</key><array><string>io.vcad.document</string></array>
    </dict>
  </array>
  <key>UTExportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>io.vcad.document</string>
      <key>UTTypeDescription</key><string>vcad Document</string>
      <key>UTTypeConformsTo</key><array><string>public.json</string><string>public.data</string></array>
      <key>UTTypeTagSpecification</key>
      <dict><key>public.filename-extension</key><array><string>vcad</string></array></dict>
    </dict>
  </array>
</dict>
</plist>
PLIST

codesign --force --sign - "$app" >/dev/null
echo "built: $app"
[ -n "${NO_LAUNCH:-}" ] && exit 0

udid="$(xcrun simctl list devices available | grep -F "$sim (" | head -1 | sed -E 's/.*\(([0-9A-F-]{36})\).*/\1/')"
[ -n "$udid" ] || { echo "no simulator named '$sim' — create one in Xcode or with simctl create" >&2; exit 1; }
xcrun simctl boot "$udid" 2>/dev/null || true
xcrun simctl install "$udid" "$app"
if [ -n "${VCAD_OPEN:-}" ]; then
  SIMCTL_CHILD_VCAD_OPEN="$VCAD_OPEN" xcrun simctl launch "$udid" io.vcad.phone
else
  xcrun simctl launch "$udid" io.vcad.phone
fi
