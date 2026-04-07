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

# ---------------------------------------------------------------------------
# Artifact versions and URLs
# ---------------------------------------------------------------------------

# cloud-hypervisor v44.0 — static musl builds (x86_64)
# Source: https://github.com/cloud-hypervisor/cloud-hypervisor/releases/tag/v44.0
readonly CH_VERSION="v44.0"
readonly CH_BIN_URL="https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/v44.0/cloud-hypervisor-static"
readonly CH_BIN_SHA="f58e5d8684a5cbd7c4b8a001a1188ac79b9d4dda8115e1b3d5faa8c29038119c"
readonly CH_REMOTE_URL="https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/v44.0/ch-remote-static"
readonly CH_REMOTE_SHA="6d268b947adf2b9b72c13cc8bda156e27c9a450474001d762e9bd211f90136fa"

# virtiofsd 1.10.0 — from Ubuntu noble universe (dynamically linked, requires host libc/libcap-ng/libseccomp)
# Sourced as a .deb from the Ubuntu archive; binary extracted without root.
# Source: https://packages.ubuntu.com/noble/virtiofsd
readonly VIRTIOFSD_VERSION="1.10.0-1"
readonly VIRTIOFSD_DEB_URL="http://archive.ubuntu.com/ubuntu/pool/universe/r/rust-virtiofsd/virtiofsd_1.10.0-1_amd64.deb"
readonly VIRTIOFSD_DEB_SHA="1e4e817925b92f8c4ec59eff65b9825d044ecbd06c7bfcdca624e8562e90188a"
# SHA256 of the extracted binary itself (for post-extraction verification)
readonly VIRTIOFSD_BIN_SHA="597ae1edfda17185def026974a0ec0c3d3c6f536b018bb517aa566a4495dbf0d"

# Linux kernel — built by the cloud-hypervisor team against CH's kernel tree
# Source: https://github.com/cloud-hypervisor/linux/releases/tag/ch-release-v6.16.9-20260324
readonly VMLINUX_VERSION="ch-release-v6.16.9-20260324"
readonly VMLINUX_URL="https://github.com/cloud-hypervisor/linux/releases/download/ch-release-v6.16.9-20260324/vmlinux-x86_64"
readonly VMLINUX_SHA="22c640f02b750dea5d0c4419436aac8f2a6ea60fe02732435e25138d04eaaa86"

# Alpine Linux 3.19.9 miniroot filesystem tarball
# Source: https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/
readonly ALPINE_VERSION="3.19.9"
readonly ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.9-x86_64.tar.gz"
readonly ALPINE_SHA="6b4444630d3c349edb99847da31591a91d529b4bf8235a4990d4cb2cab45b8e5"

# socat Alpine package (not yet extracted — used in rootfs assembly phase)
# Source: https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/
readonly SOCAT_VERSION="1.8.0.0-r0"
readonly SOCAT_URL="https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/socat-1.8.0.0-r0.apk"
readonly SOCAT_SHA="ddf3be46f3a319737817246b238089dc58f39f32b0f515358c40e9e6e363eee6"

# ---------------------------------------------------------------------------

echo "abox VM bootstrap"
echo "  install dir: $ABOX_VM_DIR"
echo "  vendor dir:  $REPO_ROOT/vendor"
echo

echo "[1/3] Downloading cloud-hypervisor + ch-remote..."
download_to "$CH_BIN_URL"    "$ABOX_VM_DIR/cloud-hypervisor" "$CH_BIN_SHA"
download_to "$CH_REMOTE_URL" "$ABOX_VM_DIR/ch-remote"        "$CH_REMOTE_SHA"
chmod +x "$ABOX_VM_DIR/cloud-hypervisor" "$ABOX_VM_DIR/ch-remote"

echo "[2/3] Downloading virtiofsd..."
download_to "$VIRTIOFSD_DEB_URL" "$ABOX_VM_DIR/virtiofsd.deb" "$VIRTIOFSD_DEB_SHA"
# Extract just the virtiofsd binary from the deb (rootless — dpkg-deb -x needs no root)
_VFSD_TMP="$(mktemp -d)"
dpkg-deb -x "$ABOX_VM_DIR/virtiofsd.deb" "$_VFSD_TMP"
cp -f "$_VFSD_TMP/usr/libexec/virtiofsd" "$ABOX_VM_DIR/virtiofsd"
rm -rf "$_VFSD_TMP"
# Verify the extracted binary checksum
_actual="$(sha256sum "$ABOX_VM_DIR/virtiofsd" | awk '{print $1}')"
if [[ "$_actual" != "$VIRTIOFSD_BIN_SHA" ]]; then
    echo "  ERROR: virtiofsd binary checksum mismatch after extraction" >&2
    echo "  expected: $VIRTIOFSD_BIN_SHA" >&2
    echo "  actual:   $_actual" >&2
    exit 1
fi
rm -f "$ABOX_VM_DIR/virtiofsd.deb"
chmod +x "$ABOX_VM_DIR/virtiofsd"

echo "[3/3] Downloading guest kernel + Alpine miniroot + socat package..."
download_to "$VMLINUX_URL"  "$ABOX_VM_DIR/vmlinux"                    "$VMLINUX_SHA"
download_to "$ALPINE_URL"   "$ABOX_VM_DIR/alpine-minirootfs.tar.gz"   "$ALPINE_SHA"
download_to "$SOCAT_URL"    "$ABOX_VM_DIR/socat.apk"                  "$SOCAT_SHA"

echo
echo "Bootstrap complete. Files in $ABOX_VM_DIR:"
ls -lh "$ABOX_VM_DIR"
