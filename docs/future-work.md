# Future Work and Roadmap

**Last updated:** 2026-08-15 (MicroSandbox migration decision — see ADR-008)

> **Strategic note (2026-08-15):** abox is migrating its sandbox substrate to
> MicroSandbox and will stop maintaining a bespoke microVM stack. See
> [ADR-008](decisions/008-microsandbox-runtime-and-product-boundary.md).
> Items below that expand the Cloud Hypervisor/virtiofsd/kernel/rootfs
> substrate are **superseded** and marked accordingly; do not invest in them.

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

1. **MicroSandbox migration (ADR-008)** — the active workstream. Runtime
   substrate parity first, then authorization parity, then deletion of the
   bespoke VM stack. See `docs/decisions/008-*.md` for phase gates.
2. **F5** — TUI dashboard refresh. Cosmetic; nice-to-have (keep it
   task/status focused, not a generic sandbox dashboard).
3. **HTTP/2 support in MITM proxy** — relevant only to whatever request-aware
   proxy path abox retains after the migration.

Superseded by ADR-008 (do not invest):

- **CI runner with `/dev/kvm` for Cloud Hypervisor e2e** — the boot path it
  would guard is being deleted; runtime e2e moves to the MicroSandbox contract
  suite.
- **aarch64 SHA verification for `bootstrap_vm.sh`** — the bootstrap pipeline
  is deleted with the raw VM stack; OCI profile images replace it.

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

### MicroSandbox migration — P0, XL

The active workstream (ADR-008). Ordered gates: runtime-neutral port →
offline MicroSandbox adapter → OCI profiles → direct-vsock command broker →
egress parity → native network policy compilation → native simple secrets →
default switch → Cloud Hypervisor deletion.

### ~~CI runner with `/dev/kvm` — P1, M~~ (superseded by ADR-008)

Phases 6-7 of `e2e_test.sh` guard the Cloud Hypervisor boot path, which is
being deleted. Runtime e2e coverage moves to the MicroSandbox runtime
contract suite.

### ~~aarch64 SHA256 verification — P2, S~~ (superseded by ADR-008)

`bootstrap_vm.sh` is deleted with the raw VM stack; profile delivery moves
to multi-arch OCI images.

### HTTP/2 ALPN in MITM proxy — P2, M

The TLS-terminating proxy negotiates HTTP/1.1 only. Clients that
require HTTP/2 (or negotiate it via ALPN) will fall back to HTTP/1.1,
which works but is suboptimal. Adding `h2` ALPN support would cover
this edge case.

---

## Longer-term ideas

These are speculative — listed so they're not lost, not as commitments.

### ~~L1. Per-sandbox resource limits (cgroup v2)~~ (superseded by ADR-008)

Resource limits are delegated to the MicroSandbox runtime. Any residual
host-process confinement work belongs to the runtime-hardening phase of the
migration (AppArmor/Landlock around the runtime process), not a bespoke
cgroup wrapper for cloud-hypervisor.

### L3. Multi-tenant `abox-proxyd` daemon

A real multi-tenant deployment (one machine, many users) would want
the daemon as the authority, with per-uid socket isolation and a shared
audit log. Big design lift; probably needs its own ADR.

### L4. Structured guest telemetry / execution receipts

Structured per-task telemetry beyond exit code — wall time, network
destinations, CLI proxy call counts, authorized/denied effects. Post-ADR-008
this becomes the "execution receipt" concept delivered through the runtime's
result channel, not the `aboxstatus` virtiofs share.

### L5. Ephemeral worktree mode

`abox run --ephemeral` already auto-cleans the sandbox, but still
creates a branch. A `--no-branch` mode that uses `git worktree --detach`
would be useful for read-only "explore the codebase" agents.

### L6. macOS host support

A recurring ask. Becomes tractable after ADR-008: MicroSandbox (libkrun)
supports macOS hosts, so this is a qualification exercise on the single
runtime rather than a second hypervisor integration.

### L7. Observability dashboard

A `/metrics` endpoint exposing per-sandbox histograms (request count,
latency buckets, denial reasons) for Prometheus ingestion.

### L8. Encrypted audit log

Age-encrypt the JSONL audit log on rotation for deployments where the
log itself is sensitive.

### L9. Expand the glibc profile family

Now that the libc axis exists (`python-glibc`), adding `node-glibc` and
`rust-glibc` profiles is a one-Dockerfile-plus-one-enum-arm change each,
built on the same `produce_glibc_base` path.

### L10. Profile naming symmetry: `python-musl` alias

A `python-musl` alias for the existing `python` profile would make the
libc axis read consistently across all profile names, without renaming or
breaking existing configurations.

### L11. `abox init` / `abox doctor` steering for data-science users

When a user's repo installs numpy/pandas/scipy without `python-glibc`, an
`abox doctor` hint (or `abox init` question) could steer them to the right
profile before they hit a confusing wheel-resolution failure.
