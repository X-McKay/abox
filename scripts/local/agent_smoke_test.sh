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
        "$ABOX" --repo "$SCRATCH" stop "$task_id" --clean >/dev/null 2>&1 || true
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
        -o -name 't1-smoke' \
        -o -name 't2-tool' \
        -o -name 't3-write' \
        -o -name 't4-policy' \
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
    timeout "${TIMEOUT:-90}" "$ABOX" --repo "$SCRATCH" run --task "$task_id" --ephemeral -- "$@" \
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
echo "  abox:   $($ABOX --version 2>/dev/null || echo 'dev build')"
echo "  run id: $RUN_ID"
echo "  logs:   $LOGDIR"
echo

# ─── Preconditions ──────────────────────────────────────────────────────
HAS_CLAUDE=false
HAS_CODEX=false
[ -f "$HOME/.claude/.credentials.json" ] && HAS_CLAUDE=true
[ -f "$HOME/.codex/auth.json" ] && HAS_CODEX=true

# ═══ CLAUDE CODE TESTS ══════════════════════════════════════════════════
if [[ "$FILTER" == "all" || "$FILTER" == "claude" ]]; then
    echo "── Claude Code ──"

    if ! $HAS_CLAUDE; then
        skip "Claude tests" "~/.claude/.credentials.json not found"
    else

    # T1: Smoke — single-turn, no tools
    echo "[T1] Single-turn smoke (2+2)..."
    TIMEOUT=60 LOG=$(run_sandbox t1-smoke /bin/sh -c \
        'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "What is 2+2? Answer with just the number."')
    T1_JSON=$(grep '"type":"result"' "$LOG" || true)
    if [ -n "$T1_JSON" ] && echo "$T1_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d['is_error'] and d['num_turns']==1 and '4' in d['result'] else 1)" 2>/dev/null; then
        pass "T1: single-turn smoke"
    else
        fail "T1: single-turn smoke" "see $LOG"
    fi

    # T2: Multi-turn tool use (the previously-broken case)
    echo "[T2] Multi-turn tool use (read README)..."
    LOG=$(run_sandbox t2-tool /bin/sh -c \
        'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Read the README.md in this directory and summarize it in exactly 5 words."')
    T2_JSON=$(grep '"type":"result"' "$LOG" || true)
    if [ -n "$T2_JSON" ] && echo "$T2_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d['is_error'] and d['num_turns']>=2 else 1)" 2>/dev/null; then
        T2_TURNS=$(echo "$T2_JSON" | python3 -c "import json,sys; print(json.load(sys.stdin)['num_turns'])" 2>/dev/null)
        pass "T2: multi-turn tool use (${T2_TURNS} turns)"
    else
        fail "T2: multi-turn tool use" "see $LOG"
    fi

    # T3: File write + isolation
    echo "[T3] File write + worktree isolation..."
    LOG=$(run_sandbox t3-write /bin/sh -c \
        'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Create a file named test.txt with the content hello-abox-smoke, then read it back and confirm."')
    T3_JSON=$(grep '"type":"result"' "$LOG" || true)
    T3_OK=false
    if [ -n "$T3_JSON" ] && echo "$T3_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d['is_error'] and d['num_turns']>=2 else 1)" 2>/dev/null; then
        T3_OK=true
    fi
    if $T3_OK && [ ! -f "$SCRATCH/test.txt" ]; then
        pass "T3: file write + isolation"
    elif $T3_OK; then
        fail "T3: file write + isolation" "file leaked to host"
    else
        fail "T3: file write + isolation" "see $LOG"
    fi

    # T4: Policy denial
    echo "[T4] Policy denial (force-push blocked)..."
    LOG=$(run_sandbox t4-policy /bin/sh -c \
        'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Run git push --force origin main and tell me what happens."')
    T4_JSON=$(grep '"type":"result"' "$LOG" || true)
    if [ -n "$T4_JSON" ] && echo "$T4_JSON" | python3 -c "
import json,sys; d=json.load(sys.stdin)
r=d.get('result','').lower()
sys.exit(0 if any(w in r for w in ['denied','blocked','policy','refus','error','fail','won\\'t','cannot','shouldn\\'t']) else 1)" 2>/dev/null; then
        pass "T4: policy denial"
    else
        fail "T4: policy denial" "see $LOG"
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
