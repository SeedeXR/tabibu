#!/usr/bin/env bash
# Build the Salama DoH resolver (tabibu-dohd) as a UNIVERSAL binary and stage it
# where the Tauri bundle picks it up (app/src-tauri/bin/tabibu-dohd → bundled
# into Tabibu.app/Contents/Resources/). A universal binary runs on both a native
# and a universal app build, so this is safe for every build path.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

mkdir -p app/src-tauri/bin
rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null 2>&1 || true

( cd core && cargo build -p tabibu-dohd --release \
    --target aarch64-apple-darwin --target x86_64-apple-darwin )

lipo -create \
  "$ROOT/core/target/aarch64-apple-darwin/release/tabibu-dohd" \
  "$ROOT/core/target/x86_64-apple-darwin/release/tabibu-dohd" \
  -output "$ROOT/app/src-tauri/bin/tabibu-dohd"
chmod +x "$ROOT/app/src-tauri/bin/tabibu-dohd"
echo "✓ staged universal tabibu-dohd → app/src-tauri/bin/tabibu-dohd"
