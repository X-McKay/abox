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

The recommended approach is to use `abox init`, which runs the bootstrap
automatically and writes a ready-to-use config file:

```bash
git clone https://github.com/X-McKay/abox.git
cd abox
cargo build --release
abox init          # guided setup: bootstraps VM stack + writes config
abox doctor        # optional: verify everything looks correct
```

Alternatively, run the steps manually:

```bash
just bootstrap-vm  # downloads VM artifacts and builds the guest rootfs
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

If you used `abox init`, the config and policy are already in place.
To set them up manually:

```bash
mkdir -p ~/.abox/policies
cp templates/config.example.toml ~/.abox/config.toml
# Edit ~/.abox/config.toml and set image_path and kernel_path:
#   image_path  = "~/.abox/vm/rootfs.raw"
#   kernel_path = "~/.abox/vm/vmlinux"
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
log out, and back in. Verify with `ls -la /dev/kvm` (look for the
`+` ACL marker or your username in the group).

**`cloud-hypervisor: command not found` when running `abox run`**
The bootstrap script symlinks the VMM binaries into `~/.local/bin/`
by default, but `~/.local/bin` may not be on your `PATH`. Either
add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile,
or `export PATH="$HOME/.abox/vm:$PATH"` if you used `--no-symlink`.

**`x86_64-unknown-linux-musl rust target is not installed`**
The shim is built as a static-musl binary so it can run inside the
minimal Alpine guest rootfs. Either install the target manually:

```bash
rustup target add x86_64-unknown-linux-musl
```

…or re-run `bootstrap_vm.sh --yes` to let the script install it.

**Phase 6 skipped in `just e2e-vm`**
Run `just bootstrap-vm` first. It's idempotent — safe to re-run.
Phase 6 is gated on `~/.abox/vm/cloud-hypervisor` and `rootfs.raw`
existing.

**`bootstrap_vm.sh` fails with a checksum mismatch**
An upstream artifact has been re-published or the download was
truncated. Check your network, delete the stale file in `vendor/`,
and re-run. If the problem persists, the pinned version may need to
be bumped — file an issue.

**`virtiofsd: Error creating listener: socket error: path must be shorter than SUN_LEN`**
Your runtime_dir path is too long. Linux Unix-domain socket paths
are capped at 108 bytes including a per-sandbox suffix like
`vfs-status-<task-id>.sock`. Move `runtime_dir` to a shorter
location (e.g., `/tmp/abox-<user>` or `~/.abox/r`) in your config
file:

```toml
runtime_dir = "/home/you/.abox/r"
```

**`abox run` finishes too fast and reports exit 1 with a "rolled back" warning**
The VM started but the guest never wrote `/abox-status/exit-code`.
Most common cause: the rootfs is stale. Re-run `just bootstrap-vm`
to rebuild the rootfs with the current `guest/init.sh`. If that
doesn't fix it, capture the console with
`RUST_LOG=abox_core=debug abox run --task X -- /bin/sh -c "echo hi"`
and look for kernel-panic / virtiofsd-error lines in the output.
