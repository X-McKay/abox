---
name: guest-image-awareness
description: Use after editing anything that feeds into the guest environment — images/** (OCI guest profile Dockerfiles + manifest), crates/abox-shim/** (abox-shim/abox-bridge are staged into every guest), or the MicroSandbox adapter. Ensures the guest side is rebuilt/validated (just build-guest-bins + just e2e-runtime) instead of tested against stale guest state. Legacy triggers (guest/**, scripts/build_rootfs.sh) still apply for the deprecated Cloud Hypervisor backend.
---

# Guest Image Awareness

Under the MicroSandbox runtime (ADR-008), guest environments are OCI images
(`images/<profile>/` build contexts, mapped by `images/manifest.toml`), plus
two host-staged static musl binaries (`abox-shim`, `abox-bridge`) that the
adapter patches into every guest at start. If you change any of these inputs
and test against stale guest state, you can hide regressions or chase phantom
ones.

## When to invoke

Triggered by edits to any of:

- `images/**` — profile Dockerfiles, the shared guest contract, or
  `manifest.toml` (note: the `digest` fields in the manifest are CI-managed;
  never hand-edit them)
- `crates/abox-shim/**` — both guest binaries live in this crate and are
  staged into every guest; protocol changes must stay in lockstep with
  `abox_core::protocol`
- `crates/abox-core/src/adapters/microsandbox.rs` — guest staging, control
  channels, ownership overlay

Legacy triggers (deprecated Cloud Hypervisor backend — see the deprecation
note below):

- `guest/**` (especially `guest/init.sh`)
- `scripts/build_rootfs.sh`
- `scripts/bootstrap_vm.sh`

## Process (MicroSandbox — default)

### 1. Rebuild the guest binaries

```bash
just build-guest-bins
```

Builds `abox-shim` + `abox-bridge` as static musl binaries for the host
architecture. Stage them under `~/.abox/guest/<arch>/` if you want normal
`abox run` invocations to pick them up (the e2e suite stages its own).

### 2. Run the live runtime suite

```bash
just e2e-runtime
```

`scripts/local/msb_e2e_test.sh` boots real MicroSandbox microVMs and
exercises exit codes, workspace write-through + isolation, the command broker
(proxied git, deny, audit attribution), and HTTPS egress. It skips cleanly
when virtualization or the msb assets are missing — a skip is not a pass for
attestation purposes.

### 3. Dockerfile changes: build the image locally

For `images/**` changes, build the affected profile for your architecture and
point abox at it via the **host-config** override — see
[`images/README.md`](../../images/README.md) for the exact commands:

```toml
# ~/.abox/config.toml
[images.overrides]
base = "abox-guest-base:dev"
```

Keep the guest contract intact: agent CLI versions pinned identically across
all five Dockerfiles, `abox` user uid/gid 1000, shim symlinks for
`git`/`gh`/`aws`, no ENTRYPOINT/USER, and both amd64 + arm64 must work.

### 4. Reflect in the PR

Any edit to these paths triggers the `runtime-attestation` CI path filter
(see [`pre-pr-checklist.md`](../../docs/contributing/pre-pr-checklist.md)):
`just e2e-runtime` must pass and the `runtime-attested` label must be on the
PR. Published-image digests are rewritten by
`.github/workflows/images.yml` — do not pin them by hand.

## Legacy process (Cloud Hypervisor backend — deprecated)

The raw-rootfs stack (`guest/init.sh`, `scripts/build_rootfs.sh`,
`~/.abox/vm/rootfs.raw`) only backs the deprecated `cloud-hypervisor`
backend and is slated for deletion (ADR-008). If you must touch it:

1. `just check-rootfs` — detect staleness (compares input hashes against the
   built image; warns, does not auto-rebuild).
2. `just rebuild-rootfs` if stale (Dockerized Alpine builder, 1–3 minutes).
3. `just e2e-vm` — the legacy live suite.

Common legacy failure modes: apk staging bugs (`bash: command not found` in
the guest → re-run rebuild), silent npm failures (`claude: command not
found`), Docker daemon missing, or `rootfs.raw not found` (run
`just bootstrap-vm` first).

## Related

- [`docs/runtime.md`](../../docs/runtime.md) — runtime architecture +
  troubleshooting.
- [`docs/runtime-upgrades.md`](../../docs/runtime-upgrades.md) — qualifying a
  MicroSandbox version bump.
- [`images/README.md`](../../images/README.md) — image contract, publishing,
  local builds.
