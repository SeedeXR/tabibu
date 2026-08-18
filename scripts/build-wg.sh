#!/usr/bin/env bash
# Build a BUNDLED userspace WireGuard (Cloudflare's boringtun-cli) as a
# universal binary and stage it where the Tauri bundle picks it up
# (app/src-tauri/bin/tabibu-wg → Tabibu.app/Contents/Resources/). This is what
# makes the VPN holistic: no Homebrew, no Go, no wireguard-tools install — the
# tunnel engine ships inside the app, mirroring how tabibu-dohd is bundled.
#
# boringtun-cli creates the utun and runs the WireGuard state machine; Tabibu
# configures the peer over its UAPI socket and sets routes/DNS with macOS base
# tools. Pinned version — no surprise upgrades of a security-critical binary.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
VER="0.7.1"

mkdir -p app/src-tauri/bin
rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null 2>&1 || true

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

for pair in "aarch64-apple-darwin:arm" "x86_64-apple-darwin:x86"; do
  target="${pair%%:*}"; slot="${pair##*:}"
  cargo install boringtun-cli --version "$VER" --locked \
    --target "$target" --root "$tmp/$slot" >/dev/null
done

lipo -create \
  "$tmp/arm/bin/boringtun-cli" \
  "$tmp/x86/bin/boringtun-cli" \
  -output "$ROOT/app/src-tauri/bin/tabibu-wg"
chmod +x "$ROOT/app/src-tauri/bin/tabibu-wg"
echo "✓ staged universal tabibu-wg (boringtun-cli $VER) → app/src-tauri/bin/tabibu-wg"
lipo -info "$ROOT/app/src-tauri/bin/tabibu-wg"
