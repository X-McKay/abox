# VM End-to-End MVP — Follow-Ups and Known Limitations

**Created:** 2026-04-08
**Source branch:** `vm-e2e-mvp` (15 commits, develop → vm-e2e-mvp-mvp)
**Related plan:** [`docs/plans/2026-04-07-vm-end-to-end-mvp.md`](../plans/2026-04-07-vm-end-to-end-mvp.md)
**Hardening branch:** `vm-e2e-hardening` (executed 2026-04-08; see [`docs/plans/2026-04-08-vm-e2e-hardening.md`](../plans/2026-04-08-vm-e2e-hardening.md))

The VM MVP is merged/mergeable with a real live-VM end-to-end test (`just e2e-vm` phase 6) verifying the happy path: boot a Cloud Hypervisor microVM, exec a guest command, route its `git`/`gh`/`aws` through the host policy proxy with per-sandbox attribution, and clean up on exit. Everything in this file was **explicitly deferred** (either in the plan itself, noted by reviewers, or discovered during implementation) and is waiting for a future sprint.

Each item has a **priority** (P0 = next up, P1 = soon, P2 = nice to have), a rough **effort** estimate (S / M / L), and a **why** explaining the deferral.

## Status (updated 2026-04-08 after vm-e2e-hardening)

| Item | Priority | Effort | Status |
|---|---|---|---|
| F1. `abox run --detach` | P1 | S | **DONE** in `af6a5cd` |
| F2. Agent exit code always 0 | P0 | S | **DONE** in `7660592` (see [ADR-002](../decisions/002-aboxstatus-share.md)) |
| F3. HTTPS credential injection | P0 | L | **DEFERRED** — spec at [`docs/plans/2026-04-08-credential-injection.md`](../plans/2026-04-08-credential-injection.md) |
| F4. `abox template create` stub | P2 | M | **OPEN** (deferred from this hardening branch) |
| F5. TUI dashboard never refreshes | P2 | S | **OPEN** (deferred from this hardening branch) |
| S1. Policy regex bypass | P1 | M | **DONE** in `3beec80` |
| S2. Egress audit attribution | P1 | S | **OPEN** (blocked on F3) |
| S3. `forward_ssh_agent` not enforced | P2 | S | **DONE** in `0600339` |
| S4. shim `getcwd()` fallback | P2 | S | **DONE** in `0600339` |
| D1. Bootstrap PATH symlinks | P1 | S | **DONE** in `702644c` |
| D2. Hardcoded timing constants | P2 | S | **DONE** in `1426089` |
| D3. Bootstrap musl target opt-in | P2 | S | **DONE** in `09130ba` |
| D4. E2E phase 6 console assertion | P2 | S | **DONE** in `836add6` |
| D5. Console tail exit signal | P1 | S | **DONE** in `ff37d35` |
| D6. CI workflow | P1 | S | **DONE** in `43d4aae` |
| H1. Squash 15 commits before merge | P2 | S | **OPEN** (controller preference) |
| H2. Plan doesn't reflect Task 9 scope | P2 | S | **DONE** — retrospective added to plan in `7cda252` |
| H3. E2E `.scratch/` cleanup robustness | P2 | S | **DONE** in `df475be` |

The detailed write-ups for each item are kept below as a running history.

---

## Functional gaps

### F1. `abox run` has no `--detach` mode — P1, S

**What:** `abox run` is a foreground supervisor. It blocks until the guest agent exits. There is no way to launch an agent and reclaim the terminal.

**Why deferred:** The foreground model is dramatically simpler for an MVP, and the typical "run an agent, watch it work, take over" workflow matches it. Detach mode is additive — future work, not a rewrite.

**How to pick it up:** Add `--detach` to `RunArgs`. When set, `run::execute` spawns the supervision future as a tokio task and returns immediately after printing the sandbox id. The detached background task needs its own lifecycle (probably write the PID somewhere under `runtime_dir` and let `abox stop` find it). Requires thinking about where console output goes when there's no terminal (log file? ring buffer?).

---

### F2. Agent exit code is always 0 — P0, S

**What:** `run_sandbox` returns `Ok(0)` regardless of the agent's actual exit status. If the agent inside the guest crashes or returns non-zero, `abox run` still reports success.

**Why deferred:** Exit-code propagation from guest to host wasn't wired up in the MVP because the obvious path (guest init writes `/run/abox-exit-code` to the workspace) pollutes the worktree, and the cleaner path (write to a tmpfs on the `aboxmeta` share, which is read-only) requires making `aboxmeta` writable.

**How to pick it up:** Give the guest a THIRD virtiofs share, read-write this time, e.g. `aboxstatus`, mounted at `/abox-status`. `guest/init.sh` writes the runner's exit code into `/abox-status/exit-code` before `poweroff`. The orchestrator, after detecting VM exit, reads that file and returns its contents instead of hardcoded `Ok(0)`. Estimated 2-3 hours.

---

### F3. HTTP egress proxy does not actually inject credentials — P0, L

**What:** `abox-proxyd::egress_proxy` is a passthrough TCP tunnel. The README promises "injects credentials via a dual-layer interception architecture" but the egress side just forwards the bytes. Every sandbox that knows the destination IP can bypass the credential model by DNS-ing out-of-band.

**Why deferred:** Properly injecting headers into a TLS-wrapped HTTPS session requires a TLS-terminating proxy with a generated CA cert installed in the guest. That's a real project on its own and was explicitly scoped out of the plan (noted as a "Phase 2 enhancement" in the source comments).

**How to pick it up:**
  1. Generate a self-signed CA at first run, install it in `/etc/ssl/certs/` inside the rootfs, rebuild the guest image
  2. Switch egress proxy to hyper with rustls, accepting CONNECT, terminating TLS, rewriting headers per the egress rules, then opening a NEW TLS connection to the real upstream
  3. Verify certificate pinning doesn't break common APIs (anthropic, openai, github)
  4. Add an integration test: guest sends a request with no credentials, host rewrites it, upstream sees the injected header

Medium–large effort (1-2 days).

---

### F4. `abox template create` is a stub — P2, M

**What:** The CLI command prints instructions instead of actually calling `SnapshotManager::create_snapshot`. The snapshot/restore path is plumbed through the adapter but not exposed.

**Why deferred:** The MVP's goal was "one VM boots correctly", not "template-based fast cloning". CH snapshots add their own complexity (pause-via-API, the on-disk snapshot format, restore semantics).

**How to pick it up:** Wire the existing `CloudHypervisorAdapter::pause` + `SnapshotManager::create_snapshot` into a real implementation in `cli/commands/template.rs`. Add an e2e phase that creates a template and confirms it restores in under 1 second.

---

### F5. TUI dashboard never refreshes — P2, S

**What:** `DashboardState::new()` initializes with empty `Vec`s and the event loop has no timer/refresh path. The footer advertises `r: refresh` but `r` is unhandled.

**Why deferred:** Pre-existing; noted in the original e2e test report and not on the VM MVP critical path.

**How to pick it up:** Add a `refresh_every: Duration` to the dashboard loop, call `orchestrator.list_sandboxes()` on a timer, repaint. Bind `r` to force-refresh. ~1 hour.

---

## Security and correctness

### S1. `PolicyEngine::evaluate_cli` regex bypass via global flags / whitespace — P1, M

**What:** The engine joins args with single spaces and pattern-matches the joined string. An attacker can sneak past allow-list patterns using `git -c foo=bar push` (the `^push` regex won't match because `-c foo=bar` is the prefix), or extra whitespace, or quoted args. Noted in the original e2e report.

**Why deferred:** Fixing this properly means reworking the match semantics — probably pull out global `git` options explicitly, normalize whitespace, match against the subcommand token. That's a judgment call that should be discussed before landing.

**How to pick it up:**
  1. Write failing tests that demonstrate the bypass (`git -c foo=bar push --force origin main` should be denied)
  2. Change `evaluate_cli` to parse args into (global_opts, subcommand, subcommand_args) and match only the subcommand + its args
  3. Document the assumption that "any unknown global option is rejected"

---

### S2. Egress proxy audit always records `sandbox_id="unknown"` — P1, S

**What:** Unlike `cli_proxy`, which now gets sandbox_id from `SandboxAttribution::Fixed` (bound to the vsock), the egress proxy has no way to know which sandbox initiated an HTTPS CONNECT. It just hard-codes `"unknown"`.

**Why deferred:** Same reason as F3 — without MITM TLS termination, there's no way to add headers to the request, and there's also no way to route per-sandbox traffic through a per-sandbox listener. Depends on F3.

**How to pick it up:** When F3 lands, give each VM its own egress proxy port (or its own listener bound to the guest's NAT IP) so every connection provably came from one specific sandbox. Audit entry then uses the known id.

---

### S3. `forward_ssh_agent` is parsed but never enforced — P2, S

**What:** `CliPolicy::forward_ssh_agent` is parsed from policy files but marked `#[allow(dead_code)]`. Guest tools that need SSH (e.g. `git push` to SSH remotes) will silently fail.

**How to pick it up:** In `cli_proxy::exec`, when the matched policy's `forward_ssh_agent` is true, set `SSH_AUTH_SOCK` from the host environment in the child's env block. Document the security implication (the host agent becomes reachable from the proxy-privileged uid).

---

### S4. `abox-shim` still falls back to `getcwd()` if `ABOX_CWD` isn't set — P2, S

**What:** The shim reads `ABOX_CWD` from the environment (set by `runner.sh`) to work around a virtiofs `/workspace` getcwd quirk. If the env var isn't set (e.g. the user shells into the guest manually and runs `git` directly), the shim falls back to `getcwd(2)`, which can return the wrong path inside virtiofs on some kernels.

**How to pick it up:** Have the shim parse `/proc/self/cwd` as a path string (it's a symlink maintained by the kernel and often more reliable than `getcwd`), and compare against both `/workspace*` and host-mapped paths.

---

## Developer experience / quality

### D1. Bootstrap requires manual PATH export — P1, S

**What:** After `just bootstrap-vm`, the user must run `export PATH="$HOME/.abox/vm:$PATH"` or symlink the four binaries into `~/.local/bin`. The README and `docs/vm-setup.md` mention it but it's an extra step and easy to miss.

**How to pick it up:** Teach `bootstrap_vm.sh` to symlink the binaries into `~/.local/bin/` automatically (with a `--no-symlink` opt-out), after first checking that `~/.local/bin` is on `$PATH`. Print a clear "add to PATH" message if it isn't.

---

### D2. Hardcoded timing constants — P2, S

**What:** Several timing constants are hardcoded:
  - `run_sandbox` polls `vm_manager.info()` every 250 ms
  - `console::tail_to_stdout` polls for new bytes every 50 ms
  - Both wait up to ~5 s for their target file to appear

**How to pick it up:** Lift them into a `VmRuntimeTuning` struct in `config.rs` with sensible defaults; tests can override.

---

### D3. Bootstrap installs the musl Rust target silently — P2, S

**What:** If `x86_64-unknown-linux-musl` isn't present, `bootstrap_vm.sh` calls `rustup target add` without asking. Fine for dev laptops but surprising for users who don't expect the script to modify their rust toolchain.

**How to pick it up:** Add a `--yes` flag required to auto-install the target. Without it, fail with a clear instruction.

---

### D4. E2E phase 6 console output not asserted — P2, S

**What:** Phase 6 asserts the audit log gets a `vm-e2e` entry but doesn't verify that the guest's serial console output (alpine boot + guest init banner) actually reached the user's terminal. The console streaming fix is real but untested in CI.

**How to pick it up:** Capture phase 6's stdout, grep for `abox guest init: online`, assert present.

---

### D5. `tail_to_stdout` polling loop has no exit signal — P1, S

**What:** The console tailer runs an infinite polling loop. It's aborted by `run_sandbox`'s `handle.abort()`, but there's no graceful shutdown. In rare cases on slow systems, the last ~50 ms of output might be dropped.

**How to pick it up:** Use `tokio::sync::Notify` or a oneshot channel to let `run_sandbox` signal "VM is gone, drain and exit" so the tailer reads to EOF before stopping.

---

### D6. No CI workflow enabled — P1, S

**What:** `.github/workflows/` is gitignored. The original repo's initial plan mentioned CI but defers it. The current commit graph includes a "chore: defer CI workflow" message. With the VM MVP working end-to-end (sans VM on CI — phase 6 is gated), this is a good time to wire up GitHub Actions for phases 1–5 plus `cargo fmt/clippy/test`.

**How to pick it up:**
  1. Remove `.github/workflows/` from `.gitignore`
  2. Write `ci.yml` that runs `just ci` on push + PR
  3. Add a `just ci` recipe (probably already exists)
  4. For phase 6 to work in CI we'd need a `/dev/kvm`-capable runner (e.g. `ubuntu-latest-nested-virt` when GH offers it); for now let it skip

---

## Housekeeping

### H1. 15 commits could be squashed before merge — P2, S

**What:** The vm-e2e-mvp branch has 15 commits, several of which are "fix the reviewer nit" follow-ups. Merging as-is gives an accurate history; squashing into ~6 commits (bootstrap, protocol+types, proxy_bridge, VMM lifecycle, CLI foreground, docs+e2e) gives a cleaner `git log`.

**Why deferred:** Controller preference; user may have opinions.

---

### H2. `docs/plans/2026-04-07-vm-end-to-end-mvp.md` doesn't reflect Task 9's absorbed scope — P2, S

**What:** Task 9's implementer ended up fixing real bugs in cloud_hypervisor.rs, proxy_bridge.rs, sandbox.rs, boot_meta.rs, and abox-shim/main.rs while making phase 6 pass. The plan still lists those as "Task 6/7/8 scope". A retrospective edit on the plan (or a `docs/plans/retrospectives/` file) would keep the historical record honest.

---

### H3. `scripts/e2e_test.sh` leaves `.scratch/` artifacts if it crashes mid-run — P2, S

**What:** The `trap cleanup EXIT` in `e2e_test.sh` removes `.scratch/e2e-run-<pid>` on normal exit. If the script is killed with SIGKILL or runs into `set -u` errors before the trap registers, the scratch dir is left behind. Not a big deal but annoying.

---

## Explicitly NOT in this backlog

These were mentioned at various points but are **not planned** as follow-ups:

- Replacing Cloud Hypervisor with Firecracker — CH was chosen deliberately for virtiofs (see [`docs/decisions/001-architecture.md`](../decisions/001-architecture.md))
- Rewriting the bootstrap in Python/Rust — bash with checksums is fine for a one-time setup
- Running multiple agents per sandbox — out of scope; each sandbox is one agent
- Supporting macOS hosts — requires a different hypervisor entirely
