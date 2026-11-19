#!/usr/bin/env bash
# Build + bundle the visionOS spike for the Vision Pro SIMULATOR and install it.
# SwiftPM has no visionOS app template, so this drives swift-build with an
# explicit target triple + SDK and assembles the .app by hand (the sibling
# VcadApp/bundle.sh trick, pointed at xrsimulator). Run build-ffi.sh first.
#
#   ./bundle.sh          build + install + launch on the booted simulator
#
# Launch-time env (VCAD_OPEN, VCAD_RELEASE, VCAD_RELEASE_SELECT, VCAD_RELEASE_RETURN,
# VCAD_RELEASE_MOVE) is forwarded into the simulator app: simctl only passes
# variables prefixed SIMCTL_CHILD_, so they're re-exported under that prefix.
#
# Needs a visionOS simulator named "Apple Vision Pro" built from the 4K device
# type (the older Apple-Vision-Pro type is incompatible with visionOS 26):
#   xcrun simctl create "Apple Vision Pro" \
#     com.apple.CoreSimulator.SimDeviceType.Apple-Vision-Pro-4K \
#     com.apple.CoreSimulator.SimRuntime.xrOS-26-5
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

# swift-build's bare --triple/--sdk emits ELF-style linker args for the xros
# triple; xcodebuild drives the same package with a proper visionOS toolchain.
echo "xcodebuild (visionOS Simulator)…"
cd "$here"
xcodebuild -scheme VcadVision \
  -destination 'platform=visionOS Simulator,name=Apple Vision Pro' \
  -derivedDataPath "$here/.build/xcode" build -quiet
bin="$here/.build/xcode/Build/Products/Debug-xrsimulator/VcadVision"

app="$here/dist/vcad-vision.app"
rm -rf "$app"
mkdir -p "$app"
cp "$bin" "$app/vcad-vision"

cat > "$app/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>vcad</string>
  <key>CFBundleDisplayName</key><string>vcad</string>
  <key>CFBundleIdentifier</key><string>io.vcad.vision</string>
  <key>CFBundleExecutable</key><string>vcad-vision</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleShortVersionString</key><string>0.1</string>
  <key>MinimumOSVersion</key><string>2.0</string>
  <key>UIDeviceFamily</key><array><integer>7</integer></array>
  <key>UIApplicationSceneManifest</key>
  <dict>
    <key>UIApplicationSupportsMultipleScenes</key><true/>
    <key>UIApplicationPreferredDefaultSceneSessionRole</key>
    <string>UIWindowSceneSessionRoleVolumetricApplication</string>
  </dict>
</dict>
</plist>
PLIST

codesign --force --sign - "$app" >/dev/null
echo "built: $app"

xcrun simctl uninstall booted io.vcad.vision 2>/dev/null || true   # fresh scene state
xcrun simctl install booted "$app"
for v in VCAD_OPEN VCAD_RELEASE VCAD_RELEASE_SELECT VCAD_RELEASE_MOVE VCAD_RELEASE_RETURN; do
  if [ -n "${!v:-}" ]; then export "SIMCTL_CHILD_$v=${!v}"; fi
done
xcrun simctl launch --terminate-running-process booted io.vcad.vision
