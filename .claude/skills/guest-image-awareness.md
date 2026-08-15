---
name: rootfs-awareness
description: Use after editing guest/init.sh, scripts/build_rootfs.sh, or any file that feeds into the guest rootfs image. Ensures `just check-rootfs` is run to detect staleness, and `just rebuild-rootfs` is run when the rootfs is out of date.
---

# Rootfs Awareness

The guest rootfs (`~/.abox/vm/rootfs.raw`) is built from `guest/init.sh`, `scripts/build_rootfs.sh`, and the embedded shim binary. It is *not* regenerated automatically. If you edit any of the inputs and forget to rebuild, your local tests and e2e runs exercise a stale guest — which can hide regressions or surface phantom ones.

## When to invoke

Triggered by edits to any of:

- `guest/**` (especially `guest/init.sh`)
- `scripts/build_rootfs.sh`
- `scripts/bootstrap_vm.sh`
- `crates/abox-shim/**` (the shim is embedded into the rootfs)

## Process

### 1. Detect staleness

```bash
just check-rootfs
```

`check-rootfs` compares the current input hashes (stored in `rootfs.raw.inputs`) against the hash of the built image. If they differ, the recipe prints a warning — it does **not** auto-rebuild.

### 2. Rebuild if stale

If `check-rootfs` warns:

```bash
just rebuild-rootfs
```

This rebuilds the static-musl shim, then runs `scripts/build_rootfs.sh` in the
repo's Dockerized Alpine builder to assemble the ext4 image. The rebuild takes
1–3 minutes depending on Docker image/package cache state and network access.

Verify success with a quick smoke run:

```bash
abox run --task rootfs-smoke --ephemeral -- \
  bash -c "echo ok && which claude && which codex && node --version"
```

Expected output includes `ok`, paths to both CLIs, and a Node version.

### 3. Reflect in the PR

The rebuilt rootfs is a host artifact, not committed. But the changes that caused the rebuild live in the diff. Any edit to `guest/**` or `scripts/build_rootfs.sh` triggers the `vm-attestation` path filter (see [`pre-pr-checklist.md`](../../docs/contributing/pre-pr-checklist.md)), so `just e2e-vm` must pass and the `vm-attested` label must be on the PR.

## Common failure modes

- `bash: command not found`, `sync: command not found`: Alpine package staging bug. Re-run `just rebuild-rootfs`; inspect `scripts/build_rootfs.sh` apk extraction.
- `claude: command not found` inside the guest: npm install in build_rootfs.sh failed silently. Check network; re-run with `-x`.
- `docker: command not found` or builder startup fails: install Docker and ensure the daemon is running; `build_rootfs.sh` shells out to a Dockerized Alpine builder.
- `rootfs.raw not found`: bootstrap has not run. `just bootstrap-vm` first, then `just rebuild-rootfs`.

## Related

- `rebuild-and-smoke.md` — longer-form rebuild-plus-smoke workflow.
- `integration-test.md` — cross-repo end-to-end test (bakudo → abox).
