#!/usr/bin/env bash
# Generate the bcrypt PASSWORD_HASH for the Salama Web admin UI without leaking
# the password into your shell history.
#
#   ./gen-password.sh          # prints PASSWORD_HASH=<raw hash>  → paste into Coolify's env UI
#   ./gen-password.sh --env    # writes it to ./.env with '$' DOUBLED (for local `docker compose`)
#
# Why two forms: a bcrypt hash contains '$'. Coolify passes env vars to the
# container verbatim (paste raw). But `docker compose` INTERPOLATES .env values,
# so a hash stored in .env must have every '$' doubled to '$$' or it is silently
# corrupted and login fails. This script emits the right form for each target.
set -euo pipefail
cd "$(dirname "$0")"

read -r -s -p "Admin password: " pw; echo
read -r -s -p "Confirm:        " pw2; echo
[ "$pw" = "$pw2" ] || { echo "passwords do not match" >&2; exit 1; }
[ -n "$pw" ] || { echo "empty password" >&2; exit 1; }

# wgpw prints  PASSWORD_HASH='<hash>'  — unwrap to the raw hash.
hash=$(docker run --rm ghcr.io/wg-easy/wg-easy:14 wgpw "$pw" | sed -E "s/^PASSWORD_HASH='(.*)'$/\1/")

if [ "${1:-}" = "--env" ]; then
  # `docker compose` interpolates .env values, so double every '$' -> '$$' or
  # the hash is corrupted on load. Replace any existing PASSWORD_HASH line.
  doubled=${hash//\$/\$\$}
  touch .env
  grep -v '^PASSWORD_HASH=' .env > .env.tmp || true
  printf 'PASSWORD_HASH=%s\n' "$doubled" >> .env.tmp
  mv .env.tmp .env
  echo "wrote PASSWORD_HASH to ./.env (\$ doubled for local 'docker compose')." >&2
  echo "For Coolify, paste the RAW hash instead — run ./gen-password.sh with no args." >&2
else
  # Raw, single-$ form: paste this straight into Coolify's env UI.
  printf 'PASSWORD_HASH=%s\n' "$hash"
fi
