#!/bin/zsh
# Package build/Tabibu.app into a compressed DMG for distribution.
#
# Usage: scripts/make-dmg.sh
#
# Output: build/Tabibu-<version>.dmg
#
# Env:
#   VERSION    overrides the version (default: CFBundleShortVersionString read
#              from the built app's Info.plist, falling back to 0.1.0)
#   NOTARIZE=1 attempt notarization + stapling. Only runs if notarytool
#              credentials exist (keychain profile "tabibu-notary" or
#              APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD env vars); otherwise
#              prints exactly what was skipped and why.
#
# Format choice: ULMO (LZMA) gives the best compression for download size and
# is supported by hdiutil on this machine (macOS 26.5); ULMO images require
# macOS 10.15+ to mount, which is fine since the app needs 13.0. We still
# probe at runtime and fall back ULFO (lzfse, 10.11+) then UDZO (zlib,
# ancient) in case this script runs on an older builder.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/build"
APP="$BUILD/Tabibu.app"

if [[ ! -d "$APP" ]]; then
  echo "error: $APP not found. Run scripts/make-app.sh first." >&2
  exit 1
fi

# --- version -----------------------------------------------------------------
PLIST="$APP/Contents/Info.plist"
if [[ -z "${VERSION:-}" ]]; then
  if [[ -f "$PLIST" ]]; then
    VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$PLIST" 2>/dev/null || true)"
  fi
  VERSION="${VERSION:-0.1.0}"
fi
DMG="$BUILD/Tabibu-$VERSION.dmg"

# --- pick the best supported compression format ------------------------------
# Probe hdiutil's advertised formats; prefer LZMA > lzfse > zlib. Pure zsh
# substring match (no `print | grep -q`: grep -q exits at first match and can
# EPIPE the writer, which under pipefail turns into a spurious failure).
SUPPORTED="$(hdiutil create -help 2>&1 || true)"
FORMAT=""
for candidate in ULMO ULFO UDZO; do
  if [[ "$SUPPORTED" == *"$candidate - compressed"* ]]; then
    FORMAT="$candidate"
    break
  fi
done
if [[ -z "$FORMAT" ]]; then
  echo "warning: could not detect compressed formats from 'hdiutil create -help'; using UDZO." >&2
  FORMAT="UDZO"
fi
case "$FORMAT" in
  ULMO) echo "DMG format: ULMO (LZMA -- best ratio, mounts on macOS 10.15+)";;
  ULFO) echo "DMG format: ULFO (lzfse -- ULMO unsupported here, mounts on 10.11+)";;
  UDZO) echo "DMG format: UDZO (zlib -- universal fallback)";;
esac

# --- stage and create --------------------------------------------------------
STAGING="$(mktemp -d "${TMPDIR:-/tmp}/tabibu-dmg.XXXXXX")"
cleanup() { rm -rf "$STAGING"; }
trap cleanup EXIT

cp -R "$APP" "$STAGING/Tabibu.app"
ln -s /Applications "$STAGING/Applications"

rm -f "$DMG"
hdiutil create -volname Tabibu -srcfolder "$STAGING" -format "$FORMAT" -ov "$DMG"

APP_SIZE_H="$(du -sh "$APP" | cut -f1)"
APP_BYTES="$(du -sk "$APP" | cut -f1)"   # KiB
DMG_BYTES_RAW="$(stat -f%z "$DMG")"
DMG_SIZE_H="$(du -sh "$DMG" | cut -f1)"
RATIO=$(( APP_BYTES * 1024.0 / DMG_BYTES_RAW ))
printf 'App size: %s  ->  DMG size: %s  (compression ratio %.2fx, format %s)\n' \
  "$APP_SIZE_H" "$DMG_SIZE_H" "$RATIO" "$FORMAT"

# --- verify the image mounts -------------------------------------------------
echo "Verifying DMG mounts..."
MOUNT_POINT="$(hdiutil attach -nobrowse -readonly "$DMG" | awk -F'\t' '/\/Volumes\// {print $NF; exit}')"
if [[ -z "$MOUNT_POINT" || ! -d "$MOUNT_POINT/Tabibu.app" ]]; then
  [[ -n "$MOUNT_POINT" ]] && hdiutil detach "$MOUNT_POINT" -quiet || true
  echo "error: DMG mounted but Tabibu.app not found inside (mount: ${MOUNT_POINT:-none})." >&2
  exit 1
fi
hdiutil detach "$MOUNT_POINT" -quiet
echo "Mount check: OK ($MOUNT_POINT)"

# --- optional notarization ----------------------------------------------------
if [[ "${NOTARIZE:-0}" == "1" ]]; then
  if xcrun notarytool history --keychain-profile tabibu-notary > /dev/null 2>&1; then
    echo "Notarizing with keychain profile 'tabibu-notary'..."
    xcrun notarytool submit "$DMG" --keychain-profile tabibu-notary --wait
    xcrun stapler staple "$DMG"
    echo "Notarized and stapled."
  elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
    echo "Notarizing with APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD..."
    xcrun notarytool submit "$DMG" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
      --password "$APPLE_APP_PASSWORD" --wait
    xcrun stapler staple "$DMG"
    echo "Notarized and stapled."
  else
    echo "SKIPPED: notarization. NOTARIZE=1 was set, but no credentials are available:"
    echo "  - no 'tabibu-notary' keychain profile (create with: xcrun notarytool store-credentials)"
    echo "  - APPLE_ID / APPLE_TEAM_ID / APPLE_APP_PASSWORD env vars not all set"
    echo "Notarization also requires a Developer ID signature; this machine has no"
    echo "Developer ID certificate, so the step is externally blocked until the"
    echo "Apple Developer Program enrollment lands. See docs/release.md."
  fi
else
  echo "Notarization not requested (set NOTARIZE=1 to attempt it)."
fi

echo "Built: $DMG"
