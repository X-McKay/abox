#!/usr/bin/env bash
#
# abox pre-release validation orchestrator.
#
# Detects host capabilities, runs test tiers in order, compares benchmarks
# against the previous release baseline, and writes attestation stamps for
# passing tiers.
#
# Usage:
#   ./scripts/pre_release.sh          # run all tiers the host can support
#   just pre-release                   # same, via justfile recipe
#
# Tiers:
#   1. ci    — fmt + clippy + test + supply-chain audit (always runs)
#   2. vm    — VM end-to-end tests (requires KVM + bootstrapped VM)
#   3. bench — criterion + VM latency benchmarks (requires tier-vm pass)
#   4. smoke — agent smoke tests with real API calls (requires credentials)
#
# The script continues across tiers even if one fails, so you always get
# a complete picture of what works.

set -uo pipefail

# ─── ANSI color helpers ─────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
    YELLOW=$'\033[33m'; BLUE=$'\033[34m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; CYAN=""; RESET=""
fi

section() { echo; echo "${BOLD}${BLUE}=== $1 ===${RESET}"; }
info()    { echo "  ${DIM}$1${RESET}"; }
ok()      { echo "  ${GREEN}ok${RESET} $1"; }
warn()    { echo "  ${YELLOW}!!${RESET} $1"; }
err()     { echo "  ${RED}FAIL${RESET} $1"; }
skip()    { echo "  ${DIM}--${RESET} $1 ${DIM}($2)${RESET}"; }

# ─── Setup ──────────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ATTESTATION_DIR="$REPO_ROOT/.abox-attestations"
ABOX_VM="$HOME/.abox/vm"

echo "${BOLD}abox pre-release validation${RESET}"
echo "  repo: $REPO_ROOT"
echo "  date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "  sha:  $(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"

# ─── Tier result tracking ───────────────────────────────────────────────────
TIER_CI="skip";    TIER_CI_REASON=""
TIER_VM="skip";    TIER_VM_REASON=""
TIER_BENCH="skip"; TIER_BENCH_REASON=""
TIER_SMOKE="skip"; TIER_SMOKE_REASON=""
BENCH_TMPFILE=""

# ─── Capability detection ───────────────────────────────────────────────────
section "Capability detection"

# KVM
HAS_KVM=false
if [[ -c /dev/kvm ]] && [[ -r /dev/kvm ]]; then
    HAS_KVM=true
    ok "/dev/kvm exists and is readable"
else
    warn "/dev/kvm not available"
fi

# VM bootstrap
HAS_VM_BOOTSTRAP=false
if [[ -x "$ABOX_VM/cloud-hypervisor" ]] && [[ -f "$ABOX_VM/rootfs.raw" ]]; then
    HAS_VM_BOOTSTRAP=true
    ok "VM bootstrap present ($ABOX_VM)"
else
    warn "VM bootstrap missing (run 'just bootstrap-vm')"
fi

# Rootfs freshness (only check if VM bootstrap is present)
ROOTFS_FRESH=false
if $HAS_VM_BOOTSTRAP; then
    if just check-rootfs >/dev/null 2>&1; then
        ROOTFS_FRESH=true
        ok "rootfs is up to date"
    else
        warn "rootfs is stale — rebuild with 'just rebuild-rootfs'"
    fi
else
    skip "rootfs freshness" "no VM bootstrap"
fi

# Claude credentials
HAS_CLAUDE=false
if [[ -f "$HOME/.claude/.credentials.json" ]]; then
    HAS_CLAUDE=true
    ok "Claude credentials found"
else
    info "Claude credentials not found (~/.claude/.credentials.json)"
fi

# Codex credentials
HAS_CODEX=false
if [[ -f "$HOME/.codex/auth.json" ]]; then
    HAS_CODEX=true
    ok "Codex credentials found"
else
    info "Codex credentials not found (~/.codex/auth.json)"
fi

# Git cleanliness
GIT_CLEAN=false
if [[ -z "$(git status --porcelain 2>/dev/null)" ]]; then
    GIT_CLEAN=true
    ok "git working tree is clean"
else
    warn "git working tree is dirty (attestation stamps will be skipped)"
fi

# ─── Plan ───────────────────────────────────────────────────────────────────
section "Plan"

# Tier 1: always runs
CAN_CI=true
info "tier-ci:    WILL RUN (always)"

# Tier 2: requires KVM + VM bootstrap + fresh rootfs
CAN_VM=false
if $HAS_KVM && $HAS_VM_BOOTSTRAP && $ROOTFS_FRESH; then
    CAN_VM=true
    info "tier-vm:    WILL RUN"
else
    if ! $HAS_KVM; then
        TIER_VM_REASON="no KVM"
    elif ! $HAS_VM_BOOTSTRAP; then
        TIER_VM_REASON="VM not bootstrapped"
    else
        TIER_VM_REASON="rootfs stale"
    fi
    info "tier-vm:    SKIP ($TIER_VM_REASON)"
fi

# Tier 3: requires same as tier-vm; also requires tier-vm pass at execution time
CAN_BENCH=$CAN_VM
if $CAN_BENCH; then
    info "tier-bench: WILL RUN (if tier-vm passes)"
else
    TIER_BENCH_REASON="$TIER_VM_REASON"
    info "tier-bench: SKIP ($TIER_BENCH_REASON)"
fi

# Tier 4: requires KVM + VM bootstrap + at least one credential set
CAN_SMOKE=false
SMOKE_FILTER="all"
if $HAS_KVM && $HAS_VM_BOOTSTRAP && $ROOTFS_FRESH; then
    if $HAS_CLAUDE && $HAS_CODEX; then
        CAN_SMOKE=true
        SMOKE_FILTER="all"
        info "tier-smoke: WILL RUN (claude + codex)"
    elif $HAS_CLAUDE; then
        CAN_SMOKE=true
        SMOKE_FILTER="claude"
        info "tier-smoke: WILL RUN (claude only)"
    elif $HAS_CODEX; then
        CAN_SMOKE=true
        SMOKE_FILTER="codex"
        info "tier-smoke: WILL RUN (codex only)"
    else
        TIER_SMOKE_REASON="no credentials"
        info "tier-smoke: SKIP ($TIER_SMOKE_REASON)"
    fi
else
    if ! $HAS_KVM; then
        TIER_SMOKE_REASON="no KVM"
    elif ! $HAS_VM_BOOTSTRAP; then
        TIER_SMOKE_REASON="VM not bootstrapped"
    elif ! $ROOTFS_FRESH; then
        TIER_SMOKE_REASON="rootfs stale — run 'just rebuild-rootfs'"
    fi
    info "tier-smoke: SKIP ($TIER_SMOKE_REASON)"
fi

# ─── Tier execution ─────────────────────────────────────────────────────────

# --- Tier 1: CI ---
section "Tier 1: CI (fmt + clippy + test + deny)"
if $CAN_CI; then
    if just tier-ci; then
        TIER_CI="pass"
        ok "tier-ci passed"
    else
        TIER_CI="fail"
        TIER_CI_REASON="tier-ci failed"
        err "tier-ci failed"
    fi
fi

# --- Tier 2: VM ---
section "Tier 2: VM end-to-end"
if $CAN_VM; then
    if just tier-vm; then
        TIER_VM="pass"
        ok "tier-vm passed"
    else
        TIER_VM="fail"
        TIER_VM_REASON="tier-vm failed"
        err "tier-vm failed"
    fi
else
    skip "tier-vm" "$TIER_VM_REASON"
fi

# --- Tier 3: Benchmarks ---
section "Tier 3: Benchmarks"
BENCH_CURRENT_JSON=""
BENCH_TMPFILE=""
if $CAN_BENCH; then
    if [[ "$TIER_VM" == "pass" ]]; then
        BENCH_FAILED=false

        # Criterion microbenchmarks
        info "running criterion microbenchmarks..."
        if cargo bench -p abox-core >/dev/null 2>&1; then
            ok "criterion benchmarks completed"
        else
            warn "criterion benchmarks failed (non-fatal)"
        fi

        # VM latency benchmarks
        info "running VM latency benchmarks (5 runs)..."
        BENCH_TMPFILE=$(mktemp)
        if ./scripts/local/bench.sh --runs 5 --json-only >"$BENCH_TMPFILE" 2>/dev/null; then
            BENCH_CURRENT_JSON=$(cat "$BENCH_TMPFILE")
            ok "VM latency benchmarks completed"
        else
            BENCH_FAILED=true
            err "VM latency benchmarks failed"
        fi

        if $BENCH_FAILED; then
            TIER_BENCH="fail"
            TIER_BENCH_REASON="benchmark execution failed"
        else
            TIER_BENCH="pass"
        fi
    else
        TIER_BENCH="skip"
        TIER_BENCH_REASON="tier-vm failed"
        skip "tier-bench" "$TIER_BENCH_REASON"
    fi
else
    skip "tier-bench" "$TIER_BENCH_REASON"
fi

# --- Tier 4: Smoke ---
section "Tier 4: Agent smoke tests"
if $CAN_SMOKE; then
    if ./scripts/local/agent_smoke_test.sh "$SMOKE_FILTER"; then
        TIER_SMOKE="pass"
        ok "tier-smoke passed ($SMOKE_FILTER)"
    else
        TIER_SMOKE="fail"
        TIER_SMOKE_REASON="smoke tests failed"
        err "tier-smoke failed"
    fi
else
    skip "tier-smoke" "$TIER_SMOKE_REASON"
fi

# ─── Benchmark comparison ───────────────────────────────────────────────────
REGRESSION_DETECTED=false
HARDWARE_MATCH=false
BASELINE_VERSION=""
COMPARISON_JSON=""

if [[ "$TIER_BENCH" == "pass" ]] && [[ -n "$BENCH_CURRENT_JSON" ]]; then
    section "Benchmark comparison"

    # Find the latest baseline
    LATEST_BASELINE=$(find benchmarks -name 'v*.json' -type f 2>/dev/null | sort -V | tail -1)

    if [[ -n "$LATEST_BASELINE" ]]; then
        BASELINE_VERSION=$(basename "$LATEST_BASELINE" .json)
        info "comparing against baseline: $LATEST_BASELINE"

        # Use python3 for the comparison, passing data via files to avoid
        # shell quoting issues with multi-line JSON
        COMPARISON_RESULT=$(python3 - "$LATEST_BASELINE" "$BENCH_TMPFILE" <<'PYEOF'
import json, sys

baseline_path = sys.argv[1]
current_path = sys.argv[2]

with open(baseline_path) as f:
    baseline = json.load(f)
with open(current_path) as f:
    current = json.load(f)

# Hardware match check
b_hw = baseline.get("hardware", {})
c_hw = current.get("hardware", {})
hw_match = (b_hw.get("arch") == c_hw.get("arch") and
            b_hw.get("cores") == c_hw.get("cores"))

metrics = ["vm_boot_ms", "proxy_roundtrip_ms", "full_run_ms", "cleanup_ms"]
comparisons = {}
regression = False

print(f"{'METRIC':<25} {'BASELINE':>10} {'CURRENT':>10} {'DELTA':>10} {'STATUS'}")
print(f"{'─' * 25} {'─' * 10} {'─' * 10} {'─' * 10} {'─' * 10}")

for m in metrics:
    b_val = baseline.get("results", {}).get(m, {}).get("avg", 0)
    c_val = current.get("results", {}).get(m, {}).get("avg", 0)
    if b_val > 0:
        delta_pct = ((c_val - b_val) / b_val) * 100
    else:
        delta_pct = 0.0

    status = "ok"
    if delta_pct > 15:
        if hw_match:
            status = "REGRESSION"
            regression = True
        else:
            status = "advisory"

    comparisons[m] = {
        "baseline": b_val,
        "current": c_val,
        "delta_pct": round(delta_pct, 1)
    }

    print(f"  {m:<23} {b_val:>8}ms {c_val:>8}ms {delta_pct:>+9.1f}%  {status}")

# Output structured result as last line (JSON)
result = {
    "hardware_match": hw_match,
    "regression_detected": regression,
    "comparisons": comparisons,
    "baseline_version": sys.argv[1].split("/")[-1].replace(".json", "")
}
# Separator line so we can split human output from JSON
print("---JSON---")
print(json.dumps(result))
PYEOF
)
        # Print the human-readable part (everything before ---JSON---)
        echo "$COMPARISON_RESULT" | sed '/^---JSON---$/,$d'

        # Extract the JSON part
        COMPARISON_LINE=$(echo "$COMPARISON_RESULT" | sed -n '/^---JSON---$/,$ p' | tail -1)
        COMPARISON_JSON="$COMPARISON_LINE"

        # Parse flags from the comparison JSON
        REGRESSION_DETECTED=$(echo "$COMPARISON_JSON" | python3 -c "import json,sys; print(json.load(sys.stdin)['regression_detected'])" 2>/dev/null)
        HARDWARE_MATCH=$(echo "$COMPARISON_JSON" | python3 -c "import json,sys; print(json.load(sys.stdin)['hardware_match'])" 2>/dev/null)

        if [[ "$REGRESSION_DETECTED" == "True" ]]; then
            err "performance regression detected on matching hardware"
            TIER_BENCH="fail"
            TIER_BENCH_REASON="regression detected"
        elif [[ "$HARDWARE_MATCH" == "False" ]]; then
            warn "hardware differs from baseline — deltas are advisory only"
        else
            ok "no regressions detected"
        fi
    else
        info "no baseline found in benchmarks/ — skipping comparison"
    fi
fi

# ─── Attestation stamp writing ──────────────────────────────────────────────
section "Attestation stamps"
STAMPS_WRITTEN=0

if ! $GIT_CLEAN; then
    warn "skipping attestation stamps — git tree is dirty"
else
    mkdir -p "$ATTESTATION_DIR"
    GIT_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
    TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    HW_ARCH=$(uname -m)
    HW_CORES=$(nproc)
    HW_KERNEL=$(uname -r)

    # write_stamp <tier> <summary> [extra_json_file]
    # Writes a JSON attestation stamp. If extra_json_file is provided and
    # non-empty, its contents are merged into the stamp object.
    write_stamp() {
        local tier="$1"
        local summary="$2"
        local extra_file="${3:-}"

        python3 - "$ATTESTATION_DIR/${tier}.json" "$extra_file" <<PYEOF
import json, sys

stamp = {
    "tier": "$tier",
    "timestamp": "$TIMESTAMP",
    "git_sha": "$GIT_SHA",
    "git_dirty": False,
    "result": "pass",
    "summary": "$summary",
    "hardware": {
        "arch": "$HW_ARCH",
        "cores": $HW_CORES,
        "kernel": "$HW_KERNEL"
    }
}

extra_file = sys.argv[2]
if extra_file:
    with open(extra_file) as f:
        stamp.update(json.load(f))

out_path = sys.argv[1]
with open(out_path, "w") as f:
    json.dump(stamp, f, indent=2)
    f.write("\n")
print(out_path)
PYEOF
    }

    # VM stamp
    if [[ "$TIER_VM" == "pass" ]]; then
        STAMP_PATH=$(write_stamp "vm" "VM end-to-end tests passed")
        ok "wrote $STAMP_PATH"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi

    # Bench stamp (includes comparison data and raw bench results)
    if [[ "$TIER_BENCH" == "pass" ]]; then
        # Build the extra JSON via python3 using temp files for all data
        # to avoid shell quoting issues with multi-line JSON
        BENCH_EXTRA_TMP=$(mktemp)
        COMPARISON_TMP=$(mktemp)
        echo "$COMPARISON_JSON" > "$COMPARISON_TMP"

        python3 - "$BENCH_EXTRA_TMP" "$COMPARISON_TMP" "$BENCH_TMPFILE" "$BASELINE_VERSION" <<'PYEOF'
import json, sys

extra_out = sys.argv[1]
comparison_path = sys.argv[2]
bench_path = sys.argv[3]
baseline_version = sys.argv[4]

extra = {}
extra["baseline_version"] = baseline_version

# Load comparison data
try:
    with open(comparison_path) as f:
        content = f.read().strip()
    if content:
        cdata = json.loads(content)
        extra["comparison"] = cdata.get("comparisons", {})
        extra["regression_detected"] = cdata.get("regression_detected", False)
        extra["hardware_match"] = cdata.get("hardware_match", True)
except (json.JSONDecodeError, FileNotFoundError):
    extra["comparison"] = {}
    extra["regression_detected"] = False
    extra["hardware_match"] = True

# Load raw bench results
try:
    with open(bench_path) as f:
        extra["bench_results"] = json.load(f)
except (json.JSONDecodeError, FileNotFoundError):
    pass

with open(extra_out, "w") as f:
    json.dump(extra, f)
PYEOF
        rm -f "$COMPARISON_TMP"

        STAMP_PATH=$(write_stamp "bench" "Benchmarks passed, no regressions" "$BENCH_EXTRA_TMP")
        ok "wrote $STAMP_PATH"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
        rm -f "$BENCH_EXTRA_TMP"
    fi

    # Smoke stamp
    if [[ "$TIER_SMOKE" == "pass" ]]; then
        STAMP_PATH=$(write_stamp "smoke" "Agent smoke tests passed ($SMOKE_FILTER)")
        ok "wrote $STAMP_PATH"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi

    if [[ "$STAMPS_WRITTEN" -eq 0 ]]; then
        info "no passing tiers to attest"
    fi
fi

# Clean up temp file
[[ -n "$BENCH_TMPFILE" ]] && rm -f "$BENCH_TMPFILE"

# ─── Summary ────────────────────────────────────────────────────────────────
section "Summary"

print_tier_status() {
    local name="$1"
    local result="$2"
    local reason="$3"

    case "$result" in
        pass) echo "  ${GREEN}PASS${RESET}  $name" ;;
        fail) echo "  ${RED}FAIL${RESET}  $name${DIM} — $reason${RESET}" ;;
        skip) echo "  ${DIM}SKIP${RESET}  $name${DIM} — $reason${RESET}" ;;
    esac
}

print_tier_status "tier-ci"    "$TIER_CI"    "$TIER_CI_REASON"
print_tier_status "tier-vm"    "$TIER_VM"    "$TIER_VM_REASON"
print_tier_status "tier-bench" "$TIER_BENCH" "$TIER_BENCH_REASON"
print_tier_status "tier-smoke" "$TIER_SMOKE" "$TIER_SMOKE_REASON"

echo
info "$STAMPS_WRITTEN attestation stamp(s) written"

# Exit code: 0 if all runnable tiers passed, 1 if any failed
ANY_FAILED=false
if [[ "$TIER_CI" == "fail" ]] || [[ "$TIER_VM" == "fail" ]] || \
   [[ "$TIER_BENCH" == "fail" ]] || [[ "$TIER_SMOKE" == "fail" ]]; then
    ANY_FAILED=true
fi

echo
if $ANY_FAILED; then
    echo "${RED}${BOLD}Pre-release validation FAILED${RESET}"
    exit 1
else
    echo "${GREEN}${BOLD}Pre-release validation PASSED${RESET}"
    exit 0
fi
