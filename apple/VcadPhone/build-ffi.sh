#!/usr/bin/env bash
# Build vcad-ffi for iPhone (device + simulator) and package an xcframework.
# Both targets are tier-2 stable Rust — no nightly/build-std, unlike visionOS.
#
#   ./build-ffi.sh            device + simulator → Libs/VcadFFI.xcframework
#   TARGETS=sim ./build-ffi.sh   simulator only (faster local loop)
#
# Libs/libvcad_ffi.a is also staged (the simulator slice) so a plain
# `swift build`-style -L search keeps working.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
config="${CONFIG:-release}"
which="${TARGETS:-all}"

python3 "$repo_root/scripts/sync-ffi-header.py"

triples=()
case "$which" in
  sim)    triples=(aarch64-apple-ios-sim) ;;
  device) triples=(aarch64-apple-ios) ;;
  *)      triples=(aarch64-apple-ios aarch64-apple-ios-sim) ;;
esac

profile_flag=(); [ "$config" = "release" ] && profile_flag=(--release)
for t in "${triples[@]}"; do
  echo "Building vcad-ffi for $t ($config)…"
  sdk=iphoneos; [[ "$t" == *-sim ]] && sdk=iphonesimulator
  SDKROOT="$(xcrun --sdk "$sdk" --show-sdk-path)" \
  IPHONEOS_DEPLOYMENT_TARGET=18.0 \
    cargo build -p vcad-ffi --manifest-path "$repo_root/Cargo.toml" \
      --target "$t" ${profile_flag[@]+"${profile_flag[@]}"}
done

mkdir -p "$here/Libs"
rm -rf "$here/Libs/VcadFFI.xcframework"
args=()
for t in "${triples[@]}"; do
  args+=(-library "$target_dir/$t/$config/libvcad_ffi.a" -headers "$here/FFIHeaders")
done
xcodebuild -create-xcframework "${args[@]}" -output "$here/Libs/VcadFFI.xcframework"
sim_lib="$target_dir/aarch64-apple-ios-sim/$config/libvcad_ffi.a"
[ -f "$sim_lib" ] && cp "$sim_lib" "$here/Libs/libvcad_ffi.a"
echo "Staged: $here/Libs/VcadFFI.xcframework"
