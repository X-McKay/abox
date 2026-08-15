# ADR-008: MicroSandbox Runtime and the abox Product Boundary

**Date:** 2026-08-15
**Status:** Accepted

## Context

abox today owns both its product layer (task/worktree orchestration, repo
behavior trust, command and request authorization, host-held credentials,
tamper-evident audit) and a large amount of generic sandbox infrastructure:
Cloud Hypervisor process management, kernel and raw-rootfs construction,
virtiofsd lifecycle and hardening, guest boot metadata, vsock bridging, a TLS
CA and HTTPS MITM proxy, network policy transport, snapshot plumbing, and VM
installation scripts.

That infrastructure is increasingly commodity. MicroSandbox provides
hardware-isolated microVM execution, OCI image handling, filesystem brokering,
mount isolation, resource limits, network isolation with DNS/SSRF protections,
TLS interception infrastructure, host-scoped secret substitution, and disk
snapshots — the generic substrate abox currently maintains by hand. Products
such as Docker Sandboxes validate that this layer has commoditized; they are
evidence for the decision, not a target backend.

abox's differentiated value is **what an autonomous agent is authorized to do
once isolated**, not the isolation mechanics themselves.

## Decisions

### 1. MicroSandbox is the target and eventual sole runtime

abox migrates its sandbox substrate to MicroSandbox. After the migration
completes, MicroSandbox is the **only** supported runtime. abox deliberately
does not become a runtime plugin framework:

- multiple security runtimes create a permanent compatibility matrix;
- semantics would degrade to the weakest common denominator;
- security behavior could drift by provider;
- every feature would need per-backend qualification;
- the runtime abstraction would itself become product surface.

### 2. Cloud Hypervisor support is transitional

The existing Cloud Hypervisor implementation remains only as a migration
reference and rollback path while MicroSandbox parity is proven. It receives
no new features — migration-critical fixes only. Deletion criteria (all must
hold before the default switches, and removal follows one release later):

1. task/worktree parity;
2. command broker parity;
3. credential isolation parity;
4. safe/scoped/open parity or documented improvement;
5. audit attribution parity;
6. service workflows required by supported use cases work;
7. environment profiles have OCI replacements;
8. CI/E2E/security suites are stable on MicroSandbox;
9. install/doctor experience is simpler than before;
10. rollback has been exercised.

### 3. abox owns agent authorization and orchestration, not generic sandbox infrastructure

abox keeps and deepens: task-to-branch/worktree orchestration, repo behavior
trust and approval, action-level authorization for privileged host commands
(`git`, `gh`), request-level authorization for credential-bearing API calls,
host-held credentials, per-task attribution, tamper-evident audit,
deterministic task execution and result collection, and divergence/merge/task
lifecycle semantics.

abox delegates to the runtime: microVM execution, VMM/kernel/boot mechanics,
filesystem brokering, OCI image handling, generic mount isolation,
CPU/memory/lifetime limits, generic network isolation, DNS/SSRF protection,
generic TLS interception, simple host-scoped secret substitution, and disk
snapshots.

The feature test going forward: a feature belongs in abox if it makes
autonomous agents more least-privileged, governable, attributable, auditable,
deterministic, reviewable, task-oriented, or safe to grant external side
effects. A feature whose primary value is making a generic sandbox nicer
(VMM selection, SSH dev environments, IDE integration, image catalogs,
sandbox dashboards) is out of scope.

### 4. Security semantics remain owned by abox

Replacing the runtime must not silently change what an agent is authorized to
do. Where MicroSandbox cannot represent an abox policy exactly, abox either
keeps enforcing that policy in a host-side abox component or rejects the
configuration at planning/launch time. No compatibility shim may silently
widen or downgrade an authorization rule. In particular, abox `open` mode
never compiles to unrestricted networking: it always excludes host, loopback,
private ranges, link-local, and cloud metadata.

### 5. The runtime is host-owned configuration

`.abox/project.toml` expresses task behavior and environment intent
(profiles, network mode, caches, services). It never selects or weakens the
host isolation boundary. Any transitional runtime selector lives in host
config or environment only and is removed when the legacy backend is deleted.

### 6. Substrate expansion is frozen

While the migration is underway, new features must not add new
VMM/virtiofs/rootfs/network infrastructure unless required for the migration
itself. `docs/future-work.md` items that expand the bespoke substrate are
superseded by this ADR.

## Consequences

- The domain port becomes runtime-neutral (`SandboxRuntimePort` /
  `SandboxRuntimeSpec`); kernel paths, raw image paths, hypervisor API
  sockets, virtiofsd sockets, and snapshot mechanics move into adapters and
  are eventually deleted.
- Guest environment profiles keep their user-facing names (`base`, `node`,
  `python`, `python-glibc`, `rust`) but are backed by pinned OCI images
  instead of raw rootfs artifacts.
- The MicroSandbox dependency is pinned exactly during the migration and
  updated only through dedicated dependency PRs qualified by the runtime
  contract and security test suites.
- The README's first-order product claim shifts from "microVM sandbox" to
  "least-privilege execution and authorization layer for autonomous coding
  agents."
