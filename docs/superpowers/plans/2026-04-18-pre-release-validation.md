# Pre-Release Validation Workflow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a single `just pre-release` command that runs all test tiers, compares benchmarks against the previous release baseline, writes attestation stamps, and gates `release.sh` on those stamps.

**Architecture:** A hybrid approach — justfile recipes for composable tier commands (`just tier-ci`, `just tier-vm`, `just tier-bench`, `just tier-smoke`), a shell orchestrator (`scripts/pre_release.sh`) for the run-what-you-can logic, attestation stamp writing, and benchmark comparison. `release.sh` is simplified to verify stamps instead of running tests inline.

**Tech Stack:** Bash, just, python3 (for JSON handling in benchmark comparison — matches existing patterns in `release.sh` and `bench.sh`)

**Spec:** `docs/superpowers/specs/2026-04-18-pre-release-validation-design.md`

---

### Task 1: Directory restructure — move test scripts to `scripts/local/`

**Files:**
- Create: `scripts/local/` (directory)
- Create: `scripts/ci/README.md`
- Move: `scripts/e2e_test.sh` → `scripts/local/e2e_test.sh`
- Move: `scripts/agent_smoke_test.sh` → `scripts/local/agent_smoke_test.sh`
- Move: `scripts/bench.sh` → `scripts/local/bench.sh`
- Move: `scripts/bench_template.sh` → `scripts/local/bench_template.sh`

- [ ] **Step 1: Create the `scripts/local/` and `scripts/ci/` directories**

```bash
mkdir -p scripts/local scripts/ci
```

- [ ] **Step 2: Create `scripts/ci/README.md` explaining the convention**

Write to `scripts/ci/README.md`:

```markdown
# CI-Safe Test Scripts

Scripts in this directory are safe to run in any environment, including
GitHub Actions runners. They require no KVM, no VM bootstrap, and no
host credentials.

Currently, CI-safe checks (`just check`, `just deny`) are pure
cargo/just commands with no wrapper scripts. This directory exists to
establish the `ci/` vs `local/` convention so future CI-only test
scripts have a home.

See `scripts/local/` for tests that require KVM, VM artifacts, or
real API credentials.
```

- [ ] **Step 3: Move test scripts to `scripts/local/`**

```bash
cd /home/al/git/bakudo-abox/abox
git mv scripts/e2e_test.sh scripts/local/e2e_test.sh
git mv scripts/agent_smoke_test.sh scripts/local/agent_smoke_test.sh
git mv scripts/bench.sh scripts/local/bench.sh
git mv scripts/bench_template.sh scripts/local/bench_template.sh
```

- [ ] **Step 4: Verify the scripts directory structure looks correct**

```bash
ls -la scripts/
ls -la scripts/local/
ls -la scripts/ci/
```

Expected: `scripts/` contains `release.sh`, `bootstrap_vm.sh`, `build_rootfs.sh`, `install.sh`, `pre_release.sh` (not yet), `lib/`. `scripts/local/` contains the four moved scripts. `scripts/ci/` contains `README.md`.

- [ ] **Step 5: Commit the directory restructure**

```bash
git add scripts/ci/README.md scripts/local/
git commit -m "chore: move local-only test scripts to scripts/local/

Establishes scripts/ci/ vs scripts/local/ convention.
CI-safe scripts stay at the top level or in ci/.
Scripts requiring KVM, VM bootstrap, or credentials live in local/.

Moved: e2e_test.sh, agent_smoke_test.sh, bench.sh, bench_template.sh"
```

---

### Task 2: Update `.gitignore` and internal script references

**Files:**
- Modify: `.gitignore`
- Modify: `scripts/local/bench.sh:66` (bench_template reference)
- Modify: `.github/workflows/ci.yml:80` (e2e script path)

- [ ] **Step 1: Add `.abox-attestations/` to `.gitignore`**

In `.gitignore`, after the `.scratch/` line, add:

```
# Pre-release attestation stamps (machine-local, not committed)
.abox-attestations/
```

- [ ] **Step 2: Check if `bench.sh` references `bench_template.sh` by relative path**

```bash
grep -n 'bench_template' scripts/local/bench.sh
```

If it uses a relative path like `./scripts/bench_template.sh` or `$SCRIPT_DIR/bench_template.sh`, verify it still resolves correctly since both files moved together to `scripts/local/`. The `SCRIPT_DIR` pattern (`$(cd "$(dirname "$0")" && pwd)`) will resolve to `scripts/local/`, so a `$SCRIPT_DIR/bench_template.sh` reference will still work. A `./scripts/bench_template.sh` reference (relative to repo root) would break and must be updated to `./scripts/local/bench_template.sh`.

- [ ] **Step 3: Update CI workflow e2e script path**

In `.github/workflows/ci.yml`, the `e2e-phases-1-5` job at line 80 runs:

```yaml
        run: ./scripts/e2e_test.sh
```

Change to:

```yaml
        run: ./scripts/local/e2e_test.sh
```

- [ ] **Step 4: Check for any other references to the old script paths**

```bash
cd /home/al/git/bakudo-abox/abox
grep -rn 'scripts/e2e_test\.sh\|scripts/agent_smoke_test\.sh\|scripts/bench\.sh' \
  --include='*.sh' --include='*.yml' --include='*.yaml' --include='*.md' --include='*.toml' \
  | grep -v 'scripts/local/' | grep -v 'CHANGELOG' | grep -v 'docs/superpowers/'
```

For each match found: update the path to `scripts/local/<script>`. Common locations: `justfile`, `release.sh`, `AGENTS.md`, `docs/contributing/pre-pr-checklist.md`, skill files. (The justfile and docs will be updated in later tasks.)

- [ ] **Step 5: Commit reference updates**

```bash
git add .gitignore .github/workflows/ci.yml
# Add any other files with updated references found in step 4
git commit -m "chore: update script paths after local/ restructure

- .gitignore: add .abox-attestations/
- ci.yml: update e2e_test.sh path to scripts/local/"
```

---

### Task 3: Update justfile with tier recipes and new paths

**Files:**
- Modify: `justfile`

- [ ] **Step 1: Update the `e2e-vm` recipe path**

In `justfile`, change line 143:

```just
e2e-vm:
    ./scripts/e2e_test.sh
```

to:

```just
e2e-vm:
    ./scripts/local/e2e_test.sh
```

- [ ] **Step 2: Update the `bench-vm` recipe path**

In `justfile`, change line 115:

```just
bench-vm:
    ./scripts/bench.sh
```

to:

```just
bench-vm:
    ./scripts/local/bench.sh
```

- [ ] **Step 3: Update the `bench-vm-n` recipe path**

In `justfile`, change line 119:

```just
bench-vm-n n="5":
    ./scripts/bench.sh --runs {{n}}
```

to:

```just
bench-vm-n n="5":
    ./scripts/local/bench.sh --runs {{n}}
```

- [ ] **Step 4: Add tier recipes and pre-release recipe**

After the `ci` recipe (line 75-76) and before the `# ─── Cleanup` section, add:

```just
# ─── Pre-Release Validation ─────────────────────────────────────────────────

# Run all pre-release validation tiers, attest what passes.
pre-release:
    ./scripts/pre_release.sh

# Tier 1: CI-safe checks (fmt + clippy + test + supply-chain audit). No KVM needed.
tier-ci: check deny

# Tier 2: VM end-to-end tests (requires KVM + bootstrapped VM). Checks rootfs freshness first.
tier-vm: check-rootfs e2e-vm

# Tier 3: Benchmarks — criterion microbenchmarks + VM latency (requires KVM + bootstrapped VM).
tier-bench: bench bench-vm-n "5"

# Tier 4: Agent smoke tests — real Claude/Codex API calls (requires KVM + credentials). Costs tokens.
tier-smoke:
    ./scripts/local/agent_smoke_test.sh
```

- [ ] **Step 5: Verify recipes parse correctly**

```bash
cd /home/al/git/bakudo-abox/abox
just --list
```

Expected: all existing recipes plus `pre-release`, `tier-ci`, `tier-vm`, `tier-bench`, `tier-smoke` appear in the list. `pre-release` will show an error about missing script (not yet created) — that's fine, we just want to confirm the justfile parses.

- [ ] **Step 6: Run `just tier-ci` to verify it works**

```bash
just tier-ci
```

Expected: runs `fmt-check`, `lint`, `test`, `deny` — all pass.

- [ ] **Step 7: Commit justfile updates**

```bash
git add justfile
git commit -m "chore: add tier recipes and pre-release entrypoint to justfile

New recipes: pre-release, tier-ci, tier-vm, tier-bench, tier-smoke.
Updated bench/e2e paths to scripts/local/."
```

---

### Task 4: Write the orchestrator script (`scripts/pre_release.sh`)

This is the largest task — the new orchestrator that detects capabilities, runs tiers, compares benchmarks, writes attestation stamps, and prints a summary.

**Files:**
- Create: `scripts/pre_release.sh`

- [ ] **Step 1: Write the orchestrator script**

Create `scripts/pre_release.sh` with the content below. The script is structured in sections: argument parsing, output helpers, capability detection, tier execution, benchmark comparison, attestation stamp writing, and summary reporting.

```bash
#!/usr/bin/env bash
#
# abox pre-release validation orchestrator.
#
# Detects host capabilities, runs all applicable test tiers, compares
# benchmarks against the previous release baseline, and writes attestation
# stamps for each passing tier.
#
# Usage:
#   ./scripts/pre_release.sh           # run all tiers the host can support
#   just pre-release                   # same thing via justfile
#
# Attestation stamps are written to .abox-attestations/{vm,bench,smoke}.json.
# release.sh checks these stamps before allowing a release to proceed.
#
# Tiers:
#   1. ci     — fmt + clippy + test + cargo deny (always runs)
#   2. vm     — e2e_test.sh all phases (requires KVM + VM bootstrap + fresh rootfs)
#   3. bench  — VM latency + criterion microbenchmarks (requires KVM + VM bootstrap)
#   4. smoke  — real Claude/Codex API calls (requires KVM + credentials, costs tokens)
#
# Exit code: 0 if all runnable tiers passed, 1 if any failed.

set -uo pipefail

# ─── Output helpers ────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
    YELLOW=$'\033[33m'; BLUE=$'\033[34m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; RESET=""
fi

section() { printf '\n%s━━━ %s ━━━%s\n' "$BOLD$BLUE" "$1" "$RESET"; }
info()    { printf '  %s\n' "$1"; }
ok()      { printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$1"; }
warn()    { printf '  %s⚠%s %s\n' "$YELLOW" "$RESET" "$1"; }
err()     { printf '  %s✗%s %s\n' "$RED" "$RESET" "$1"; }
skip()    { printf '  %s⊘%s %s\n' "$DIM" "$RESET" "$1"; }

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ATTESTATION_DIR="$REPO_ROOT/.abox-attestations"
ABOX_VM="$HOME/.abox/vm"

# ─── Tier result tracking ─────────────────────────────────────────────────
# Each tier is one of: pass, fail, skip
TIER_CI="skip"
TIER_VM="skip"
TIER_BENCH="skip"
TIER_SMOKE="skip"
TIER_CI_REASON=""
TIER_VM_REASON=""
TIER_BENCH_REASON=""
TIER_SMOKE_REASON=""

# ─── Capability detection ─────────────────────────────────────────────────
section "capability detection"

HAS_KVM=false
HAS_VM_BOOTSTRAP=false
ROOTFS_FRESH=false
HAS_CLAUDE=false
HAS_CODEX=false
GIT_CLEAN=false

# KVM
if [[ -c /dev/kvm ]] && [[ -r /dev/kvm ]]; then
    HAS_KVM=true; ok "KVM: available"
else
    warn "KVM: not available"
fi

# VM bootstrap
if [[ -x "$ABOX_VM/cloud-hypervisor" ]] && [[ -f "$ABOX_VM/rootfs.raw" ]]; then
    HAS_VM_BOOTSTRAP=true; ok "VM bootstrap: present"
else
    warn "VM bootstrap: not found (run 'just bootstrap-vm')"
fi

# Rootfs freshness
if $HAS_VM_BOOTSTRAP; then
    if just check-rootfs >/dev/null 2>&1; then
        ROOTFS_FRESH=true; ok "rootfs: fresh"
    else
        warn "rootfs: STALE — run 'just rebuild-rootfs' before VM tests"
    fi
fi

# Credentials
if [[ -f "$HOME/.claude/.credentials.json" ]]; then
    HAS_CLAUDE=true; ok "Claude credentials: found"
else
    info "Claude credentials: not found"
fi
if [[ -f "$HOME/.codex/auth.json" ]]; then
    HAS_CODEX=true; ok "Codex credentials: found"
else
    info "Codex credentials: not found"
fi

# Git cleanliness (for attestation writing, not for running tests)
if [[ -z "$(git status --porcelain)" ]]; then
    GIT_CLEAN=true; ok "git tree: clean"
else
    warn "git tree: dirty (tests will run but attestation stamps will not be written)"
fi

GIT_SHA=$(git rev-parse --short HEAD)

# ─── Plan ──────────────────────────────────────────────────────────────────
section "plan"

CAN_VM=false
CAN_BENCH=false
CAN_SMOKE=false

info "tier-ci:    WILL RUN (always)"

if $HAS_KVM && $HAS_VM_BOOTSTRAP && $ROOTFS_FRESH; then
    CAN_VM=true
    info "tier-vm:    WILL RUN"
else
    if ! $ROOTFS_FRESH && $HAS_KVM && $HAS_VM_BOOTSTRAP; then
        info "tier-vm:    SKIP (rootfs stale)"
        TIER_VM_REASON="rootfs stale — run 'just rebuild-rootfs'"
    else
        info "tier-vm:    SKIP (missing KVM or VM bootstrap)"
        TIER_VM_REASON="missing KVM or VM bootstrap"
    fi
fi

if $CAN_VM; then
    CAN_BENCH=true
    info "tier-bench: WILL RUN"
else
    info "tier-bench: SKIP (requires VM tier prerequisites)"
    TIER_BENCH_REASON="requires KVM + VM bootstrap + fresh rootfs"
fi

if $HAS_KVM && $HAS_VM_BOOTSTRAP && ($HAS_CLAUDE || $HAS_CODEX); then
    CAN_SMOKE=true
    SMOKE_FILTER="all"
    if ! $HAS_CLAUDE; then SMOKE_FILTER="codex"; fi
    if ! $HAS_CODEX; then SMOKE_FILTER="claude"; fi
    info "tier-smoke: WILL RUN ($SMOKE_FILTER)"
else
    info "tier-smoke: SKIP (missing KVM, VM bootstrap, or credentials)"
    TIER_SMOKE_REASON="missing KVM, VM bootstrap, or credentials"
fi

# ─── Tier 1: CI ────────────────────────────────────────────────────────────
section "tier 1 — ci (fmt + clippy + test + deny)"

if just tier-ci; then
    TIER_CI="pass"
    ok "tier-ci passed"
else
    TIER_CI="fail"
    TIER_CI_REASON="just tier-ci exited non-zero"
    err "tier-ci FAILED"
fi

# ─── Tier 2: VM ────────────────────────────────────────────────────────────
section "tier 2 — vm (e2e tests, all phases)"

if $CAN_VM; then
    if just tier-vm; then
        TIER_VM="pass"
        ok "tier-vm passed"
    else
        TIER_VM="fail"
        TIER_VM_REASON="just tier-vm exited non-zero"
        err "tier-vm FAILED"
    fi
else
    skip "tier-vm skipped: $TIER_VM_REASON"
fi

# ─── Tier 3: Bench ─────────────────────────────────────────────────────────
section "tier 3 — bench (criterion + VM latency)"

BENCH_CURRENT_JSON=""
if $CAN_BENCH && [[ "$TIER_VM" == "pass" ]]; then
    BENCH_FAIL=false

    # Run criterion microbenchmarks first (no VM needed).
    info "running criterion microbenchmarks..."
    if cargo bench -p abox-core >/dev/null 2>&1; then
        ok "criterion benchmarks passed"
    else
        err "criterion benchmarks failed"
        BENCH_FAIL=true
    fi

    # Run VM latency benchmarks and capture JSON output.
    if ! $BENCH_FAIL; then
        info "running VM latency benchmarks (5 runs)..."
        BENCH_CURRENT_JSON=$(./scripts/local/bench.sh --runs 5 2>/dev/null || true)
        if [[ -n "$BENCH_CURRENT_JSON" ]]; then
            ok "VM benchmarks captured"
        else
            err "VM benchmarks failed"
            BENCH_FAIL=true
        fi
    fi

    if ! $BENCH_FAIL; then
        TIER_BENCH="pass"
        ok "tier-bench passed"
    else
        TIER_BENCH="fail"
        TIER_BENCH_REASON="benchmark execution failed"
        err "tier-bench FAILED"
    fi
elif [[ "$TIER_VM" == "fail" ]]; then
    skip "tier-bench skipped: tier-vm failed (benchmarks would be meaningless)"
    TIER_BENCH_REASON="tier-vm failed"
else
    skip "tier-bench skipped: $TIER_BENCH_REASON"
fi

# ─── Tier 4: Smoke ─────────────────────────────────────────────────────────
section "tier 4 — smoke (agent API calls)"

if $CAN_SMOKE; then
    if ./scripts/local/agent_smoke_test.sh "$SMOKE_FILTER"; then
        TIER_SMOKE="pass"
        ok "tier-smoke passed"
    else
        TIER_SMOKE="fail"
        TIER_SMOKE_REASON="agent_smoke_test.sh exited non-zero"
        err "tier-smoke FAILED"
    fi
else
    skip "tier-smoke skipped: $TIER_SMOKE_REASON"
fi

# ─── Benchmark comparison ──────────────────────────────────────────────────
BENCH_COMPARISON=""
BENCH_REGRESSION=false
BENCH_HW_MATCH=false
BENCH_BASELINE_VERSION=""

if [[ "$TIER_BENCH" == "pass" ]] && [[ -n "$BENCH_CURRENT_JSON" ]]; then
    section "benchmark comparison"

    # Find the latest baseline by semantic version.
    BASELINE_FILE=$(ls benchmarks/v*.json 2>/dev/null | sort -V | tail -1 || true)

    if [[ -n "$BASELINE_FILE" ]]; then
        BENCH_BASELINE_VERSION=$(basename "$BASELINE_FILE" .json)

        # Run comparison via python3.
        BENCH_COMPARISON=$(python3 - "$BASELINE_FILE" "$BENCH_CURRENT_JSON" <<'PYEOF'
import json, sys

baseline_path = sys.argv[1]
current_json = sys.argv[2]

with open(baseline_path) as f:
    baseline = json.load(f)
current = json.loads(current_json)

# Hardware match: same arch and core count.
hw_match = (
    baseline.get("hardware", {}).get("arch") == current.get("hardware", {}).get("arch") and
    baseline.get("hardware", {}).get("cores") == current.get("hardware", {}).get("cores")
)

metrics = ["vm_boot_ms", "proxy_roundtrip_ms", "full_run_ms", "cleanup_ms"]
results = {}
regression = False

for m in metrics:
    b_avg = baseline.get("results", {}).get(m, {}).get("avg", -1)
    c_avg = current.get("results", {}).get(m, {}).get("avg", -1)
    if b_avg > 0 and c_avg > 0:
        delta_pct = ((c_avg - b_avg) / b_avg) * 100
    else:
        delta_pct = 0
    results[m] = {"baseline": b_avg, "current": c_avg, "delta_pct": round(delta_pct, 1)}
    if delta_pct > 15 and hw_match:
        regression = True

out = {
    "hw_match": hw_match,
    "regression": regression,
    "metrics": results
}
print(json.dumps(out))
PYEOF
        )

        # Parse and display results.
        HW_MATCH_STR=$(echo "$BENCH_COMPARISON" | python3 -c "import json,sys; print(json.load(sys.stdin)['hw_match'])")
        REGRESSION_STR=$(echo "$BENCH_COMPARISON" | python3 -c "import json,sys; print(json.load(sys.stdin)['regression'])")

        if [[ "$HW_MATCH_STR" == "True" ]]; then
            BENCH_HW_MATCH=true
            info "hardware: matches baseline"
        else
            info "hardware: differs from baseline (regressions are advisory only)"
        fi

        if [[ "$REGRESSION_STR" == "True" ]]; then
            BENCH_REGRESSION=true
        fi

        info "baseline: $BENCH_BASELINE_VERSION"
        printf '\n'
        printf '  %-24s %-12s %-12s %s\n' "METRIC" "BASELINE" "CURRENT" "DELTA"
        printf '  %-24s %-12s %-12s %s\n' "────────────────────────" "──────────" "──────────" "──────────"

        for metric in vm_boot_ms proxy_roundtrip_ms full_run_ms cleanup_ms; do
            b=$(echo "$BENCH_COMPARISON" | python3 -c "import json,sys; print(json.load(sys.stdin)['metrics']['$metric']['baseline'])")
            c=$(echo "$BENCH_COMPARISON" | python3 -c "import json,sys; print(json.load(sys.stdin)['metrics']['$metric']['current'])")
            d=$(echo "$BENCH_COMPARISON" | python3 -c "import json,sys; print(json.load(sys.stdin)['metrics']['$metric']['delta_pct'])")
            printf '  %-24s %-12s %-12s %s%%\n' "$metric" "${b} ms" "${c} ms" "${d}"
        done
        printf '\n'

        if $BENCH_REGRESSION; then
            if $BENCH_HW_MATCH; then
                err "REGRESSION detected (>15% on matching hardware)"
                TIER_BENCH="fail"
                TIER_BENCH_REASON="benchmark regression >15% on matching hardware"
            else
                warn "potential regression detected, but hardware differs — advisory only"
            fi
        else
            ok "no regressions (threshold: 15%)"
        fi
    else
        info "no baseline found in benchmarks/ — skipping comparison"
    fi
fi

# ─── Write attestation stamps ──────────────────────────────────────────────
section "attestation"

write_stamp() {
    # write_stamp <tier> <result> <summary> [extra_json_fields]
    local tier="$1" result="$2" summary="$3" extra="${4:-}"
    local stamp_file="$ATTESTATION_DIR/${tier}.json"
    local hw_arch hw_cores hw_kernel
    hw_arch=$(uname -m)
    hw_cores=$(nproc)
    hw_kernel=$(uname -r)

    local json
    json=$(python3 -c "
import json
stamp = {
    'tier': '$tier',
    'timestamp': '$(date -u +%Y-%m-%dT%H:%M:%SZ)',
    'git_sha': '$GIT_SHA',
    'git_dirty': False,
    'result': '$result',
    'summary': '$summary',
    'hardware': {
        'arch': '$hw_arch',
        'cores': $hw_cores,
        'kernel': '$hw_kernel'
    }
}
extra = '''$extra'''
if extra:
    stamp.update(json.loads(extra))
print(json.dumps(stamp, indent=2))
")
    echo "$json" > "$stamp_file"
    ok "wrote $stamp_file"
}

STAMPS_WRITTEN=0

if ! $GIT_CLEAN; then
    warn "git tree dirty — skipping attestation stamps"
else
    mkdir -p "$ATTESTATION_DIR"

    # VM stamp
    if [[ "$TIER_VM" == "pass" ]]; then
        write_stamp "vm" "pass" "e2e tests passed (all phases)"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi

    # Bench stamp
    if [[ "$TIER_BENCH" == "pass" ]] && ! $BENCH_REGRESSION; then
        BENCH_EXTRA=""
        if [[ -n "$BENCH_COMPARISON" ]] && [[ -n "$BENCH_BASELINE_VERSION" ]]; then
            # Build extra JSON fields for the bench stamp.
            BENCH_EXTRA=$(python3 -c "
import json, sys
comp = json.loads('$BENCH_COMPARISON')
extra = {
    'baseline_version': '$BENCH_BASELINE_VERSION',
    'comparison': comp['metrics'],
    'regression_detected': comp['regression'],
    'hardware_match': comp['hw_match']
}
# Include the raw benchmark results for release.sh to use.
current = json.loads('''$BENCH_CURRENT_JSON''')
extra['bench_results'] = current
print(json.dumps(extra))
")
        elif [[ -n "$BENCH_CURRENT_JSON" ]]; then
            # No baseline to compare against, but still save results.
            BENCH_EXTRA=$(python3 -c "
import json
current = json.loads('''$BENCH_CURRENT_JSON''')
extra = {
    'baseline_version': None,
    'comparison': None,
    'regression_detected': False,
    'hardware_match': False,
    'bench_results': current
}
print(json.dumps(extra))
")
        fi
        write_stamp "bench" "pass" "benchmarks passed, no regressions" "$BENCH_EXTRA"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi

    # Smoke stamp
    if [[ "$TIER_SMOKE" == "pass" ]]; then
        write_stamp "smoke" "pass" "agent smoke tests passed"
        STAMPS_WRITTEN=$((STAMPS_WRITTEN + 1))
    fi
fi

# ─── Summary ──────────────────────────────────────────────────────────────
section "summary"

ANY_FAIL=false

for tier_name in ci vm bench smoke; do
    eval "result=\$TIER_$(echo "$tier_name" | tr '[:lower:]' '[:upper:]')"
    eval "reason=\$TIER_$(echo "$tier_name" | tr '[:lower:]' '[:upper:]')_REASON"
    case "$result" in
        pass) ok "tier-$tier_name: PASSED" ;;
        fail) err "tier-$tier_name: FAILED ($reason)"; ANY_FAIL=true ;;
        skip) skip "tier-$tier_name: SKIPPED ($reason)" ;;
    esac
done

printf '\n'
if $GIT_CLEAN; then
    info "attestation stamps written: $STAMPS_WRITTEN"
    info "git sha: $GIT_SHA"
else
    warn "no stamps written (dirty tree)"
fi

printf '\n'
if $ANY_FAIL; then
    printf '%s✗ pre-release FAILED%s — fix the failing tiers and re-run\n' "$RED$BOLD" "$RESET"
    exit 1
else
    printf '%s✓ pre-release PASSED%s\n' "$GREEN$BOLD" "$RESET"
    exit 0
fi
```

- [ ] **Step 2: Make the script executable**

```bash
chmod +x scripts/pre_release.sh
```

- [ ] **Step 3: Verify the script runs the capability detection phase**

```bash
cd /home/al/git/bakudo-abox/abox
# Just test the script starts and detects capabilities — Ctrl-C after the plan section
# if you don't want to run all tiers right now.
./scripts/pre_release.sh
```

Expected: capability detection shows current host state (KVM, VM bootstrap, credentials, git cleanliness). The plan section shows which tiers will run. Tiers execute in order.

- [ ] **Step 4: Commit the orchestrator**

```bash
git add scripts/pre_release.sh
git commit -m "feat: add pre-release validation orchestrator

scripts/pre_release.sh detects host capabilities, runs applicable
test tiers (ci, vm, bench, smoke), compares benchmarks against the
previous release baseline, and writes attestation stamps for each
passing tier.

Invoked via 'just pre-release'."
```

---

### Task 5: Update `release.sh` to verify attestation stamps

**Files:**
- Modify: `scripts/release.sh`

- [ ] **Step 1: Add `--skip-attestation` to argument parsing**

In `scripts/release.sh`, add a new variable after `DRY_RUN=0` (line 25):

```bash
SKIP_ATTESTATION=0
```

And add a new case in the `while` loop (after the `--dry` case, around line 29):

```bash
        --skip-attestation) SKIP_ATTESTATION=1; shift ;;
```

Update the help text (inside the `--help|-h)` case) to include:

```
  --skip-attestation  Skip attestation stamp verification (emergency use only)
```

- [ ] **Step 2: Update the header comment**

Replace the header comment (lines 1-19) with:

```bash
#!/usr/bin/env bash
#
# abox release script.
#
# Verifies pre-release attestation stamps, bumps the workspace version,
# builds the release binary, updates the benchmark table in README.md,
# generates a changelog entry, and commits + tags the result.
#
# Usage:
#   ./scripts/release.sh 0.2.0                    # release v0.2.0
#   ./scripts/release.sh 0.2.0 --dry              # show what would happen
#   ./scripts/release.sh 0.2.0 --skip-attestation # emergency: skip stamp check
#
# Prerequisites:
#   - Run `just pre-release` first to generate attestation stamps
#   - Clean git working tree (no uncommitted changes)
#   - just, cargo, python3
#
# The script does NOT push. After it completes, review the commit and run:
#   git push origin main --tags
```

- [ ] **Step 3: Update step count from 12 to 13 throughout**

Replace all `[N/12]` markers with `[N/13]` where N stays the same for step 1, and all subsequent steps shift by +1. The new step 2 is attestation verification.

Change line 92:
```bash
echo "[1/12] Preflight checks..."
```
to:
```bash
echo "[1/13] Preflight checks..."
```

- [ ] **Step 4: Add attestation verification as new step 2**

After step 1 (the `echo "  ✓ clean tree, tag v$VERSION available"` line, currently line 105), insert:

```bash

# ─── Step 2: Verify attestation stamps ──────────────────────────────────────
echo "[2/13] Verifying attestations..."

ATTESTATION_DIR="$REPO_ROOT/.abox-attestations"
HEAD_SHA=$(git rev-parse --short HEAD)

if [[ "$SKIP_ATTESTATION" == "1" ]]; then
    echo ""
    echo "  ██████████████████████████████████████████████████████████████"
    echo "  ██  WARNING: --skip-attestation — skipping stamp checks    ██"
    echo "  ██  This should ONLY be used in emergencies.               ██"
    echo "  ██  Run 'just pre-release' before releasing normally.      ██"
    echo "  ██████████████████████████████████████████████████████████████"
    echo ""
else
    ATTEST_FAIL=0
    for stamp_name in vm bench smoke; do
        STAMP_FILE="$ATTESTATION_DIR/${stamp_name}.json"
        if [[ ! -f "$STAMP_FILE" ]]; then
            echo "  ✗ ${stamp_name}: stamp not found at $STAMP_FILE" >&2
            ATTEST_FAIL=1
            continue
        fi
        STAMP_SHA=$(python3 -c "import json; print(json.load(open('$STAMP_FILE'))['git_sha'])")
        STAMP_DIRTY=$(python3 -c "import json; print(json.load(open('$STAMP_FILE'))['git_dirty'])")
        STAMP_RESULT=$(python3 -c "import json; print(json.load(open('$STAMP_FILE'))['result'])")
        STAMP_TS=$(python3 -c "import json; print(json.load(open('$STAMP_FILE'))['timestamp'])")

        if [[ "$STAMP_SHA" != "$HEAD_SHA" ]]; then
            echo "  ✗ ${stamp_name}: stamp sha=$STAMP_SHA does not match HEAD=$HEAD_SHA" >&2
            ATTEST_FAIL=1
        elif [[ "$STAMP_DIRTY" == "True" ]]; then
            echo "  ✗ ${stamp_name}: stamp was generated on a dirty tree" >&2
            ATTEST_FAIL=1
        elif [[ "$STAMP_RESULT" != "pass" ]]; then
            echo "  ✗ ${stamp_name}: stamp result='$STAMP_RESULT' (expected 'pass')" >&2
            ATTEST_FAIL=1
        else
            echo "  ✓ ${stamp_name}: sha=$STAMP_SHA, passed $STAMP_TS"
        fi
    done

    if [[ "$ATTEST_FAIL" != "0" ]]; then
        echo "" >&2
        echo "ERROR: attestation stamps are missing or invalid." >&2
        echo "Run 'just pre-release' to generate them, then re-run this script." >&2
        echo "Use --skip-attestation to bypass (emergency use only)." >&2
        exit 1
    fi
fi
```

- [ ] **Step 5: Remove steps 3 and 4 (quality checks and e2e tests)**

Delete the old step 3 (quality checks, lines 115-120) and step 4 (e2e tests, lines 122-126). These are now covered by attestation stamps.

Replace them with a comment:

```bash
# Steps 3-4 (quality checks + e2e tests) are now covered by attestation
# stamps verified in step 2. See 'just pre-release'.
```

- [ ] **Step 6: Remove step 6 (VM benchmarks) and replace with stamp-based data**

Delete the old step 6 (VM benchmarks, lines 134-143). Replace with:

```bash
# ─── Step 5: Read benchmark data from attestation stamp ──────────────────────
echo "[5/13] Reading benchmark data..."

BENCH_JSON=""
if [[ "$SKIP_ATTESTATION" == "0" ]] && [[ -f "$ATTESTATION_DIR/bench.json" ]]; then
    BENCH_JSON=$(python3 -c "
import json
stamp = json.load(open('$ATTESTATION_DIR/bench.json'))
bench = stamp.get('bench_results')
if bench:
    print(json.dumps(bench))
else:
    print('')
")
fi

if [[ -n "$BENCH_JSON" ]]; then
    echo "  ✓ benchmark data loaded from attestation stamp"
else
    echo "  ⊘ no benchmark data in attestation stamp"
fi
```

- [ ] **Step 7: Renumber all remaining steps**

After the above changes, renumber all steps to be sequential 1-13:

1. Preflight checks
2. Verify attestations
3. Bump version
4. Release build
5. Read benchmark data from attestation stamp
6. Update README benchmark table (still runs criterion inline for the table)
7. Save benchmark JSON
8. Generate changelog entry
9. Install release binary
10. Commit
11. Tag

Actually, this reduces to 11 steps since we removed 2 steps (old 3, 4) and added 1 (new attestation). Renumber to `[N/11]` throughout. Update all step markers accordingly.

The step that runs criterion benchmarks for the README table (old step 7, the `CRITERION_OUT` section) still needs to run inline because criterion results aren't stored in the attestation stamp. Keep this step but renumber it.

- [ ] **Step 8: Update the help text step list**

Update the `Steps performed:` section in the help text to match the new flow:

```
Steps performed:
  1. Validate version format and working tree cleanliness
  2. Verify pre-release attestation stamps (vm, bench, smoke)
  3. Bump workspace version in Cargo.toml + Cargo.lock
  4. cargo build --release
  5. Read VM benchmark data from attestation stamp
  6. Run criterion microbenchmarks + update benchmark table in README.md
  7. Save full benchmark JSON to benchmarks/<version>.json
  8. Generate CHANGELOG.md entry from git log since last tag
  9. cargo install --path crates/abox-cli (refresh local binary)
  10. Commit version bump + benchmarks + changelog
  11. Tag v<version>
```

- [ ] **Step 9: Update e2e_test.sh path in release.sh if still referenced**

Search `release.sh` for any remaining references to `scripts/e2e_test.sh` and update to `scripts/local/e2e_test.sh`. (After step 5, these references should already be removed, but verify.)

```bash
grep -n 'scripts/e2e_test\|scripts/bench\|scripts/agent_smoke' scripts/release.sh
```

- [ ] **Step 10: Verify the modified release.sh parses correctly**

```bash
./scripts/release.sh --help
```

Expected: help text shows the new step list and `--skip-attestation` flag.

- [ ] **Step 11: Commit release.sh changes**

```bash
git add scripts/release.sh
git commit -m "refactor: release.sh verifies attestation stamps instead of running tests

- New step 2: verify .abox-attestations/{vm,bench,smoke}.json stamps
- Removed inline quality checks and e2e tests (covered by stamps)
- Reads benchmark data from attestation stamp instead of running bench.sh
- Added --skip-attestation escape hatch for emergencies
- Criterion microbenchmarks still run inline for README table"
```

---

### Task 6: Test robustness improvements

**Files:**
- Modify: `scripts/local/agent_smoke_test.sh`
- Modify: `scripts/local/e2e_test.sh`

- [ ] **Step 1: Add retry-once for Codex tests in `agent_smoke_test.sh`**

In `scripts/local/agent_smoke_test.sh`, after line 30 (`FILTER="${1:-all}"`), add a retry helper function:

```bash
# Retry a command once after a 5-second pause. Used for Codex tests
# where LLM output parsing is inherently nondeterministic.
retry_once() {
    "$@" && return 0
    echo "  retrying in 5s..."
    sleep 5
    "$@"
}
```

- [ ] **Step 2: Tighten Codex test timeouts from 90s to 60s**

In the Codex C1 test (around line 159), change:

```bash
    TIMEOUT=60 LOG=$(run_sandbox c1-smoke /bin/sh -c \
```

This is already 60s. Verify C2 is also set appropriately. The default `TIMEOUT` for `run_sandbox` is 90s (line 47: `timeout "${TIMEOUT:-90}"`). Add `TIMEOUT=60` prefix to the C2 test if it doesn't have one:

Find the C2 test line and ensure it has:

```bash
    TIMEOUT=60 LOG=$(run_sandbox c2-tool /bin/sh -c \
```

- [ ] **Step 3: Wrap Codex C1 and C2 assertions in retry_once**

For the C1 test, wrap the assertion block. The current pattern is:

```bash
    if grep -P '...' "$LOG" >/dev/null 2>&1 || \
       grep -v "..." "$LOG" | grep -q "6"; then
        pass "C1: single-turn smoke"
    else
        fail "C1: single-turn smoke" "see $LOG"
    fi
```

Refactor to extract the assertion into a function and wrap with retry:

```bash
    c1_check() {
        TIMEOUT=60 LOG=$(run_sandbox c1-smoke /bin/sh -c \
            'cd /workspace && codex exec --full-auto "What is 3+3? Answer with just the number." 2>&1')
        grep -v "INFO\|WARN\|ERROR\|virtiofsd\|cloud-hyper\|socat\|abox\|Debug\|Sandbox\|tokens\|Reconnect\|bubblewrap\|gitdir\|session\|OpenAI\|workdir\|model:\|provider:\|approval:\|sandbox:\|reasoning\|user$" "$LOG" | grep -q "6"
    }
    if c1_check; then
        pass "C1: single-turn smoke"
    else
        echo "  retrying C1 in 5s..."
        sleep 5
        if c1_check; then
            pass "C1: single-turn smoke (retry)"
        else
            fail "C1: single-turn smoke" "see $LOG"
        fi
    fi
```

Apply the same retry pattern to C2.

- [ ] **Step 4: Tighten VM timeout in e2e_test.sh from 90s to 30s**

In `scripts/local/e2e_test.sh`, replace all `timeout 90` calls in phases 6-7 with `timeout 30`:

```bash
cd /home/al/git/bakudo-abox/abox
grep -n 'timeout 90' scripts/local/e2e_test.sh
```

For each match, change `timeout 90` to `timeout 30`.

- [ ] **Step 5: Add `inotifywait` optimization for proxyd socket wait**

In `scripts/local/e2e_test.sh`, around lines 306-309, the socket polling loop is:

```bash
for _ in $(seq 1 40); do
    [[ -S "$SOCK" ]] && break
    sleep 0.05
done
```

Replace with:

```bash
if command -v inotifywait >/dev/null 2>&1; then
    inotifywait -qq -t 2 -e create "$(dirname "$SOCK")" 2>/dev/null &
    INOTIFY_PID=$!
    # Check if socket already appeared before inotifywait started.
    if [[ -S "$SOCK" ]]; then
        kill "$INOTIFY_PID" 2>/dev/null || true
        wait "$INOTIFY_PID" 2>/dev/null || true
    else
        wait "$INOTIFY_PID" 2>/dev/null || true
    fi
else
    # Fallback: poll for socket (max ~2s).
    for _ in $(seq 1 40); do
        [[ -S "$SOCK" ]] && break
        sleep 0.05
    done
fi
```

- [ ] **Step 6: Verify e2e tests still pass**

```bash
just tier-ci
```

Expected: all tests pass (the e2e changes are cosmetic for non-VM environments, and the robustness changes should not affect behavior on your dev box).

- [ ] **Step 7: Commit robustness improvements**

```bash
git add scripts/local/agent_smoke_test.sh scripts/local/e2e_test.sh
git commit -m "fix: improve test robustness

- agent_smoke_test.sh: retry-once for Codex tests (LLM output is nondeterministic)
- agent_smoke_test.sh: tighten Codex test timeout to 60s
- e2e_test.sh: tighten VM operation timeout from 90s to 30s
- e2e_test.sh: use inotifywait for proxyd socket readiness when available"
```

---

### Task 7: Update `docs/contributing/pre-pr-checklist.md`

This is the canonical source of truth. Update it first, then update the files that reference it.

**Files:**
- Modify: `docs/contributing/pre-pr-checklist.md`

- [ ] **Step 1: Update the e2e script path in the Always section**

Change:

```markdown
- [ ] `./scripts/e2e_test.sh` passes phases 1–5 locally.
```

to:

```markdown
- [ ] `just tier-ci` passes (equivalent: `just check` + `just deny`).
- [ ] `./scripts/local/e2e_test.sh` passes phases 1–5 locally (or run `just tier-vm` for all phases if you have KVM).
```

Wait — the original has `just check` and `just deny` as separate items plus `./scripts/e2e_test.sh`. Consolidate to:

Replace lines 8-10:

```markdown
- [ ] `just check` passes (fmt + clippy + test).
- [ ] `just deny` passes (supply-chain audit).
- [ ] `./scripts/e2e_test.sh` passes phases 1–5 locally.
```

with:

```markdown
- [ ] `just tier-ci` passes (fmt + clippy + test + supply-chain audit).
- [ ] `./scripts/local/e2e_test.sh` passes phases 1–5 locally (or run `just tier-vm` if you have KVM + bootstrapped VM for all phases).
```

- [ ] **Step 2: Update the VM/guest/proxy section script paths**

Change:

```markdown
- [ ] `just e2e-vm` passes locally (phases 6–7; requires bootstrapped VM and `/dev/kvm`).
```

This stays the same since `just e2e-vm` is still a valid recipe (it just calls `scripts/local/e2e_test.sh` now). No change needed here.

- [ ] **Step 3: Add a note about the release process at the bottom**

Before the "After the checklist" section, add:

```markdown
## Before a release

The PR-level gates above are necessary but not sufficient for releasing. Before running `just release <version>`, run:

```bash
just pre-release
```

This runs all four test tiers (ci, vm, bench, smoke) that the host supports, compares benchmarks against the previous release baseline, and writes attestation stamps to `.abox-attestations/`. `release.sh` verifies these stamps before proceeding. See the [pre-release validation spec](../superpowers/specs/2026-04-18-pre-release-validation-design.md) for details.
```

- [ ] **Step 4: Update the tooling-change meta-rule**

In the Always section, the line about tooling changes currently says:

```markdown
- [ ] If you changed a `just` recipe, a CI workflow, or a release step → the matching update to `AGENTS.md` and any relevant skill in `.claude/skills/` lands in **the same PR**.
```

Update to:

```markdown
- [ ] If you changed a `just` recipe, a CI workflow, `scripts/release.sh`, or `scripts/pre_release.sh` → the matching update to `AGENTS.md` and any relevant skill in `.claude/skills/` lands in **the same PR**.
```

- [ ] **Step 5: Commit**

```bash
git add docs/contributing/pre-pr-checklist.md
git commit -m "docs: update pre-pr-checklist with tier vocabulary and release process

- Replace check/deny/e2e items with tier-ci reference
- Update e2e script path to scripts/local/
- Add 'Before a release' section documenting just pre-release
- Include pre_release.sh in the tooling-change meta-rule"
```

---

### Task 8: Update `AGENTS.md`

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Update the "Before Opening a PR" section**

Replace lines 61-65:

```markdown
Key gates at a glance:

- `just check` and `just deny` pass.
- `scripts/e2e_test.sh` phases 1–5 pass locally.
- If the diff touches VM/guest/proxy code (see the checklist for the exact path list), `just e2e-vm` passes and the PR carries the `vm-attested` label.
```

with:

```markdown
Key gates at a glance:

- `just tier-ci` passes (fmt + clippy + test + supply-chain audit).
- `scripts/local/e2e_test.sh` phases 1–5 pass locally (or `just tier-vm` for all phases with KVM).
- If the diff touches VM/guest/proxy code (see the checklist for the exact path list), `just e2e-vm` passes and the PR carries the `vm-attested` label.
```

- [ ] **Step 2: Update the tooling-change meta-rule**

In the "When You Change `just`, CI, or Release Steps" section, update lines 69-75 to also mention `scripts/pre_release.sh`:

```markdown
### When You Change `just`, CI, or Release Steps

Tooling changes ship with their documentation update **in the same PR**. If you:

- Add or modify a recipe in `justfile` →
- Add or modify a workflow under `.github/workflows/` →
- Change a step in `scripts/release.sh` or `scripts/pre_release.sh` →

…then the same PR must update `AGENTS.md` and any affected skill in `.claude/skills/`. The pre-PR checklist and an advisory CI reminder both flag this; the PR is not complete until the docs reflect the new reality. This keeps AI assistants (Copilot, Codex, Claude Code) from steering future contributors toward stale commands.
```

- [ ] **Step 3: Add a Test Tiers section**

After the "Testing" section (after line 113, the test categories), add:

```markdown
### Test Tiers

Tests are organized into four tiers by their requirements:

| Tier | Recipe | What Runs | Requires |
|------|--------|-----------|----------|
| 1 | `just tier-ci` | fmt + clippy + test + cargo deny | Nothing special |
| 2 | `just tier-vm` | e2e_test.sh (all phases) + rootfs freshness check | `/dev/kvm` + bootstrapped VM |
| 3 | `just tier-bench` | criterion + VM latency benchmarks (5 runs) | `/dev/kvm` + bootstrapped VM |
| 4 | `just tier-smoke` | real Claude/Codex API calls through MITM proxy | KVM + real OAuth credentials |

**CI-safe vs local-only:** Scripts in `scripts/ci/` are safe for GitHub Actions. Scripts in `scripts/local/` require KVM, VM artifacts, or credentials — never wire them into CI workflows.

**Before a release:** Run `just pre-release`. It detects host capabilities, runs all applicable tiers, compares benchmarks against the previous release, and writes attestation stamps. `release.sh` verifies these stamps before tagging.

**During development:** Run individual tiers as needed — `just tier-ci` after any code change, `just tier-vm` after VM/guest changes.
```

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md
git commit -m "docs: update AGENTS.md with test tier system and pre-release workflow

- Replace check/deny/e2e references with tier-ci
- Add Test Tiers section documenting all four tiers
- Include pre_release.sh in tooling-change meta-rule"
```

---

### Task 9: Update `.claude/skills/release-preparation.md`

**Files:**
- Modify: `.claude/skills/release-preparation.md`

- [ ] **Step 1: Update the skill description**

Change the frontmatter description to mention pre-release:

```yaml
---
name: release-preparation
description: Use when the user asks to cut a release, run release.sh, or tag a new version (e.g. "release v0.5.0"). Ensures pre-release attestation stamps exist, then walks release.sh.
---
```

- [ ] **Step 2: Update preconditions**

Replace the "Preconditions" section with:

```markdown
## Preconditions (check before running)

- `git status` shows a clean working tree on `main` (up to date with `origin/main`).
- **`just pre-release` has been run** and all attestation stamps in `.abox-attestations/` match HEAD. If stamps are missing or stale, run `just pre-release` first.
- The version number follows SemVer: `v<major>.<minor>.<patch>`. The leading `v` is optional; `release.sh` normalizes.

If any precondition fails, stop and report. Do not use `--skip-attestation` unless the user explicitly requests it for an emergency.
```

- [ ] **Step 3: Update the step summary**

Replace the "What the script does" section with:

```markdown
## What the script does (summary; see `scripts/release.sh --help` for the definitive list)

1. Preflight (clean tree, version validity).
2. Verify attestation stamps (vm, bench, smoke must match HEAD and pass).
3. Bump `Cargo.toml` + `Cargo.lock`.
4. Build `--release`.
5. Read benchmark data from attestation stamp.
6. Run criterion microbenchmarks + update benchmark table in `README.md`.
7. Save benchmark JSON to `benchmarks/<version>.json`.
8. Generate `CHANGELOG.md` entry from `git log` since last tag.
9. `cargo install --path` locally so the developer has the new binary.
10. `git commit` version bump + benchmarks + changelog.
11. `git tag v<version>` (no push).

After step 11, the developer pushes manually: `git push origin main --tags`. The tag push triggers the `release.yml` GitHub Actions workflow.
```

- [ ] **Step 4: Update the "Do not" section**

Replace with:

```markdown
## Do not

- Edit `CHANGELOG.md` by hand as part of release prep. The script generates it from commit messages.
- Skip `just pre-release`. The attestation stamps are the proof that all tiers passed.
- Use `--skip-attestation` routinely. It exists for emergencies only.
- Push the tag before `release.sh` has committed the version bump + benchmarks.
```

- [ ] **Step 5: Commit**

```bash
git add .claude/skills/release-preparation.md
git commit -m "docs: update release-preparation skill for attestation workflow

- Pre-release attestation is now a precondition
- Updated step summary to match new 11-step release.sh
- Added --skip-attestation to Do Not section"
```

---

### Task 10: Update `.claude/skills/pre-pr-checklist.md`

**Files:**
- Modify: `.claude/skills/pre-pr-checklist.md`

- [ ] **Step 1: Update the "Always gates" commands**

In the "Run the Always gates" section, replace:

```markdown
```bash
just check
just deny
./scripts/e2e_test.sh
```

`just check` = `fmt-check + lint + test`. `just deny` = `cargo deny check`. The e2e script runs phases 1–5 on any host (no VM needed). If any of these fail, stop and fix. Do not report success until they are green.
```

with:

```markdown
```bash
just tier-ci
./scripts/local/e2e_test.sh
```

`just tier-ci` = `fmt-check + lint + test + cargo deny check`. The e2e script runs phases 1–5 on any host (no VM needed). If any of these fail, stop and fix. Do not report success until they are green.
```

- [ ] **Step 2: Update the VM attestation e2e command**

In section 3, the instruction says:

```markdown
- Run `just e2e-vm` and wait for completion. This needs a bootstrapped VM and `/dev/kvm`.
```

This is still correct — `just e2e-vm` is still a valid recipe. No change needed.

- [ ] **Step 3: Add a note distinguishing PR gates from release gates**

After section 3 (VM attestation), before section 4 (documentation), add:

```markdown
### 3b. Note on release vs PR gates

The above gates are for PRs. For releases, the full pre-release validation (`just pre-release`) must pass — this includes benchmarks and agent smoke tests. See the `release-preparation` skill.
```

- [ ] **Step 4: Update the meta-rule**

In section 5, update the list of files that trigger doc updates:

```markdown
If the diff touches `justfile`, `.github/workflows/**`, `scripts/release.sh`, or `scripts/pre_release.sh`, verify that **this same PR** also updates `AGENTS.md` and any affected skill under `.claude/skills/`. If not, refuse to mark the PR ready; add the missing updates first.
```

- [ ] **Step 5: Commit**

```bash
git add .claude/skills/pre-pr-checklist.md
git commit -m "docs: update pre-pr-checklist skill with tier vocabulary

- Replace check/deny/e2e with tier-ci + local/e2e_test.sh
- Add note distinguishing PR gates from release gates
- Include pre_release.sh in tooling-change meta-rule"
```

---

### Task 11: End-to-end verification

- [ ] **Step 1: Verify `just --list` shows all new recipes**

```bash
cd /home/al/git/bakudo-abox/abox
just --list
```

Expected: `pre-release`, `tier-ci`, `tier-vm`, `tier-bench`, `tier-smoke` all appear alongside existing recipes.

- [ ] **Step 2: Run `just tier-ci` to verify it works**

```bash
just tier-ci
```

Expected: fmt + clippy + test + deny all pass.

- [ ] **Step 3: Run `just pre-release` end-to-end**

```bash
just pre-release
```

Expected: capability detection runs, all applicable tiers execute, benchmark comparison prints (if VM is available), attestation stamps are written to `.abox-attestations/`, summary shows results.

- [ ] **Step 4: Verify attestation stamps were written**

```bash
ls -la .abox-attestations/
cat .abox-attestations/vm.json
cat .abox-attestations/bench.json
cat .abox-attestations/smoke.json
```

Expected: each stamp has the current HEAD SHA, `git_dirty: false`, `result: "pass"`. The bench stamp includes comparison data.

- [ ] **Step 5: Verify `release.sh --help` shows the updated flow**

```bash
./scripts/release.sh --help
```

Expected: shows the 11-step flow with attestation verification at step 2 and `--skip-attestation` in the flags.

- [ ] **Step 6: Verify `release.sh` dry run catches stale stamps**

```bash
# Make a trivial change to invalidate stamps
echo "" >> README.md
git add README.md && git commit -m "test: trivial change to test stale stamp detection"

./scripts/release.sh 99.99.99 --dry
```

Expected: step 2 fails with "stamp sha=... does not match HEAD=..." error. Then undo the test commit:

```bash
git reset --soft HEAD~1
git checkout README.md
```

- [ ] **Step 7: Verify CI workflow still references the correct path**

```bash
grep 'e2e_test' .github/workflows/ci.yml
```

Expected: `./scripts/local/e2e_test.sh` (not the old path).

- [ ] **Step 8: Check for any remaining references to old script paths**

```bash
grep -rn 'scripts/e2e_test\.sh\|scripts/agent_smoke_test\.sh\|scripts/bench\.sh' \
  --include='*.sh' --include='*.yml' --include='*.yaml' --include='*.md' --include='*.toml' \
  --include='justfile' \
  | grep -v 'scripts/local/' | grep -v 'CHANGELOG' | grep -v 'docs/superpowers/'
```

Expected: no matches (all references updated to `scripts/local/` paths). If any remain, update them.
