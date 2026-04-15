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

for cmd in tar mkfs.ext4 dd install fakeroot; do
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

# ── Install packages via apk-static (rootless with fakeroot) ───────────
echo "  downloading apk-tools-static..."
APK_STATIC_URL="https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/apk-tools-static-2.14.4-r0.apk"
APK_STATIC_APK="$ABOX_VM_DIR/apk-tools-static.apk"
if [[ ! -f "$APK_STATIC_APK" ]]; then
    curl -fsSL -o "$APK_STATIC_APK" "$APK_STATIC_URL"
fi
# Extract the static apk binary
tar -xzf "$APK_STATIC_APK" --warning=no-unknown-keyword -C "$STAGE" \
    sbin/apk.static 2>/dev/null
APK_STATIC="$STAGE/sbin/apk.static"

echo "  installing bash, nodejs, npm via apk.static..."
# Configure Alpine repositories so apk can resolve packages.
mkdir -p "$STAGE/etc/apk/keys"
cp "$STAGE"/usr/share/apk/keys/x86/*.pub "$STAGE/etc/apk/keys/" 2>/dev/null || true
echo "https://dl-cdn.alpinelinux.org/alpine/v3.19/main" > "$STAGE/etc/apk/repositories"
echo "https://dl-cdn.alpinelinux.org/alpine/v3.19/community" >> "$STAGE/etc/apk/repositories"
fakeroot "$APK_STATIC" --root "$STAGE" --initdb --no-cache --no-scripts add \
    bash nodejs npm 2>&1 | tail -10
# Clean up the static apk binary — not needed in the guest.
rm -f "$APK_STATIC"

# ── Install Claude Code and Codex CLIs via npm ─────────────────────────
echo "  installing Claude Code and Codex CLIs..."
# npm install into the staged rootfs's global prefix so the binaries
# land at /usr/local/bin inside the guest.
NPM_PREFIX="$STAGE/usr/local"
mkdir -p "$NPM_PREFIX/lib" "$NPM_PREFIX/bin"
npm install --global --prefix "$NPM_PREFIX" \
    @anthropic-ai/claude-code @openai/codex 2>&1 | tail -5

# ── Copy CA cert into guest trust store (for TLS-terminating proxy) ─────
if [ -f "$HOME/.abox/ca/root.crt" ]; then
    echo "  installing abox CA cert into guest trust store..."
    mkdir -p "$STAGE/etc/ssl/certs"
    cp "$HOME/.abox/ca/root.crt" "$STAGE/etc/ssl/certs/abox-ca.pem"
fi

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
# 512 MiB for miniroot + shim + socat + bash + nodejs + npm + CLI tools
dd if=/dev/zero of="$IMG" bs=1M count=512 status=none
mkfs.ext4 -q -F -E root_owner=0:0 -d "$STAGE" "$IMG"

echo "  rootfs.raw built ($(du -h "$IMG" | cut -f1))"

# Record the hashes of the files that went into this rootfs so that
# `just check-rootfs` can detect when the image is stale after an
# init.sh or shim change. Content-addressed inputs only — the Alpine
# tarball itself rarely changes and is checksummed at download time.
STAMP="$IMG.inputs"
{
    echo "# Inputs that produced this rootfs (sha256). Generated by build_rootfs.sh."
    echo "init_sh=$(sha256sum "$GUEST_INIT" | cut -d' ' -f1)"
    echo "shim=$(sha256sum "$SHIM_BIN" | cut -d' ' -f1)"
    echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$STAMP"
