#!/usr/bin/env bash
#
# abox template startup benchmark — cold boot vs warm (snapshot restore).
#
# Compares fresh VM boot time against snapshot-restore startup to quantify
# the speedup from P3 (Snapshot/Template Fast Startup).
#
# Usage:
#   ./scripts/local/bench_template.sh                 # 5 runs (default)
#   ./scripts/local/bench_template.sh --runs 10       # 10 runs
#   ./scripts/local/bench_template.sh --template base # use a specific template name
#
# Prerequisites:
#   - A working abox installation (see scripts/bootstrap_vm.sh)
#   - /dev/kvm accessible to the current user
#   - A template named "base" (or --template <name>) must exist:
#       abox run --task template-src -- true
#       abox template create --name base --from template-src
#       abox stop template-src --clean
#
# Output: human-readable table with p50/p95/p99 percentiles.
# Target: warm start under 100ms.

set -euo pipefail

RUNS=5
TEMPLATE="base"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --runs)   RUNS="$2"; shift 2 ;;
        --template) TEMPLATE="$2"; shift 2 ;;
        *)        echo "Unknown arg: $1" >&2; exit 1 ;;
    esac
done

ABOX="${ABOX:-cargo run --release --quiet --bin abox --}"

# ─── Helpers ──────────────────────────────────────────────────────────────────

time_ms() {
    local start end
    start=$(date +%s%N)
    "$@" >/dev/null 2>&1
    end=$(date +%s%N)
    echo $(( (end - start) / 1000000 ))
}

percentile() {
    # $1 = percentile (e.g. 50), rest = values
    local p="$1"; shift
    local sorted
    sorted=$(printf '%s\n' "$@" | sort -n)
    local count=$#
    local idx=$(( (p * count + 99) / 100 ))
    [[ $idx -lt 1 ]] && idx=1
    echo "$sorted" | sed -n "${idx}p"
}

# ─── Preflight checks ────────────────────────────────────────────────────────

if ! command -v cloud-hypervisor &>/dev/null; then
    echo "ERROR: cloud-hypervisor not found. Run scripts/bootstrap_vm.sh." >&2
    exit 1
fi

if ! [[ -r /dev/kvm ]]; then
    echo "ERROR: /dev/kvm not accessible." >&2
    exit 1
fi

# ─── Cold start benchmark ────────────────────────────────────────────────────

echo "=== Cold start (fresh boot) — $RUNS runs ==="
cold_times=()
for i in $(seq 1 "$RUNS"); do
    task="bench-cold-$i-$$"
    ms=$(time_ms $ABOX run --task "$task" -- true)
    cold_times+=("$ms")
    # Clean up
    $ABOX stop "$task" --clean >/dev/null 2>&1 || true
    echo "  run $i: ${ms}ms"
done

# ─── Warm start benchmark ────────────────────────────────────────────────────

echo ""
echo "=== Warm start (from template '$TEMPLATE') — $RUNS runs ==="
warm_times=()
for i in $(seq 1 "$RUNS"); do
    task="bench-warm-$i-$$"
    ms=$(time_ms $ABOX run --task "$task" --template "$TEMPLATE" -- true)
    warm_times+=("$ms")
    # Clean up
    $ABOX stop "$task" --clean >/dev/null 2>&1 || true
    echo "  run $i: ${ms}ms"
done

# ─── Results ──────────────────────────────────────────────────────────────────

echo ""
echo "=== Results ==="
printf "%-12s %8s %8s %8s\n" "" "p50" "p95" "p99"
printf "%-12s %8s %8s %8s\n" "----------" "--------" "--------" "--------"

cold_p50=$(percentile 50 "${cold_times[@]}")
cold_p95=$(percentile 95 "${cold_times[@]}")
cold_p99=$(percentile 99 "${cold_times[@]}")
printf "%-12s %7sms %7sms %7sms\n" "Cold" "$cold_p50" "$cold_p95" "$cold_p99"

warm_p50=$(percentile 50 "${warm_times[@]}")
warm_p95=$(percentile 95 "${warm_times[@]}")
warm_p99=$(percentile 99 "${warm_times[@]}")
printf "%-12s %7sms %7sms %7sms\n" "Warm" "$warm_p50" "$warm_p95" "$warm_p99"

echo ""
if [[ "$warm_p50" -lt 100 ]]; then
    echo "TARGET MET: warm p50 (${warm_p50}ms) < 100ms"
else
    echo "TARGET MISSED: warm p50 (${warm_p50}ms) >= 100ms"
fi
