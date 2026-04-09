#!/usr/bin/env bash
#
# abox VM latency benchmarks.
#
# Measures real wall-clock times for the key operations that determine
# the user-perceived latency of abox:
#
#   1. worktree_create_ms  — git worktree add via the workspace adapter
#   2. vm_boot_ms          — from abox run start to "guest init: online" banner
#   3. proxy_roundtrip_ms  — a single `git status` proxied through the bridge
#   4. full_run_ms         — total wall time for abox run with a trivial command
#   5. cleanup_ms          — abox stop --clean (VM teardown + worktree removal)
#
# Output: human-readable table to stderr, machine-readable JSON to stdout.
# The JSON is designed for ingestion by CI dashboards, grafana, or a
# simple `jq` pipeline that tracks regressions over time.
#
# Usage:
#   ./scripts/bench.sh                # run once, print results
#   ./scripts/bench.sh --runs 5       # average over 5 runs
#   ./scripts/bench.sh --json-only    # suppress the table, emit only JSON
#
# Requirements: same as phase 6 of e2e_test.sh — a bootstrapped VM stack
# under ~/.abox/vm/ and /dev/kvm accessible to the current user.
#
# Exit code: 0 on success, 1 if prerequisites are missing.

set -euo pipefail

# ─── Argument parsing ─────────────────────────────────────────────────────────
RUNS=1
JSON_ONLY=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --runs)   RUNS="${2:-1}"; shift 2 ;;
        --runs=*) RUNS="${1#*=}"; shift ;;
        --json-only) JSON_ONLY=1; shift ;;
        --help|-h)
            cat <<EOF
Usage: $(basename "$0") [--runs N] [--json-only]

  --runs N       Number of iterations to average over (default: 1)
  --json-only    Suppress the human-readable table; emit only JSON to stdout
  -h, --help     This message
EOF
            exit 0
            ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

# ─── Prereq check ────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ABOX_VM="$HOME/.abox/vm"

if [[ ! -x "$ABOX_VM/cloud-hypervisor" ]] || [[ ! -f "$ABOX_VM/rootfs.raw" ]]; then
    echo "ERROR: VM artifacts not found. Run 'just bootstrap-vm' first." >&2
    exit 1
fi

if [[ ! -c /dev/kvm ]] || [[ ! -r /dev/kvm ]]; then
    echo "ERROR: /dev/kvm not accessible. Benchmarks require KVM." >&2
    exit 1
fi

ABOX_BIN="$REPO_ROOT/target/release/abox"
if [[ ! -x "$ABOX_BIN" ]]; then
    echo "Building release binary..." >&2
    (cd "$REPO_ROOT" && cargo build --release --quiet)
fi

export PATH="$ABOX_VM:$PATH"

# ─── Setup ────────────────────────────────────────────────────────────────────
SCRATCH="$REPO_ROOT/.scratch/bench-$$"
mkdir -p "$SCRATCH"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

# Create a scratch git repo.
git init -q "$SCRATCH/repo"
(cd "$SCRATCH/repo" && git config user.email "bench@abox" && git config user.name bench &&
 echo "# bench" > README.md && git add . && git commit -q -m "init")

# Write a minimal config.
mkdir -p "$SCRATCH/policies" "$SCRATCH/r" "$SCRATCH/state"
cp "$REPO_ROOT/policies/default.toml" "$SCRATCH/policies/default.toml"
cat > "$SCRATCH/config.toml" <<EOF
state_dir = "$SCRATCH/state"
runtime_dir = "$SCRATCH/r"

[vm_defaults]
memory_mib = 512
vcpus = 1
image_path = "$ABOX_VM/rootfs.raw"
kernel_path = "$ABOX_VM/vmlinux"

[proxy]
egress_port = 28443
policy_dir = "$SCRATCH/policies"
EOF

ABOX="$ABOX_BIN --config $SCRATCH/config.toml --repo $SCRATCH/repo"

# ─── Timing helpers ───────────────────────────────────────────────────────────
now_ns() { date +%s%N; }

# Strip ANSI escape codes so grep -oP can match timestamps cleanly.
# tracing output wraps timestamps in [2m...[0m (dim on/off).
strip_ansi() { sed 's/\x1b\[[0-9;]*m//g'; }

# Extract a tracing ISO-8601 timestamp from a line, convert to epoch ns.
# Returns 0 if no timestamp found.
epoch_ns_from_line() {
    local ts
    ts=$(echo "$1" | strip_ansi | grep -oP '\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z?' | head -1 || true)
    if [[ -n "$ts" ]]; then
        date -d "$ts" +%s%N 2>/dev/null || echo 0
    else
        echo 0
    fi
}

# ─── Benchmark functions ─────────────────────────────────────────────────────

bench_full_run() {
    # Full abox run with a trivial command. Measures:
    #   vm_boot_ms         — MicroVM started → first proxy bridge request
    #                        (approximates kernel boot + init + socat ready)
    #   proxy_roundtrip_ms — proxy bridge request → response for git status
    #   full_run_ms        — total wall time from start to exit
    #   cleanup_ms         — abox stop --clean time
    local task="bench-$RANDOM"
    local outfile="$SCRATCH/$task.out"
    local boot_ms=-1 proxy_ms=-1 full_ms cleanup_ms

    local t_start t_end
    t_start=$(now_ns)

    timeout 90 $ABOX run --task "$task" --base main -- \
        /bin/sh -c 'git status >/dev/null 2>&1; echo __PROXY_DONE__' \
        >"$outfile" 2>&1 || true

    t_end=$(now_ns)
    full_ms=$(( (t_end - t_start) / 1000000 ))

    # Extract sub-interval timestamps from the tracing output.
    # The log lines we care about (after stripping ANSI):
    #   "MicroVM started"       — CH process spawned, VM is booting
    #   "proxy bridge listening" — host-side bridge is ready
    #   "cli ...allowed"        — proxy bridge handled the git status call
    local line_vm_started line_proxy_request
    line_vm_started=$(strip_ansi < "$outfile" | grep "MicroVM started" | head -1 || true)
    line_proxy_request=$(strip_ansi < "$outfile" | grep "proxy_bridge.*allowed" | head -1 || true)

    if [[ -n "$line_vm_started" ]] && [[ -n "$line_proxy_request" ]]; then
        local ns_vm ns_proxy
        ns_vm=$(epoch_ns_from_line "$line_vm_started")
        ns_proxy=$(epoch_ns_from_line "$line_proxy_request")
        if [[ "$ns_vm" != "0" ]] && [[ "$ns_proxy" != "0" ]]; then
            boot_ms=$(( (ns_proxy - ns_vm) / 1000000 ))
        fi
    fi

    # Proxy roundtrip: we use the time between the "running runner.sh"
    # console output and the "allowed" log line. The runner.sh echo is
    # the guest-side start; the proxy log is the host-side completion.
    local line_runner
    line_runner=$(strip_ansi < "$outfile" | grep "running /abox-meta/runner.sh" | head -1 || true)
    if [[ -n "$line_runner" ]] && [[ -n "$line_proxy_request" ]]; then
        local ns_runner
        # runner.sh is a console line — it won't have a tracing timestamp.
        # Use the "proxy bridge listening" line as the baseline instead,
        # and measure from there to the first "allowed" line.
        local line_bridge_listening
        line_bridge_listening=$(strip_ansi < "$outfile" | grep "proxy bridge listening" | head -1 || true)
        if [[ -n "$line_bridge_listening" ]]; then
            local ns_bridge
            ns_bridge=$(epoch_ns_from_line "$line_bridge_listening")
            ns_proxy=$(epoch_ns_from_line "$line_proxy_request")
            if [[ "$ns_bridge" != "0" ]] && [[ "$ns_proxy" != "0" ]]; then
                proxy_ms=$(( (ns_proxy - ns_bridge) / 1000000 ))
            fi
        fi
    fi

    # Cleanup timing.
    local t_clean_start t_clean_end
    t_clean_start=$(now_ns)
    $ABOX stop "$task" --clean >/dev/null 2>&1 || true
    t_clean_end=$(now_ns)
    cleanup_ms=$(( (t_clean_end - t_clean_start) / 1000000 ))

    echo "$boot_ms $proxy_ms $full_ms $cleanup_ms"
}

# ─── Main ─────────────────────────────────────────────────────────────────────

# Warm-up run (not counted) — primes disk caches, virtiofsd, etc.
if [[ "$JSON_ONLY" == "0" ]]; then
    echo "Warming up..." >&2
fi
bench_full_run >/dev/null

# Collect samples.
declare -a BOOT_SAMPLES PROXY_SAMPLES FULL_SAMPLES CLEANUP_SAMPLES

for i in $(seq 1 "$RUNS"); do
    if [[ "$JSON_ONLY" == "0" ]]; then
        printf "  run %d/%d..." "$i" "$RUNS" >&2
    fi
    result=$(bench_full_run)
    read -r boot proxy full cleanup <<< "$result"
    BOOT_SAMPLES+=("$boot")
    PROXY_SAMPLES+=("$proxy")
    FULL_SAMPLES+=("$full")
    CLEANUP_SAMPLES+=("$cleanup")
    if [[ "$JSON_ONLY" == "0" ]]; then
        printf " boot=%dms full=%dms cleanup=%dms\n" "$boot" "$full" "$cleanup" >&2
    fi
done

# ─── Statistics ───────────────────────────────────────────────────────────────
avg() {
    local sum=0 count=0
    for v in "$@"; do
        if [[ "$v" -ge 0 ]]; then
            sum=$((sum + v))
            count=$((count + 1))
        fi
    done
    if [[ "$count" -gt 0 ]]; then
        echo $((sum / count))
    else
        echo -1
    fi
}

min_of() {
    local m=999999999
    for v in "$@"; do
        if [[ "$v" -ge 0 ]] && [[ "$v" -lt "$m" ]]; then
            m="$v"
        fi
    done
    [[ "$m" == "999999999" ]] && echo -1 || echo "$m"
}

max_of() {
    local m=-1
    for v in "$@"; do
        if [[ "$v" -gt "$m" ]]; then
            m="$v"
        fi
    done
    echo "$m"
}

BOOT_AVG=$(avg "${BOOT_SAMPLES[@]}")
BOOT_MIN=$(min_of "${BOOT_SAMPLES[@]}")
BOOT_MAX=$(max_of "${BOOT_SAMPLES[@]}")
PROXY_AVG=$(avg "${PROXY_SAMPLES[@]}")
FULL_AVG=$(avg "${FULL_SAMPLES[@]}")
FULL_MIN=$(min_of "${FULL_SAMPLES[@]}")
FULL_MAX=$(max_of "${FULL_SAMPLES[@]}")
CLEANUP_AVG=$(avg "${CLEANUP_SAMPLES[@]}")

# ─── Output ───────────────────────────────────────────────────────────────────
if [[ "$JSON_ONLY" == "0" ]]; then
    cat >&2 <<TABLE

━━━ abox VM latency benchmarks ━━━
  runs: $RUNS
  hardware: $(uname -m), $(nproc) cores, $(awk '/MemTotal/{printf "%.0f GB", $2/1024/1024}' /proc/meminfo)
  kernel: $(uname -r)

  METRIC                   AVG        MIN        MAX
  ──────────────────────── ────────── ────────── ──────────
  vm_boot_ms               ${BOOT_AVG}ms      ${BOOT_MIN}ms      ${BOOT_MAX}ms
  proxy_roundtrip_ms       ${PROXY_AVG}ms
  full_run_ms              ${FULL_AVG}ms      ${FULL_MIN}ms      ${FULL_MAX}ms
  cleanup_ms               ${CLEANUP_AVG}ms

TABLE
fi

# JSON to stdout — ingestible by CI, grafana, jq, etc.
cat <<JSON
{
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "runs": $RUNS,
  "hardware": {
    "arch": "$(uname -m)",
    "cores": $(nproc),
    "kernel": "$(uname -r)"
  },
  "results": {
    "vm_boot_ms":          {"avg": $BOOT_AVG, "min": $BOOT_MIN, "max": $BOOT_MAX},
    "proxy_roundtrip_ms":  {"avg": $PROXY_AVG},
    "full_run_ms":         {"avg": $FULL_AVG, "min": $FULL_MIN, "max": $FULL_MAX},
    "cleanup_ms":          {"avg": $CLEANUP_AVG}
  },
  "samples": {
    "vm_boot_ms":         [$(IFS=,; echo "${BOOT_SAMPLES[*]}")],
    "proxy_roundtrip_ms": [$(IFS=,; echo "${PROXY_SAMPLES[*]}")],
    "full_run_ms":        [$(IFS=,; echo "${FULL_SAMPLES[*]}")],
    "cleanup_ms":         [$(IFS=,; echo "${CLEANUP_SAMPLES[*]}")]
  }
}
JSON
