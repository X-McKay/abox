#!/usr/bin/env bash
# build-guest-image.sh — Build the abox guest root filesystem.
#
# This script creates an Alpine Linux-based root filesystem image with:
# - abox-shim installed and symlinked for proxied commands
# - socat for VSock-to-Unix bridging
# - Common development tools (git, python3, nodejs, etc.)
# - An init script that mounts the virtiofs workspace and starts the proxy bridge
#
# Usage:
#   sudo ./build-guest-image.sh [--output /path/to/rootfs.raw] [--size 4G]
#
# Prerequisites:
#   - Root privileges (for chroot and mount)
#   - qemu-img, mkfs.ext4, alpine-make-rootfs (or debootstrap for Debian)
#   - abox-shim binary (cross-compiled for musl)

set -euo pipefail

OUTPUT="${1:-rootfs.raw}"
SIZE="${2:-4G}"
SHIM_BINARY="${SHIM_BINARY:-../target/x86_64-unknown-linux-musl/release/abox-shim}"

echo "=== Building abox guest image ==="
echo "Output: ${OUTPUT}"
echo "Size:   ${SIZE}"

# ── Step 1: Create a raw disk image ──
echo "[1/6] Creating raw disk image..."
qemu-img create -f raw "${OUTPUT}" "${SIZE}"
mkfs.ext4 -F "${OUTPUT}"

# ── Step 2: Mount and bootstrap Alpine ──
echo "[2/6] Bootstrapping Alpine Linux..."
MOUNT_DIR=$(mktemp -d)
mount -o loop "${OUTPUT}" "${MOUNT_DIR}"

# Use alpine-make-rootfs or manual bootstrap
if command -v alpine-make-rootfs &>/dev/null; then
    alpine-make-rootfs "${MOUNT_DIR}" --packages \
        "bash coreutils socat git python3 py3-pip nodejs npm curl wget openssh-client"
else
    # Manual bootstrap fallback
    echo "alpine-make-rootfs not found. Using manual bootstrap..."
    # This is a simplified version — in production, use alpine-make-rootfs
    mkdir -p "${MOUNT_DIR}"/{bin,sbin,etc,proc,sys,dev,tmp,run,workspace}
    mkdir -p "${MOUNT_DIR}"/usr/{bin,lib,local/bin}
    mkdir -p "${MOUNT_DIR}"/etc/init.d
fi

# ── Step 3: Install abox-shim ──
echo "[3/6] Installing abox-shim..."
if [ -f "${SHIM_BINARY}" ]; then
    cp "${SHIM_BINARY}" "${MOUNT_DIR}/usr/local/bin/abox-shim"
    chmod +x "${MOUNT_DIR}/usr/local/bin/abox-shim"

    # Create symlinks for proxied commands
    for cmd in git gh aws; do
        ln -sf /usr/local/bin/abox-shim "${MOUNT_DIR}/usr/local/bin/${cmd}"
    done
    echo "abox-shim installed and symlinked for: git gh aws"
else
    echo "WARNING: abox-shim binary not found at ${SHIM_BINARY}"
    echo "Build it with: cargo build --release --target x86_64-unknown-linux-musl -p abox-shim"
fi

# ── Step 4: Install the init script ──
echo "[4/6] Installing init script..."
cat > "${MOUNT_DIR}/etc/init.d/abox-init" << 'INITEOF'
#!/bin/sh
# abox-init: Guest initialization script.
# Runs at boot to set up the abox environment.

# Mount the virtiofs workspace
mkdir -p /workspace
mount -t virtiofs workspace /workspace 2>/dev/null || \
    echo "WARNING: virtiofs mount failed (not running in a VM?)"

# Bridge VSock to a Unix socket for abox-shim
# VSock CID 2 = host, port 5000 = proxy daemon
socat UNIX-LISTEN:/run/abox-proxy.sock,fork,reuseaddr \
    VSOCK-CONNECT:2:5000 &

# Set up the HTTP egress proxy
# The proxy runs on the host; we configure the guest to use it.
# The host's IP from the guest's perspective is the gateway.
GATEWAY=$(ip route | awk '/default/ { print $3 }')
export HTTPS_PROXY="http://${GATEWAY}:18443"
export HTTP_PROXY="http://${GATEWAY}:18443"
export https_proxy="${HTTPS_PROXY}"
export http_proxy="${HTTP_PROXY}"

# Write proxy env vars so all processes inherit them
cat > /etc/profile.d/abox-proxy.sh << EOF
export HTTPS_PROXY="${HTTPS_PROXY}"
export HTTP_PROXY="${HTTP_PROXY}"
export https_proxy="${HTTPS_PROXY}"
export http_proxy="${HTTP_PROXY}"
EOF

# Ensure /usr/local/bin is first in PATH so shim symlinks take precedence
echo 'export PATH="/usr/local/bin:${PATH}"' > /etc/profile.d/abox-path.sh

echo "abox-init: environment ready"
INITEOF
chmod +x "${MOUNT_DIR}/etc/init.d/abox-init"

# ── Step 5: Configure auto-start ──
echo "[5/6] Configuring auto-start..."
# Add abox-init to the boot sequence
mkdir -p "${MOUNT_DIR}/etc/runlevels/default"
ln -sf /etc/init.d/abox-init "${MOUNT_DIR}/etc/runlevels/default/abox-init" 2>/dev/null || true

# Also add to inittab for simpler init systems
if [ -f "${MOUNT_DIR}/etc/inittab" ]; then
    echo "::once:/etc/init.d/abox-init" >> "${MOUNT_DIR}/etc/inittab"
fi

# ── Step 6: Clean up ──
echo "[6/6] Finalizing..."
umount "${MOUNT_DIR}"
rmdir "${MOUNT_DIR}"

echo "=== Guest image built: ${OUTPUT} ==="
echo ""
echo "To use with abox, set in ~/.abox/config.toml:"
echo "  [vm_defaults]"
echo "  image_path = \"$(realpath "${OUTPUT}")\""
