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
    bash nodejs npm su-exec ca-certificates gcompat 2>&1 | tail -10
# Clean up the static apk binary — not needed in the guest.
rm -f "$APK_STATIC"

# ── Create the unprivileged abox user (uid=1000) ───────────────────────
# The agent command drops to this user via setpriv in runner.sh. PID 1
# (init.sh) stays root for mounts and socat bridges; only the final exec
# of the agent runs unprivileged. See ADR-004.
echo "  creating abox user (uid=1000)..."
# Fallback: fakeroot chroot adduser is not available on this host (chroot
# requires real root even under fakeroot). Instead, append the user/group
# entries directly to the staged rootfs's /etc/passwd, /etc/group, and
# /etc/shadow, then create the home directory via install. This is
# equivalent to what Alpine's adduser/addgroup would do inside the chroot.
{
    printf 'abox:x:1000:1000:Linux User,,,:/home/abox:/bin/bash\n' >> "$STAGE/etc/passwd"
    printf 'abox:x:1000:\n' >> "$STAGE/etc/group"
    # Shadow entry: locked password ('!'), no aging fields set.
    printf 'abox:!::0:::::\n' >> "$STAGE/etc/shadow"
    # Create home dir and .claude subdir with correct ownership markers.
    # Note: standalone fakeroot install sessions do not persist virtual ownership
    # across calls. The -o/-g flags are cosmetic here — mkfs.ext4 -d reads real
    # stat(). Ownership is correct when the build host uid is 1000 (the common
    # case). For other build hosts, the runner script fixes ownership at boot
    # time via chown before dropping privileges. See Task 3 / ADR-004.
    fakeroot install -d -m 755 -o 1000 -g 1000 "$STAGE/home/abox"
    fakeroot install -d -m 700 -o 1000 -g 1000 "$STAGE/home/abox/.claude"
} || {
    echo "ERROR: failed to create abox user in rootfs stage" >&2
    exit 1
}

# ── Install Claude Code and Codex CLIs via npm ─────────────────────────
echo "  installing Claude Code and Codex CLIs..."
# npm install into the staged rootfs's global prefix so the binaries
# land at /usr/local/bin inside the guest.
NPM_PREFIX="$STAGE/usr/local"
mkdir -p "$NPM_PREFIX/lib" "$NPM_PREFIX/bin"
# Pin claude-code to a Node.js-script version. Newer versions ship a native
# glibc binary (claude.exe) that requires glibc compat on Alpine. Codex is
# a Rust binary but works via gcompat. Update these pins when rootfs glibc
# support is properly tested.
npm install --global --prefix "$NPM_PREFIX" \
    @anthropic-ai/claude-code@2.1.109 @openai/codex@0.121.0 2>&1 | tail -5
# Fix absolute shebangs in npm-generated shims to use /usr/bin/env node.
# Host npm may embed the host's node path which won't exist in the guest.
find "$NPM_PREFIX/bin" -type f -exec sed -i '1s|^#!.*node$|#!/usr/bin/env node|' {} +

# ── Build the system CA trust bundle ──────────────────────────────────
# apk --no-scripts installs individual PEM files from ca-certificates but
# does NOT run update-ca-certificates to build the bundle. We concatenate
# them manually so OpenSSL/rustls-native-certs/Go crypto find the system
# trust store at /etc/ssl/certs/ca-certificates.crt.
echo "  building system CA trust bundle..."
mkdir -p "$STAGE/etc/ssl/certs"
if ls "$STAGE/usr/share/ca-certificates/mozilla/"*.crt >/dev/null 2>&1; then
    cat "$STAGE/usr/share/ca-certificates/mozilla/"*.crt \
        > "$STAGE/etc/ssl/certs/ca-certificates.crt"
fi
# The abox MITM CA is NOT baked into the rootfs. It is injected at boot
# via the aboxmeta virtiofs share so that CI-built and user-built rootfs
# images are identical and each user's per-machine CA is trusted. See
# guest/init.sh for the boot-time injection logic.

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
# 768 MiB for miniroot + shim + socat + bash + nodejs + npm + CLI tools
# + ca-certificates trust store. The content is ~500 MiB; 768 leaves
# headroom for runtime tmpfiles and guest mounts.
dd if=/dev/zero of="$IMG" bs=1M count=768 status=none
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
