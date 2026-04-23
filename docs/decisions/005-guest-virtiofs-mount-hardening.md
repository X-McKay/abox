# ADR 005: Guest-Side virtiofs Mount Hardening

**Date:** 2026-04-23
**Status:** Accepted

## Context

abox mounts three virtiofs shares inside the guest VM:

| Mount point   | Tag          | Access | Contents                                |
|---------------|--------------|--------|-----------------------------------------|
| `/workspace`  | `workspace`  | rw     | Git worktree (agent's working directory)|
| `/abox-meta`  | `aboxmeta`   | ro     | Boot metadata, runner.sh, credentials   |
| `/abox-status`| `aboxstatus` | rw     | Exit-code file for host reporting       |

Prior to this change, all three mounts used default Linux mount options,
which permit:

1. **Device file creation** (`mknod`) on the mounted filesystem. If an agent
   writes a device file to `/workspace` and the host later accesses that path
   (e.g. via `git status`), the host process may inadvertently open a device
   node rather than a regular file.

2. **Setuid/setgid execution** (`nosuid` absent). If a setuid binary is
   placed on the workspace share and executed inside the guest, it could
   elevate the agent process from uid 1000 to a higher-privileged uid within
   the guest, potentially enabling further privilege escalation.

3. **Post-boot modification of `/abox-meta`** (no `ro` flag). The host stages
   `runner.sh` and credential files into the meta directory before boot, but
   the guest mounts it read-write. A compromised root process inside the guest
   (e.g. a bug in the CA injection code that runs as root in `init.sh`) could
   modify `runner.sh` or overwrite staged credentials between the time the
   host stages them and the time `init.sh` executes them — a classic TOCTOU
   (time-of-check/time-of-use) race.

## Decision

All three virtiofs mounts are hardened with explicit mount options in
`guest/init.sh`:

### `/workspace` — `nodev,nosuid`

```sh
mount -t virtiofs -o nodev,nosuid workspace /workspace
```

`nodev` prevents device file creation on the workspace share. `nosuid`
prevents setuid/setgid execution on files within the share. The workspace
is a git worktree; no device files or setuid binaries should ever
legitimately appear there.

The workspace mount remains read-write: the agent must be able to create,
modify, and delete files in the worktree.

Mount failure is now a hard boot error (`boot_fail 71`) rather than a
warning. The workspace is the agent's primary working directory; a sandbox
that cannot mount it is not functional and should not proceed.

### `/abox-meta` — `ro,nodev,nosuid`

```sh
mount -t virtiofs -o ro,nodev,nosuid aboxmeta /abox-meta
```

`ro` enforces read-only access at the guest kernel level. Even a root
process inside the guest cannot write to `/abox-meta` after this mount.
This closes the TOCTOU window: once `init.sh` mounts the share, the
contents are immutable from the guest's perspective.

`nodev` and `nosuid` are set for the same reasons as the workspace share.

The CA injection step (`cat /abox-meta/root.crt >> /etc/ssl/certs/...`)
reads from `/abox-meta` but writes to `/etc/ssl/certs/` on the rootfs —
this is unaffected by the `ro` flag on the meta mount.

Mount failure is a hard boot error (`boot_fail 72`): without boot metadata,
the guest cannot determine what agent command to run.

### `/abox-status` — `nodev,nosuid`

```sh
mount -t virtiofs -o nodev,nosuid aboxstatus /abox-status
```

`nodev` and `nosuid` are set. The status share must remain read-write so
the guest can write the exit-code file. Mount failure remains a soft
warning (the guest proceeds and the host detects the missing exit-code as
a crash).

## Consequences

### Positive

- **TOCTOU prevention.** The `ro` flag on `/abox-meta` eliminates the race
  between host staging and guest execution of `runner.sh`. A compromised
  root process inside the guest cannot modify staged credentials or the
  runner script after boot.

- **Device file containment.** `nodev` on all three shares prevents the
  agent from creating device nodes that could be exploited if the host
  later accesses the shared directory.

- **Setuid containment.** `nosuid` on all three shares prevents privilege
  escalation via setuid binaries placed on virtiofs-backed paths.

- **Fail-fast on workspace mount failure.** Promoting the workspace mount
  error from a warning to a hard boot error (`boot_fail 71`) ensures that
  a misconfigured or failed virtiofsd is detected immediately rather than
  producing a confusing "agent ran but produced no output" failure.

### Negative / Trade-offs

- **Rootfs rebuild required.** `guest/init.sh` is an input to the rootfs
  freshness check (`abox doctor`). After pulling this change, users must
  run `just rebuild-rootfs` to update the rootfs image.

- **Kernel support.** The `ro`, `nodev`, and `nosuid` mount options are
  standard Linux VFS flags and are supported by all kernels that support
  virtiofs. No compatibility concern is expected.

## What is NOT addressed here

- **`noexec` on `/workspace`.** The workspace share is intentionally left
  executable. Node.js, Python packaging, and shell-based toolchains
  frequently execute temporary helpers from the working directory. Adding
  `noexec` would break these workflows. This is a conscious trade-off
  documented in ADR-004 for the tmpfs scratch mount and applies equally
  here.

- **`noexec` on `/abox-meta`.** The `runner.sh` script is executed via
  `sh /abox-meta/runner.sh` (the shell is the executor, not the script
  file itself), so `noexec` would not prevent script execution. The `ro`
  flag is the meaningful protection here.
