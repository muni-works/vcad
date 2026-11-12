#!/usr/bin/env bash
# Build the slim kernel WASM the landing site's demos load (crates/vcad-site-wasm).
#
#   www/scripts/build-site-kernel.sh [out-dir]     (default: www/site-kernel)
#
# Output is a site-only build artifact (gitignored) — NOT the checked-in
# packages/kernel-wasm bundle, which has a single writer (wasm-refresh.yml).
#
# Pipeline: cargo (profile site-wasm: opt-level z, fat LTO, 1 CGU, panic=abort,
# strip, +simd128) → wasm-bindgen --target web → wasm-opt -Oz. Needs the
# wasm32-unknown-unknown target; wasm-bindgen-cli at the Cargo.lock version and
# binaryen's wasm-opt are fetched on demand (cargo install / npx).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$ROOT/www/site-kernel}"
PROFILE=site-wasm
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

BINDGEN_VER="$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/version = |"/,""); print; exit}' "$ROOT/Cargo.lock")"
if ! wasm-bindgen --version 2>/dev/null | grep -q "$BINDGEN_VER"; then
  cargo install -q wasm-bindgen-cli --version "$BINDGEN_VER" --locked
fi

RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+simd128,+bulk-memory,+nontrapping-fptoint,+sign-ext" \
  cargo build -p vcad-site-wasm --target wasm32-unknown-unknown --profile "$PROFILE" \
  --manifest-path "$ROOT/Cargo.toml"

RAW="$TARGET_DIR/wasm32-unknown-unknown/$PROFILE/vcad_site_wasm.wasm"
mkdir -p "$OUT"
wasm-bindgen "$RAW" --out-dir "$OUT" --target web --no-typescript

WASM="$OUT/vcad_site_wasm_bg.wasm"
before=$(wc -c <"$WASM")
if command -v wasm-opt >/dev/null; then OPT=(wasm-opt); else OPT=(npx -y -p binaryen@123 wasm-opt); fi
"${OPT[@]}" -Oz --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int \
  --enable-sign-ext --enable-mutable-globals --strip-debug --strip-producers \
  "$WASM" -o "$WASM.opt"
mv "$WASM.opt" "$WASM"

after=$(wc -c <"$WASM")
# sizes + the meta the loader uses for its progress bar (decoded bytes)
node -e '
const fs=require("fs"),z=require("zlib");const b=fs.readFileSync(process.argv[1]);
const gz=z.gzipSync(b,{level:9}).length;
const br=z.brotliCompressSync(b,{params:{[z.constants.BROTLI_PARAM_QUALITY]:11,[z.constants.BROTLI_PARAM_SIZE_HINT]:b.length}}).length;
fs.writeFileSync(process.argv[2],JSON.stringify({bytes:b.length,gzip:gz,brotli:br},null,1)+"\n");
console.log(`vcad-site-wasm: bindgen ${process.argv[3]} B -> wasm-opt ${b.length} B · gzip -9 ${gz} B · brotli 11 ${br} B`);
' "$WASM" "$OUT/meta.json" "$before"
