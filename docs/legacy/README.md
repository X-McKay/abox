# Legacy Files

This directory contains files that are no longer part of the active workflow
but are retained for historical reference.

## `build-guest-image.sh.legacy`

This was the original guest image builder. It required `sudo`, `qemu-img`,
and `alpine-make-rootfs` to be installed on the host, and ran a privileged
`chroot`-based build.

**It has been superseded by `scripts/bootstrap_vm.sh`**, which is fully
rootless, checksummed, and idempotent. To set up the VM stack, run:

```bash
./scripts/bootstrap_vm.sh --yes
# or, equivalently:
abox init
```
