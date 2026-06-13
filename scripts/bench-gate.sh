#!/bin/zsh
# Performance regression gate for the Rust core.
#
# Usage:
#   scripts/bench-gate.sh                  run benches; compare vs core/benches-baseline/
#                                          and exit 1 if any mean regressed > 5%
#   scripts/bench-gate.sh --update-baseline  run benches, then bless the new numbers
#                                            into core/benches-baseline/
#
# How it works:
#   1. Runs the criterion benches in tabibu-walk and tabibu-dupes:
#        cargo bench -p <crate> --bench <bench> -- --save-baseline current
#      (--bench <name> is required: plain `cargo bench -p crate -- --save-baseline`
#      also runs the libtest unit-test harness, which rejects criterion flags.)
#   2. Criterion writes, per benchmark function:
#        core/target/criterion/<benchmark-name>/current/estimates.json
#        core/target/criterion/<benchmark-name>/current/benchmark.json
#      (verified on this repo: e.g.
#       target/criterion/find_duplicates_2k_files_30pct_dupes/current/estimates.json)
#      estimates.json has keys mean/median/median_abs_dev/slope/std_dev; we use
#      .mean.point_estimate (nanoseconds).
#   3. The blessed baseline lives in core/benches-baseline/<benchmark-name>/
#      estimates.json (a copy of a previous "current"). Before benching we also
#      restore it into target/criterion/<name>/main/ so the on-disk layout
#      matches criterion's `--baseline main` naming and its HTML report can
#      reference it; the pass/fail decision is made here by parsing the JSON
#      (cargo-critcmp is not installed), with python3 (preferred) or jq.
#   4. Gate: fail if current mean > baseline mean * 1.05.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORE="$ROOT/core"
CRITERION_DIR="$CORE/target/criterion"
BASELINE_DIR="$CORE/benches-baseline"
THRESHOLD_PCT=5

UPDATE=0
if [[ "${1:-}" == "--update-baseline" ]]; then
  UPDATE=1
elif [[ -n "${1:-}" ]]; then
  echo "usage: $0 [--update-baseline]" >&2
  exit 2
fi

# crate:bench-target pairs to run.
BENCHES=(
  "tabibu-walk:walk"
  "tabibu-dupes:dupes"
)

# Pick a JSON parser: python3 preferred, jq as fallback.
if command -v python3 > /dev/null; then
  PARSER=python3
elif command -v jq > /dev/null; then
  PARSER=jq
else
  echo "error: neither python3 nor jq found; cannot parse criterion JSON." >&2
  exit 1
fi

mean_of() {
  # mean_of <estimates.json> -> mean point estimate in ns
  local f="$1"
  if [[ "$PARSER" == "python3" ]]; then
    python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['mean']['point_estimate'])" "$f"
  else
    jq -r '.mean.point_estimate' "$f"
  fi
}

# Restore blessed baselines into criterion's `main` baseline slot so the
# layout matches criterion's --baseline main convention.
if [[ -d "$BASELINE_DIR" ]]; then
  for d in "$BASELINE_DIR"/*(N/); do
    name="$(basename "$d")"
    mkdir -p "$CRITERION_DIR/$name/main"
    cp "$d"/*.json "$CRITERION_DIR/$name/main/"
  done
fi

# --- run the benches ---------------------------------------------------------
for pair in "${BENCHES[@]}"; do
  crate="${pair%%:*}"
  bench="${pair##*:}"
  echo "==> cargo bench -p $crate --bench $bench -- --save-baseline current"
  (cd "$CORE" && cargo bench -p "$crate" --bench "$bench" -- --save-baseline current)
done

# --- collect results ---------------------------------------------------------
typeset -a CURRENT_DIRS
CURRENT_DIRS=("$CRITERION_DIR"/*/current(N/))
if (( ${#CURRENT_DIRS[@]} == 0 )); then
  echo "error: no */current/ results under $CRITERION_DIR -- did the benches run?" >&2
  exit 1
fi

if (( UPDATE )); then
  echo "==> Blessing current results into $BASELINE_DIR"
  for cur in "${CURRENT_DIRS[@]}"; do
    name="$(basename "$(dirname "$cur")")"
    mkdir -p "$BASELINE_DIR/$name"
    cp "$cur/estimates.json" "$cur/benchmark.json" "$BASELINE_DIR/$name/"
    printf '  %s: mean %.3f ms\n' "$name" "$(( $(mean_of "$cur/estimates.json") / 1e6 ))"
  done
  echo "Baseline updated. Commit core/benches-baseline/ to make it the CI reference."
  exit 0
fi

if [[ ! -d "$BASELINE_DIR" ]]; then
  echo "note: $BASELINE_DIR does not exist -- nothing to compare against."
  echo "      Run '$0 --update-baseline' on a known-good commit to create it."
  echo "Benches ran successfully; gate passes trivially."
  exit 0
fi

# --- gate ---------------------------------------------------------------------
FAILED=0
COMPARED=0
echo ""
echo "Regression gate (threshold: +${THRESHOLD_PCT}% on mean):"
for cur in "${CURRENT_DIRS[@]}"; do
  name="$(basename "$(dirname "$cur")")"
  base="$BASELINE_DIR/$name/estimates.json"
  if [[ ! -f "$base" ]]; then
    echo "  $name: NEW (no baseline) -- recorded, not gated"
    continue
  fi
  cur_mean="$(mean_of "$cur/estimates.json")"
  base_mean="$(mean_of "$base")"
  # zsh does native float arithmetic, so the gate math needs no extra tools.
  delta_pct=$(( (cur_mean / base_mean - 1.0) * 100.0 ))
  COMPARED=$((COMPARED + 1))
  verdict="ok"
  if (( cur_mean > base_mean * (1.0 + THRESHOLD_PCT / 100.0) )); then
    verdict="REGRESSION"
    FAILED=1
  fi
  printf '  %-45s base %10.3f ms  ->  current %10.3f ms  (%+.2f%%)  %s\n' \
    "$name" "$((base_mean / 1e6))" "$((cur_mean / 1e6))" "$delta_pct" "$verdict"
done

if (( FAILED )); then
  echo ""
  echo "FAIL: at least one benchmark regressed more than ${THRESHOLD_PCT}%." >&2
  echo "If the regression is intentional, re-bless with: $0 --update-baseline" >&2
  exit 1
fi
echo ""
echo "PASS: $COMPARED benchmark(s) within ${THRESHOLD_PCT}% of baseline."
