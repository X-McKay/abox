#!/usr/bin/env bash
#
# abox MicroSandbox live end-to-end test (ADR-008 runtime).
#
# Boots real MicroSandbox microVMs (libkrun: KVM on Linux, Hypervisor.framework
# on macOS) and exercises the full substrate: run/exit-code propagation,
# workspace write-through + isolation, ephemeral cleanup, timeouts, the
# command broker (proxied git + policy deny + audit attribution), the
# host-mediated HTTPS egress proxy, and post-run hygiene.
#
# Preflight requirements (the suite SKIPS cleanly — exit 0 — when unmet):
#   - macOS or Linux with virtualization available
#   - cargo + the host arch's musl target (for guest binaries)
#   - MicroSandbox runtime assets under $MSB_HOME (default ~/.microsandbox):
#     bin/msb and lib/libkrunfw*
#
# Everything lives under a fresh SHORT state dir (/tmp/abox-msb-e2e.XXXX —
# unix socket paths are capped at ~104 bytes) and is removed on exit.
#
# Usage:  MSB_HOME=~/.microsandbox ./scripts/local/msb_e2e_test.sh
#
# Exit code: 0 on success or skip, non-zero if any assertion failed.

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

warn() {
    printf '  %s⚠ warn%s    %s\n' "$YELLOW" "$RESET" "$1"
}

skip_suite() {
    printf '  %sskipped:%s %s\n' "$YELLOW" "$RESET" "$1"
    exit 0
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

assert_file_exists() {
    if [[ -e "$2" ]]; then pass "$1"; else fail "$1" "missing: $2"; fi
}

assert_file_absent() {
    if [[ ! -e "$2" ]]; then pass "$1"; else fail "$1" "should be gone: $2"; fi
}

# ─── Preflight (skip, never fail, when the substrate is unavailable) ────────
section "abox MicroSandbox e2e — preflight"

OS="$(uname -s)"
case "$OS" in
    Darwin)
        HV=$(sysctl -n kern.hv_support 2>/dev/null || echo 0)
        [[ "$HV" == "1" ]] || skip_suite "Hypervisor.framework unavailable (kern.hv_support != 1)"
        ;;
    Linux)
        [[ -e /dev/kvm ]] || skip_suite "/dev/kvm not present — KVM unavailable"
        ;;
    *)
        skip_suite "unsupported OS '$OS' (need macOS or Linux)"
        ;;
esac

command -v cargo >/dev/null 2>&1 || skip_suite "cargo not found on PATH"
command -v git >/dev/null 2>&1 || skip_suite "git not found on PATH"

export MSB_HOME="${MSB_HOME:-$HOME/.microsandbox}"
if [[ ! -x "$MSB_HOME/bin/msb" ]]; then
    skip_suite "MicroSandbox runtime not installed: $MSB_HOME/bin/msb missing (run 'abox init')"
fi
if ! ls "$MSB_HOME"/lib/libkrunfw* >/dev/null 2>&1; then
    skip_suite "MicroSandbox guest firmware missing: no $MSB_HOME/lib/libkrunfw*"
fi

HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
    arm64|aarch64)  RUST_ARCH="aarch64"; MUSL_TARGET="aarch64-unknown-linux-musl" ;;
    x86_64|amd64)   RUST_ARCH="x86_64";  MUSL_TARGET="x86_64-unknown-linux-musl" ;;
    *)              skip_suite "unsupported host arch '$HOST_ARCH'" ;;
esac

if ! rustup target list --installed 2>/dev/null | grep -q "^$MUSL_TARGET$"; then
    skip_suite "musl target $MUSL_TARGET not installed (rustup target add $MUSL_TARGET)"
fi

printf '%sos:%s       %s (%s)\n' "$DIM" "$RESET" "$OS" "$HOST_ARCH"
printf '%smsb:%s      %s\n' "$DIM" "$RESET" "$MSB_HOME"
printf '%starget:%s   %s\n' "$DIM" "$RESET" "$MUSL_TARGET"

# ─── Setup ──────────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ABOX_BIN="$REPO_ROOT/target/debug/abox"

# SHORT state dir: per-sandbox unix control sockets live under <state>/r and
# socket paths are capped at ~104 bytes on macOS / 108 on Linux.
STATE="$(mktemp -d /tmp/abox-msb-e2e.XXXX)"
REPO="$STATE/repo"
CONFIG="$STATE/config.toml"
AUDIT="$STATE/logs/audit.jsonl"

ALL_TASKS=(t01 t02 t03 t04a t04b t05 t06 t10 t11 t12 t20 t21 t22 t30 t31 t32 t33)

cleanup() {
    # Best-effort teardown of anything the suite left behind.
    if [[ -x "$ABOX_BIN" && -d "$REPO" ]]; then
        for t in "${ALL_TASKS[@]}"; do
            "$ABOX_BIN" --repo "$REPO" --config "$CONFIG" stop "$t" --clean \
                >/dev/null 2>&1 || true
        done
    fi
    # MSB_E2E_KEEP=1 preserves the state dir for post-mortem debugging.
    if [[ -z "${MSB_E2E_KEEP:-}" ]]; then
        rm -rf "$STATE"
    fi
}
trap cleanup EXIT INT TERM

section "abox MicroSandbox e2e"
printf '%srepo:%s    %s\n' "$DIM" "$RESET" "$REPO_ROOT"
printf '%sstate:%s   %s\n' "$DIM" "$RESET" "$STATE"

# ─── Build ──────────────────────────────────────────────────────────────────
section "phase 0 — build + scratch layout"

step "Compile abox-cli (debug)"
how "cargo build -p abox-cli"
expect "exit 0; abox binary present"
if (cd "$REPO_ROOT" && cargo build -p abox-cli --quiet); then
    pass "cargo build -p abox-cli"
else
    fail "cargo build -p abox-cli"
    exit 1
fi
assert_file_exists "abox binary present" "$ABOX_BIN"

step "Build static musl guest binaries (abox-shim + abox-bridge) for $RUST_ARCH"
how "RUSTFLAGS='-C linker=rust-lld -C link-self-contained=yes' cargo build --release --target $MUSL_TARGET -p abox-shim"
expect "exit 0; both guest binaries present"
if (cd "$REPO_ROOT" && RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes" \
        cargo build --release --target "$MUSL_TARGET" -p abox-shim --quiet); then
    pass "guest binaries built"
else
    fail "guest binaries built"
    exit 1
fi
GUEST_OUT="$REPO_ROOT/target/$MUSL_TARGET/release"
assert_file_exists "abox-shim built" "$GUEST_OUT/abox-shim"
assert_file_exists "abox-bridge built" "$GUEST_OUT/abox-bridge"

step "Set up isolated state dir (config, policy, guest bins, scratch repo)"
how "config.toml + policies/default.toml + guest/$RUST_ARCH/* + git repo under $STATE"
expect "layout complete; scratch repo has an initial commit"
cat > "$CONFIG" <<EOF
state_dir = "$STATE"

[sandbox_defaults]
memory_mib = 512
vcpus = 1

[proxy]
policy_dir = "$STATE/policies"

[images]
overrides = { base = "alpine:3.19" }
EOF
mkdir -p "$STATE/policies" "$STATE/guest/$RUST_ARCH"
cp "$REPO_ROOT/policies/default.toml" "$STATE/policies/default.toml"
cp "$GUEST_OUT/abox-shim" "$GUEST_OUT/abox-bridge" "$STATE/guest/$RUST_ARCH/"
mkdir -p "$REPO"
(
    cd "$REPO"
    git init -q -b main
    git config user.email "e2e@abox.test"
    git config user.name "abox-msb-e2e"
    echo "# scratch repo for abox msb e2e" > README.md
    git add README.md
    git commit -q -m "init"
)
INIT_LOG=$(git -C "$REPO" log --oneline)
assert_contains "scratch repo has init commit" "init" "$INIT_LOG"

# Run helper -----------------------------------------------------------------
# run_task <task> [abox run flags...] -- <argv...>
# Captures stdout/stderr separately (abox logs INFO/WARN to stderr; guest
# agent stdout arrives on stdout). Sets RC / OUT / ERR.
run_task() {
    local task="$1"; shift
    "$ABOX_BIN" --repo "$REPO" --config "$CONFIG" run --task "$task" "$@" \
        >"$STATE/out-$task.stdout" 2>"$STATE/out-$task.stderr"
    RC=$?
    OUT="$(cat "$STATE/out-$task.stdout" 2>/dev/null)"
    ERR="$(cat "$STATE/out-$task.stderr" 2>/dev/null)"
}

# ─── Phase 1: substrate ─────────────────────────────────────────────────────
section "phase 1 — substrate (boot, exit codes, workspace, isolation)"

step "Run a task that exits 0"
how "abox run --task t01 -- sh -c 'exit 0'"
expect "abox exits 0"
run_task t01 -- sh -c 'exit 0'
assert_eq "exit-0 propagation" "0" "$RC"

step "Guest exit code propagates to the CLI"
how "abox run --task t02 -- sh -c 'exit 7'"
expect "abox exits 7"
run_task t02 -- sh -c 'exit 7'
assert_eq "exit-7 propagation" "7" "$RC"

step "Guest write to /workspace appears in the host worktree"
how "abox run --task t03 -- sh -c 'echo hello-from-guest > out.txt'"
expect "worktree file exists on the host with the guest's content"
run_task t03 -- sh -c 'echo hello-from-guest > out.txt'
assert_eq "t03 run exit" "0" "$RC"
WT_FILE="$STATE/worktrees/t03/out.txt"
assert_file_exists "guest write visible on host" "$WT_FILE"
assert_eq "guest write content" "hello-from-guest" "$(cat "$WT_FILE" 2>/dev/null)"

step "Primary checkout stays clean (agents work on worktrees, never main)"
how "git -C \$REPO status --porcelain"
expect "empty output"
PORCELAIN=$(git -C "$REPO" status --porcelain)
assert_eq "primary checkout clean" "" "$PORCELAIN"

step "Sandboxes are isolated from each other"
how "t04a writes secret.txt in its workspace; t04b must not see it"
expect "t04b's 'test ! -e secret.txt' exits 0"
run_task t04a -- sh -c 'echo A-private > secret.txt'
assert_eq "t04a wrote its file" "0" "$RC"
run_task t04b -- sh -c 'test ! -e secret.txt'
assert_eq "t04b cannot see t04a's file" "0" "$RC"

step "--ephemeral cleans the worktree and branch after the run"
how "abox run --task t05 --ephemeral -- sh -c 'echo ephemeral > tmp.txt'"
expect "exit 0; no worktree dir; no agent/t05 branch"
run_task t05 --ephemeral -- sh -c 'echo ephemeral > tmp.txt'
assert_eq "ephemeral run exit" "0" "$RC"
assert_file_absent "ephemeral worktree removed" "$STATE/worktrees/t05"
T05_BRANCH=$(git -C "$REPO" branch --list 'agent/t05')
assert_eq "ephemeral branch removed" "" "$T05_BRANCH"

step "--timeout kills a stuck agent with exit 124"
how "abox run --task t06 --timeout 2 -- sh -c 'sleep 30'"
expect "abox exits 124 (like GNU timeout)"
run_task t06 --timeout 2 -- sh -c 'sleep 30'
assert_eq "timeout exit code" "124" "$RC"

# ─── Phase 2: command broker ────────────────────────────────────────────────
section "phase 2 — command broker (proxied git, policy, audit)"

step "git status inside the guest is brokered to the host"
how "abox run --task t10 -- sh -c 'git status'"
expect "exit 0; output mentions branch agent/t10"
run_task t10 -- sh -c 'git status'
assert_eq "brokered git status exit" "0" "$RC"
assert_contains "git status names the agent branch" "agent/t10" "$OUT"

step "20 consecutive brokered git calls all succeed"
how "abox run --task t11 -- sh -c '20x git status loop'"
expect "exit 0; guest prints LOOPOK"
read -r -d '' GIT_LOOP <<'EOF' || true
i=0
while [ $i -lt 20 ]; do
  git status >/dev/null || exit 1
  i=$((i+1))
done
echo LOOPOK
EOF
run_task t11 -- sh -c "$GIT_LOOP"
assert_eq "git loop exit" "0" "$RC"
assert_contains "all 20 brokered calls succeeded" "LOOPOK" "$OUT"

step "git push --force is denied by policy"
how "abox run --task t12 -- sh -c 'git push --force origin main'"
expect "exit 126; output mentions 'denied'"
run_task t12 -- sh -c 'git push --force origin main'
assert_eq "force-push exit code" "126" "$RC"
assert_contains "force-push denial message" "denied" "$OUT$ERR"

step "Audit log attributes allowed and denied entries to their tasks"
how "grep $AUDIT"
expect "allowed entries for t10; denied entry for t12"
assert_file_exists "audit log present" "$AUDIT"
if grep -q '"sandbox_id":"t10"' "$AUDIT" && \
   grep '"sandbox_id":"t10"' "$AUDIT" | grep -q '"decision":"allowed"'; then
    pass "allowed audit entries attributed to t10"
else
    fail "allowed audit entries attributed to t10"
fi
if grep '"sandbox_id":"t12"' "$AUDIT" | grep -q '"decision":"denied"'; then
    pass "denied audit entry attributed to t12"
else
    fail "denied audit entry attributed to t12"
fi

step "Audit chain integrity"
how "abox audit verify --log $AUDIT (if the subcommand exists)"
expect "verification exits 0"
if "$ABOX_BIN" audit --help 2>/dev/null | grep -q "verify"; then
    if "$ABOX_BIN" --repo "$REPO" --config "$CONFIG" audit verify --log "$AUDIT" \
            >"$STATE/audit-verify.out" 2>&1; then
        pass "abox audit verify (hash chain intact)"
    else
        fail "abox audit verify" "exited non-zero; tail below"
        tail -5 "$STATE/audit-verify.out" | sed "s/^/    /"
    fi
else
    # No verify subcommand in this build: fall back to a structural check.
    if grep -q '"decision"' "$AUDIT"; then
        pass "audit jsonl has decision entries (verify subcommand absent)"
    else
        fail "audit jsonl structural check"
    fi
fi

# ─── Phase 3: egress ────────────────────────────────────────────────────────
section "phase 3 — HTTPS egress proxy (policy-enforced CONNECT)"

step "CONNECT to a non-managed domain is denied (403)"
how "guest: printf 'CONNECT example.com:443 ...' | nc -w 2 127.0.0.1 18443"
expect "response line contains 403"
read -r -d '' CONNECT_DENIED <<'EOF' || true
printf 'CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n' \
  | nc -w 2 127.0.0.1 18443 | head -1
EOF
run_task t20 -- sh -c "$CONNECT_DENIED"
assert_eq "denied CONNECT run exit" "0" "$RC"
assert_contains "example.com CONNECT gets 403" "403" "$OUT"

step "CONNECT to a managed domain is allowed (200)"
how "guest: printf 'CONNECT api.anthropic.com:443 ...' | nc -w 2 127.0.0.1 18443"
expect "response line contains 200"
read -r -d '' CONNECT_ALLOWED <<'EOF' || true
printf 'CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\n\r\n' \
  | nc -w 2 127.0.0.1 18443 | head -1
EOF
run_task t21 -- sh -c "$CONNECT_ALLOWED"
assert_eq "allowed CONNECT run exit" "0" "$RC"
assert_contains "api.anthropic.com CONNECT gets 200" "200" "$OUT"

step "Denied CONNECT is stable across 10 consecutive attempts"
how "guest loop: 10x CONNECT example.com through the egress bridge"
expect "all 10 responses are 403; guest prints EGRESS10OK"
read -r -d '' EGRESS_LOOP <<'EOF' || true
ok=0
i=0
while [ $i -lt 10 ]; do
  r=$(printf 'CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n' \
      | nc -w 2 127.0.0.1 18443 | head -1)
  case "$r" in *" 403"*) ok=$((ok+1));; esac
  i=$((i+1))
done
[ "$ok" -eq 10 ] && echo EGRESS10OK
[ "$ok" -eq 10 ]
EOF
run_task t22 -- sh -c "$EGRESS_LOOP"
assert_eq "egress loop exit" "0" "$RC"
assert_contains "10/10 denied CONNECTs" "EGRESS10OK" "$OUT"

# ─── Phase 4: hygiene ───────────────────────────────────────────────────────
section "phase 4 — hygiene (sockets, list, stop --clean)"

step "No leftover control sockets for stopped sandboxes (best effort)"
how "ls $STATE/r/msb-*.sock_* for tasks whose runs completed"
expect "none present (warn, not fail, if any linger)"
LEFTOVER=$(ls "$STATE"/r/msb-*.sock_* 2>/dev/null || true)
if [[ -z "$LEFTOVER" ]]; then
    pass "no leftover msb control sockets"
else
    warn "leftover control sockets (not fatal): $LEFTOVER"
    pass "socket hygiene checked (leftovers warned above)"
fi

step "abox list shows the completed (non-ephemeral) sandboxes"
how "abox list"
expect "t01 and t03 listed with their agent/ branches"
LIST_OUT=$("$ABOX_BIN" --repo "$REPO" --config "$CONFIG" list 2>&1)
assert_contains "list shows t01" "t01" "$LIST_OUT"
assert_contains "list shows t03" "t03" "$LIST_OUT"

step "abox stop --clean removes the worktree"
how "abox stop t03 --clean"
expect "worktree dir gone; t03 no longer listed"
"$ABOX_BIN" --repo "$REPO" --config "$CONFIG" stop t03 --clean \
    >"$STATE/stop-t03.out" 2>&1 || true
assert_file_absent "t03 worktree removed" "$STATE/worktrees/t03"
LIST_AFTER=$("$ABOX_BIN" --repo "$REPO" --config "$CONFIG" list 2>&1)
if [[ "$LIST_AFTER" == *"t03"* ]]; then
    fail "t03 gone from list" "still present after stop --clean"
else
    pass "t03 gone from list"
fi

# ─── Phase 5: filesystem adversarial ────────────────────────────────────────
section "phase 5 — filesystem adversarial (escape attempts)"

step "Host filesystem is invisible to the guest"
how "guest stats a host-only path (this state dir) and the host home dir"
expect "both absent in the guest"
run_task t30 --ephemeral -- sh -c "ls '$STATE' >/dev/null 2>&1 && echo HOST-STATE-VISIBLE; ls /Users >/dev/null 2>&1 && echo HOST-USERS-VISIBLE; echo FS-PROBE-DONE"
assert_eq "fs probe run exit" "0" "$RC"
assert_contains "fs probe completed" "FS-PROBE-DONE" "$OUT"
if [[ "$OUT" == *"HOST-STATE-VISIBLE"* || "$OUT" == *"HOST-USERS-VISIBLE"* ]]; then
    fail "host filesystem invisible" "guest can see host paths: $OUT"
else
    pass "host filesystem invisible"
fi

step "Absolute symlink in the worktree resolves guest-side, not host-side"
how "host plants 'hostlink' -> /etc/os-release in the worktree; guest reads it"
expect "guest sees its own /etc/os-release (Alpine), never host content"
ln -sfn /etc/os-release "$REPO/hostlink" 2>/dev/null || true
git -C "$REPO" add -A >/dev/null 2>&1 && git -C "$REPO" commit -qm symlink >/dev/null 2>&1
HOST_OS_RELEASE="$(cat /etc/os-release 2>/dev/null || echo NO-HOST-OS-RELEASE)"
run_task t31 --ephemeral -- sh -c 'cat /workspace/hostlink 2>/dev/null || echo LINK-UNREADABLE'
assert_eq "symlink probe run exit" "0" "$RC"
if [[ "$HOST_OS_RELEASE" != "NO-HOST-OS-RELEASE" && "$OUT" == *"$HOST_OS_RELEASE"* ]]; then
    fail "symlink resolves guest-side" "guest read HOST /etc/os-release through the bind mount"
elif [[ "$OUT" == *"Alpine"* || "$OUT" == *"LINK-UNREADABLE"* || "$OUT" == *"alpine"* ]]; then
    pass "symlink resolves guest-side"
else
    fail "symlink resolves guest-side" "unexpected output: $OUT"
fi
rm -f "$REPO/hostlink"
git -C "$REPO" add -A >/dev/null 2>&1 && git -C "$REPO" commit -qm rm-symlink >/dev/null 2>&1

step "Path traversal out of /workspace stays inside the guest"
how "guest lists /workspace/../ and checks for host worktree siblings"
expect "parent is the guest root, not the host worktrees dir"
run_task t32 --ephemeral -- sh -c 'ls /workspace/../ 2>/dev/null; ls /workspace/../t01 >/dev/null 2>&1 && echo SIBLING-WORKTREE-VISIBLE; echo TRAVERSE-DONE'
assert_eq "traversal probe run exit" "0" "$RC"
assert_contains "traversal probe completed" "TRAVERSE-DONE" "$OUT"
if [[ "$OUT" == *"SIBLING-WORKTREE-VISIBLE"* ]]; then
    fail "no sibling worktree via traversal" "guest reached another task's worktree"
else
    pass "no sibling worktree via traversal"
fi

step "Host-staged transport declaration is read-only for the agent"
how "guest (uid 1000) appends to /etc/abox/transport"
expect "write fails; file content unchanged"
run_task t33 --ephemeral -- sh -c 'BEFORE=$(cat /etc/abox/transport); { echo tampered >> /etc/abox/transport; } 2>/dev/null; AFTER=$(cat /etc/abox/transport); [ "$BEFORE" = "$AFTER" ] && echo TRANSPORT-IMMUTABLE || echo TRANSPORT-TAMPERED'
assert_eq "transport probe run exit" "0" "$RC"
assert_contains "transport declaration immutable for agent" "TRANSPORT-IMMUTABLE" "$OUT"

# ─── Summary ────────────────────────────────────────────────────────────────
section "summary"
TOTAL=$((PASS_COUNT + FAIL_COUNT))
printf '  total assertions: %d\n' "$TOTAL"
printf '  %spassed:%s %d\n' "$GREEN" "$RESET" "$PASS_COUNT"
printf '  %sfailed:%s %d\n' "$RED" "$RESET" "$FAIL_COUNT"

if [[ "$FAIL_COUNT" -gt 0 ]]; then
    printf '\n%s✗ msb e2e FAILED%s\n' "$RED$BOLD" "$RESET"
    exit 1
fi

printf '\n%s✓ msb e2e PASSED%s\n' "$GREEN$BOLD" "$RESET"
exit 0
