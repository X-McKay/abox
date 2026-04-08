# Future Work and Roadmap

**Last updated:** 2026-04-08 (after the `vm-e2e-hardening` branch landed)

This document is the **forward-looking** companion to
[`docs/backlog/2026-04-08-vm-e2e-mvp-followups.md`](backlog/2026-04-08-vm-e2e-mvp-followups.md).
The backlog file is a **historical** record — a snapshot of what was
deferred from a specific branch at a specific moment, with the
discussion that produced each item kept verbatim. This file is the
opposite: it tells you what to work on **next**, in what order, and
which longer-term ideas are worth keeping in mind even if they aren't
ready to plan yet.

When the next sprint lands, file a new dated backlog from this
roadmap and prune what got done.

---

## TL;DR — recommended priority order

1. **F3** — HTTPS credential injection. Spec is written:
   [`docs/plans/2026-04-08-credential-injection.md`](plans/2026-04-08-credential-injection.md).
   Until this lands, the README's "credentials never enter the VM"
   claim is only half-true (CLI side correct, HTTPS side passthrough).
   Estimated 2-3 working days.
2. **S2** — Egress audit attribution. Falls out almost for free
   once F3 lands (per-sandbox listener is part of the F3 plan).
3. **F4** — `abox template create` wiring. The snapshot/restore
   plumbing exists end-to-end; this is just CLI surface work to
   expose it.
4. **CI runner with `/dev/kvm`** — see "New items discovered" below.
   Phase 6 of the e2e is currently only verified by hand.
5. **F5** — TUI dashboard refresh. Cosmetic; nice-to-have.

Everything else in "Longer-term ideas" is a "would be nice" not
a "must do".

---

## Open backlog items (status as of vm-e2e-hardening)

For full descriptions, see the
[backlog file](backlog/2026-04-08-vm-e2e-mvp-followups.md).

| Item | Priority | Effort | Status | Why it matters now |
|---|---|---|---|---|
| **F3** HTTPS credential injection | P0 | L | spec written | The README claims credentials are injected; the egress proxy is currently a passthrough. Closing this fixes both correctness and the marketing copy. |
| **S2** Egress audit attribution | P1 | S | blocked on F3 | Without per-sandbox listeners, every egress audit row is `sandbox_id="unknown"`. F3 introduces per-sandbox listeners for free. |
| **F4** `abox template create` wiring | P2 | M | open | Exposes existing snapshot/restore plumbing. Unlocks fast clone-from-template workflows. |
| **F5** TUI dashboard refresh | P2 | S | open | Cosmetic — the dashboard panel currently never repaints. |
| **H1** Squash 15 commits before merge | P2 | S | controller call | The vm-e2e-mvp branch has 15 commits that could be squashed to ~6. Pure cosmetic. |

---

## New items discovered during vm-e2e-hardening

These came up while executing `2026-04-08-vm-e2e-hardening.md`. They
aren't in the original backlog because they didn't exist there; the
hardening branch surfaced them or created them.

### N1. CI runner with `/dev/kvm` (so phase 6 is gated by CI, not by humans) — P1, M

**What:** The new GitHub Actions workflow
([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs
phases 1-5 of the e2e on every push and pull_request to main / develop.
Phase 6 (full live VM boot) is **gated** on `~/.abox/vm/` artifacts
existing, so it's automatically skipped on stock GitHub runners — they
don't expose `/dev/kvm`.

This means a regression in `cloud_hypervisor.rs`, `proxy_bridge.rs`,
`guest/init.sh`, or the `aboxstatus` exit-code path can land on main
with green CI.

**How to fix:**
- Option A: Add a self-hosted runner with KVM enabled. Bare-metal
  Linux box, label `kvm-enabled`, run phase 6 there.
- Option B: Use GitHub Actions' nested-virt runners when they're
  generally available (was a beta as of 2025).
- Option C: Run phase 6 on a scheduled cron job (nightly) on a
  self-hosted runner; not blocking on PRs but at least catches
  regressions within 24 h.

The cheapest option that gets meaningful coverage is C.

### N2. SUN_LEN (108-byte unix socket path limit) is undocumented — P2, S

**What:** Linux's `sockaddr_un.sun_path` is exactly 108 bytes. Cloud
Hypervisor and virtiofsd both bind unix sockets like
`<runtime>/vfs-status-<task-id>.sock`. With long worktree paths
(e.g., the e2e test's `.scratch/e2e-run-NNNNN/r/`), the
fully-qualified path can overflow before you notice.

The hardening branch fixed it twice — once by shortening the
prefixes from `virtiofs-*` to `vfs-*`, and once by moving the e2e
test's `runtime_dir` from `state/run` to `r`. But there's nothing in
the code that **prevents** a future change from re-introducing the
problem.

**How to fix:**
- Add a startup check in `CloudHypervisorAdapter::new` (or
  `AboxConfig::ensure_dirs`) that computes the longest possible
  socket path with a sentinel task id (`vfs-status-` +
  `MAX_TASK_ID_LEN` chars + `.sock`) and bails out at config load
  time if it exceeds 108 bytes.
- Document `MAX_TASK_ID_LEN` (currently undefined, effectively
  ~32 chars given typical runtime_dir paths).
- Mention this in `docs/vm-setup.md` Troubleshooting (already done
  in this branch — see the "SUN_LEN" entry).

### N3. Silent VM failure → exit 1 + worktree rollback semantics need a test — P2, S

**What:** During Task 5 of the hardening branch, I added a fallback
in `run_sandbox`: if the guest never wrote `/abox-status/exit-code`,
log a warning, roll back the worktree, and return 1. This restored a
broken phase 4 e2e assertion. The fix is correct but **only** verified
by the integration test that uses an `ExitingMockVm` with a pre-staged
exit-code file. There is no test for the **missing** exit-code path.

**How to fix:** Add an `ExitingMockVmNoStatus` variant whose
`status_dir()` returns `Some(<empty dir>)`, and a test that asserts
`run_sandbox` returns `Ok(1)` *and* the worktree no longer exists on
disk. ~30 minutes.

### N4. `tracing_subscriber` env filter swallows the new "rolled back" warning — P2, S

**What:** The default env filter is `warn,abox=info`. The
`tracing::warn!` from N3's silent-failure path **does** print, but
the message is one line buried in a blob of cloud-hypervisor and
virtiofsd output. Easy for a user to miss when their `abox run`
exits 1 unexpectedly.

**How to fix:** Print the warning to stderr in plain text (not
through `tracing`) when the silent-failure path triggers:

```rust
eprintln!(
    "abox: sandbox '{task_id}' did not report an exit code; \
     rolling back worktree (the VM may have crashed before guest \
     init ran — check the console log)"
);
```

Two-line change.

### N5. The shim's `/proc/self/cwd` fallback is untested in CI — P2, S

**What:** The S4 fix in [`0600339`](https://github.com/X-McKay/abox/commit/0600339)
made `abox-shim` prefer `/proc/self/cwd` over `getcwd(2)`. It only
runs inside the guest, so phase 6 is the only path that exercises
it. If S4 regressed, only manual phase-6 runs would catch it.

**How to fix:** Add a unit test in `crates/abox-shim/src/main.rs`
that exercises the resolution chain via a mockable `CwdResolver`
trait (right now the resolution is inline in `run()`). Light
refactor to make it testable. ~1 hour.

### N6. `abox run --detach` has no inline test that the spawned child actually runs — P2, S

**What:** Task 5 (F1) added `--detach` and three unit tests for
`strip_detach_flag`. The argv-rebuild logic is tested but the
actual fork+exec+pidfile flow is not — only the help text is
verified to mention `--detach`.

**How to fix:** Add an integration test that runs
`abox run --task X --detach -- /bin/sleep 5`, asserts the pid file
exists, asserts `kill -0 <pid>` succeeds, then `abox stop X` and
asserts the pid file is gone. Needs a test config that points
runtime_dir into a tempdir. ~1 hour.

### N7. Bootstrap is x86_64-only — P2, M

**What:** Every URL and SHA256 in `bootstrap_vm.sh` is for x86_64.
Cloud Hypervisor supports aarch64; the kernel and Alpine miniroot
also have aarch64 builds; virtiofsd is portable. The blocker is
that `abox-shim` is built for `x86_64-unknown-linux-musl`
unconditionally.

**How to fix:** Detect host arch (`uname -m`), parameterize the
URLs and SHAs, build the shim for `aarch64-unknown-linux-musl` on
ARM hosts. Test on an Apple Silicon Linux VM or a Raspberry Pi 5.
Half-day for a careful pass.

---

## Longer-term ideas

These are speculative — listed so they're not lost, not as commitments.

### L1. Per-sandbox resource limits (cgroup v2)

Today, `abox run` lets each VM have whatever memory/cpu the
`vm_defaults` say. There's no way to say "this agent gets at most
500 MB and 30 minutes of wall time". A cgroup v2 wrapper around
the cloud-hypervisor process would do it. Useful when running many
agents in parallel.

### L2. Snapshot-and-restore for fast cloning

Cloud Hypervisor supports `pause` + `snapshot` + `restore`. The
hexagonal architecture has the trait for it (`SnapshotManager`)
but no real implementation. F4 partially unlocks this; a real
fast-clone path (boot once, snapshot, restore in <100 ms per new
sandbox) would change the per-sandbox latency from "seconds" to
"milliseconds" and make abox usable for sub-second batch agent
runs.

### L3. Multi-tenant `abox-proxyd` daemon

Right now the per-VM bridge runs inside the orchestrator process,
and the standalone `abox-proxyd` daemon is mostly used for tests.
A real multi-tenant deployment (one machine, many users, each
running their own agents) would want the daemon to be the
authority, with per-uid socket isolation and a shared audit log.
Big design lift; probably needs its own ADR.

### L4. Structured guest telemetry

The `aboxstatus` virtiofs share is currently used for one thing:
a single integer exit code. The same channel could carry
structured agent telemetry — wall time, peak RSS, network bytes
in/out, cli proxy call counts — without changing the protocol
between guest and host. Would be useful for cost tracking and
anomaly detection.

### L5. Support for `git worktree --detach` style ephemeral runs

Sometimes you want a sandbox that doesn't create a new branch at
all — just runs the agent against an ephemeral worktree of `main`,
no commit, no merge target, no `agent/<task>` branch. Would be
useful for "explore the codebase" agents.

### L6. macOS host support

Requires a different hypervisor entirely (Hypervisor.framework via
`vmnet` + `Virtualization.framework`, or Lima/Colima as a slow
fallback). Out of scope for the current architecture but a
recurring ask.

### L7. Observability dashboard

Currently there are three signals: stdout (console streamer),
stderr (tracing), and the audit log JSONL file. A `/metrics`
endpoint exposing per-sandbox histograms (request count, latency
buckets, denial reasons) would let you point Prometheus or
similar at it. Cheap to add once one user actually wants it.

### L8. Encrypted audit log

The audit log is plaintext JSONL. For deployments where the audit
log itself is sensitive (it records every `git push --force` an
agent attempted, which is a leak vector), age-encrypt the log file
on rotation. Trivial integration; no decision yet on whether
anyone needs it.

---

## What this branch landed (for context)

If you're reading this fresh, see the
[plan retrospective](plans/2026-04-07-vm-end-to-end-mvp.md#retrospective-added-2026-04-08)
on the original VM MVP plan, the
[hardening plan itself](plans/2026-04-08-vm-e2e-hardening.md), and
the
[backlog status table](backlog/2026-04-08-vm-e2e-mvp-followups.md#status-updated-2026-04-08-after-vm-e2e-hardening)
for the canonical "what got done in the hardening pass" answer.

In one sentence: 13 P0/P1/P2 items closed, F3 deferred to its own
spec, F4/F5 explicitly skipped, all behavior changes covered by
TDD, branch ends green on `cargo check` + `cargo test --workspace`
(92 tests) + `./scripts/e2e_test.sh` (36/36 assertions including
phase 6 live VM).
