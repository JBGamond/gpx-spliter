#!/bin/bash
set -e

# 1. Build the WASM module locally
echo "Building WASM module..."
# Check if wasm-pack is in PATH, otherwise try the cargo bin path
if command -v wasm-pack &> /dev/null; then
    WASM_PACK="$(command -v wasm-pack)"
else
    WASM_PACK="$HOME/.cargo/bin/wasm-pack"
fi

if [ ! -f "$WASM_PACK" ]; then
    echo "Error: wasm-pack not found. Please install it with 'cargo install wasm-pack'."
    exit 1
fi

$WASM_PACK build --target web --out-dir static/pkg -- --no-default-features --features wasm

# 2. Build the Docker image
echo "Building Docker image..."
docker build -t gpx-splitter . --no-cache

echo "Done! You can now run the app with:"
echo "docker run -p 8080:80 gpx-splitter"
# Or if using docker-compose:
# docker compose up -d
