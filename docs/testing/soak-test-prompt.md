# abox Soak Test — Targeted Validation Prompt

> Self-contained prompt for a fresh Claude Code session. Reads cold — no prior
> context required. Intended to soak-test the sandbox + credential forwarding
> path after the PR #6 hyper MITM refactor landed on `main`.

## Context (read this first)

You're running targeted tests on a just-merged change to **abox**, a Rust sandbox that runs AI coding agents inside Cloud Hypervisor microVMs. The repo is at `/home/al/git/bakudo-abox/abox/`.

**What just changed (PRs #4–#6, all merged to `main`):**

- **PR #4** landed credential forwarding: per-sandbox MITM TLS proxy that injects the host's real OAuth token into guest API calls, using a stub `~/.claude/.credentials.json` inside the guest.
- **PR #5** addressed 6 review findings on PR #4: tightened stub-staging (host file must exist), fixed a silent-failure in policy load, deferred CA load to `abox run`, Arc-wrapped `bypass_tls`, buffered HTTP header reads, replaced an `init.sh` race.
- **PR #6** replaced the hand-rolled byte-level MITM with `hyper::client::conn::http1` + `hyper::server::conn::http1::serve_connection`. Fixed a real bug: previously, `claude --print "Read README.md..."` (a 2-turn tool-use query) returned `401 "Invalid bearer token"` mid-session. Root cause was a combination of broken keep-alive, header ordering, and body framing in the old proxy. All three go away by using hyper's HTTP/1.1 codec. A follow-up commit on the same PR (b069afe) addressed Codex review feedback: restored always-insert behavior (vs replace-only) and resolve-credential-per-request (vs cached-per-tunnel).

**Important context about the bug being tested:**

- The failure mode was *specifically* multi-turn queries (those that used tool calls). Single-turn queries worked.
- The 401 was embedded in the Anthropic SSE stream body, not an HTTP status. At the transport layer every request returned 200 OK — the server was ACCEPTING the HTTP request, then rejecting the bearer during stream setup.
- Post-fix smoke test: `claude --print "Read README.md and tell me what it says in 5 words."` succeeds with `is_error: false`, `num_turns: 2`, billed ~$0.012.

**Your job now:** run a broader set of targeted tests to confirm the hyper refactor hasn't introduced regressions and that credential forwarding works across more realistic workloads.

## Preconditions — verify before running tests

```bash
cd /home/al/git/bakudo-abox/abox

# 1. Binary up to date
abox --version   # expect 0.1.0 (the build from current main)

# 2. Environment healthy
abox doctor      # all 9 checks should be green

# 3. Host has valid Claude OAuth
python3 -c "import json,datetime as dt; d=json.load(open('/home/al/.claude/.credentials.json')); e=d['claudeAiOauth']['expiresAt']; print('expires:', dt.datetime.fromtimestamp(e/1000), 'valid:', e > dt.datetime.now().timestamp()*1000)"
# expect "valid: True"

# 4. Clean working tree on main
git status                # clean
git log --oneline -1      # should show the hyper refactor squash commit (fix(egress)...)

# 5. Scratch repo for tests
rm -rf /tmp/abox-soak-repo
mkdir -p /tmp/abox-soak-repo
cd /tmp/abox-soak-repo
git init -q -b main
git config user.email t@t.test
git config user.name tester
cat > README.md <<'EOF'
# soak-test

This is a scratch repo for abox soak testing.
The quick brown fox jumps over the lazy dog.
EOF
git add . && git commit -qm "init"
```

If any precondition fails, stop and investigate before running tests.

## Test Suite

**General conventions for each test:**

- Run from `/tmp/abox-soak-repo` (cd there first).
- Use `abox run --task <test-name> --ephemeral` so each sandbox is a fresh guest and is cleaned up after.
- Parse Claude's JSON output: `... 2>/dev/null | grep '"type":"result"' | python3 -c "import json,sys; d=json.load(sys.stdin); print(d)"`.
- `--dangerously-skip-permissions` = "live dangerously" mode = auto-approve all tool calls without asking. Required for non-interactive tests.

---

### Test 1 — No-network smoke

**What it exercises:** VM boot, shim PID 1, Claude CLI basic startup, single /v1/messages call without tools.

```bash
cd /tmp/abox-soak-repo
abox run --task t1-smoke --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "What is 2+2? Answer with just the number."' \
  2>/dev/null | grep '"type":"result"' | python3 -c "
import json,sys; d=json.load(sys.stdin)
print('PASS' if not d['is_error'] and d['num_turns']==1 and '4' in d['result'] else f'FAIL: {d}')"
```

**Expected:** `PASS`. is_error=false, num_turns=1, result contains "4", cost < $0.01.
**If fails:** VM or Claude CLI itself is broken. Check `abox doctor`; inspect logs with `RUST_LOG=info` prefix.

---

### Test 2 — Single-tool 2-turn (the previously-broken case)

**What it exercises:** The specific regression the hyper refactor fixed — multi-turn tool use with credential injection per request.

```bash
abox run --task t2-twoturn --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Read the README.md in this directory and summarize it in exactly 5 words."' \
  2>/dev/null | grep '"type":"result"' | python3 -c "
import json,sys; d=json.load(sys.stdin)
print('PASS' if not d['is_error'] and d['num_turns']==2 else f'FAIL: is_error={d[\"is_error\"]} turns={d[\"num_turns\"]} result={d[\"result\"][:200]}')"
```

**Expected:** `PASS`. num_turns=2, result is an actual 5-word-ish summary.
**If fails:** the hyper MITM regression is back. Re-run with `RUST_LOG=abox_core::egress=debug` and look for `Injected credential header` debug events per request. Compare against git blame on `crates/abox-core/src/egress.rs`.

---

### Test 3 — Multi-step tool chain

**What it exercises:** Several sequential API calls with Bash + Read + possibly Write tool calls. Validates that keep-alive reuse + per-request credential resolution both work.

```bash
abox run --task t3-chain --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "List the files in /workspace, then read each file, then report total line count across all of them."' \
  2>/dev/null | grep '"type":"result"' | python3 -c "
import json,sys; d=json.load(sys.stdin)
print(f'is_error={d[\"is_error\"]} turns={d[\"num_turns\"]} result={d[\"result\"][:300]}')"
```

**Expected:** is_error=false, num_turns >= 3, result mentions a small number (README is 3 lines).
**If fails:** likely a multi-request credential issue. Check debug logs for injection count vs request count.

---

### Test 4 — Streaming response (long output)

**What it exercises:** MITM body framing under long Server-Sent Events streams. The old proxy's `copy_bidirectional` could mis-frame large SSE responses.

```bash
abox run --task t4-stream --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Write a 300-word explanation of how microVMs differ from containers. No tool calls needed, just answer directly."' \
  2>/dev/null | grep '"type":"result"' | python3 -c "
import json,sys; d=json.load(sys.stdin)
r=d.get('result','')
print(f'is_error={d[\"is_error\"]} turns={d[\"num_turns\"]} words={len(r.split())} result_preview={r[:150]}')"
```

**Expected:** is_error=false, num_turns=1, words >= 200. Result is coherent prose about microVMs.
**If fails with truncated or garbled output:** body framing regression. Inspect the response with `RUST_LOG=abox_core::egress=debug` and look at byte counts.

---

### Test 5 — File write + read-back (worktree isolation + tool chain)

**What it exercises:** Guest-side file I/O, git worktree mount, tool-use loop.

```bash
abox run --task t5-write --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Create a file named test.txt with the content hello-abox-soak, then read it back and confirm the content."' \
  2>/dev/null | grep '"type":"result"' | python3 -c "
import json,sys; d=json.load(sys.stdin)
r=d.get('result','')
print(f'is_error={d[\"is_error\"]} turns={d[\"num_turns\"]} mentions_content={\"hello-abox-soak\" in r} result_preview={r[:200]}')"

# Also verify the file did NOT leak to host cwd:
test ! -f /tmp/abox-soak-repo/test.txt && echo "✓ file isolated to worktree (good)" || echo "✗ FAIL: file leaked to host"
```

**Expected:** is_error=false, num_turns >= 3, result mentions `hello-abox-soak`. Host workspace is unchanged (the worktree is gone after `--ephemeral`).
**If the file leaks to host:** worktree isolation is broken — stop and investigate immediately, this is a security regression.

---

### Test 6 — Policy denial still works

**What it exercises:** CLI proxy + policy engine. Unrelated to MITM but confirms the CLI bridge hasn't been collateral damaged.

```bash
abox run --task t6-policy --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --output-format json --dangerously-skip-permissions "Try to run git push --force origin main. Report what happens."' \
  2>/dev/null | grep '"type":"result"' | python3 -c "
import json,sys; d=json.load(sys.stdin)
r=d.get('result','').lower()
denied = 'denied' in r or 'blocked' in r or 'policy' in r or 'refus' in r
print(f'is_error={d[\"is_error\"]} mentions_denial={denied} result={d[\"result\"][:200]}')"
```

**Expected:** Claude's result mentions that the command was denied / blocked by policy. (is_error may be false — Claude successfully REPORTED the denial.)
**If force-push would have succeeded:** policy engine is broken. Critical.

---

### Test 7 — Two concurrent sandboxes

**What it exercises:** Per-sandbox egress proxy isolation. Sandbox A's traffic should not appear attributed to Sandbox B in the audit log.

```bash
# Fire two sandboxes in parallel
(abox run --task t7a-concur --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --dangerously-skip-permissions "Say exactly the word ALPHA."' \
  > /tmp/t7a.out 2>&1 ) &
A_PID=$!
(abox run --task t7b-concur --ephemeral -- /bin/sh -c \
  'cd /workspace && claude --print --dangerously-skip-permissions "Say exactly the word BRAVO."' \
  > /tmp/t7b.out 2>&1 ) &
B_PID=$!
wait $A_PID $B_PID
echo "---a---"; tail -5 /tmp/t7a.out
echo "---b---"; tail -5 /tmp/t7b.out
echo "---audit attribution---"
grep -c '"sandbox_id":"t7a-concur"' ~/.abox/logs/audit.jsonl
grep -c '"sandbox_id":"t7b-concur"' ~/.abox/logs/audit.jsonl
```

**Expected:** both sandboxes complete without error, one says ALPHA, other says BRAVO. Both sandbox_ids present in audit log with non-zero counts.
**If both say the same word OR one blocks the other:** concurrency / proxy-isolation regression.

---

## Success criteria (overall)

All 7 tests pass with their expected outcomes. No regressions observed. No failed assertions. No leaked files. No audit misattribution.

## Troubleshooting — common failure modes

| Symptom | Likely cause | Investigation |
|---|---|---|
| All tests hang or timeout | VM isn't booting | `abox doctor` → verify `/dev/kvm`, VM artifacts |
| Test 2 fails with 401 | Credential forwarding regression | `RUST_LOG=abox_core::egress=debug` → count `Injected credential header` events vs request count |
| Test 4 output truncated | MITM body framing | `RUST_LOG=abox_core::egress=debug` + reproduce; compare to `hyper`'s error logs |
| Test 5 leaks file to host | Worktree isolation broken | Check `state/worktrees/` vs host cwd; verify `--ephemeral` cleanup ran |
| Test 6 doesn't deny | Policy engine not loaded / rule missing | Check `~/.abox/policies/default.toml` has `git push --force` deny pattern |
| Test 7 sandboxes interfere | Per-sandbox egress proxy port collision or socket clash | Check `~/.abox/r/vsock-*.sock_5001` — one per sandbox |

If any test fails:

1. **Do not** attempt fixes. Reproduce the failure with `RUST_LOG=abox_core=debug` to get the relevant log output.
2. Open a new feature branch (`fix/<descriptive-slug>` per `docs/contributing/branching.md`).
3. Follow the `superpowers:systematic-debugging` skill if available.
4. File an issue with: the failing test, the output, the debug log snippets, the `abox --version` output.

## Next steps if all tests pass

The hyper MITM refactor is confirmed stable across a representative workload. At this point:

1. **Decision point:** cut `v0.1.0`? The release process per `docs/contributing/branching.md` + `docs/rollback.md`:

   ```bash
   cd /home/al/git/bakudo-abox/abox
   git checkout main && git pull
   just release-dry v0.1.0     # preview only
   just release v0.1.0         # runs full gate + benchmarks + tag
   git push origin main --tags # triggers release.yml
   ```

   The `release.sh` script auto-generates `CHANGELOG.md` from conventional-commit subjects since the last tag (there are no prior tags, so it starts from repo-init).

2. **If not ready to release:** record the soak-test result (which tests passed, date, claude-cli version inside guest) somewhere, and move on to the next piece of work.

3. **If you found minor issues** that don't block release: open them as issues for follow-up, tag them `post-v0.1.0`, and proceed with the release if the issues aren't blockers.

## Resources

- Main spec: `docs/superpowers/specs/2026-04-12-credential-forwarding-design.md`
- ADR: `docs/decisions/003-https-credential-injection.md`
- Pre-PR process: `docs/contributing/pre-pr-checklist.md`
- Branching: `docs/contributing/branching.md`
- Rollback: `docs/rollback.md`
- Recent PRs for context: `gh pr list --state merged --limit 5`
