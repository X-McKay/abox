#!/bin/sh
# abox guest init — runs as PID 1.
#
# Responsibilities:
#   1. Mount /proc, /sys, /dev
#   2. Mount /workspace from virtiofs (the git worktree)
#   3. Mount /abox-meta from virtiofs (boot metadata, read-mostly)
#   4. Mount /abox-status from virtiofs (read-write; for exit-code reporting)
#   5. Bridge /run/abox-proxy.sock <-> vsock host:5000 via socat
#   6. Exec the agent command via /abox-meta/runner.sh
#   7. Write the agent's exit code to /abox-status/exit-code
#   8. Power off cleanly so the host orchestrator unblocks

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

# Status share (writable) for reporting the agent's exit code back to host.
mkdir -p /abox-status
mount -t virtiofs aboxstatus /abox-status 2>/dev/null || \
    echo "WARNING: failed to mount aboxstatus virtiofs"

if [ -f /abox-meta/runner.sh ]; then
    echo "==> running /abox-meta/runner.sh"
    # `set -e` is in effect; use an if-conditional so a non-zero runner
    # exit does NOT terminate init.sh before we report the exit code.
    if sh /abox-meta/runner.sh; then
        RC=0
    else
        RC=$?
    fi
else
    echo "==> no /abox-meta/runner.sh found"
    RC=127
fi

# Report exit code back to host through the writable status share.
echo "$RC" > /abox-status/exit-code 2>/dev/null || \
    echo "WARNING: could not write /abox-status/exit-code"
sync

# Tear down the socat bridge (best-effort).
kill "$SOCAT_PID" 2>/dev/null || true

echo
echo "==> abox guest init: poweroff (rc=$RC)"
poweroff -f
