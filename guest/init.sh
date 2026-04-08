#!/bin/sh
# abox guest init — runs as PID 1.
#
# Responsibilities:
#   1. Mount /proc, /sys, /dev
#   2. Mount /workspace from virtiofs (the git worktree)
#   3. Mount /abox-meta from virtiofs (boot metadata)
#   4. Bridge /run/abox-proxy.sock <-> vsock host:5000 via socat
#   5. Exec the agent command via /abox-meta/runner.sh
#   6. Power off cleanly so the host orchestrator unblocks

set -e

mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sys /sys 2>/dev/null || true
mount -t devtmpfs dev /dev 2>/dev/null || true

mkdir -p /run /workspace /abox-meta

# Workspace share
mount -t virtiofs workspace /workspace 2>/dev/null || \
    echo "WARNING: failed to mount workspace virtiofs"
# Boot metadata share
mount -t virtiofs aboxmeta /abox-meta 2>/dev/null || \
    echo "WARNING: failed to mount aboxmeta virtiofs"

echo
echo "==> abox guest init: online"
echo "    kernel: $(uname -r)"
echo "    root:   $(mount | awk '/ \/ /{print $1,$5}')"
echo

# ── Bridge vsock → unix socket for the abox proxy daemon ──
# The host proxy_bridge listens on vsock port 5000 (CID 2 = host).
# socat forks a relay for each connection, so the shim can connect
# to /run/abox-proxy.sock and talk to the host proxy transparently.
# Use the absolute path since PID 1 may have a minimal PATH.
SOCAT_BIN=/usr/bin/socat
if [ -x "$SOCAT_BIN" ]; then
    "$SOCAT_BIN" UNIX-LISTEN:/run/abox-proxy.sock,fork,reuseaddr \
                 VSOCK-CONNECT:2:5000 &
    SOCAT_PID=$!
    # Give socat a moment to bind the socket before exec'ing the agent.
    sleep 0.5
else
    echo "WARNING: $SOCAT_BIN not found; proxy bridge unavailable"
fi

if [ -f /abox-meta/runner.sh ]; then
    echo "==> running /abox-meta/runner.sh"
    sh /abox-meta/runner.sh || true
else
    echo "==> no /abox-meta/runner.sh found"
fi

# Tear down the socat bridge (best-effort).
kill "$SOCAT_PID" 2>/dev/null || true

sync
echo
echo "==> abox guest init: poweroff"
poweroff -f
