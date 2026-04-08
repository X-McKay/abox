#!/bin/sh
# abox guest init — runs as PID 1.
#
# Responsibilities:
#   1. Mount /proc, /sys, /dev
#   2. Mount /workspace from virtiofs (the git worktree)
#   3. Mount /abox-meta from virtiofs (read-only boot metadata, added in
#      Task 6 — for now this mount is optional and skipped if the tag
#      isn't present)
#   4. Bridge /run/abox-proxy.sock <-> vsock host:5000 via socat
#   5. Exec the agent command (Task 6 will inject via /abox-meta/runner.sh)
#   6. Power off cleanly so the host orchestrator unblocks
#
# This file is the plumbing; the actual boot-meta / agent-command wiring
# lands in Task 6. For now, it's enough to boot, print a banner, and
# poweroff, so the Task 3 smoke-boot can prove the rootfs is bootable.

set -e

mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sys /sys 2>/dev/null || true
mount -t devtmpfs dev /dev 2>/dev/null || true

mkdir -p /run /workspace /abox-meta

# Workspace share — Task 6+ will guarantee the tag exists. Until then,
# not being able to mount is not fatal (smoke boot).
mount -t virtiofs workspace /workspace 2>/dev/null || true
mount -t virtiofs aboxmeta /abox-meta -o ro 2>/dev/null || true

echo
echo "==> abox guest init: online"
echo "    kernel: $(uname -r)"
echo "    root:   $(mount | awk '/ \/ /{print $1,$5}')"
echo

if [ -f /abox-meta/runner.sh ]; then
    echo "==> running /abox-meta/runner.sh"
    sh /abox-meta/runner.sh || true
else
    echo "==> no /abox-meta/runner.sh (Task 6 will wire this)"
fi

sync
echo
echo "==> abox guest init: poweroff"
poweroff -f
