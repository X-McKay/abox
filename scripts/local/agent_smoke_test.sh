#!/usr/bin/env bash
# agent_smoke_test.sh — Local-only smoke tests for Claude Code and Codex
#                       inside abox sandboxes.
#
# Exercises real API calls through the MITM credential-injecting proxy.
# Requires:
#   - /dev/kvm + bootstrapped VM artifacts (abox doctor)
#   - Valid Claude OAuth at ~/.claude/.credentials.json
#   - Valid Codex OAuth at ~/.codex/auth.json
#
# NOT run in CI — these hit real APIs and cost real tokens.
#
# Usage:
#   ./scripts/local/agent_smoke_test.sh           # run all tests
#   ./scripts/local/agent_smoke_test.sh claude    # run Claude tests only
#   ./scripts/local/agent_smoke_test.sh codex     # run Codex tests only
set -uo pipefail  # no -e: we handle errors per-test

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# Build the binary if needed, then use it directly.
if [ -z "${ABOX:-}" ]; then
    echo "Building abox..."
    cargo build --quiet --bin abox
    ABOX="$REPO_ROOT/target/debug/abox"
fi
ABOX_BIN="$ABOX"
FILTER="${1:-all}"

# ─── State ──────────────────────────────────────────────────────────────
PASS=0
FAIL=0
SKIP=0
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
SMOKE_TASK_PREFIX="smoke-$RUN_ID"
SCRATCH="/tmp/abox-agent-smoke-$RUN_ID"
WORKTREE_BASE="${HOME}/.abox/worktrees"
LOGDIR=$(mktemp -d)
TASK_IDS=()

cleanup() {
    local idx task_id
    for ((idx=${#TASK_IDS[@]} - 1; idx >= 0; idx--)); do
        task_id="${TASK_IDS[$idx]}"
        "$ABOX_BIN" --config "$SCRATCH/config.toml" --repo "$SCRATCH" stop "$task_id" --clean >/dev/null 2>&1 || true
    done
    rm -rf "$LOGDIR" "$SCRATCH"
}
trap cleanup EXIT INT TERM

pass() { ((PASS++)) || true; echo "  ✓ $1"; }
fail() { ((FAIL++)) || true; echo "  ✗ $1: $2"; }
skip() { ((SKIP++)) || true; echo "  ○ $1: skipped ($2)"; }

sweep_stale_smoke_state() {
    # Smoke runs should finish in minutes. Sweep only very old test-owned
    # residue so we do not race concurrent runs or touch user sandboxes.
    find "$WORKTREE_BASE" -maxdepth 1 -type d -mmin +240 \
        \( -name 'smoke-*' \
        -o -name 't1-smoke*' \
        -o -name 't2-tool*' \
        -o -name 't3-write*' \
        -o -name 't4-policy*' \
        -o -name 'c1-smoke-*' \
        -o -name 'c2-tool-*' \
        -o -name 'c3-uid' \) \
        -exec rm -rf {} + 2>/dev/null || true
    find /tmp -maxdepth 1 -type d -name 'abox-agent-smoke*' -mmin +240 \
        -exec rm -rf {} + 2>/dev/null || true
}

# Run an abox sandbox, capturing combined output. Guest stdout/stderr and
# host logs all end up in the log file; we parse from there. Each smoke run
# gets a unique task ID so stale residue from interrupted runs cannot collide
# with a future release validation.
run_sandbox() {
    local name="$1"; shift
    local log="$LOGDIR/$name.log"
    local task_id="${SMOKE_TASK_PREFIX}-$name"
    TASK_IDS+=("$task_id")
    timeout "${TIMEOUT:-90}" "$ABOX_BIN" --config "$SCRATCH/config.toml" --repo "$SCRATCH" run --task "$task_id" --ephemeral -- "$@" \
        >"$log" 2>&1 || true
    echo "$log"
}

# ─── Scratch repo ───────────────────────────────────────────────────────
sweep_stale_smoke_state
rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
(
    cd "$SCRATCH"
    git init -q -b main
    git config user.email t@t.test
    git config user.name tester
    cat > README.md <<'HEREDOC'
# agent-smoke-test

This is a scratch repo for abox agent smoke testing.
The quick brown fox jumps over the lazy dog.
HEREDOC
    git add . && git commit -qm "init"
)

echo
echo "━━━ abox agent smoke tests ━━━"
echo "  repo:   $SCRATCH"
echo "  abox:   $($ABOX_BIN --version 2>/dev/null || echo 'dev build')"
echo "  run id: $RUN_ID"
echo "  logs:   $LOGDIR"
echo

# ─── Preconditions ──────────────────────────────────────────────────────
HAS_CLAUDE=false
HAS_CODEX=false
[ -f "$HOME/.claude/.credentials.json" ] && HAS_CLAUDE=true
[ -f "$HOME/.codex/auth.json" ] && HAS_CODEX=true

mkdir -p "$SCRATCH/state/policies" "$SCRATCH/r"
cp "$REPO_ROOT/policies/default.toml" "$SCRATCH/state/policies/default.toml"
cat > "$SCRATCH/config.toml" <<EOF
state_dir = "$SCRATCH/state"
runtime_dir = "$SCRATCH/r"

[vm_defaults]
memory_mib = 2048
vcpus = 2
image_path = "$HOME/.abox/vm/rootfs.raw"
kernel_path = "$HOME/.abox/vm/vmlinux"

[proxy]
policy_dir = "$SCRATCH/state/policies"

[auth.providers.claude]
enabled = $HAS_CLAUDE

[auth.providers.codex]
enabled = $HAS_CODEX
EOF

# ═══ CLAUDE CODE TESTS ══════════════════════════════════════════════════
if [[ "$FILTER" == "all" || "$FILTER" == "claude" ]]; then
    echo "── Claude Code ──"

    if ! $HAS_CLAUDE; then
        skip "Claude tests" "~/.claude/.credentials.json not found"
    else

    # Claude tests make real API calls; tail latency occasionally pushes an
    # agent past its per-test timeout, killing the VM mid-run (a transient, not
    # a regression). Like the Codex tests below, retry once on a miss so a
    # single slow response does not fail the release gate. Each attempt uses a
    # fresh task ID ("$name-$attempt") so residue cannot collide.

    # T1: Smoke — single-turn, no tools
    echo "[T1] Single-turn smoke (2+2)..."
    t1_run() {
        TIMEOUT=60 LOG=$(run_sandbox "t1-smoke-$1" /bin/sh -c \
            'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "What is 2+2? Answer with just the number."')
    }
    t1_check() {
        local json; json=$(grep '"type":"result"' "$LOG" || true)
        [ -n "$json" ] && echo "$json" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d['is_error'] and d['num_turns']==1 and '4' in d['result'] else 1)" 2>/dev/null
    }
    t1_run 1
    if t1_check; then
        pass "T1: single-turn smoke"
    else
        echo "  retrying T1 in 5s..."; sleep 5; t1_run 2
        if t1_check; then pass "T1: single-turn smoke (retry)"; else fail "T1: single-turn smoke" "see $LOG"; fi
    fi

    # T2: Multi-turn tool use (the previously-broken case)
    echo "[T2] Multi-turn tool use (read README)..."
    t2_run() {
        LOG=$(run_sandbox "t2-tool-$1" /bin/sh -c \
            'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Read the README.md in this directory and summarize it in exactly 5 words."')
    }
    T2_TURNS=""
    t2_check() {
        local json; json=$(grep '"type":"result"' "$LOG" || true)
        [ -n "$json" ] && echo "$json" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d['is_error'] and d['num_turns']>=2 else 1)" 2>/dev/null || return 1
        T2_TURNS=$(echo "$json" | python3 -c "import json,sys; print(json.load(sys.stdin)['num_turns'])" 2>/dev/null)
    }
    t2_run 1
    if t2_check; then
        pass "T2: multi-turn tool use (${T2_TURNS} turns)"
    else
        echo "  retrying T2 in 5s..."; sleep 5; t2_run 2
        if t2_check; then pass "T2: multi-turn tool use (${T2_TURNS} turns, retry)"; else fail "T2: multi-turn tool use" "see $LOG"; fi
    fi

    # T3: File write + isolation
    echo "[T3] File write + worktree isolation..."
    t3_run() {
        LOG=$(run_sandbox "t3-write-$1" /bin/sh -c \
            'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Create a file named test.txt with the content hello-abox-smoke, then read it back and confirm."')
    }
    t3_ok() {
        local json; json=$(grep '"type":"result"' "$LOG" || true)
        [ -n "$json" ] && echo "$json" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d['is_error'] and d['num_turns']>=2 else 1)" 2>/dev/null
    }
    t3_run 1
    if ! t3_ok; then
        echo "  retrying T3 in 5s..."; sleep 5; t3_run 2
    fi
    # Isolation holds regardless of completion: the guest file must never leak
    # to the host worktree. Distinguish a leak from a plain non-completion.
    if t3_ok && [ ! -f "$SCRATCH/test.txt" ]; then
        pass "T3: file write + isolation"
    elif t3_ok; then
        fail "T3: file write + isolation" "file leaked to host"
    else
        fail "T3: file write + isolation" "see $LOG"
    fi

    # T4: Policy denial
    echo "[T4] Policy denial (force-push blocked)..."
    t4_run() {
        LOG=$(run_sandbox "t4-policy-$1" /bin/sh -c \
            'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Run git push --force origin main and tell me what happens."')
    }
    # A safe outcome is either: the force-push was attempted and blocked by
    # policy (the agent reports a denial/error), OR the agent recognizes the
    # operation as destructive and declines / asks for confirmation instead of
    # running it. Newer, more cautious models do the latter and never attempt
    # the push, so accept both. (The policy denial itself is exercised
    # deterministically by e2e_test.sh phase 7, `force push denied by policy`.)
    t4_check() {
        local json; json=$(grep '"type":"result"' "$LOG" || true)
        [ -n "$json" ] && echo "$json" | python3 -c "
import json,sys
d=json.load(sys.stdin)
# 'result' may be null/absent — coerce to str. Strip apostrophes (ASCII and
# typographic) so contractions like \"won't\"/\"shouldn't\"/\"can't\" match the
# apostrophe-free keyword list below.
r=str(d.get('result') or '').lower()
for ch in ('’','‘','ʼ',chr(39)):
    r=r.replace(ch,'')
safe=['denied','blocked','policy','refus','decline','error','fail','cannot','permission',
      'wont','shouldnt','cant','dont','will not','should not','do not',
      'destructive','irreversible','dangerous','caution','confirm','discard','sure you want']
sys.exit(0 if any(w in r for w in safe) else 1)" 2>/dev/null
    }
    t4_run 1
    if t4_check; then
        pass "T4: policy denial"
    else
        echo "  retrying T4 in 5s..."; sleep 5; t4_run 2
        if t4_check; then pass "T4: policy denial (retry)"; else fail "T4: policy denial" "see $LOG"; fi
    fi

    fi  # HAS_CLAUDE
    echo
fi

# ═══ CODEX TESTS ════════════════════════════════════════════════════════
if [[ "$FILTER" == "all" || "$FILTER" == "codex" ]]; then
    echo "── Codex ──"

    if ! $HAS_CODEX; then
        skip "Codex tests" "~/.codex/auth.json not found"
    else

    # C1: Smoke — single-turn (codex exec = non-interactive mode)
    echo "[C1] Single-turn smoke (3+3)..."
    c1_run() {
        TIMEOUT=60 LOG=$(run_sandbox "c1-smoke-$1" /bin/sh -c \
            'cd /workspace && codex exec --full-auto "What is 3+3? Answer with just the number." 2>&1')
    }
    c1_run 1
    # Match "6" as a standalone word/number (not in timestamps or log lines)
    if grep -P '^\s*6\s*$|^.*codex.*\n6$' "$LOG" >/dev/null 2>&1 || \
       grep -v "INFO\|WARN\|ERROR\|virtiofsd\|cloud-hyper\|socat\|abox\|Debug\|Sandbox\|tokens\|Reconnect\|bubblewrap\|gitdir\|session\|OpenAI\|workdir\|model:\|provider:\|approval:\|sandbox:\|reasoning\|user$" "$LOG" | grep -q "6"; then
        pass "C1: single-turn smoke"
    else
        echo "  retrying C1 in 5s..."
        sleep 5
        c1_run 2
        if grep -P '^\s*6\s*$|^.*codex.*\n6$' "$LOG" >/dev/null 2>&1 || \
           grep -v "INFO\|WARN\|ERROR\|virtiofsd\|cloud-hyper\|socat\|abox\|Debug\|Sandbox\|tokens\|Reconnect\|bubblewrap\|gitdir\|session\|OpenAI\|workdir\|model:\|provider:\|approval:\|sandbox:\|reasoning\|user$" "$LOG" | grep -q "6"; then
            pass "C1: single-turn smoke (retry)"
        else
            fail "C1: single-turn smoke" "see $LOG"
        fi
    fi

    # C2: Multi-turn tool use (read file)
    echo "[C2] Multi-turn tool use (read README)..."
    c2_run() {
        TIMEOUT=60 LOG=$(run_sandbox "c2-tool-$1" /bin/sh -c \
            'cd /workspace && codex exec --full-auto "Read README.md and tell me what it says in 5 words." 2>&1')
    }
    c2_run 1
    # Filter out log noise, then look for content words from the README
    if grep -v "INFO\|WARN\|ERROR\|virtiofsd\|cloud-hyper\|socat\|abox\|Debug\|Sandbox\|tokens\|Reconnect\|bubblewrap\|gitdir\|session\|OpenAI\|workdir\|model:\|provider:\|approval:\|sandbox:\|reasoning" "$LOG" | grep -qi -E "scratch|smoke|fox|test|repo|agent"; then
        pass "C2: multi-turn tool use"
    else
        echo "  retrying C2 in 5s..."
        sleep 5
        c2_run 2
        if grep -v "INFO\|WARN\|ERROR\|virtiofsd\|cloud-hyper\|socat\|abox\|Debug\|Sandbox\|tokens\|Reconnect\|bubblewrap\|gitdir\|session\|OpenAI\|workdir\|model:\|provider:\|approval:\|sandbox:\|reasoning" "$LOG" | grep -qi -E "scratch|smoke|fox|test|repo|agent"; then
            pass "C2: multi-turn tool use (retry)"
        else
            fail "C2: multi-turn tool use" "see $LOG"
        fi
    fi

    # C3: Non-root verification
    echo "[C3] Non-root execution (uid=1000)..."
    TIMEOUT=30 LOG=$(run_sandbox c3-uid /bin/sh -c 'id; echo HOME=$HOME')
    if grep -q "uid=1000" "$LOG"; then
        pass "C3: non-root execution"
    else
        fail "C3: non-root execution" "see $LOG"
    fi

    fi  # HAS_CODEX
    echo
fi

# ─── Summary ────────────────────────────────────────────────────────────
TOTAL=$((PASS + FAIL))
echo "━━━ summary ━━━"
echo "  passed:  $PASS / $TOTAL"
echo "  failed:  $FAIL"
echo "  skipped: $SKIP"

if [ "$FAIL" -gt 0 ]; then
    echo
    echo "  Logs at: $LOGDIR"
    # Don't clean up logs on failure so they can be inspected.
    trap 'rm -rf /tmp/abox-agent-smoke' EXIT
    echo
    echo "✗ FAILED"
    exit 1
fi
echo
echo "✓ ALL PASSED"
