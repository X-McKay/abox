#!/usr/bin/env bash
# pre_release.sh — pre-release validation pipeline.
#
# Detects host capabilities, runs the validation tiers in order, and writes
# attestation stamps for passing tiers. `scripts/release.sh` verifies the
# stamps before cutting a release.
#
# Tiers:
#   1. ci      — fmt + clippy + tests + cargo-deny (always runs)
#   2. bench   — Criterion microbenchmarks (always runs)
#   3. runtime — live MicroSandbox e2e suite (needs virtualization + msb assets)
#   4. smoke   — real agent API calls through the broker (needs credentials)
#
# The script continues across tiers even if one fails, so you always get
# a complete report.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ATTESTATION_DIR="$REPO_ROOT/.abox-attestations"

# ─── Colors ──────────────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    GREEN=$'\033[32m'; RED=$'\033[31m'; YELLOW=$'\033[33m'
    DIM=$'\033[2m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
else
    GREEN=""; RED=""; YELLOW=""; DIM=""; BOLD=""; RESET=""
fi

section() { printf '\n%s━━━ %s ━━━%s\n' "$BOLD" "$1" "$RESET"; }
ok()      { printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$1"; }
warn()    { printf '  %s!%s %s\n' "$YELLOW" "$RESET" "$1"; }
err()     { printf '  %s✗%s %s\n' "$RED" "$RESET" "$1"; }
info()    { printf '  %s·%s %s\n' "$DIM" "$RESET" "$1"; }
skip()    { printf '  %sSKIP%s %s — %s\n' "$DIM" "$RESET" "$1" "$2"; }

TIER_CI="skip";      TIER_CI_REASON=""
TIER_BENCH="skip";   TIER_BENCH_REASON=""
TIER_RUNTIME="skip"; TIER_RUNTIME_REASON=""
TIER_SMOKE="skip";   TIER_SMOKE_REASON=""

# ─── Capability detection ────────────────────────────────────────────────────
section "Host capabilities"

# Hardware virtualization for the MicroSandbox runtime (KVM on Linux,
# Hypervisor.framework on macOS arm64).
HAS_VIRT=false
if [[ "$(uname -s)" == "Darwin" ]]; then
    if [[ "$(uname -m)" == "arm64" ]] && [[ "$(sysctl -n kern.hv_support 2>/dev/null)" == "1" ]]; then
        HAS_VIRT=true
        ok "Hypervisor.framework available"
    else
        warn "Hypervisor.framework not available"
    fi
elif [[ -c /dev/kvm ]] && [[ -r /dev/kvm ]]; then
    HAS_VIRT=true
    ok "/dev/kvm exists and is readable"
else
    warn "/dev/kvm not available"
fi

# MicroSandbox runtime assets (msb + libkrunfw)
MSB_HOME_DIR="${MSB_HOME:-$HOME/.microsandbox}"
HAS_MSB=false
if [[ -x "$MSB_HOME_DIR/bin/msb" ]] && compgen -G "$MSB_HOME_DIR/lib/libkrunfw*" >/dev/null; then
    HAS_MSB=true
    ok "MicroSandbox runtime present ($MSB_HOME_DIR)"
else
    warn "MicroSandbox runtime missing (run 'abox init')"
fi

# Agent credentials
HAS_CLAUDE=false
if [[ -f "$HOME/.claude/.credentials.json" ]]; then
    HAS_CLAUDE=true
    ok "Claude credentials found"
else
    info "Claude credentials not found (~/.claude/.credentials.json)"
fi

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

CAN_CI=true
info "tier-ci:      WILL RUN (always)"
info "tier-bench:   WILL RUN (always)"

CAN_RUNTIME=false
if $HAS_VIRT && $HAS_MSB; then
    CAN_RUNTIME=true
    info "tier-runtime: WILL RUN"
else
    if ! $HAS_VIRT; then
        TIER_RUNTIME_REASON="no hardware virtualization"
    else
        TIER_RUNTIME_REASON="MicroSandbox runtime not installed"
    fi
    info "tier-runtime: SKIP ($TIER_RUNTIME_REASON)"
fi

CAN_SMOKE=false
SMOKE_FILTER=""
if $CAN_RUNTIME; then
    if $HAS_CLAUDE && $HAS_CODEX; then
        CAN_SMOKE=true; SMOKE_FILTER="all"
        info "tier-smoke:   WILL RUN (claude + codex)"
    elif $HAS_CLAUDE; then
        CAN_SMOKE=true; SMOKE_FILTER="claude"
        info "tier-smoke:   WILL RUN (claude only)"
    elif $HAS_CODEX; then
        CAN_SMOKE=true; SMOKE_FILTER="codex"
        info "tier-smoke:   WILL RUN (codex only)"
    else
        TIER_SMOKE_REASON="no agent credentials"
        info "tier-smoke:   SKIP ($TIER_SMOKE_REASON)"
    fi
else
    TIER_SMOKE_REASON="$TIER_RUNTIME_REASON"
    info "tier-smoke:   SKIP ($TIER_SMOKE_REASON)"
fi

# ─── Tier execution ─────────────────────────────────────────────────────────

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

section "Tier 2: Criterion microbenchmarks"
if just tier-bench; then
    TIER_BENCH="pass"
    ok "tier-bench passed"
else
    TIER_BENCH="fail"
    TIER_BENCH_REASON="criterion benchmarks failed"
    err "tier-bench failed"
fi

section "Tier 3: MicroSandbox runtime end-to-end"
if $CAN_RUNTIME; then
    if just e2e-runtime; then
        TIER_RUNTIME="pass"
        ok "tier-runtime passed"
    else
        TIER_RUNTIME="fail"
        TIER_RUNTIME_REASON="e2e-runtime failed"
        err "tier-runtime failed"
    fi
else
    skip "tier-runtime" "$TIER_RUNTIME_REASON"
fi

section "Tier 4: Agent smoke tests"
if $CAN_SMOKE; then
    SMOKE_ARGS=()
    [[ "$SMOKE_FILTER" != "all" ]] && SMOKE_ARGS+=("$SMOKE_FILTER")
    if ./scripts/local/agent_smoke_test.sh "${SMOKE_ARGS[@]}"; then
        TIER_SMOKE="pass"
        ok "tier-smoke passed"
    else
        TIER_SMOKE="fail"
        TIER_SMOKE_REASON="agent smoke tests failed"
        err "tier-smoke failed"
    fi
else
    skip "tier-smoke" "$TIER_SMOKE_REASON"
fi

# ─── Attestation stamps ─────────────────────────────────────────────────────
section "Attestation"

STAMPS_WRITTEN=0
if ! $GIT_CLEAN; then
    warn "working tree dirty — stamps skipped (release.sh requires clean-tree stamps)"
else
    mkdir -p "$ATTESTATION_DIR"

    GIT_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
    TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    HW_ARCH=$(uname -m)
    HW_CORES=$(nproc 2>/dev/null || sysctl -n hw.ncpu)
    HW_KERNEL=$(uname -r)

    # write_stamp <tier> <summary>
    write_stamp() {
        local tier="$1"
        local summary="$2"

        python3 - "$ATTESTATION_DIR/${tier}.json" <<PYEOF
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

out_path = sys.argv[1]
with open(out_path, "w") as f:
    json.dump(stamp, f, indent=2)
    f.write("\n")
print(out_path)
PYEOF
    }

    if [[ "$TIER_RUNTIME" == "pass" ]]; then
        STAMP_PATH=$(write_stamp "runtime" "MicroSandbox runtime e2e passed")
        ok "wrote $STAMP_PATH"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi

    if [[ "$TIER_BENCH" == "pass" ]]; then
        STAMP_PATH=$(write_stamp "bench" "Criterion microbenchmarks passed")
        ok "wrote $STAMP_PATH"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi

    if [[ "$TIER_SMOKE" == "pass" ]]; then
        STAMP_PATH=$(write_stamp "smoke" "Agent smoke tests passed ($SMOKE_FILTER)")
        ok "wrote $STAMP_PATH"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi

    if [[ "$STAMPS_WRITTEN" -eq 0 ]]; then
        info "no passing tiers to attest"
    fi
fi

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

print_tier_status "tier-ci"      "$TIER_CI"      "$TIER_CI_REASON"
print_tier_status "tier-bench"   "$TIER_BENCH"   "$TIER_BENCH_REASON"
print_tier_status "tier-runtime" "$TIER_RUNTIME" "$TIER_RUNTIME_REASON"
print_tier_status "tier-smoke"   "$TIER_SMOKE"   "$TIER_SMOKE_REASON"

echo
info "$STAMPS_WRITTEN attestation stamp(s) written"

ANY_FAILED=false
if [[ "$TIER_CI" == "fail" ]] || [[ "$TIER_BENCH" == "fail" ]] || [[ "$TIER_RUNTIME" == "fail" ]] || [[ "$TIER_SMOKE" == "fail" ]]; then
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
