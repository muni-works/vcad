#!/bin/bash
# Deploy render-api to HuggingFace Spaces
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL_WASM_DIR="$SCRIPT_DIR/../../kernel-wasm"

echo "Preparing render-api for deployment..."

# Copy kernel-wasm package
rm -rf "$SCRIPT_DIR/kernel-wasm"
mkdir -p "$SCRIPT_DIR/kernel-wasm"

cp "$KERNEL_WASM_DIR/package.json" "$SCRIPT_DIR/kernel-wasm/"
cp "$KERNEL_WASM_DIR/vcad_kernel_wasm.js" "$SCRIPT_DIR/kernel-wasm/"
cp "$KERNEL_WASM_DIR/vcad_kernel_wasm.d.ts" "$SCRIPT_DIR/kernel-wasm/"
cp "$KERNEL_WASM_DIR/vcad_kernel_wasm_bg.wasm" "$SCRIPT_DIR/kernel-wasm/"
cp "$KERNEL_WASM_DIR/vcad_kernel_wasm_bg.wasm.d.ts" "$SCRIPT_DIR/kernel-wasm/"

echo "kernel-wasm copied"

# Check if we have HF CLI
if ! command -v hf &> /dev/null; then
    echo "hf CLI not found. Install with: uv tool install huggingface_hub"
    exit 1
fi

# Create or update the Space
HF_SPACE="campedersen/vcad-render"

echo "Deploying to HuggingFace Space: $HF_SPACE"

cd "$SCRIPT_DIR"

# Upload all files (excludes node_modules and .git)
hf upload "$HF_SPACE" . . \
    --repo-type space \
    --exclude "node_modules/*" \
    --exclude ".git/*" \
    --exclude "*.sh" \
    --commit-message "Deploy vcad render API"

echo "Deployed! Visit: https://huggingface.co/spaces/$HF_SPACE"
