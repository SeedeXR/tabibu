#!/bin/zsh
# Assemble Tabibu.app (and the embedded TabibuMonitor.app login item) from
# already-built Swift binaries, generate Info.plists, and code-sign.
#
# Usage: scripts/make-app.sh
#
# Inputs (first match wins, per binary):
#   build/Tabibu                                          - staged by CI / hand
#   Tabibu/.build/apple/Products/Release/Tabibu           - `swift build -c release
#                                                           --arch arm64 --arch x86_64`
#   Tabibu/.build/release/Tabibu                          - single-arch `swift build`
#   (same three locations for TabibuMonitor)
#   build/AppIcon.icns                                    - optional app icon
#
# Output: build/Tabibu.app
#
# Env:
#   VERSION        bundle version            (default 0.1.0)
#   SIGN_IDENTITY  codesign identity         (default "-" = ad-hoc)
#   NOTARIZE=1     handled by make-dmg.sh, not here
#
# Signing notes:
#   - We sign nested-first (TabibuMonitor.app, then Tabibu.app). `--deep` is
#     deprecated and signs nested code with the *outer* requirements, so we
#     avoid it for signing and only use it for verification.
#   - The hardened runtime (--options runtime) is technically allowed with
#     ad-hoc signatures, but it is only *required* for notarization, and it
#     can surface entitlement/dyld issues during local iteration. We therefore
#     enable it only when a real Developer ID identity is supplied, keeping
#     local ad-hoc builds frictionless.
#   - On this machine there are NO Developer ID certificates
#     (`security find-identity -v -p codesigning` -> 0 identities), so the
#     default is ad-hoc. Gatekeeper (spctl) will reject ad-hoc apps; that is
#     expected and does not fail this script.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/build"
VERSION="${VERSION:-0.1.0}"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"

APP="$BUILD/Tabibu.app"

# --- locate a built binary, trying the documented locations in order -------
find_binary() {
  local name="$1"
  local candidates=(
    "$BUILD/$name"
    "$ROOT/$name/.build/apple/Products/Release/$name"
    "$ROOT/$name/.build/release/$name"
  )
  for c in "${candidates[@]}"; do
    if [[ -f "$c" && -x "$c" ]]; then
      print -r -- "$c"
      return 0
    fi
  done
  return 1
}

write_plist() {
  # write_plist <path> <bundle-id> <name> <ui-element: true|false>
  local plist="$1" bundle_id="$2" name="$3" ui_element="$4"
  cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>$bundle_id</string>
	<key>CFBundleName</key>
	<string>$name</string>
	<key>CFBundleExecutable</key>
	<string>$name</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>LSUIElement</key>
	<$ui_element/>
EOF
  if [[ "$name" == "Tabibu" && -f "$BUILD/AppIcon.icns" ]]; then
    cat >> "$plist" <<'EOF'
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
EOF
  fi
  cat >> "$plist" <<'EOF'
</dict>
</plist>
EOF
  plutil -lint "$plist" > /dev/null
}

sign_app() {
  # sign_app <bundle-path>
  local bundle="$1"
  local -a sign_args
  sign_args=(--force --timestamp=none -s "$SIGN_IDENTITY")
  if [[ "$SIGN_IDENTITY" != "-" ]]; then
    # Real identity: hardened runtime (required for notarization) and a
    # proper secure timestamp.
    sign_args=(--force --options runtime --timestamp -s "$SIGN_IDENTITY")
  fi
  codesign "${sign_args[@]}" "$bundle"
}

# --- main app binary --------------------------------------------------------
if ! TABIBU_BIN="$(find_binary Tabibu)"; then
  echo "error: no built Tabibu binary found. Looked in:" >&2
  echo "  - $BUILD/Tabibu" >&2
  echo "  - $ROOT/Tabibu/.build/apple/Products/Release/Tabibu" >&2
  echo "  - $ROOT/Tabibu/.build/release/Tabibu" >&2
  echo "Build it first, e.g.:" >&2
  echo "  swift build -c release --arch arm64 --arch x86_64 --package-path Tabibu" >&2
  exit 1
fi
echo "Using Tabibu binary: $TABIBU_BIN"
lipo -info "$TABIBU_BIN" 2>/dev/null || true

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$TABIBU_BIN" "$APP/Contents/MacOS/Tabibu"
write_plist "$APP/Contents/Info.plist" "xr.seede.tabibu" "Tabibu" "false"

# SwiftPM emits a `<Pkg>_<Target>.bundle` next to the executable for any
# target with `resources:` (Tabibu bundles its Lucide icons). Bundle.module
# resolves it from Contents/Resources/ inside the .app — copy it or the app
# fatal-errors at launch ("unable to find bundle named …").
copy_resource_bundles() {
  local bin="$1"
  local srcdir; srcdir="$(dirname "$bin")"
  for b in "$srcdir"/*.bundle; do
    [[ -d "$b" ]] || continue
    cp -R "$b" "$APP/Contents/Resources/"
    echo "Embedded resource bundle: $(basename "$b")"
  done
}
copy_resource_bundles "$TABIBU_BIN"

if [[ -f "$BUILD/AppIcon.icns" ]]; then
  cp "$BUILD/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
  echo "Embedded icon: build/AppIcon.icns"
else
  echo "note: build/AppIcon.icns not found; building without an icon."
fi

# --- optional menu-bar agent, embedded as a login item ----------------------
MONITOR_APP=""
if MONITOR_BIN="$(find_binary TabibuMonitor)"; then
  echo "Using TabibuMonitor binary: $MONITOR_BIN"
  MONITOR_APP="$APP/Contents/Library/LoginItems/TabibuMonitor.app"
  mkdir -p "$MONITOR_APP/Contents/MacOS"
  cp "$MONITOR_BIN" "$MONITOR_APP/Contents/MacOS/TabibuMonitor"
  write_plist "$MONITOR_APP/Contents/Info.plist" \
    "xr.seede.tabibu.monitor" "TabibuMonitor" "true"
else
  echo "note: TabibuMonitor binary not found; skipping login-item embed."
  echo "      (build it with: swift build -c release --arch arm64 --arch x86_64 --package-path TabibuMonitor)"
fi

# --- sign: nested first, then the outer app ---------------------------------
if [[ "$SIGN_IDENTITY" == "-" ]]; then
  echo "Signing ad-hoc (SIGN_IDENTITY=-). Set SIGN_IDENTITY='Developer ID Application: ...' for distribution."
else
  echo "Signing with identity: $SIGN_IDENTITY (hardened runtime enabled)"
fi
[[ -n "$MONITOR_APP" ]] && sign_app "$MONITOR_APP"
sign_app "$APP"

# --- verify ------------------------------------------------------------------
codesign --verify --deep --strict "$APP"
echo "codesign --verify --deep --strict: OK"

echo "spctl assessment (informational; ad-hoc builds are expected to be rejected):"
if spctl -a -t exec -vv "$APP" 2>&1; then
  echo "spctl: accepted"
else
  if [[ "$SIGN_IDENTITY" == "-" ]]; then
    echo "spctl: rejected -- expected for an ad-hoc signature. Gatekeeper only"
    echo "accepts Developer ID-signed, notarized apps; both are externally"
    echo "blocked right now (no Developer ID certificate, no notarytool"
    echo "credentials). This does not affect local launching via 'open' or Finder"
    echo "right-click > Open."
  else
    echo "spctl: rejected despite a real identity -- the app is probably not"
    echo "notarized yet. Run make-dmg.sh with NOTARIZE=1 once credentials exist."
  fi
fi

echo "Built: $APP"
