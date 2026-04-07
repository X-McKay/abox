#!/usr/bin/env bash
# bootstrap_vm.sh — one-command setup for abox VM execution.
#
# Downloads cloud-hypervisor, virtiofsd, a kernel, and an Alpine miniroot.
# Builds the abox-shim for static musl. Assembles a guest rootfs image.
# Writes everything to ~/.abox/vm/ and updates ~/.abox/config.toml so
# `abox run` works out of the box.
#
# This script is idempotent and uses checksummed cached downloads under vendor/.
# It does NOT require sudo, docker, chroot, or root.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ABOX_VM_DIR="${ABOX_VM_DIR:-$HOME/.abox/vm}"

source "$REPO_ROOT/scripts/lib/download.sh"

mkdir -p "$ABOX_VM_DIR" "$REPO_ROOT/vendor"

echo "abox VM bootstrap"
echo "  install dir: $ABOX_VM_DIR"
echo "  vendor dir:  $REPO_ROOT/vendor"
echo

# (Phases filled in by Task 2.)
echo "Bootstrap skeleton OK."
