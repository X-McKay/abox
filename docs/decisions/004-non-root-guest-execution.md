# ADR-004: Non-Root Guest Execution for Agent Commands

**Status:** Accepted
**Date:** 2026-04-16

## Context

Until now, the abox guest has executed every agent command as `uid=0` (root)
because `guest/init.sh` is PID 1 and invokes `runner.sh` without dropping
privileges. This was never an explicit design choice — it was the path of
least resistance while the rootfs, virtiofs mounts, and egress proxy were
being built. With those foundations stable, running the agent as root is a
liability on two fronts:

1. **Defense in depth.** The microVM already isolates guest state from the
   host, but a bug in the guest kernel, virtiofsd, or cloud-hypervisor that
   allowed a guest-to-host escape would escape with root privileges. Having
   the agent process unprivileged shrinks the blast radius of any such
   escape to the smaller set of kernel interfaces usable from uid != 0.

2. **Agent CLI expectations.** Claude Code 2.1.109 refuses to run with
   `--dangerously-skip-permissions` when `geteuid() == 0`, producing:

       --dangerously-skip-permissions cannot be used with root/sudo
       privileges for security reasons

   This surfaced during soak testing of PR #6 — every test in the suite
   blocked on this check before any request reached the egress proxy.
   Codex and any future agents are likely to adopt similar checks; running
   as root is fighting the ecosystem.

The obvious "just run as a non-root user" fix is complicated by virtiofsd's
passthrough behaviour. The host user owns the git worktree (typically
`uid=1000`); virtiofsd without uid translation exports those files into the
guest with their literal host uid. A guest user with uid != host uid would
be unable to read or write the worktree.

## Decision

The agent command runs as a dedicated unprivileged `abox` user (uid=1000,
gid=1000) inside the guest. Privileged boot-time work (mounts, socat
bridges, credential staging from `/abox-meta/`) remains root; only the
final `exec` of the agent command drops privileges.

Mechanism:

1. **Rootfs.** `scripts/build_rootfs.sh` runs inside a Dockerized Alpine
   builder as real root, installs the guest packages into the staged
   miniroot with `apk --root`, creates the `abox` user via
   `chroot "$STAGE" adduser -D -u 1000 -G abox -h /home/abox -s /bin/bash
   abox`, and pre-populates `/home/abox/.claude/` owned by `abox:abox`
   with mode `0700`. The container writes only the final ext4 image and
   its stamp back into `~/.abox/vm/`; the source repo is mounted read-only.

2. **Virtiofsd uid/gid remapping.** The workspace virtiofsd instance in
   `crates/abox-core/src/adapters/cloud_hypervisor.rs` is launched with
   `--uid-map=:1000:<host_uid>:1:` and `--gid-map=:1000:<host_gid>:1:` so
   host uid/gid map bidirectionally to guest uid/gid 1000. Files created by
   the agent land on the host owned by the host user; host-created files
   appear to the guest agent as uid/gid 1000. The `/abox-meta/` and
   `/abox-status/` virtiofsd instances are left in default passthrough —
   only root touches them inside the guest.

3. **Guest scratch tmpfs (`guest/init.sh`).** Before invoking the runner,
   PID 1 mounts a dedicated tmpfs at `/run/abox-tmp` with
   `mode=0700,uid=1000,gid=1000,nodev,nosuid`, then repairs ownership and
   permissions defensively. This gives the unprivileged agent a VM-local
   `TMPDIR` that is separate from both `/workspace` (host-backed) and
   `/home/abox` (credentials/config). `noexec` is intentionally not set:
   npm, Python packaging, shells, and similar toolchains sometimes execute
   temp helpers, and breaking that would push users back toward less
   isolated paths. Mount failure is treated as a boot-time error rather
   than falling back to some other location; the sandbox powers off and
   reports a non-zero exit so scratch regressions are visible immediately.

4. **Runner script (`crates/abox-core/src/boot_meta.rs`).** The generated
   `runner.sh` begins with a `getent passwd abox` pre-flight check,
   exports `TMPDIR=/run/abox-tmp` after applying caller-supplied
   environment variables, stages credentials as root (`cp`, `chmod`, then
   `chown abox:abox`), and ends with:

       exec su-exec abox:abox env HOME=/home/abox USER=abox <user-command>

   `su-exec` is Alpine's standard atomic uid/gid-drop-and-exec tool —
   equivalent to util-linux's `setpriv` but available in BusyBox-based
   rootfs (Alpine's BusyBox `setpriv` only supports capabilities, not
   `--reuid`/`--regid`). `su-exec` sets uid, gid, and supplementary
   groups from `/etc/group`, then execs the command. `HOME` and `USER`
   are injected via `env` because `su-exec` does not touch the
   environment. The pre-flight check exits with a distinctive rc (69,
   `EX_UNAVAILABLE`) and a clear remediation message if the rootfs is
   missing the `abox` user.

5. **Credential stub path — tilde expansion symmetric with `host`.** The
   `host` field in `[[guest.credential_files]]` already expands `~` against
   the host user's home. The `guest` field gains the same convention: a
   `~/` prefix expands against the guest agent's home (compiled-in
   constant `GUEST_AGENT_HOME = "/home/abox"`). This keeps the config
   self-documenting and host-independent. Expansion rules:
   - `~/…` → `/home/abox/…`
   - `/…` → absolute, unchanged (explicit overrides remain possible)
   - Anything else → rejected at config-load time with a clear error

   The default migrates from the literal absolute path
   `/.claude/.credentials.json` to the symmetric form
   `~/.claude/.credentials.json`. Fresh installs require no action. Users
   who inherited the prior default by never writing their own config are
   transparently updated on first read. Users who explicitly overrode the
   `guest` field to the literal prior default need to edit one line; this
   is flagged in release notes. No compat shim — v0.1.0 is the first
   tagged release, so there is no external install base to migrate, and
   rewrite-with-warning code is the kind of thing that outlives its
   usefulness. `templates/config.example.toml` is updated in lockstep so
   commented-out snippets show the new form.

6. **Onboarding hygiene checks.** Two `abox doctor` additions and one
   `abox run` warning lift common "silent failure" classes into clear,
   pre-boot signals:
   - `virtiofsd --uid-map` capability check (required for the uid
     remapping above).
   - Rootfs freshness check — compares recorded input hashes in
     `~/.abox/vm/rootfs.raw.inputs` against live `guest/init.sh` and
     shim hashes; reports red with the rebuild command when they drift.
     Skipped (neutral) for released binaries that have no source tree
     on disk, where the check is moot.
   - Missing-host-credential warning — `abox run` logs at `warn` when a
     `[[guest.credential_files]]` entry has a `stub` but no host file,
     so first-time users understand why auth is going to fail instead
     of debugging the resulting 401 inside an SSE stream.

## Consequences

### Positive

- Agent process runs with the smallest privilege set that supports its
  workflow. A guest-to-host escape via the agent would need an additional
  privilege escalation inside the guest first.
- Aligns abox with Claude Code / Codex CLI expectations. `--dangerously-
  skip-permissions` and the similar class of "auto-approve tool calls"
  flags work out of the box.
- Matches how other microVM sandboxes (Firecracker jailer, kata-containers)
  structure guest execution: privileged init → unprivileged payload.
- Guest tools get a private scratch area by default, so temp files no
  longer need to spill into `/workspace` or the agent home directory.
- No change to the host-side CA, MITM proxy, policy engine, shim, or
  abox CLI surface. The privilege drop is invisible to the agent.

### Negative / Risks

- **Rootfs rebuild required.** The change touches `build_rootfs.sh` and
  `guest/init.sh` inputs (hashes), so every existing install must
  `just rebuild-rootfs` after pulling. `just check-rootfs` surfaces the
  staleness. This is an already-documented workflow but still friction.
- **Config migration — narrow case only.** Users who explicitly pinned
  `guest = "/.claude/.credentials.json"` in `~/.abox/config.toml` need to
  change the field to `~/.claude/.credentials.json` (or an equivalent
  absolute path under `/home/abox/`). Users who inherited the default
  and users writing a fresh config require no action. Release notes
  flag the narrow case explicitly.
- **virtiofsd `--uid-map` coverage.** Present in virtiofsd 1.10+ (the
  version shipped in `~/.abox/vm/virtiofsd`). Older forks of virtiofsd
  without the flag are incompatible; `abox doctor` should verify.
- **Single-uid mapping only.** The current design maps exactly one host
  uid to guest 1000. Multi-user hosts or shared-worktree scenarios are
  not supported. Neither were they under the root-in-guest design, so
  this is a carry-forward limitation, not a regression.
- **`su-exec` failure modes.** If the rootfs is ever built without `abox`
  in `/etc/passwd`, the `getent passwd abox` pre-flight exits with rc 69
  before `su-exec` runs. Not silent. Covered by unit tests on the
  runner-script shape.

## Alternatives Considered

- **A: abox user, host-uid assumption baked in at build time.** Cheaper —
  no virtiofsd flag change. But every rootfs is per-host (uid must match
  the building host's), breaking shared/prebuilt rootfs distribution.
  Rejected in favour of the runtime `--uid-map` approach, which keeps the
  rootfs host-independent.

- **B: Stay as root, set a Claude-CLI bypass env var (if one exists).**
  Minimal code change, no rebuild. Rejected because (a) it depends on an
  undocumented vendor-specific override that can be removed in any patch
  release, (b) it addresses the symptom, not the underlying defense-in-
  depth concern, and (c) it doesn't generalise to Codex or other agents
  with their own root checks.

- **`su -l abox -c …` instead of `su-exec`.** Works, but `su` wraps PAM
  and shell setup, introduces a login shell between init and the agent,
  and has historically been a source of subtle bugs around signal
  handling, tty allocation, and environment inheritance. `su-exec` is the
  sharper tool for this job.

- **util-linux `setpriv` instead of `su-exec`.** The original design
  specified `setpriv --reuid=abox --regid=abox --clear-groups --init-groups`.
  Alpine's BusyBox `setpriv` only supports capability manipulation, not
  uid/gid switching. Installing `util-linux-misc` would provide the full
  `setpriv`, but `su-exec` is lighter (single 10KB binary), is the
  standard Alpine/Docker privilege-drop primitive, and achieves the same
  atomic uid/gid-switch-and-exec.

- **Run init.sh itself as non-root.** Would require delegating mounts and
  socat bridges to the kernel / a pre-init stage. Substantially more
  invasive than dropping privileges only at the final `exec`. Rejected
  on cost/benefit grounds; the blast-radius argument does not apply to
  PID 1 since it never handles agent-supplied data.

- **`nobody` (uid 65534) instead of a dedicated `abox` user.** Cleaner in
  spirit (no new user to manage), but `nobody` has no home directory and
  Claude's credential file must live under a real `$HOME`. Dedicating
  `abox` keeps the credential-stub path principled.
