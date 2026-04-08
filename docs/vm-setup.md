# VM Setup

`abox` runs each AI coding agent inside a Cloud Hypervisor microVM for
hardware-enforced isolation. To boot real VMs you need:

1. A Linux host with `/dev/kvm` accessible to your user.
   If `ls -la /dev/kvm` shows `crw-rw----+ root kvm`, just
   `sudo usermod -aG kvm $USER` and log out/in once.
2. `cloud-hypervisor`, `virtiofsd`, a Linux kernel built for direct boot,
   and a guest root filesystem image.
3. The Rust toolchain (`rustup` + `cargo`) and `just` (`cargo install just`).

The `bootstrap_vm.sh` script handles items 2 with one command. It does
**not** require sudo, docker, chroot, or root.

## Quick start

```bash
git clone https://github.com/X-McKay/abox.git
cd abox
cargo build --release
just bootstrap-vm
```

This downloads ~60 MB of pinned, checksummed artifacts to `~/.abox/vm/`,
builds the abox guest shim for static musl, and assembles a minimal ext4
rootfs (~10 MB used inside a 96 MiB sparse image) containing busybox +
socat + the shim + a tiny guest init script. After it finishes you'll
have:

```
~/.abox/vm/
  cloud-hypervisor   # the VMM
  ch-remote          # the VMM control client
  virtiofsd          # filesystem sharing daemon
  vmlinux            # guest kernel
  rootfs.raw         # guest root filesystem (96 MiB sparse)
```

By default, `bootstrap_vm.sh` symlinks `cloud-hypervisor`,
`ch-remote`, and `virtiofsd` into `~/.local/bin/` so they're
discoverable on a typical PATH. If `~/.local/bin` is not already on
your PATH, the script warns and prints the one-line `export` to add
to your shell profile.

To opt out (e.g., for a shared install you want to manage manually),
pass `--no-symlink` and add `~/.abox/vm` to `PATH` yourself:

```bash
./scripts/bootstrap_vm.sh --no-symlink
export PATH="$HOME/.abox/vm:$PATH"
```

Drop the default policy and config into `~/.abox/`:

```bash
mkdir -p ~/.abox/policies
cp templates/config.example.toml ~/.abox/config.toml
cp policies/default.toml ~/.abox/policies/default.toml
```

## Running an agent

```bash
# Inside a git repo you want the agent to work on:
abox run --task fix-auth --base main -- claude
```

`abox run` boots a fresh microVM with your git worktree mounted at
`/workspace`, exec's `claude` inside it, and streams the console back
to your terminal. When the agent exits, the VM powers off and `abox run`
returns its exit code. The agent's `git`/`gh`/`aws` calls are routed
through the host policy proxy and attributed to the sandbox id in the
audit log at `~/.abox/logs/audit.jsonl`.

## Verifying the install

```bash
just e2e-vm
```

`phase 6 — full VM end-to-end` boots a real VM, runs `git status`
inside it, and confirms the call was attributed to the correct sandbox
in the audit log. If the phase prints `skipped: VM artifacts not
found`, the bootstrap hasn't completed yet.

## Customizing

The bootstrap writes default values; you can override them in
`~/.abox/config.toml`:

```toml
[vm_defaults]
image_path  = "/custom/rootfs.raw"
kernel_path = "/custom/vmlinux"
memory_mib  = 1024
vcpus       = 2
```

## Where artifacts come from

| Artifact          | Source                                                |
|-------------------|-------------------------------------------------------|
| cloud-hypervisor  | github.com/cloud-hypervisor/cloud-hypervisor (v44.0)  |
| virtiofsd         | Ubuntu noble apt archive (rootless deb extract)       |
| vmlinux           | github.com/cloud-hypervisor/linux release builds      |
| alpine-minirootfs | dl-cdn.alpinelinux.org (3.19.x)                       |
| socat (apk)       | dl-cdn.alpinelinux.org                                |

All downloads are pinned by exact version and SHA-256 in
`scripts/bootstrap_vm.sh`. The `vendor/` directory caches them so
re-running the bootstrap is fast (seconds) and offline-friendly.

## Troubleshooting

**`Permission denied (os error 13)` on `/dev/kvm`**
Your user isn't in the `kvm` group. Run `sudo usermod -aG kvm $USER`,
log out, and back in.

**`cloud-hypervisor: command not found` when running `abox run`**
You haven't added `~/.abox/vm/` to `PATH`. Either export it in your
shell or symlink the binaries into `~/.local/bin/`.

**Phase 6 skipped in `just e2e-vm`**
Run `just bootstrap-vm` first. It's idempotent — safe to re-run.

**`bootstrap_vm.sh` fails with a checksum mismatch**
An upstream artifact has been re-published or the download was
truncated. Check your network, delete the stale file in `vendor/`,
and re-run. If the problem persists, the pinned version may need to
be bumped — file an issue.
