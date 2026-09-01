#!/usr/bin/env bash
# Give Tabibu a STABLE local code identity so macOS keeps its Full Disk Access
# and Notification grants across rebuilds — with NO Apple Developer account.
#
# How: a self-signed code-signing certificate, created once in a dedicated
# Tabibu keychain and trusted locally. Signing with it yields a cert-based
# "designated requirement" (constant across builds), instead of the per-build
# cdhash an unsigned/ad-hoc app gets (which makes macOS treat each rebuild as a
# new app and drop the prior grant).
#
# This is NOT notarization (that needs a paid Developer ID and only helps OTHER
# Macs open the app). It changes nothing outside THIS Mac.
#
#   ./scripts/dev-sign.sh                      # create the identity if missing
#   ./scripts/dev-sign.sh /path/to/Tabibu.app  # ...and sign that app with it
#   ./scripts/dev-sign.sh --remove             # delete the identity + keychain
#
# build-app.sh / install.sh call this automatically after a build when the
# identity exists (else they fall back to ad-hoc).
set -euo pipefail

IDENTITY="Tabibu Local Signing"
BUNDLE_ID="xr.seede.tabibu"
KC="$HOME/Library/Keychains/tabibu-signing.keychain-db"
PW_FILE="$HOME/.config/tabibu/.signing-keychain-pw"

kc_password() {
  # A generated password for the DEDICATED keychain (not your login one), stored
  # 0600 so the build can unlock it non-interactively. It guards only a local,
  # self-signed cert used for local signing — not a sensitive secret.
  if [ -f "$PW_FILE" ]; then cat "$PW_FILE"; return; fi
  mkdir -p "$(dirname "$PW_FILE")"; chmod 700 "$(dirname "$PW_FILE")"
  local pw; pw=$(openssl rand -hex 24)
  printf '%s' "$pw" > "$PW_FILE"; chmod 600 "$PW_FILE"
  printf '%s' "$pw"
}

in_search_list() { security list-keychains -d user | sed 's/"//g' | grep -qF "$KC"; }
add_to_search_list() {
  in_search_list && return 0
  local others; others=$(security list-keychains -d user | sed 's/"//g' | xargs)
  # shellcheck disable=SC2086
  security list-keychains -d user -s "$KC" $others
}

remove_identity() {
  security list-keychains -d user -s $(security list-keychains -d user | sed 's/"//g' | grep -vF "$KC" | xargs) 2>/dev/null || true
  security delete-keychain "$KC" 2>/dev/null || true
  rm -f "$PW_FILE"
  echo "✓ removed the Tabibu signing identity and keychain."
}

ensure_identity() {
  if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$IDENTITY"; then
    return 0
  fi
  echo "▶ creating a one-time self-signed code-signing identity ('$IDENTITY')…"
  local pw; pw=$(kc_password)
  local tmp; tmp=$(mktemp -d); trap 'rm -rf "$tmp"' RETURN
  openssl req -x509 -newkey rsa:2048 -keyout "$tmp/k.pem" -out "$tmp/c.pem" -days 3650 -nodes \
    -subj "/CN=$IDENTITY" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature" >/dev/null 2>&1
  # -legacy: openssl 3's default PKCS#12 MAC isn't importable by macOS `security`.
  openssl pkcs12 -export -legacy -out "$tmp/id.p12" -inkey "$tmp/k.pem" -in "$tmp/c.pem" \
    -passout "pass:$pw" -name "$IDENTITY" >/dev/null 2>&1

  [ -f "$KC" ] || security create-keychain -p "$pw" "$KC"
  security set-keychain-settings "$KC"          # no auto-lock timeout
  security unlock-keychain -p "$pw" "$KC"
  add_to_search_list
  security import "$tmp/id.p12" -k "$KC" -P "$pw" -T /usr/bin/codesign -A >/dev/null
  security add-trusted-cert -d -r trustRoot -p codeSign -k "$KC" "$tmp/c.pem" >/dev/null 2>&1 || true
  # Let codesign use the key without a GUI prompt.
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pw" "$KC" >/dev/null 2>&1 || true
  echo "✓ identity ready."
}

sign_app() {
  local app="$1"
  [ -d "$app" ] || { echo "✗ not an app bundle: $app" >&2; exit 1; }
  security unlock-keychain -p "$(kc_password)" "$KC" 2>/dev/null || true
  add_to_search_list
  echo "▶ signing $(basename "$app") with '$IDENTITY'…"
  codesign --force --deep --sign "$IDENTITY" --identifier "$BUNDLE_ID" "$app"
  echo "✓ signed. Designated requirement (stable across rebuilds):"
  codesign -d -r- "$app" 2>&1 | sed -n 's/^designated => /   /p'
}

case "${1:-}" in
  --remove) remove_identity; exit 0 ;;
  -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
esac

ensure_identity
[ $# -ge 1 ] && sign_app "$1" || echo "Identity ready. Pass a .app path to sign it."
