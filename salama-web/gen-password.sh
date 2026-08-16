#!/usr/bin/env bash
# Generate the bcrypt PASSWORD_HASH for the Salama Web admin UI without leaking
# the password into your shell history. Prints the raw, single-$ hash line you
# paste into Coolify's env UI (or into .env).
#
#   ./gen-password.sh          # prompts silently, prints PASSWORD_HASH=...
#   ./gen-password.sh --env    # also appends it to ./.env
set -euo pipefail
cd "$(dirname "$0")"

read -r -s -p "Admin password: " pw; echo
read -r -s -p "Confirm:        " pw2; echo
[ "$pw" = "$pw2" ] || { echo "passwords do not match" >&2; exit 1; }
[ -n "$pw" ] || { echo "empty password" >&2; exit 1; }

# wgpw prints  PASSWORD_HASH='<hash>'  — unwrap to the raw hash.
hash=$(docker run --rm ghcr.io/wg-easy/wg-easy:14 wgpw "$pw" | sed -E "s/^PASSWORD_HASH='(.*)'$/\1/")

if [ "${1:-}" = "--env" ]; then
  # Replace any existing PASSWORD_HASH line, else append.
  touch .env
  grep -v '^PASSWORD_HASH=' .env > .env.tmp || true
  printf 'PASSWORD_HASH=%s\n' "$hash" >> .env.tmp
  mv .env.tmp .env
  echo "wrote PASSWORD_HASH to ./.env"
else
  printf 'PASSWORD_HASH=%s\n' "$hash"
fi
