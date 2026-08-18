# ADR-009: Runtime-Process Confinement

**Date:** 2026-08-17
**Status:** Accepted — feasibility gate before enforcement

## Context

The MicroSandbox `msb` process is part of abox's trusted computing base. It
currently runs with the invoking user's ambient authority, so a compromise of
the runtime could access the same host credentials and files as that user.
This is the remaining defence-in-depth gap called out by ADR-008 and the
security model.

MicroSandbox is the sole runtime. abox must not reintroduce a second runtime or
put hypervisor details into the runtime-neutral domain port merely to solve this
problem.

## Decision

abox will pursue a Linux confinement implementation only after a feasibility
gate proves that it restricts the *actual* `msb` child process. The initial
target is Landlock with `no_new_privs`; AppArmor is optional operator-managed
defence in depth, not a portability requirement. macOS has no equivalent
v0.7.2 enforcement path and must report that confinement is unavailable rather
than imply parity.

The preferred implementation is an upstream MicroSandbox child pre-exec hook.
If that is unavailable, abox may use a small host-owned `msb` guard selected
through MicroSandbox's documented executable override. The guard must derive a
task-specific allowlist from host configuration and the runtime spec, apply
Landlock immediately before `exec` of the real `msb`, and fail closed in
enforcement mode.

Applying Landlock in abox's normal Tokio process is prohibited: Landlock is
inherited by descendants of the restricted thread, so that approach can either
miss `msb` or permanently constrain abox's command/request brokers.

## Feasibility gate

Before exposing an enforcement option, a live test must prove that the confined
runtime can access only its executable/library/firmware/image state, the task
worktree, declared caches, its task control sockets, and required KVM devices.
It must be unable to read host credential stores, abox CA/audit keys, sibling
worktrees, or the primary checkout. The test suite must also prove fail-closed
behaviour when the kernel ABI, guard, policy, or real runtime path is missing.

If the gate fails, abox retains its exact runtime pin and qualified-upgrade
process without claiming process confinement. Any MicroSandbox dependency
upgrade required for the preferred hook is a dedicated qualified PR under
ADR-008, not an incidental v0.7.2 dependency change.

## Consequences

- Runtime-process confinement remains host-owned configuration; repositories
  cannot enable, weaken, or configure it.
- A shipped implementation changes runtime code and therefore requires the
  live `just e2e-runtime` gate and runtime attestation.
- The scope is filesystem/device authority. Network policy remains owned by
  the existing abox policy compiler and MicroSandbox network plan.
