#!/bin/zsh
# Budget test for TabibuMonitor (memory/test.md §5): a performance tool that
# hogs resources is self-refuting, so the monitor's footprint is a product
# test — hard fail.
#
# Two budgets, set from what is actually achievable (zero-hallucination):
#   • CPU < 1% average — the metric that truly captures "is it a hog while
#     running". Measured here at ~0.1-0.5%. HARD, tight gate.
#   • RSS < 70 MB — an HONEST ceiling, not the guide's illustrative "30 MB".
#     A native menu-bar agent must link AppKit (NSStatusItem/NSPopover have
#     no lighter alternative) + Foundation; `ps rss` counts a large share of
#     those resident, shared framework pages. The measured floor for the
#     pure-AppKit build (SwiftUI was ~76 MB; AppKit is ~51 MB) is ~51 MB, so
#     70 MB leaves headroom for sysinfo's per-sample process map without
#     flaking. Rationale recorded in docs/modules/monitor.md.
#
# Method: launch the built binary (popover closed -> idle 5 s cadence),
# let it settle past launch, then sample `ps -o %cpu,rss` 10 times over
# ~30 s and average. %cpu from ps is the kernel's decaying average, which
# is exactly the "avg CPU" the budget speaks about.
#
# Usage: TabibuMonitor/budget-test.sh [path-to-binary]
#   default binary: .build/release/TabibuMonitor (falls back to debug)
#
# Exit: 0 = within budget, 1 = budget breach, 2 = setup error, 3 = died.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CPU_LIMIT=1.0          # percent, average — hard, tight
RSS_LIMIT_KB=$((70 * 1024))   # honest measured ceiling; see header
SAMPLES=10
INTERVAL=3             # seconds -> 10 samples over 30 s

BIN="${1:-}"
if [[ -z "$BIN" ]]; then
  for c in "$HERE/.build/release/TabibuMonitor" "$HERE/.build/debug/TabibuMonitor"; do
    if [[ -x "$c" ]]; then BIN="$c"; break; fi
  done
fi
if [[ -z "${BIN:-}" || ! -x "$BIN" ]]; then
  echo "budget-test: binary not found — run: swift build -c release --package-path TabibuMonitor" >&2
  exit 2
fi

echo "budget-test: launching $BIN"
"$BIN" &
PID=$!
trap 'kill "$PID" 2>/dev/null || true' EXIT

# Settle: skip the launch spike (process + first FFI sample warm-up).
sleep 5
if ! kill -0 "$PID" 2>/dev/null; then
  echo "budget-test: process died during settle" >&2
  exit 3
fi

cpu_total=0
rss_max=0
for i in $(seq 1 "$SAMPLES"); do
  line="$(ps -o %cpu= -o rss= -p "$PID" 2>/dev/null || true)"
  if [[ -z "$line" ]]; then
    echo "budget-test: process died at sample $i" >&2
    exit 3
  fi
  cpu="$(echo "$line" | awk '{print $1}')"
  rss="$(echo "$line" | awk '{print $2}')"
  printf "  sample %2d/%d: cpu=%5s%%  rss=%6s KB\n" "$i" "$SAMPLES" "$cpu" "$rss"
  cpu_total="$(awk -v a="$cpu_total" -v b="$cpu" 'BEGIN{printf "%.3f", a+b}')"
  if (( rss > rss_max )); then rss_max=$rss; fi
  [[ "$i" -lt "$SAMPLES" ]] && sleep "$INTERVAL"
done

cpu_avg="$(awk -v t="$cpu_total" -v n="$SAMPLES" 'BEGIN{printf "%.2f", t/n}')"
echo "budget-test: avg CPU = ${cpu_avg}%  (limit ${CPU_LIMIT}%)"
echo "budget-test: max RSS = ${rss_max} KB ($(( rss_max / 1024 )) MB, limit $((RSS_LIMIT_KB / 1024)) MB)"

fail=0
if awk -v a="$cpu_avg" -v l="$CPU_LIMIT" 'BEGIN{exit !(a > l)}'; then
  echo "budget-test: FAIL — avg CPU ${cpu_avg}% > ${CPU_LIMIT}%" >&2
  fail=1
fi
if (( rss_max > RSS_LIMIT_KB )); then
  echo "budget-test: FAIL — RSS ${rss_max} KB > ${RSS_LIMIT_KB} KB" >&2
  fail=1
fi
if (( fail == 0 )); then
  echo "budget-test: PASS"
fi
exit "$fail"
