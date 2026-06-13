#!/bin/zsh
# Build the Rust core as a universal static library and stage it (plus the
# C header) where the Swift packages expect it. Usage: scripts/build-core.sh
# [--debug]. Produces: build/libtabibu_ffi.a (universal) + build/include/.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Match the Swift package's minimum platform (Package.swift: macOS 14).
export MACOSX_DEPLOYMENT_TARGET=14.0
PROFILE="release"
FLAG="--release"
if [[ "${1:-}" == "--debug" ]]; then PROFILE="debug"; FLAG=""; fi

cd "$ROOT/core"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build $FLAG -p tabibu-ffi --target "$target"
done

mkdir -p "$ROOT/build/include"
lipo -create \
  "$ROOT/core/target/aarch64-apple-darwin/$PROFILE/libtabibu_ffi.a" \
  "$ROOT/core/target/x86_64-apple-darwin/$PROFILE/libtabibu_ffi.a" \
  -output "$ROOT/build/libtabibu_ffi.a"
cp "$ROOT/core/include/tabibu_core.h" "$ROOT/build/include/"

lipo -info "$ROOT/build/libtabibu_ffi.a"
echo "Staged: $ROOT/build/libtabibu_ffi.a"
