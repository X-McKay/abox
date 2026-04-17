# abox Codex Validation — Targeted Test Prompt

> Self-contained prompt for a fresh Claude Code or human session. Validates
> that Codex (OpenAI) works inside the abox sandbox with the same credential-
> forwarding, non-root execution, and policy enforcement guarantees as
> Claude Code. Run after confirming the Claude soak test passes.

## Context

abox forwards credentials for both Claude Code and Codex via TLS-terminating
MITM proxy. The flows differ slightly:

| | Claude Code | Codex |
|---|---|---|
| Auth file | `~/.claude/.credentials.json` | `~/.codex/auth.json` |
| Auth type | Anthropic OAuth | ChatGPT OAuth |
| API domain | `api.anthropic.com` | `api.openai.com` |
| Credential field | `claudeAiOauth.accessToken` | `tokens.access_token` |
| CLI flag for auto-approve | `--dangerously-skip-permissions` | `--full-auto` |

Both use the same mechanism: a stub credential file in the guest that
passes the CLI's startup auth check, with the real token injected by
the host-side egress proxy at the network layer.

## Preconditions

```bash
cd /home/al/git/bakudo-abox/abox

# 1. Binary up to date
abox --version   # expect 0.1.0

# 2. Environment healthy
abox doctor      # all checks green

# 3. Host has valid Codex OAuth
python3 -c "
import json
d = json.load(open('$HOME/.codex/auth.json'))
t = d.get('tokens', {})
print('auth_mode:', d.get('auth_mode'))
print('has_access_token:', bool(t.get('access_token')))
print('has_refresh_token:', bool(t.get('refresh_token')))
"
# expect auth_mode: chatgpt, has_access_token: True, has_refresh_token: True

# 4. Policy rule covers api.openai.com with credential_file fallback
grep -A4 'api.openai.com' ~/.abox/policies/default.toml
# expect: credential_file = "~/.codex/auth.json"
#         json_path = "tokens.access_token"

# 5. Config has Codex credential entry with stub
grep -A8 'codex/auth.json' ~/.abox/config.toml
# expect: guest = "~/.codex/auth.json" with [stub] block

# 6. Scratch repo
rm -rf /tmp/abox-codex-repo
mkdir -p /tmp/abox-codex-repo && cd /tmp/abox-codex-repo
git init -q -b main
git config user.email t@t.test && git config user.name tester
cat > README.md <<'EOF'
# codex-test
This is a scratch repo for Codex validation.
EOF
git add . && git commit -qm "init"
```

If any precondition fails, stop and investigate.

## Test Suite

### Test C1 — Codex smoke (single-turn, no tools)

**What it exercises:** VM boot, non-root execution, Codex CLI startup with
stub auth, single API call through the MITM proxy to api.openai.com.

```bash
cd /tmp/abox-codex-repo
abox run --task c1-smoke --ephemeral -- /bin/sh -c \
  'cd /workspace && codex --full-auto --quiet "What is 3+3? Answer with just the number."' \
  2>&1 | tail -10
```

**Expected:** Output contains "6". No auth errors. Exit code 0.
**If fails with auth error:** MITM credential injection for `api.openai.com`
is not resolving `tokens.access_token` from the host's `auth.json`. Check
`RUST_LOG=abox_core::egress=debug` for `Injected credential header` events.

### Test C2 — Codex multi-turn with tool use

**What it exercises:** Multi-turn tool-use flow through the MITM proxy,
same pattern that previously broke Claude in PR #6.

```bash
abox run --task c2-tool --ephemeral -- /bin/sh -c \
  'cd /workspace && codex --full-auto --quiet "Read README.md and tell me what it says in 5 words."' \
  2>&1 | tail -10
```

**Expected:** Output is a 5-word-ish summary of the README. No errors.
**If fails:** Multi-turn credential injection regression. Debug with
`RUST_LOG=abox_core::egress=debug`.

### Test C3 — Codex file write (worktree isolation)

**What it exercises:** Guest-side file I/O by Codex, worktree isolation.

```bash
abox run --task c3-write --ephemeral -- /bin/sh -c \
  'cd /workspace && codex --full-auto --quiet "Create a file named hello.txt containing hello-codex-test, then read it back."' \
  2>&1 | tail -10

# Verify isolation:
test ! -f /tmp/abox-codex-repo/hello.txt && echo "✓ isolated" || echo "✗ LEAKED"
```

**Expected:** Codex reports the file content. File does not leak to host.

### Test C4 — Non-root verification

**What it exercises:** Codex runs as abox user (uid=1000), not root.

```bash
abox run --task c4-uid --ephemeral -- /bin/sh -c \
  'id; echo HOME=$HOME; echo USER=$USER; which codex; \
   ls -la /home/abox/.codex/auth.json 2>&1' \
  2>&1 | grep -E "uid=|HOME=|USER=|codex|auth.json"
```

**Expected:** uid=1000(abox), HOME=/home/abox, USER=abox. Codex binary at
`/usr/local/bin/codex`. Stub auth.json exists at `/home/abox/.codex/auth.json`
owned by abox.

### Test C5 — Policy denial (same as Claude Test 6)

**What it exercises:** Policy engine blocks Codex from force-pushing.

```bash
abox run --task c5-policy --ephemeral -- /bin/sh -c \
  'cd /workspace && codex --full-auto --quiet "Run git push --force origin main and report what happens."' \
  2>&1 | tail -10
```

**Expected:** Force-push is denied/refused/blocked. The shim intercepts
`git push --force` regardless of which agent invokes it.

## Success criteria

All 5 tests pass. Codex credential forwarding works end-to-end through
the same MITM proxy path as Claude Code.

## Troubleshooting

| Symptom | Likely cause | Investigation |
|---|---|---|
| "Not authenticated" / auth error | Stub not staged or wrong format | Check `/home/abox/.codex/auth.json` inside guest |
| 401 in API response body | MITM not injecting token for api.openai.com | `RUST_LOG=abox_core::egress=debug` — look for credential injection events |
| Codex hangs or times out | HTTPS_PROXY not routing through MITM | Check `env \| grep -i proxy` inside guest |
| "permission denied" on file ops | Non-root uid mismatch | Verify uid=1000, /workspace owned 1000:1000 |
| `codex: command not found` | Rootfs missing Codex CLI | `just rebuild-rootfs` and re-run |
