#!/usr/bin/env bash
# Single source of truth for the app version: the root VERSION file.
# Propagates it into every manifest + the docs version chip so cargo, tauri,
# npm, and the docs site all stamp the same version. Run after bumping VERSION
# (and CI runs it before every build, so compilation always picks it up).
#
#   ./scripts/sync-version.sh            # write VERSION into all targets
#   ./scripts/sync-version.sh --check    # fail (non-zero) if anything is stale
set -euo pipefail
cd "$(dirname "$0")/.."

VER="$(tr -d '[:space:]' < VERSION)"
[ -n "$VER" ] || { echo "VERSION is empty"; exit 1; }

# Cargo: replace ONLY the first column-0 `version = "x"` (the package version,
# which always precedes any `[dependencies.*]` sub-table that could also carry a
# column-0 version line). Spacing around `=` is tolerated. One perl invocation
# PER FILE so the `$d` first-match guard resets between files.
bump_cargo() {
  VER="$VER" perl -i -pe \
    'if (!$d && s/^version\s*=\s*"\d[^"]*"/version = "$ENV{VER}"/) { $d = 1 }' "$1"
}
write() {
  bump_cargo core/Cargo.toml
  bump_cargo app/src-tauri/Cargo.toml
  # JSON: the single top-level "version" string in each file.
  VER="$VER" perl -i -pe 's/"version":\s*"\d[^"]*"/"version": "$ENV{VER}"/' \
    app/src-tauri/tauri.conf.json app/package.json
  # Docs landing-page version chip: <span id="appver">x.y.z</span>
  VER="$VER" perl -i -pe 's/(<span id="appver">)[^<]*/${1}$ENV{VER}/' docs/index.html
}

# Check every file `write` touches — not a subset — so drift can't slip past CI.
check() {
  local ok=1
  grep -Eq "^version *= *\"$VER\"" core/Cargo.toml            || ok=0
  grep -Eq "^version *= *\"$VER\"" app/src-tauri/Cargo.toml   || ok=0
  grep -q  "\"version\": \"$VER\"" app/src-tauri/tauri.conf.json || ok=0
  grep -q  "\"version\": \"$VER\"" app/package.json           || ok=0
  grep -q  "id=\"appver\">$VER<"   docs/index.html            || ok=0
  [ "$ok" = 1 ]
}

if [ "${1:-}" = "--check" ]; then
  if check; then echo "version in sync: $VER"; exit 0; fi
  echo "OUT OF SYNC — run ./scripts/sync-version.sh (want $VER)"; exit 1
fi

write
echo "synced all manifests + docs to v$VER"
