#!/usr/bin/env bash
# Bundle the NATIVE egui app (core/crates/tabibu-egui) into a signed Tabibu.app.
#
# This is the eframe counterpart to build-app.sh (which bundles the Tauri app).
# It builds the egui binary, assembles a minimal macOS .app by hand (no Tauri /
# cargo-bundle), copies the shared icon, and re-signs with the SAME stable local
# identity so a granted Full Disk Access + Notifications survive rebuilds.
#
#   ./scripts/build-egui.sh            # native (host arch) release  [fast]
#   ./scripts/build-egui.sh --universal# universal (arm64 + x86_64) release
#   ./scripts/build-egui.sh --debug    # quick unoptimized build for testing
#
# Output: core/target/bundle/Tabibu.app  (a STAGING dir — NOT /Applications).
# It is intentionally NOT installed: the Tauri app stays the live /Applications
# build until the migration reaches parity. Same bundle id (xr.seede.tabibu), so
# copying it over the Tauri app at cutover is a true drop-in (FDA/notifications
# carry over via the stable signing identity).
#
# NEVER run with sudo (root-owns target → every later cargo build fails).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

BUNDLE_ID="xr.seede.tabibu"
APP_NAME="Tabibu"
VER="$(tr -d '[:space:]' < VERSION)"
BIN="tabibu-egui" # [[bin]] name in the crate

case "${1:-native}" in
  --debug)      PROFILE_FLAG=();          PROFILE_DIR="debug";   LABEL="debug";             UNIVERSAL=0 ;;
  --native|"")  PROFILE_FLAG=(--release); PROFILE_DIR="release"; LABEL="native release";    UNIVERSAL=0 ;;
  --universal)  PROFILE_FLAG=(--release); PROFILE_DIR="release"; LABEL="universal release"; UNIVERSAL=1 ;;
  -h|--help)    echo "usage: build-egui.sh [--native|--universal|--debug]"; exit 0 ;;
  *)            echo "unknown option: $1 (try --native, --universal, --debug)"; exit 1 ;;
esac

echo "▶ building $APP_NAME (egui) v$VER — $LABEL"

# Build the binary (one arch, or both + lipo for universal).
if [ "$UNIVERSAL" = 1 ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  ( cd core && cargo build -p "$BIN" --release --target aarch64-apple-darwin )
  ( cd core && cargo build -p "$BIN" --release --target x86_64-apple-darwin )
  BIN_ARM="$ROOT/core/target/aarch64-apple-darwin/release/$BIN"
  BIN_X86="$ROOT/core/target/x86_64-apple-darwin/release/$BIN"
  BIN_OUT="$(mktemp -d)/$BIN"
  lipo -create -output "$BIN_OUT" "$BIN_ARM" "$BIN_X86"
else
  ( cd core && cargo build -p "$BIN" "${PROFILE_FLAG[@]}" )
  BIN_OUT="$ROOT/core/target/$PROFILE_DIR/$BIN"
fi
[ -f "$BIN_OUT" ] || { echo "✗ build produced no binary at $BIN_OUT" >&2; exit 1; }

# Assemble the .app from scratch (replace any prior staging build).
APP="$ROOT/core/target/bundle/$APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN_OUT" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"
cp "$ROOT/app/src-tauri/icons/icon.icns" "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>             <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>      <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>       <string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key>       <string>$APP_NAME</string>
  <key>CFBundleIconFile</key>         <string>icon</string>
  <key>CFBundlePackageType</key>      <string>APPL</string>
  <key>CFBundleShortVersionString</key> <string>$VER</string>
  <key>CFBundleVersion</key>          <string>$VER</string>
  <key>LSMinimumSystemVersion</key>   <string>11.0</string>
  <key>NSHighResolutionCapable</key>  <true/>
  <!-- Regular app: Dock icon + window + a menu-bar tray. (The Tauri shell also
       drops to Accessory when the window closes; that activation-policy toggle
       is still TODO in the egui build, so it stays Regular for now.) -->
</dict>
</plist>
PLIST

# Stable local signing (same identity as the Tauri app) so FDA/notifications
# survive rebuilds. Falls back to ad-hoc if the identity was never created.
if security find-identity -v -p codesigning 2>/dev/null | grep -qF "Tabibu Local Signing"; then
  "$ROOT/scripts/dev-sign.sh" "$APP" >/dev/null
  SIGNED=1
else
  codesign --force --deep --sign - --identifier "$BUNDLE_ID" "$APP"
  SIGNED=0
fi

echo
echo "✓ $APP_NAME (egui) v$VER built — $LABEL"
echo "  app: $APP"
if [ "${SIGNED:-0}" = 1 ]; then
  echo "  signed with 'Tabibu Local Signing' (FDA/notifications survive rebuilds)."
else
  echo "  ad-hoc signed. Run ./scripts/dev-sign.sh once, then rebuild, for stable grants."
fi
echo
echo "NOT installed (Tauri app stays live in /Applications until cutover). To test:"
echo "  open \"$APP\""
