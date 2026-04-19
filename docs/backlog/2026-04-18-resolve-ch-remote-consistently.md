# Resolve `ch-remote` Consistently Through the VM Binary Resolver

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P3  
**Effort:** S  
**Severity:** Medium  
**Area:** install ergonomics, snapshotting, binary resolution  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md)

## Summary

The Cloud Hypervisor adapter resolves VM binaries from the managed install location for startup and restore flows, but `pause()` and `resume()` still invoke `ch-remote` directly from `$PATH`.

That means a curl-pipe or local bootstrap install can succeed for normal VM startup and still fail later when snapshot-related commands need `ch-remote`.

## Why It Matters

This creates an avoidable split-brain install experience:

- `abox run` may work;
- `abox template create` or other pause/resume flows may fail unexpectedly;
- users get confusing behavior that depends on which subcommand they happen to use first.

Consistency here matters because binary discovery is already an explicit concern in the codebase.

That said, this is more of a polish and consistency issue than a likely `0.3.0` blocker unless snapshot-heavy workflows are expected to be central to the release.

## Current Behavior

- `start()` and restore paths use `self.resolve_binary(...)`.
- `pause()` and `resume()` call `Command::new("ch-remote")` directly.

The code therefore bypasses the resolver exactly where the project already knows the binary may live outside the normal host `$PATH`.

## Affected Code

- `crates/abox-core/src/adapters/cloud_hypervisor.rs`

## Recommended Fix

Use the same binary resolution strategy for every Cloud Hypervisor helper binary.

1. Replace direct `Command::new("ch-remote")` calls with `Command::new(self.resolve_binary("ch-remote")?)`.
2. Audit the rest of the adapter and snapshot code for any similar direct binary invocations.
3. Add tests or structured validation where possible.

## Suggested Implementation Notes

- Prefer one resolver path for `cloud-hypervisor`, `virtiofsd`, and `ch-remote`.
- If the resolver ever changes, all call sites should inherit the change automatically.

## Acceptance Criteria

- Pause and resume work when the VM tools are installed under `~/.abox/vm` but not on `$PATH`.
- Existing startup behavior continues to work unchanged.
- Tests or mocks cover the resolver being consulted for `ch-remote`.

## Validation Ideas

- Add an adapter-level test that stubs or verifies resolved binary paths.
- Run snapshot/template flows in an environment where `ch-remote` is available only via the managed install location.
