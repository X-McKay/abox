# Future Work and Roadmap

**Last updated:** 2026-04-12 (credential forwarding feature landed)

This document is the **forward-looking** companion to
[`docs/backlog/2026-04-08-vm-e2e-mvp-followups.md`](backlog/2026-04-08-vm-e2e-mvp-followups.md).
The backlog file is a **historical** record — a snapshot of what was
deferred from a specific branch at a specific moment. This file tells
you what to work on **next** and which longer-term ideas are worth
keeping in mind.

---

## What landed in the 2026-04-10 priorities roadmap

Five priorities were executed from
[`docs/plans/2026-04-09-priorities-roadmap.md`](plans/2026-04-09-priorities-roadmap.md):

| Priority | What landed |
|----------|-------------|
| **P2** Stabilization (N2-N7) | SUN_LEN guard, silent failure test + stderr warning, shim CWD tests, detach e2e test, aarch64 bootstrap parameterization |
| **P5** Runtime controls | `--timeout` (exit 124, graceful+force kill) and `--ephemeral` (auto-cleanup) flags |
| **P1** HTTPS credential injection | TLS-terminating MITM proxy, CA generation + leaf signing, header injection, per-sandbox egress ports, bypass list, `abox ca` CLI, ADR-003 |
| **P3** Snapshot/template fast startup | `template create --from`, `StartMode::Restore`, virtiofsd metadata, benchmark harness (~100ms warm starts vs ~1s cold) |
| **P4** Installation story | GitHub Actions release workflow, VM assets bundle, SHA256SUMS, `scripts/install.sh` one-command installer |

Current state: 120 tests pass, clippy clean, 46/46 e2e assertions green
across all 7 phases.

## What landed in the 2026-04-12 credential forwarding work

Closed the remaining gap in the OAuth tool authentication story:

- **Per-sandbox egress proxy via vsock** — `run_sandbox()` now spawns a
  per-sandbox `EgressProxyServer` on vsock port 5001. Guest `init.sh` bridges
  vsock to TCP `127.0.0.1:18443` via socat. Audit entries now carry correct
  per-sandbox attribution.
- **Stub credential files** — `[guest] credential_files` config supports an
  optional `stub` that writes a placeholder credential file into the guest,
  satisfying tools like Claude Code that check for local credentials before
  making any network calls.
- **`credential_file` + `json_path` egress policy fields** — alternative to
  `env_var` for reading the real token from a JSON credential file on the host
  at proxy request time.
- **`NODE_EXTRA_CA_CERTS` injection** — Node.js-based tools now trust the abox
  MITM CA without a rootfs rebuild.

---

## TL;DR — recommended priority order

1. **CI runner with `/dev/kvm`** — Phases 6-7 of e2e only run when
   VM artifacts are present. A self-hosted runner or scheduled nightly
   job would catch regressions in the VM boot path.
2. **F5** — TUI dashboard refresh. Cosmetic; nice-to-have.
3. **aarch64 SHA verification** — N7 parameterized bootstrap for
   aarch64 but SHA256 checksums are placeholders. Needs ARM hardware
   to verify.
4. **HTTP/2 support in MITM proxy** — Current proxy is HTTP/1.1 only.
   Some clients negotiate HTTP/2 via ALPN.

Everything else in "Longer-term ideas" is a "would be nice" not
a "must do".

---

## Closed backlog items (2026-04-10)

| Item | Status | How |
|---|---|---|
| **F3** HTTPS credential injection | **Done** | P1: TLS MITM proxy + header injection + ADR-003 |
| **S2** Egress audit attribution | **Done** | Per-sandbox egress proxy via vsock + credential file injection |
| **F4** `abox template create` wiring | **Done** | P3: CLI + restore mode + virtiofsd metadata |
| **N2** SUN_LEN guard | **Done** | P2: `anyhow::ensure!` in `CloudHypervisorAdapter::new()` |
| **N3** Silent failure test | **Done** | P2: `ExitingMockVmNoStatus` + rollback assertion |
| **N4** stderr warning | **Done** | P2: `eprintln!` before `tracing::warn!` |
| **N5** Shim CWD tests | **Done** | P2: `resolve_cwd()` extraction + 3 tests + serial enforcement |
| **N6** Detach integration test | **Done** | P2: e2e phase 6 lifecycle test |
| **N7** aarch64 bootstrap | **Done** | P2: parameterized URLs (SHA placeholders remain) |

---

## Open items

### CI runner with `/dev/kvm` — P1, M

Phases 6-7 of `e2e_test.sh` (full VM boot, agent lifecycle, credential
injection) are gated on VM artifacts. They pass locally but are skipped
on stock GitHub runners. A self-hosted runner with KVM or a nightly
scheduled job would close this gap.

### aarch64 SHA256 verification — P2, S

`bootstrap_vm.sh` is parameterized for aarch64 but all SHA256 checksums
for ARM artifacts are placeholder zeros. Needs someone with ARM hardware
(Raspberry Pi 5, Ampere, Apple Silicon Linux VM) to download the
artifacts and fill in real checksums.

### HTTP/2 ALPN in MITM proxy — P2, M

The TLS-terminating proxy negotiates HTTP/1.1 only. Clients that
require HTTP/2 (or negotiate it via ALPN) will fall back to HTTP/1.1,
which works but is suboptimal. Adding `h2` ALPN support would cover
this edge case.

---

## Longer-term ideas

These are speculative — listed so they're not lost, not as commitments.

### L1. Per-sandbox resource limits (cgroup v2)

A cgroup v2 wrapper around the cloud-hypervisor process could enforce
memory and CPU limits per sandbox. `--memory` and `--cpus` control VM
allocation but don't limit the VMM process itself.

### L3. Multi-tenant `abox-proxyd` daemon

A real multi-tenant deployment (one machine, many users) would want
the daemon as the authority, with per-uid socket isolation and a shared
audit log. Big design lift; probably needs its own ADR.

### L4. Structured guest telemetry

The `aboxstatus` virtiofs share could carry structured agent telemetry
beyond exit code — wall time, peak RSS, network bytes, CLI proxy call
counts. Useful for cost tracking and anomaly detection.

### L5. Ephemeral worktree mode

`abox run --ephemeral` already auto-cleans the sandbox, but still
creates a branch. A `--no-branch` mode that uses `git worktree --detach`
would be useful for read-only "explore the codebase" agents.

### L6. macOS host support

Requires a different hypervisor (Hypervisor.framework + Virtualization.framework,
or Lima/Colima). Out of scope but a recurring ask.

### L7. Observability dashboard

A `/metrics` endpoint exposing per-sandbox histograms (request count,
latency buckets, denial reasons) for Prometheus ingestion.

### L8. Encrypted audit log

Age-encrypt the JSONL audit log on rotation for deployments where the
log itself is sensitive.
