#!/usr/bin/env bash
#
# abox end-to-end test script.
#
# This script exercises the parts of abox that do *not* require a host with
# KVM + cloud-hypervisor + virtiofsd installed: the workspace orchestrator
# (worktree create / list / divergence / merge / stop --clean), the credential
# proxy daemon (CLI policy enforcement, audit log), and the rollback path
# when VM start fails because the hypervisor is not available.
#
# It is self-contained: everything lives under a fresh sandbox directory
# inside this repo (.scratch/e2e-run-<pid>) and is removed on exit.
#
# Usage:  ./scripts/e2e_test.sh
#
# Exit code: 0 on success, non-zero on first failed assertion.

set -u
set -o pipefail

# ─── Output helpers ─────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
    YELLOW=$'\033[33m'; BLUE=$'\033[34m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; CYAN=""; RESET=""
fi

PASS_COUNT=0
FAIL_COUNT=0
CURRENT_TEST=""

section() {
    printf '\n%s━━━ %s ━━━%s\n' "$BOLD$BLUE" "$1" "$RESET"
}

step() {
    CURRENT_TEST="$1"
    printf '%s· %s%s\n' "$CYAN" "$1" "$RESET"
}

how() {
    printf '  %show:%s     %s\n' "$DIM" "$RESET" "$1"
}

expect() {
    printf '  %sexpect:%s  %s\n' "$DIM" "$RESET" "$1"
}

pass() {
    PASS_COUNT=$((PASS_COUNT + 1))
    printf '  %s✓ pass%s    %s\n' "$GREEN" "$RESET" "${1:-$CURRENT_TEST}"
}

fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    printf '  %s✗ FAIL%s    %s\n' "$RED" "$RESET" "${1:-$CURRENT_TEST}"
    if [[ -n "${2:-}" ]]; then
        printf '            %s%s%s\n' "$DIM" "$2" "$RESET"
    fi
}

# Assertions ----------------------------------------------------------------
assert_eq() {
    # assert_eq <label> <expected> <actual>
    if [[ "$2" == "$3" ]]; then
        pass "$1"
    else
        fail "$1" "expected='$2' actual='$3'"
    fi
}

assert_contains() {
    # assert_contains <label> <needle> <haystack>
    if [[ "$3" == *"$2"* ]]; then
        pass "$1"
    else
        fail "$1" "needle='$2' not found in output"
    fi
}

assert_not_contains() {
    if [[ "$3" != *"$2"* ]]; then
        pass "$1"
    else
        fail "$1" "needle='$2' should NOT be in output"
    fi
}

assert_file_exists() {
    if [[ -e "$2" ]]; then pass "$1"; else fail "$1" "missing: $2"; fi
}

assert_file_absent() {
    if [[ ! -e "$2" ]]; then pass "$1"; else fail "$1" "should be gone: $2"; fi
}

# ─── Setup ──────────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRATCH="$REPO_ROOT/.scratch/e2e-run-$$"
ABOX_BIN="$REPO_ROOT/target/debug/abox"
PROXYD_BIN="$REPO_ROOT/target/debug/abox-proxyd"

cleanup() {
    if [[ -n "${PROXYD_PID:-}" ]] && kill -0 "$PROXYD_PID" 2>/dev/null; then
        kill "$PROXYD_PID" 2>/dev/null || true
        wait "$PROXYD_PID" 2>/dev/null || true
    fi
    rm -rf "$SCRATCH"
}
trap cleanup EXIT

section "abox end-to-end test"
printf '%srepo:%s    %s\n' "$DIM" "$RESET" "$REPO_ROOT"
printf '%sscratch:%s %s\n' "$DIM" "$RESET" "$SCRATCH"

# ─── Build ──────────────────────────────────────────────────────────────────
section "phase 1 — build"
step "Compile workspace in debug mode"
how "cargo build --workspace"
expect "exit 0; abox and abox-proxyd binaries present"
if cargo build --workspace --quiet 2>&1; then
    pass "cargo build --workspace"
else
    fail "cargo build --workspace"
    exit 1
fi
assert_file_exists "abox binary present" "$ABOX_BIN"
assert_file_exists "abox-proxyd binary present" "$PROXYD_BIN"

# ─── Unit tests ─────────────────────────────────────────────────────────────
section "phase 2 — unit + integration tests"
step "Run cargo test --workspace"
how "cargo test --workspace 2>&1 | grep 'test result:'"
expect "every test result line shows 0 failed"
TEST_OUTPUT=$(cargo test --workspace 2>&1 || true)
echo "$TEST_OUTPUT" | grep "test result:" | sed "s/^/  ${DIM}│${RESET} /"
if echo "$TEST_OUTPUT" | grep -q "test result: FAILED"; then
    fail "cargo test --workspace"
else
    pass "cargo test --workspace (no failures)"
fi

# ─── Scratch repo + config ──────────────────────────────────────────────────
section "phase 3 — scratch git repo + abox config"
mkdir -p "$SCRATCH/repo" "$SCRATCH/state"

step "Initialize scratch git repo with main branch and one commit"
how "git init -b main; git commit ..."
expect "git log shows one commit"
(
    cd "$SCRATCH/repo"
    git init -q -b main
    git config user.email "e2e@abox.test"
    git config user.name "abox-e2e"
    echo "# scratch repo for abox e2e" > README.md
    git add README.md
    git commit -q -m "init"
)
INIT_LOG=$(cd "$SCRATCH/repo" && git log --oneline)
assert_contains "scratch repo has init commit" "init" "$INIT_LOG"

step "Write config that points runtime_dir into scratch (no /run/abox needed)"
how "write TOML at $SCRATCH/config.toml"
expect "abox CLI works as a non-root user"
# NOTE: runtime_dir is a *short* path ('$SCRATCH/r') because Cloud
# Hypervisor / virtiofsd unix sockets are capped at SUN_LEN (108 bytes)
# and the scratch dir path already eats 70+ characters before we append
# the per-sandbox socket suffix.
cat > "$SCRATCH/config.toml" <<EOF
state_dir = "$SCRATCH/state"
runtime_dir = "$SCRATCH/r"

[vm_defaults]
memory_mib = 512
vcpus = 1

[proxy]
egress_port = 28443
policy_dir = "$SCRATCH/state/policies"
EOF
mkdir -p "$SCRATCH/state/policies" "$SCRATCH/r"
cp "$REPO_ROOT/policies/default.toml" "$SCRATCH/state/policies/default.toml"
pass "config + default policy installed"

ABOX="$ABOX_BIN --config $SCRATCH/config.toml --repo $SCRATCH/repo"

# ─── CLI: list when empty ───────────────────────────────────────────────────
section "phase 4 — abox CLI workspace ops"

step "abox list with no sandboxes"
how "abox list"
expect "prints 'No active sandboxes.'"
OUT=$($ABOX list 2>&1)
assert_contains "list-empty output" "No active sandboxes" "$OUT"

# ─── CLI: run (expected to fail at VM start, must roll back) ────────────────
step "abox run --task fix-auth (no hypervisor; verifies rollback)"
how "abox run --task fix-auth --base main -- echo hi  (virtiofsd absent)"
expect "non-zero exit AND no leftover worktree (rollback worked)"
RUN_OUT=$($ABOX run --task fix-auth --base main -- echo hi 2>&1 || true)
if [[ -d "$SCRATCH/state/worktrees/fix-auth" ]]; then
    fail "rollback removes worktree" "leftover at $SCRATCH/state/worktrees/fix-auth"
    echo "$RUN_OUT" | sed "s/^/    /"
else
    pass "VM start failure rolled the worktree back"
fi
LIST_OUT=$($ABOX list 2>&1)
assert_contains "list still empty after rollback" "No active sandboxes" "$LIST_OUT"

# ─── CLI: simulate a real worktree by creating one directly via git ─────────
# We can't actually boot a VM in this environment, so simulate the post-boot
# state: a worktree on agent/<task> with a commit, then exercise list /
# divergence / merge / stop --clean against it.
step "Create an abox-managed worktree directly via git (simulates a booted sandbox)"
how "git -C repo worktree add state/worktrees/fix-auth -b agent/fix-auth"
expect "list shows fix-auth, divergence reports the new file"
git -C "$SCRATCH/repo" worktree add -q "$SCRATCH/state/worktrees/fix-auth" -b agent/fix-auth
(
    cd "$SCRATCH/state/worktrees/fix-auth"
    echo "fixed!" > auth.txt
    git add auth.txt
    git -c user.email=e2e@abox.test -c user.name=e2e commit -q -m "fix auth"
)
pass "simulated worktree created with one new commit"

step "abox list shows the simulated sandbox"
how "abox list"
expect "table contains 'fix-auth' and 'agent/fix-auth'"
LIST_OUT=$($ABOX list 2>&1)
echo "$LIST_OUT" | sed "s/^/    /"
assert_contains "list shows id" "fix-auth" "$LIST_OUT"
assert_contains "list shows branch" "agent/fix-auth" "$LIST_OUT"

step "abox divergence reports the new file"
how "abox divergence"
expect "shows auth.txt with status Added"
DIV_OUT=$($ABOX divergence 2>&1)
echo "$DIV_OUT" | sed "s/^/    /"
assert_contains "divergence file" "auth.txt" "$DIV_OUT"
assert_contains "divergence status" "Added" "$DIV_OUT"

step "abox merge fix-auth (should succeed cleanly)"
how "abox merge fix-auth"
expect "merge prints success and main contains the new commit"
MERGE_OUT=$($ABOX merge fix-auth 2>&1)
echo "$MERGE_OUT" | sed "s/^/    /"
assert_contains "merge success message" "Successfully merged" "$MERGE_OUT"
MAIN_LOG=$(cd "$SCRATCH/repo" && git log --oneline main)
assert_contains "main has merge commit" "fix auth" "$MAIN_LOG"

step "abox stop fix-auth --clean (VM never ran; must still clean worktree)"
how "abox stop fix-auth --clean"
expect "exit 0; worktree directory removed"
STOP_OUT=$($ABOX stop fix-auth --clean 2>&1)
echo "$STOP_OUT" | sed "s/^/    /"
assert_file_absent "worktree dir removed" "$SCRATCH/state/worktrees/fix-auth"
LIST_AFTER=$($ABOX list 2>&1)
assert_contains "list empty after clean" "No active sandboxes" "$LIST_AFTER"

# ─── Proxyd: policy enforcement + audit attribution ─────────────────────────
section "phase 5 — abox-proxyd CLI policy enforcement"

step "Start abox-proxyd with --config pointing at the scratch config"
how "abox-proxyd --config $SCRATCH/config.toml &"
expect "proxyd creates the cli-proxy.sock under runtime_dir"
ABOX_HOME_DEFAULT="$HOME/.abox"
"$PROXYD_BIN" --config "$SCRATCH/config.toml" >"$SCRATCH/proxyd.log" 2>&1 &
PROXYD_PID=$!
# Wait for socket to appear (max ~2s).
SOCK="$SCRATCH/r/cli-proxy.sock"
for _ in $(seq 1 40); do
    [[ -S "$SOCK" ]] && break
    sleep 0.05
done
assert_file_exists "cli-proxy.sock present" "$SOCK"
# Confirm proxyd actually honored --config (not ~/.abox/...).
if grep -q "$SCRATCH/state/logs/audit.jsonl" "$SCRATCH/proxyd.log"; then
    pass "proxyd honored --config (audit path under scratch)"
else
    fail "proxyd honored --config" "audit log path not found in proxyd.log"
    sed "s/^/    /" "$SCRATCH/proxyd.log"
fi

# Helper to send a JSON request to the CLI proxy via Python.
proxy_send() {
    # proxy_send <json>
    python3 - "$SOCK" "$1" <<'PY'
import socket, sys, json
sock_path, payload = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX)
s.connect(sock_path)
s.sendall((payload + "\n").encode())
s.shutdown(socket.SHUT_WR)
chunks = []
while True:
    chunk = s.recv(8192)
    if not chunk: break
    chunks.append(chunk)
print(b"".join(chunks).decode(), end="")
PY
}

step "Allowed: git status -s with sandbox_id=fix-auth"
how 'send {"command":"git","args":["status","-s"],"cwd":"'$SCRATCH'/repo","sandbox_id":"fix-auth"}'
expect "exit_code=0 in JSON response"
REQ='{"command":"git","args":["status","-s"],"cwd":"'$SCRATCH'/repo","sandbox_id":"fix-auth"}'
RESP=$(proxy_send "$REQ")
echo "  $DIM→$RESET $RESP"
EXIT_CODE=$(echo "$RESP" | python3 -c "import sys,json;print(json.loads(sys.stdin.read())['exit_code'])")
assert_eq "git status exit_code" "0" "$EXIT_CODE"

step "Denied: git push --force origin main"
how 'send {"command":"git","args":["push","--force","origin","main"],...,"sandbox_id":"fix-auth"}'
expect "exit_code=126 and stderr mentions 'denied'"
REQ='{"command":"git","args":["push","--force","origin","main"],"cwd":"/tmp","sandbox_id":"fix-auth"}'
RESP=$(proxy_send "$REQ")
echo "  $DIM→$RESET $RESP"
EXIT_CODE=$(echo "$RESP" | python3 -c "import sys,json;print(json.loads(sys.stdin.read())['exit_code'])")
STDERR=$(echo "$RESP" | python3 -c "import sys,json;print(json.loads(sys.stdin.read())['stderr'])")
assert_eq "force-push exit_code" "126" "$EXIT_CODE"
assert_contains "force-push stderr" "denied" "$STDERR"

step "Denied: unknown command (rm -rf /)"
how 'send {"command":"rm","args":["-rf","/"],"cwd":"/tmp","sandbox_id":"fix-auth"}'
expect "exit_code=126; default-deny applies"
REQ='{"command":"rm","args":["-rf","/"],"cwd":"/tmp","sandbox_id":"fix-auth"}'
RESP=$(proxy_send "$REQ")
echo "  $DIM→$RESET $RESP"
EXIT_CODE=$(echo "$RESP" | python3 -c "import sys,json;print(json.loads(sys.stdin.read())['exit_code'])")
assert_eq "rm exit_code" "126" "$EXIT_CODE"

step "Audit log attributes requests to sandbox_id (not 'unknown')"
how "tail $SCRATCH/state/logs/audit.jsonl"
expect "every entry has sandbox_id=fix-auth, both allowed and denied present"
AUDIT="$SCRATCH/state/logs/audit.jsonl"
assert_file_exists "audit log file" "$AUDIT"
sed "s/^/    /" "$AUDIT"
UNKNOWN_COUNT=$(grep -c '"sandbox_id":"unknown"' "$AUDIT" || true)
FIXAUTH_COUNT=$(grep -c '"sandbox_id":"fix-auth"' "$AUDIT" || true)
ALLOWED_COUNT=$(grep -c '"decision":"allowed"' "$AUDIT" || true)
DENIED_COUNT=$(grep -c '"decision":"denied"' "$AUDIT" || true)
assert_eq "no 'unknown' entries" "0" "$UNKNOWN_COUNT"
[[ "$FIXAUTH_COUNT" -ge 3 ]] && pass "fix-auth attribution count >= 3 ($FIXAUTH_COUNT)" \
    || fail "fix-auth attribution" "got $FIXAUTH_COUNT"
[[ "$ALLOWED_COUNT" -ge 1 ]] && pass "at least one allowed audit entry" \
    || fail "allowed audit entry"
[[ "$DENIED_COUNT" -ge 2 ]] && pass "at least two denied audit entries ($DENIED_COUNT)" \
    || fail "denied audit entries" "got $DENIED_COUNT"

step "Legacy shim compatibility (request without sandbox_id field)"
how 'send {"command":"git","args":["status"],"cwd":"'$SCRATCH'/repo"}  (no sandbox_id)'
expect "still allowed; audit row records sandbox_id=unknown"
REQ='{"command":"git","args":["status"],"cwd":"'$SCRATCH'/repo"}'
RESP=$(proxy_send "$REQ")
EXIT_CODE=$(echo "$RESP" | python3 -c "import sys,json;print(json.loads(sys.stdin.read())['exit_code'])")
assert_eq "legacy request exit_code" "0" "$EXIT_CODE"
LEGACY_UNKNOWN=$(grep -c '"sandbox_id":"unknown"' "$AUDIT" || true)
[[ "$LEGACY_UNKNOWN" -ge 1 ]] && pass "legacy entry recorded as unknown" \
    || fail "legacy entry attribution"

# ─── Phase 6: full VM end-to-end (gated on bootstrap artifacts) ─────────────
section "phase 6 — full VM end-to-end (gated)"

ABOX_VM="$HOME/.abox/vm"
if [[ ! -x "$ABOX_VM/cloud-hypervisor" ]] || [[ ! -f "$ABOX_VM/rootfs.raw" ]]; then
    printf '  %sskipped:%s VM artifacts not found. Run `just bootstrap-vm` to enable this phase.\n' \
        "$YELLOW" "$RESET"
else
    # Make CH/virtiofsd discoverable to the abox adapter (it spawns
    # them as `cloud-hypervisor` and `virtiofsd` from PATH).
    export PATH="$ABOX_VM:$PATH"

    step "Inject VM image/kernel paths into the scratch config"
    how "inserting image_path/kernel_path after vcpus line in [vm_defaults] in $SCRATCH/config.toml"
    expect "abox run can find the kernel + rootfs"
    # The [vm_defaults] section already exists (memory_mib/vcpus from phase 3).
    # The [proxy] section follows it, so we cannot simply append to the file.
    # Use sed to insert image_path/kernel_path immediately after 'vcpus = 1'.
    sed -i "s|vcpus = 1|vcpus = 1\nimage_path  = \"$ABOX_VM/rootfs.raw\"\nkernel_path = \"$ABOX_VM/vmlinux\"|" \
        "$SCRATCH/config.toml"
    pass "config updated"

    step "Boot a real VM and run \`git status\` inside the guest"
    how 'abox run --task vm-e2e --base main -- /usr/local/bin/git status'
    expect "agent exits 0; audit log records sandbox_id=vm-e2e for the git call"

    # Run with a generous timeout so a stuck VM cannot hang the test.
    if RUN_OUT=$(timeout 90 $ABOX run --task vm-e2e --base main -- \
        /usr/local/bin/git status 2>&1); then
        pass "vm boot + agent exec"
    else
        rc=$?
        fail "vm boot + agent exec" "exit=$rc; tail of output below"
        echo "$RUN_OUT" | tail -20 | sed "s/^/    /"
    fi

    AUDIT_VM="$SCRATCH/state/logs/audit.jsonl"
    if [[ -f "$AUDIT_VM" ]] && grep -q '"sandbox_id":"vm-e2e"' "$AUDIT_VM"; then
        pass "audit log attributes guest call to vm-e2e"
    else
        fail "audit log attribution from real guest" \
             "no vm-e2e entries in $AUDIT_VM"
    fi

    # Cleanup the leftover sandbox state so the test can be re-run.
    $ABOX stop vm-e2e --clean 2>/dev/null || true

    step "Non-zero agent exit propagates to abox run"
    how 'abox run --task vm-e2e-fail -- /bin/sh -c "exit 7"'
    expect "abox run exits with 7 (guest runner.sh RC bubbled out through aboxstatus)"
    if timeout 90 $ABOX run --task vm-e2e-fail --base main -- \
        /bin/sh -c "exit 7" >"$SCRATCH/fail-run.log" 2>&1; then
        fail "exit code propagation" "abox run returned 0 but guest exited 7"
        tail -20 "$SCRATCH/fail-run.log" | sed "s/^/    /"
    else
        rc=$?
        if [[ "$rc" == "7" ]]; then
            pass "exit code propagation (rc=7)"
        else
            fail "exit code propagation" "expected rc=7, got rc=$rc"
            tail -20 "$SCRATCH/fail-run.log" | sed "s/^/    /"
        fi
    fi
    $ABOX stop vm-e2e-fail --clean 2>/dev/null || true
fi

# ─── Summary ────────────────────────────────────────────────────────────────
section "summary"
TOTAL=$((PASS_COUNT + FAIL_COUNT))
printf '  total assertions: %d\n' "$TOTAL"
printf '  %spassed:%s %d\n' "$GREEN" "$RESET" "$PASS_COUNT"
printf '  %sfailed:%s %d\n' "$RED" "$RESET" "$FAIL_COUNT"

if [[ "$FAIL_COUNT" -gt 0 ]]; then
    printf '\n%s✗ e2e FAILED%s\n' "$RED$BOLD" "$RESET"
    exit 1
fi

printf '\n%s✓ e2e PASSED%s\n' "$GREEN$BOLD" "$RESET"
exit 0
