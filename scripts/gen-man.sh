#!/usr/bin/env bash
# Refresh the committed man page from the clap definition. build.rs writes the
# canonical page to OUT_DIR on every build; this copies that snapshot into the
# repo (core/crates/tabibu-cli/man/tabibu.1) for viewing and packaging.
set -euo pipefail
cd "$(dirname "$0")/.."

( cd core && cargo build -p tabibu-cli >/dev/null )
# Pick the NEWEST generated page: there can be several build-script OUT_DIRs
# (debug + release + stale hashes), and an arbitrary one (e.g. an old release
# build) would copy a STALE man page. A `find | xargs ls -t` pipe is NOT used:
# under GNU xargs an empty match still runs `ls` (listing the cwd), which would
# defeat the not-found guard below. This newer-than loop is portable and, on no
# match, leaves MAN empty so the guard fires.
MAN=""
while IFS= read -r f; do
  if [ -z "$MAN" ] || [ "$f" -nt "$MAN" ]; then MAN="$f"; fi
done < <(find core/target -path '*/build/tabibu-cli-*/out/tabibu.1' 2>/dev/null)
[ -n "$MAN" ] || { echo "error: generated tabibu.1 not found in OUT_DIR" >&2; exit 1; }

mkdir -p core/crates/tabibu-cli/man
cp "$MAN" core/crates/tabibu-cli/man/tabibu.1
echo "✓ refreshed core/crates/tabibu-cli/man/tabibu.1"
