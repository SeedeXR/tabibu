#!/usr/bin/env bash
# Build EVERYTHING and INSTALL it, in one shot:
#   • the Tabibu desktop app (+ its bundled root helpers tabibu-dohd / tabibu-wg)
#     → /Applications/Tabibu.app
#   • the `tabibu` command-line companion            → /usr/local/bin/tabibu
#   • its man page                                    → /usr/local/share/man/man1/tabibu.1
#
#   ./scripts/install.sh              # universal release app + host-arch release CLI, then install
#   ./scripts/install.sh --native     # app for this Mac's arch only (faster)
#   ./scripts/install.sh --debug      # quick unoptimized build of both
#   ./scripts/install.sh --no-install # build everything, install nothing (just report paths)
#   ./scripts/install.sh --no-sign    # skip the stable local code-signing (ad-hoc only)
#
# By default it also creates (once) a LOCAL self-signed code-signing identity and
# signs the app with it, so Full Disk Access + Notification grants survive future
# rebuilds — no Apple account, not notarization, this Mac only (see dev-sign.sh).
#
# Override install locations with env vars (handy for testing):
#   APPDIR=/Applications  BINDIR=/usr/local/bin  MANDIR=/usr/local/share/man/man1
#
# `sudo` is used ONLY for a destination that isn't writable as you — so a normal
# /Applications install needs no password, while /usr/local/bin usually does.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

MODE="universal"
INSTALL=1
SIGN=1
for a in "$@"; do
  case "$a" in
    --no-install)          INSTALL=0 ;;
    --no-sign)             SIGN=0 ;;
    --debug|--native)      MODE="$a" ;;
    universal)             MODE="universal" ;;
    -h|--help)             sed -n '2,21p' "$0"; exit 0 ;;
    *) echo "unknown option: $a (try --native, --debug, --no-install, --no-sign)"; exit 1 ;;
  esac
done

# Per-mode: where `tauri build` puts the bundle, and how to build the CLI.
case "$MODE" in
  --debug)   SUB="debug/bundle";                          CLI_FLAG=(); CLI_DIR="debug" ;;
  --native)  SUB="release/bundle";                        CLI_FLAG=(--release); CLI_DIR="release" ;;
  universal) SUB="universal-apple-darwin/release/bundle"; CLI_FLAG=(--release); CLI_DIR="release" ;;
esac

APPDIR="${APPDIR:-/Applications}"
BINDIR="${BINDIR:-/usr/local/bin}"
MANDIR="${MANDIR:-/usr/local/share/man/man1}"

# ---- stable local signing (once) -----------------------------------------
# Ensure the self-signed identity exists BEFORE the build, so build-app.sh signs
# the app with it (giving a code identity that's constant across rebuilds, so a
# Full Disk Access grant sticks). Skipped with --no-sign; failure is non-fatal
# (falls back to ad-hoc).
if [ "$SIGN" = 1 ]; then
  ./scripts/dev-sign.sh || echo "  (couldn't set up local signing — continuing ad-hoc)"
fi

# ---- build ---------------------------------------------------------------
# The desktop app (stamps VERSION, builds the bundled helpers, runs tauri build).
./scripts/build-app.sh "$MODE"

# The CLI (host arch is all a local install needs; --debug tracks the app mode).
echo "▶ building the tabibu CLI"
( cd core && cargo build ${CLI_FLAG[@]+"${CLI_FLAG[@]}"} -p tabibu-cli )

# Refresh the committed man page from the clap definition.
./scripts/gen-man.sh >/dev/null

APP="$(find "app/src-tauri/target/$SUB/macos" -maxdepth 1 -name '*.app' 2>/dev/null | head -1)"
CLI="core/target/$CLI_DIR/tabibu"
MAN="core/crates/tabibu-cli/man/tabibu.1"
[ -d "$APP" ] || { echo "✗ no .app found under app/src-tauri/target/$SUB/macos"; exit 1; }
[ -x "$CLI" ] || { echo "✗ CLI binary not found at $CLI"; exit 1; }

if [ "$INSTALL" = 0 ]; then
  echo
  echo "✓ built (not installed):"
  echo "  app: $APP"
  echo "  cli: $CLI"
  echo "  man: $MAN"
  exit 0
fi

# ---- install -------------------------------------------------------------
# Run a command, elevating with sudo only if it fails as the current user.
run_or_sudo() {
  if "$@" 2>/dev/null; then return 0; fi
  echo "  (elevating: sudo $*)"
  sudo "$@"
}

echo "▶ installing"
# App → /Applications (replace any existing copy).
run_or_sudo rm -rf "$APPDIR/$(basename "$APP")"
run_or_sudo cp -R "$APP" "$APPDIR/"
echo "  ✓ $APPDIR/$(basename "$APP")"

# CLI → /usr/local/bin.
run_or_sudo mkdir -p "$BINDIR"
run_or_sudo cp "$CLI" "$BINDIR/tabibu"
echo "  ✓ $BINDIR/tabibu"

# Man page → /usr/local/share/man/man1.
run_or_sudo mkdir -p "$MANDIR"
run_or_sudo cp "$MAN" "$MANDIR/tabibu.1"
echo "  ✓ $MANDIR/tabibu.1"

echo
echo "Done. Try:  tabibu doctor    ·    man tabibu    ·    open -a Tabibu"
echo "(First launch: right-click → Open.)"
if [ "$SIGN" = 1 ]; then
  echo "Signed with your local identity — grant Full Disk Access once and it survives rebuilds."
else
  echo "Ad-hoc build (--no-sign): Full Disk Access won't persist across rebuilds."
fi
