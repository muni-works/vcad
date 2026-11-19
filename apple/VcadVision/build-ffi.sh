#!/usr/bin/env bash
# Build vcad-ffi for the Vision Pro simulator and stage it for SwiftPM.
# aarch64-apple-visionos-sim is a tier-3 Rust target: nightly + build-std, and
# the vendored third_party/clipper-sys + third_party/atomic-wait (workspace
# [patch]) supply the visionOS arms the registry crates are missing.
#
# The `recon` feature (photo → CAD via irys-recon) is left out: its kiddo →
# generator dependency doesn't compile for visionOS (libc has no MAP_STACK
# there), and the Vision app doesn't ship photo import.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"

python3 "$repo_root/scripts/sync-ffi-header.py"

echo "Building vcad-ffi for aarch64-apple-visionos-sim (nightly, build-std)…"
SDKROOT="$(xcrun --sdk xrsimulator --show-sdk-path)" XROS_DEPLOYMENT_TARGET=2.0 \
  cargo +nightly build -p vcad-ffi \
  --manifest-path "$repo_root/Cargo.toml" \
  --target aarch64-apple-visionos-sim \
  --no-default-features --features no-builtin-font \
  -Zbuild-std=std,panic_abort --release

mkdir -p "$here/Libs"
cp "$target_dir/aarch64-apple-visionos-sim/release/libvcad_ffi.a" "$here/Libs/"
echo "Staged: $here/Libs/libvcad_ffi.a"
