#!/usr/bin/env bash
# Manual local build of the Tabibu desktop app. Always stamps the version from
# the root VERSION file first (single source of truth), then runs `tauri build`.
#
#   ./scripts/build-app.sh            # universal release .app + DMG (distributable)
#   ./scripts/build-app.sh --native   # release, host arch only (faster)
#   ./scripts/build-app.sh --debug    # quick unoptimized build for testing
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

./scripts/sync-version.sh >/dev/null
VER="$(tr -d '[:space:]' < VERSION)"

UNIVERSAL=0
case "${1:-universal}" in
  --debug)        BUILD=(--debug);                             SUB="debug/bundle";                       LABEL="debug" ;;
  --native)       BUILD=();                                    SUB="release/bundle";                     LABEL="native release" ;;
  universal|"")   BUILD=(--target universal-apple-darwin);     SUB="universal-apple-darwin/release/bundle"; LABEL="universal release"; UNIVERSAL=1 ;;
  -h|--help)      echo "usage: build-app.sh [--debug|--native|universal]"; exit 0 ;;
  *)              echo "unknown option: $1 (try --debug, --native, or no arg)"; exit 1 ;;
esac

# Universal needs both arches; install them (no-op if already present).
# `if` (not `... && ...`) so a false test never trips `set -e`.
if [ "$UNIVERSAL" = 1 ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
fi

cd app
[ -d node_modules ] || npm install   # Tauri CLI (frontend is static, no bundler)

echo "▶ building Tabibu v$VER — $LABEL"
# `${BUILD[@]+...}` so an empty array (--native) doesn't trip `set -u` on bash 3.2.
npx tauri build ${BUILD[@]+"${BUILD[@]}"}

bundle="$ROOT/app/src-tauri/target/$SUB"

# Stable local signing: if you've created the self-signed identity (once, via
# scripts/dev-sign.sh), re-sign the built app with it so macOS keeps Full Disk
# Access + Notification grants across rebuilds. No Apple account needed; this
# only affects this Mac. Otherwise the app stays ad-hoc signed.
APP="$(find "$bundle/macos" -maxdepth 1 -name '*.app' 2>/dev/null | head -1)"
if [ -n "$APP" ] && security find-identity -v -p codesigning 2>/dev/null | grep -qF "Tabibu Local Signing"; then
  "$ROOT/scripts/dev-sign.sh" "$APP" || true
  SIGNED=1
fi

echo
echo "✓ Tabibu v$VER built — $LABEL"
[ -n "$APP" ] && echo "  app: $APP"
[ -d "$bundle/dmg" ] && find "$bundle/dmg" -maxdepth 1 -name "*.dmg" -exec echo "  dmg: {}" \;
echo
if [ "${SIGNED:-0}" = 1 ]; then
  echo "Signed with your local identity — a granted Full Disk Access survives rebuilds."
else
  echo "Ad-hoc signed (no Developer ID). Tip: run ./scripts/dev-sign.sh once, then rebuild —"
  echo "Full Disk Access & notifications then survive rebuilds (no Apple account needed)."
fi
echo "On other Macs it's still unsigned: open via right-click → Open."
