# Future Work and Roadmap

**Last updated:** 2026-08-17 (MicroSandbox migration complete — see ADR-008)

This document is the **forward-looking** companion to the historical records
under [`docs/backlog/`](backlog/) and [`docs/plans/`](plans/). Those files
are snapshots of what was deferred or planned at specific moments; this file
tells you what to work on **next** and which longer-term ideas are worth
keeping in mind.

---

## The ADR-008 migration is complete

The [ADR-008](decisions/008-microsandbox-runtime-and-product-boundary.md)
workstream — migrating the sandbox substrate to MicroSandbox and deleting
the bespoke microVM stack — has landed in full. MicroSandbox (libkrun) is
the **sole** runtime; the legacy Cloud Hypervisor backend, memory
snapshots/templates, `abox attach`, and the raw rootfs/kernel/bootstrap
pipeline have been deleted. abox now concentrates on agent governance
(worktrees, policy, host-held credentials, attribution, audit) and delegates
generic isolation to the pinned runtime.

Residual work that survives the migration is tracked below as ordinary
items (runtime-process hardening, HTTP/2 in the request broker).

---

## TL;DR — recommended priority order

1. **Runtime-process confinement feasibility** — implement only after the
   Linux gate in [ADR-009](decisions/009-runtime-process-confinement.md) can
   prove that the actual `msb` child is constrained. The runtime is pinned and
   qualified, but it still runs with the invoking user's ambient authority
   (see [`security-model.md`](security-model.md#defense-in-depth)).
2. **F5** — TUI dashboard refresh. Cosmetic; nice-to-have (keep it
   task/status focused, not a generic sandbox dashboard).
3. **HTTP/2 support in the request broker** — see below.

---

## Open items

### Runtime-process confinement — P1, M

Confine the actual MicroSandbox `msb` child process so a runtime compromise is
not automatically host-user compromise. [ADR-009](decisions/009-runtime-process-confinement.md)
sets the prerequisite Linux Landlock feasibility gate and rules out applying
Landlock to abox's normal Tokio process. Today the mitigation is the exact
version pin plus qualified upgrades ([`runtime-upgrades.md`](runtime-upgrades.md)).

### HTTP/2 ALPN in the request broker — P2, M

The TLS-terminating proxy negotiates HTTP/1.1 only. Clients that
require HTTP/2 (or negotiate it via ALPN) will fall back to HTTP/1.1,
which works but is suboptimal. Adding `h2` ALPN support would cover
this edge case.

---

## Longer-term ideas

These are speculative — listed so they're not lost, not as commitments.

### L3. Multi-tenant `abox-proxyd` daemon

A real multi-tenant deployment (one machine, many users) would want
the daemon as the authority, with per-uid socket isolation and a shared
audit log. Big design lift; probably needs its own ADR.

### L4. Structured guest telemetry / execution receipts

Structured per-task telemetry beyond exit code — wall time, network
destinations, CLI proxy call counts, authorized/denied effects — delivered
as an "execution receipt" through the runtime's result channel.

### L5. Ephemeral worktree mode

`abox run --ephemeral` already auto-cleans the sandbox, but still
creates a branch. A `--no-branch` mode that uses `git worktree --detach`
would be useful for read-only "explore the codebase" agents.

### L7. Observability dashboard

A `/metrics` endpoint exposing per-sandbox histograms (request count,
latency buckets, denial reasons) for Prometheus ingestion.

### L8. Encrypted audit log

Age-encrypt the JSONL audit log on rotation for deployments where the
log itself is sensitive.

### L9. Expand the glibc profile family

Now that the libc axis exists (`python-glibc`), adding `node-glibc` and
`rust-glibc` profiles is a one-Dockerfile-plus-one-manifest-entry change
each, following the same guest contract in [`images/`](../images/).

### L10. Profile naming symmetry: `python-musl` alias

A `python-musl` alias for the existing `python` profile would make the
libc axis read consistently across all profile names, without renaming or
breaking existing configurations.

## Closed / historical

- **MicroSandbox migration (ADR-008)** — complete; see above and the
  CHANGELOG. Items it superseded (bespoke-VM CI runners, bootstrap SHA
  verification, cgroup wrappers, a separate macOS-host qualification track)
  are closed with it.
- **Repository profile and doctor steering** — `abox project init` now
  detects common Rust, Node, and Python markers advisory-only; `abox doctor`
  identifies a scientific-Python profile mismatch.
- Earlier closed backlog and roadmap items are recorded in
  [`docs/backlog/2026-04-08-vm-e2e-mvp-followups.md`](backlog/2026-04-08-vm-e2e-mvp-followups.md)
  and [`docs/plans/`](plans/).
