#!/usr/bin/env bash
# Refresh the committed man page from the clap definition. build.rs writes the
# canonical page to OUT_DIR on every build; this copies that snapshot into the
# repo (core/crates/tabibu-cli/man/tabibu.1) for viewing and packaging.
set -euo pipefail
cd "$(dirname "$0")/.."

( cd core && cargo build -p tabibu-cli >/dev/null )
# Pick the NEWEST generated page: there can be several build-script OUT_DIRs
# (debug + release + stale hashes), and an arbitrary one (e.g. an old release
# build) would copy a STALE man page. `ls -t` orders by mtime, newest first.
MAN=$(find core/target -path '*/build/tabibu-cli-*/out/tabibu.1' 2>/dev/null | xargs ls -t 2>/dev/null | head -1)
[ -n "$MAN" ] || { echo "error: generated tabibu.1 not found in OUT_DIR" >&2; exit 1; }

mkdir -p core/crates/tabibu-cli/man
cp "$MAN" core/crates/tabibu-cli/man/tabibu.1
echo "✓ refreshed core/crates/tabibu-cli/man/tabibu.1"
