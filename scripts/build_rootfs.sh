#!/usr/bin/env bash
# build_rootfs.sh — assemble the abox guest ext4 image without sudo.
#
# Inputs (env vars set by bootstrap_vm.sh):
#   ABOX_VM_DIR   — where alpine-minirootfs.tar.gz, socat.apk, vmlinux live
#   SHIM_BIN      — path to the static musl abox-shim binary
#   GUEST_INIT    — path to guest/init.sh
#
# Output: $ABOX_VM_DIR/rootfs.raw
set -euo pipefail

: "${ABOX_VM_DIR:?ABOX_VM_DIR must be set}"
: "${SHIM_BIN:?SHIM_BIN must be set}"
: "${GUEST_INIT:?GUEST_INIT must be set}"

for cmd in tar mkfs.ext4 dd install; do
    command -v "$cmd" >/dev/null 2>&1 || {
        echo "ERROR: required command '$cmd' not found in PATH" >&2
        exit 1
    }
done

if [[ ! -f "$SHIM_BIN" ]]; then
    echo "ERROR: shim binary not found at $SHIM_BIN" >&2
    exit 1
fi
if [[ ! -f "$GUEST_INIT" ]]; then
    echo "ERROR: guest init script not found at $GUEST_INIT" >&2
    exit 1
fi

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

echo "  staging Alpine miniroot..."
tar -xzf "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" -C "$STAGE"

echo "  extracting socat from apk..."
# .apk files are gzipped tar archives. They contain a mix of file trees
# and metadata; we only need the binary (and any shared libs it needs on
# Alpine). On Alpine, socat is a dynamically linked binary that depends on
# libcrypto/libssl — but the Alpine miniroot already includes these in
# /lib and /usr/lib. So we only need to extract the socat binary itself.
mkdir -p "$STAGE/usr/bin"
tar -xzf "$ABOX_VM_DIR/socat.apk" -C "$STAGE" \
    --warning=no-unknown-keyword \
    --wildcards \
    'usr/bin/socat' 2>/dev/null || {
    echo "ERROR: failed to extract usr/bin/socat from socat.apk" >&2
    echo "Listing socat.apk contents for debugging:" >&2
    tar -tzf "$ABOX_VM_DIR/socat.apk" --warning=no-unknown-keyword 2>/dev/null | head -40 >&2
    exit 1
}

echo "  installing abox-shim and symlinks..."
mkdir -p "$STAGE/usr/local/bin" "$STAGE/sbin"
install -m 0755 "$SHIM_BIN" "$STAGE/usr/local/bin/abox-shim"
for cmd in git gh aws; do
    ln -sf /usr/local/bin/abox-shim "$STAGE/usr/local/bin/$cmd"
done

echo "  installing init as /sbin/init..."
install -m 0755 "$GUEST_INIT" "$STAGE/sbin/init"

echo "  creating ext4 image..."
IMG="$ABOX_VM_DIR/rootfs.raw"
rm -f "$IMG"
# 96 MiB is plenty for miniroot + shim + socat (typical usage ~15 MiB)
dd if=/dev/zero of="$IMG" bs=1M count=96 status=none
mkfs.ext4 -q -F -E root_owner=0:0 -d "$STAGE" "$IMG"

echo "  rootfs.raw built ($(du -h "$IMG" | cut -f1))"
