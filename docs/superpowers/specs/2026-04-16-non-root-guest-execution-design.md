# Non-Root Guest Execution — Design Spec

**Date:** 2026-04-16
**Status:** Approved for implementation
**Related:** [ADR-004](../../decisions/004-non-root-guest-execution.md),
[soak-test-prompt.md](../../testing/soak-test-prompt.md)

## Problem

The abox guest executes every agent command as `uid=0` (root). During soak
testing of PR #6 (hyper MITM refactor) this surfaced as an immediate blocker:
Claude Code 2.1.109 refuses `--dangerously-skip-permissions` when running as
root, so every test in the suite exits with rc=1 before any HTTP request
reaches the egress proxy. The hyper refactor itself is not exercised; the
suite cannot validate release readiness.

Beyond the immediate symptom, running the agent as root is the wrong
defense-in-depth posture. A guest-to-host escape via a bug in the guest
kernel, virtiofsd, or cloud-hypervisor would escape with maximal
privilege. The agent should run with the minimum privilege its workflow
requires — which is none of root's.

## Goals

1. Agent command runs as an unprivileged user inside the guest.
2. The privilege drop is transparent to the agent (agents and soak tests do
   not need to know about it).
3. Workspace read/write continues to work regardless of host uid — the
   rootfs must remain a host-independent artefact.
4. Credential stub forwarding continues to work.
5. Soak test suite (all 7 tests) passes end to end.

## Non-goals

- Multi-user hosts or shared-worktree scenarios. The current single-host-
  user model is preserved, not extended.
- Making `init.sh` itself non-root. PID 1's privilege is justified by its
  responsibilities (mounts, socat bridges) and it never processes agent-
  supplied data.
- HTTP/2 proxy support, new credential types, or any non-privilege change.

## Architecture

Three coordinated changes land together:

```
┌─────────── host ────────────┐        ┌──────────── guest ─────────────┐
│                             │        │                                │
│  abox run                   │        │  /sbin/init (init.sh, root)    │
│    ├─ virtiofsd workspace ──┼────────┼─► /workspace  (uid-mapped)     │
│    │    --uid-map=:1000:U:1 │        │                                │
│    ├─ virtiofsd meta ───────┼────────┼─► /abox-meta   (passthrough)   │
│    ├─ virtiofsd status ─────┼────────┼─► /abox-status (passthrough)   │
│    └─ cloud-hypervisor      │        │                                │
│                             │        │  sh /abox-meta/runner.sh  ←root│
│                             │        │    │                           │
│                             │        │    │ stage creds, chown abox   │
│                             │        │    ▼                           │
│                             │        │  exec setpriv --reuid=abox …   │
│                             │        │    env HOME=/home/abox USER=…  │
│                             │        │    <agent command>      ←abox  │
└─────────────────────────────┘        └────────────────────────────────┘
```

Host uid `U` (typically 1000) bidirectionally maps to guest uid 1000 for
the workspace share only. Meta and status shares keep default passthrough
because only root reads/writes them inside the guest.

## Components

### 1. Rootfs — `scripts/build_rootfs.sh`

Add user creation after `apk add bash nodejs npm` and before the npm
global CLI install:

```sh
echo "  creating abox user (uid=1000)..."
fakeroot chroot "$STAGE" /bin/sh -c '
  addgroup -g 1000 abox &&
  adduser -D -u 1000 -G abox -h /home/abox -s /bin/bash abox &&
  mkdir -p /home/abox/.claude &&
  chown -R abox:abox /home/abox &&
  chmod 700 /home/abox/.claude
'
```

`fakeroot chroot` is acceptable here because Alpine's busybox `adduser`
is minimal and does not depend on login.defs or PAM. If this path proves
finicky during implementation, the fallback is to stage `/etc/passwd`,
`/etc/group`, `/etc/shadow` entries and `/home/abox/` directly (the same
technique already used for every other file in the rootfs) — with a clear
comment naming the reason.

No new packages needed: `setpriv` is already present in Alpine's miniroot
at `/bin/setpriv`.

### 2. Virtiofsd flags — `crates/abox-core/src/adapters/cloud_hypervisor.rs`

The workspace virtiofsd invocation gains two flags:

```rust
.arg(format!("--uid-map=:1000:{}:1:", host_uid))
.arg(format!("--gid-map=:1000:{}:1:", host_gid))
```

`host_uid` and `host_gid` are resolved at adapter construction time via
`std::fs::metadata("/proc/self")` and `std::os::unix::fs::MetadataExt::{uid,gid}` —
no new workspace dependency required (the crates graph has neither `nix`
nor `libc` today, and `std::os::unix` covers this case). Format is
`:namespace_uid:host_uid:count:` per virtiofsd 1.10+; meaning: map the
single host uid `U` to guest uid 1000 (and symmetrically for gid).

Semantics:
- Guest reading a file owned by `U` on the host sees uid 1000.
- Guest (as uid 1000) writing a file: file lands on the host as uid `U`.
- Host files owned by other uids remain unmapped and inaccessible to
  the guest agent — a deliberate safety property.

Meta and status virtiofsd invocations do not change.

### 3. Runner script — `crates/abox-core/src/boot_meta.rs`

`runner_script()` output changes from:

```sh
set -e
mkdir -p '/.claude'
cp '/abox-meta/credentials/0' '/.claude/.credentials.json'
chmod 0600 '/.claude/.credentials.json'
exec <user-command>
```

to:

```sh
set -e
mkdir -p '/home/abox/.claude'
cp '/abox-meta/credentials/0' '/home/abox/.claude/.credentials.json'
chmod 0600 '/home/abox/.claude/.credentials.json'
chown abox:abox '/home/abox/.claude' '/home/abox/.claude/.credentials.json'
exec setpriv --reuid=abox --regid=abox --clear-groups --init-groups \
    -- env HOME=/home/abox USER=abox <user-command>
```

- `setpriv` is util-linux's atomic uid/gid/cap/group drop-and-exec. It
  does not alter the environment, so `HOME` and `USER` are set via `env`.
- `--clear-groups --init-groups` wipes inherited supplementary groups
  and repopulates from `/etc/group` entries for the target user.
- Credential staging runs while still root (inherited from init.sh),
  which is the only window where both `/abox-meta/` and the agent user's
  home are writable. After `chown`, the stub belongs to `abox:abox`.

### 4. Config default — `crates/abox-core/src/config.rs`

The default value for `guest.credential_files[0].guest` changes from
`/.claude/.credentials.json` to `/home/abox/.claude/.credentials.json`.
Existing user configs that override `guest =` explicitly are not
rewritten; release notes call out the required update.

### 5. Doctor check — `crates/abox-cli/src/commands/doctor.rs`

Add one verification: run `virtiofsd --help` once and grep for
`--uid-map`. If absent, report a red check with text:

    virtiofsd must support --uid-map (requires virtiofsd >= 1.10).
    The shipped binary at ~/.abox/vm/virtiofsd is older. Run
    'just bootstrap-vm' to refresh.

## Data flow

Credential forwarding flow remains unchanged at the host boundary (the
host-side MITM proxy still injects the real token into outbound
requests). The only change is where the stub file lives inside the
guest and who owns it:

```
host ~/.claude/.credentials.json
    │   (read by abox-core → serialised stub written to meta dir)
    ▼
meta virtiofs ─► /abox-meta/credentials/0        (root:root in guest)
                     │
                     │ runner.sh (root) — cp + chmod + chown
                     ▼
                 /home/abox/.claude/.credentials.json   (abox:abox, 0600)
                     │
                     │ agent reads via $HOME lookup after setpriv
                     ▼
                 claude / codex — begins auth; MITM host-side
                 injects real Authorization header on egress
```

## Error handling

| Failure | Surfaced as | Blast radius |
|---|---|---|
| `adduser` in rootfs build fails | `just rebuild-rootfs` exits non-zero with fakeroot error | Build-time only; no bad rootfs shipped |
| virtiofsd rejects `--uid-map` (too old) | `abox doctor` fails with the message above, before any sandbox boot | Pre-run check; user sees actionable message |
| `setpriv` missing from `$PATH` in runner | Runner exits non-zero; sandbox reports rc; stderr contains `setpriv: command not found` | Per-sandbox; no privilege leak |
| abox user missing from `/etc/passwd` | `setpriv` exits with `user 'abox': no such user`, non-zero; sandbox exits with rc | Per-sandbox; no privilege leak |
| Host uid not 1000 and uid-map somehow missing | Guest sees worktree files as the literal host uid; agent (uid=1000) gets EACCES on first read | Visible in tests immediately; doctor check prevents silent mis-config |
| Config override still points at `/.claude/.credentials.json` | Stub staged to the old path; agent does not find it, auth fails with a clear error early | User-addressable via release notes; no stability surprise |

## Testing

**Unit (fast, no VM):**

- `boot_meta::runner_script()`:
  - Asserts `cp` target is `/home/abox/.claude/.credentials.json`.
  - Asserts `chown abox:abox` line appears before `exec`.
  - Asserts the `exec` line is exactly
    `exec setpriv --reuid=abox --regid=abox --clear-groups --init-groups -- env HOME=/home/abox USER=abox <cmd>`.
  - Asserts ordering invariant: mkdir → cp → chmod → chown → exec setpriv.
- `config::GuestConfig` default roundtrip covers the new guest path.
- `cloud_hypervisor`: table test that the virtiofsd `Command` for the
  workspace share includes both map flags with the process's real uid/gid,
  and that meta/status commands do not include map flags.

**Integration (microVM; `just e2e-vm`):**

- `e2e_non_root.rs` (new): ephemeral sandbox running
  `id; whoami; pwd; env | grep -E '^(HOME|USER)='; stat -c '%u:%g' /workspace`.
  Assertions: uid=1000, user=abox, HOME=/home/abox, USER=abox, /workspace
  stat shows 1000:1000.
- `e2e_workspace_writeback.rs` (new): non-ephemeral sandbox, agent writes
  `probe.txt`, host stats the file post-run and asserts `%u` matches
  `id -u` of the host user (not 1000).
- Existing policy/denial e2e coverage runs unchanged — the shim and host
  policy engine are untouched.

**Red-green discipline:**

- Each new assertion is written first and verified to fail on current
  `main` before the corresponding implementation lands. This is part of
  the plan's checklist, not optional.

**Manual checkpoint tests during implementation** (to catch integration
problems before the full soak re-run):

1. After rootfs changes only: `abox run -- sh -c 'id; grep abox /etc/passwd'` — user exists, still runs as uid=0.
2. After virtiofsd changes only: `abox run -- stat -c '%u' /workspace` — shows 1000 as root.
3. After runner changes: `abox run -- id` — shows `uid=1000(abox)`.
4. Soak Test 1 (no-tool) passes end-to-end.
5. Soak Tests 1–7 pass end-to-end.

## Migration

- **Fresh installs:** no user-visible migration — `just bootstrap-vm`
  builds the new rootfs; default config points at the new guest path.
- **Existing installs with default config:** run `just rebuild-rootfs`.
  Config default is updated automatically on first read (no cached
  override exists for `guest.credential_files`).
- **Existing installs with custom `credential_files` in config:** release
  notes document the one-line change. `abox doctor` does not currently
  inspect config for this field; adding a warning is a post-v0.1.0
  enhancement if the class of user warrants it.

## Rollout

This change lands on a feature branch (`fix/non-root-guest-execution`),
is validated against the full soak test suite before merge, and is
included in v0.1.0. No feature flag — the privilege drop is a pure
improvement and toggling it adds complexity without benefit.

## Out of scope

- Using user namespaces inside the guest for further privilege separation.
- Running the egress proxy TLS decryption as non-root on the host.
- Host-side worktree permission hardening (e.g., 700 on per-sandbox dirs).
- Agent-facing configuration for choosing the guest user. The `abox` user
  is a hard-coded implementation detail.
