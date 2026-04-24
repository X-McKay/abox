# ADR 004: virtiofsd Process Hardening

**Date:** 2026-04-23
**Status:** Accepted

## Context

abox uses three `virtiofsd` instances per sandbox to expose host directories
to the guest VM via the virtio-fs protocol:

| Instance  | Shared directory              | Access | Purpose                          |
|-----------|-------------------------------|--------|----------------------------------|
| workspace | `~/.abox/worktrees/<task-id>` | rw     | Git worktree for the agent       |
| meta      | `<runtime>/meta-<task-id>`    | ro     | Boot metadata, CA cert, runner   |
| status    | `<runtime>/status-<task-id>`  | rw     | Guest exit-code reporting        |

`virtiofsd` runs as a host process with direct access to the host filesystem.
A vulnerability in `virtiofsd` (e.g. a path-traversal bug in FUSE request
handling) could allow the guest to read or write host files outside the
intended shared directory. This is the primary host-escape vector for the
virtiofs layer.

## Decision

Three hardening measures are applied to all `virtiofsd` instances:

### 1. `--sandbox=namespace` on all three instances

Previously, only the workspace instance used `--sandbox=namespace`. The meta
and status instances were spawned without it, meaning they ran in the host's
user/mount/PID namespaces with no process-level confinement.

`--sandbox=namespace` causes `virtiofsd` to call `unshare(CLONE_NEWUSER |
CLONE_NEWNS | CLONE_NEWPID)` before entering the FUSE event loop. This
confines the process to its own namespace tree, so that even if a path-
traversal bug allows the guest to escape the shared directory at the FUSE
level, the virtiofsd process itself cannot access host mounts or host PIDs.

All three instances now use `--sandbox=namespace`.

### 2. `--log-level=warn` on all three instances

The default log level for `virtiofsd` is `info`, which includes per-request
FUSE operation logs. These logs can reveal:

- The full paths of files accessed by the guest (information disclosure).
- Timing information useful for side-channel analysis.
- Internal virtiofsd state that aids exploit development.

Setting `--log-level=warn` suppresses all operational logs and retains only
warnings and errors, which are the only entries relevant to operators. This
reduces the information available to an attacker who gains read access to
host logs (e.g. via a separate vulnerability or misconfigured log forwarding).

### 3. AppArmor mandatory access control profile

An AppArmor profile is provided at `apparmor/usr.bin.virtiofsd`. It confines
`virtiofsd` to:

- The specific runtime and worktree directories it legitimately accesses.
- Unix socket creation (no network access).
- `/dev/fuse` for FUSE operations.
- Explicit denies for sensitive paths (`/etc/shadow`, `/root/**`,
  `/proc/*/mem`, etc.).

The profile uses variables (`@{ABOX_RUNTIME_DIR}`, `@{ABOX_STATE_DIR}`) that
operators must adjust to match their deployment. Installation instructions are
in the profile header.

AppArmor provides defence-in-depth: even if `--sandbox=namespace` is bypassed
(e.g. on a kernel without unprivileged user namespaces), the MAC policy still
restricts what `virtiofsd` can read or write.

## Consequences

### Positive

- All three `virtiofsd` instances are now process-isolated via user namespaces.
- Log volume is reduced, limiting information disclosure via log access.
- An AppArmor profile provides a second confinement layer independent of
  namespace isolation.

### Negative / Trade-offs

- `--sandbox=namespace` requires unprivileged user namespaces
  (`kernel.unprivileged_userns_clone=1`), which is the default on most
  modern Linux distributions but may be disabled in hardened environments.
  In that case, `virtiofsd` must run with `CAP_SYS_ADMIN`, which should be
  granted via a systemd service unit rather than a setuid binary.

- The AppArmor profile requires manual installation and path adjustment.
  It is not automatically activated by `scripts/bootstrap_vm.sh`. A future
  improvement would be to integrate profile installation into the bootstrap
  script.

## What is NOT addressed here

The virtiofs mount options (`nodev`, `nosuid`) are enforced by the **guest**
kernel at mount time, not by the host. The guest `init.sh` already mounts
the workspace share with default options. Explicitly adding `nodev,nosuid`
to the guest-side mount command is a separate hardening step that belongs in
`guest/init.sh` and is tracked separately.
