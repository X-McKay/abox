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

echo "  extracting socat + shared libraries from apks..."
# .apk files are gzipped tar archives.
# socat on Alpine is dynamically linked and requires:
#   - libcrypto/libssl (included in the Alpine miniroot)
#   - libreadline → libncursesw (NOT in the miniroot by default)
# We download and embed these here so socat starts correctly inside the guest.
# Note: /usr/bin/socat is a symlink to /usr/bin/socat1; extract both.

_download_apk() {
    local dst="$1" url="$2" expected_sha="$3"
    if [[ ! -f "$dst" ]]; then
        echo "  downloading $(basename "$dst")..."
        curl -fsSL -o "$dst" "$url"
        local actual
        actual="$(sha256sum "$dst" | awk '{print $1}')"
        if [[ "$actual" != "$expected_sha" ]]; then
            echo "ERROR: $(basename "$dst") checksum mismatch (got $actual, want $expected_sha)" >&2
            rm -f "$dst"
            exit 1
        fi
    fi
}

_download_apk "$ABOX_VM_DIR/readline.apk" \
    "https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/readline-8.2.1-r2.apk" \
    "8b57deab29fa2230318065f83b45bb17d386f0352e29d9a7e5224722a3722365"

_download_apk "$ABOX_VM_DIR/libncursesw.apk" \
    "https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/libncursesw-6.4_p20231125-r0.apk" \
    "94830f70d5b4480be58e01c3bcdf6b05a95181603e31339d59411eacaedbf0e0"

mkdir -p "$STAGE/usr/bin" "$STAGE/usr/lib"
tar -xzf "$ABOX_VM_DIR/socat.apk" -C "$STAGE" \
    --warning=no-unknown-keyword \
    --wildcards \
    'usr/bin/socat' 'usr/bin/socat1' 2>/dev/null || {
    echo "ERROR: failed to extract socat binaries from socat.apk" >&2
    echo "Listing socat.apk contents for debugging:" >&2
    tar -tzf "$ABOX_VM_DIR/socat.apk" --warning=no-unknown-keyword 2>/dev/null | head -40 >&2
    exit 1
}
tar -xzf "$ABOX_VM_DIR/readline.apk" -C "$STAGE" \
    --warning=no-unknown-keyword \
    --wildcards \
    'usr/lib/libreadline*' 2>/dev/null || {
    echo "ERROR: failed to extract libreadline from readline.apk" >&2
    exit 1
}
tar -xzf "$ABOX_VM_DIR/libncursesw.apk" -C "$STAGE" \
    --warning=no-unknown-keyword \
    --wildcards \
    'usr/lib/libncursesw*' 2>/dev/null || {
    echo "ERROR: failed to extract libncursesw from libncursesw.apk" >&2
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
