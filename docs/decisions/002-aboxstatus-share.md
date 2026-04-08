# ADR-002: Exit Code Propagation via `aboxstatus` Virtiofs Share

**Status:** Accepted
**Date:** 2026-04-08
**Supersedes:** —
**Related:** [ADR-001](001-architecture.md), [`docs/plans/2026-04-08-vm-e2e-hardening.md`](../plans/2026-04-08-vm-e2e-hardening.md) (Task 1)

## Context

The original VM MVP (see ADR-001 and `docs/plans/2026-04-07-vm-end-to-end-mvp.md`) mounts two virtiofs shares into every guest:

1. `workspace` (read-write) — the git worktree
2. `aboxmeta` (read-only in practice) — `boot.json` + `runner.sh`

After the guest agent exits and the VM powers off, the host orchestrator had no channel to read the agent's exit status. `SandboxOrchestrator::run_sandbox` returned a hardcoded `Ok(0)`, silently masking any non-zero exit from the guest command. Users scripting `abox run` (CI pipelines, batch agent runners) had no way to react to a failing agent.

Three options were considered:

1. **Write `/run/abox-exit-code` into the worktree.** Simplest implementation, but pollutes the user's source tree with an abox-internal file. Also race-prone if the agent writes large files concurrently, and would appear in `git status` output.
2. **Serial console exit-code marker.** Have `guest/init.sh` print a sentinel like `__ABOX_EXIT__=N` on the last console line; the host parses it from the captured console log. Rejected: parsing stdout is fragile, and the marker can be interleaved with agent output.
3. **A third virtiofs share, writable, called `aboxstatus`.** One additional `virtiofsd` process per sandbox; clean separation of concerns; the share is purpose-specific and never overlaps with user data.

A fourth option — vsock exit notification — was also considered: the guest sends a one-byte vsock message to a new port before poweroff. This was rejected because it would add a new server in `proxy_bridge.rs` for what is ultimately a single integer, and the per-VM bridge already binds vsock port 5000 for the CLI proxy.

## Decision

Adopt option 3.

The orchestrator creates `<runtime>/status-<id>/exit-code` (an empty file at boot), exports the directory via a third `virtiofsd` process, and passes a third `--fs tag=aboxstatus,...` argument to cloud-hypervisor. `guest/init.sh` mounts the share at `/abox-status` (read-write), runs `runner.sh`, captures `$?` into `/abox-status/exit-code` before `poweroff`. The host reads the file after detecting VM exit (before tearing down the status dir) and returns its contents from `run_sandbox`.

The virtiofs socket prefix was shortened from `virtiofs-` / `virtiofs-meta-` / `virtiofs-status-` to `vfs-` / `vfs-meta-` / `vfs-status-` to keep the fully-qualified socket path under Linux's `SUN_LEN` limit (108 bytes) for combinations of long worktree paths and long task ids.

## Consequences

**Positive:**
- No worktree pollution — the writable share is separate from the user's source.
- Single-purpose write target: one file, one integer. Trivially verifiable.
- Extensible without a protocol change — future fields (crash dumps, resource metrics, structured agent telemetry) can live in the same share.
- The host-side `read_exit_code(status_dir)` helper is a free function, easy to unit-test (3 new tests cover present / missing / malformed cases).

**Negative:**
- One more `virtiofsd` process per sandbox (~1 MB RSS each) and one more unix socket in `runtime_dir/`. Immaterial for the MVP's expected scale (≤10 sandboxes per host).
- The cleanup flow is now two-phase: `info()` (which detects natural VM exit) must NOT remove `status_dir`, leaving it for `run_sandbox` to read; only `stop()` (explicit teardown) and `run_sandbox` (after reading the file) remove it. The first draft missed this; it's now made explicit with a `remove_status_dir: bool` parameter on `cleanup_vm_files`.

**Drive-by improvement:**
When the guest never wrote an exit code (because the VM crashed or virtiofsd failed before `init.sh` could mount `/abox-status`), `run_sandbox` now logs a warning, rolls back the worktree like a failed VM start, and returns 1 — instead of silently returning 0. This fixed a class of false positives where catastrophic VM failures looked like clean exits.

## Alternatives Considered (and Rejected)

- **Worktree-internal file** (option 1): pollution + races.
- **Console marker parsing** (option 2): fragile.
- **Vsock exit channel**: protocol surface area for one integer.
- **`ch-remote` exit-status API**: Cloud Hypervisor does not expose guest-process exit status; this would require kernel-level hooking. Rejected as out of scope.
