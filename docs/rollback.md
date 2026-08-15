# Rollback

Releases of `abox` are immutable GitHub releases. Rolling back a bad version means (a) installing a known-good previous version on affected hosts and (b) communicating to users that a version should be skipped. There is no automated rollback mechanism and none is planned at this scale.

## Pin an install to a specific version

`scripts/install.sh` honors the `ABOX_VERSION` environment variable. Users who already hit a bad version can re-run the installer pinned to the previous tag:

```bash
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/main/scripts/install.sh \
  | ABOX_VERSION=v0.4.2 bash
```

Or from a local checkout:

```bash
ABOX_VERSION=v0.4.2 ./scripts/install.sh
```

The installer verifies SHA256SUMS for the pinned release in the same way as for `latest`.

## Mark a release as pre-release / yank

If a published release is discovered to be broken:

1. Open the release on GitHub: `Releases` → the bad version → `Edit release`.
2. Tick `Set as a pre-release` and save.
   - The "Latest" badge moves to the previous good release.
   - `install.sh` (without `ABOX_VERSION`) now resolves `latest` to the previous good release automatically because the GitHub API excludes pre-releases from `latest`.
3. Amend the release notes at the top with a bold warning:
   ```markdown
   **⚠️ YANKED 2026-04-14 — do not install.** Regression in <area>; use v0.4.2 or v0.4.4+.
   ```
4. Publish an announcement (see template below).

Do not delete the release or the tag. The bad artifacts remain reachable for anyone who needs to reproduce a failure; the pre-release flag is sufficient to stop accidental new installs.

## Communication template

Post in the project's primary channel (Discussions, Slack, or announcement issue):

> **Known-bad release: abox vX.Y.Z**
>
> We have yanked vX.Y.Z. Symptom: <one-sentence description>. If you installed vX.Y.Z, roll back with:
>
> ```bash
> ABOX_VERSION=v<previous> ./scripts/install.sh
> ```
>
> Fix tracked in #<issue>. A patched release is targeted for <date>.

## Switching back to the legacy runtime backend

Separately from version rollback, the ADR-008 migration keeps the deprecated
Cloud Hypervisor backend available as an explicit runtime-level rollback path
while MicroSandbox parity is proven. Select it in **host-owned** config only
(repo config can never choose the runtime):

```toml
# ~/.abox/config.toml
[runtime]
backend = "cloud-hypervisor"
```

or per-process:

```bash
ABOX_RUNTIME_BACKEND=cloud-hypervisor abox run --task fix-auth -- claude
```

What the legacy backend still requires — none of this is installed by the
default `abox init` flow anymore:

- The full legacy VM artifact stack under `~/.abox/vm/` (`cloud-hypervisor`,
  `ch-remote`, `virtiofsd` with the `cap_sys_admin+ep` file capability,
  `vmlinux`, and per-profile `rootfs.raw` images). Install via
  `just bootstrap-vm` / `scripts/bootstrap_vm.sh` — see
  [`vm-setup.md`](vm-setup.md).
- `image_path` / `kernel_path` set in `[vm_defaults]`.
- A Linux **x86_64** host with `/dev/kvm`. The legacy backend does not run on
  aarch64 or macOS.

Legacy-only features that come back with it: memory snapshots/templates
(`abox snapshot`, `abox template`) and `abox attach` (serial console). Policy,
credential isolation, attribution, and the audit log behave the same on both
backends.

The legacy backend receives migration-critical fixes only and is deleted per
ADR-008's criteria; treat this switch as a short-lived escape hatch and
report whatever pushed you to it.

## What is not here

- No automated "yank registry" that `abox --version` checks. Users find out through GitHub or the announcement.
- No CI-driven post-release smoke test. Releases are cut locally via `scripts/release.sh`, which runs the full quality gate and e2e before tagging. That is the last-line defense against shipping broken artifacts.
