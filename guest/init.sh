#!/bin/sh
# abox guest init — runs as PID 1.
#
# Responsibilities:
#   1. Mount /proc, /sys, /dev
#   2. Mount /workspace from virtiofs (the git worktree)
#   3. Mount /abox-meta from virtiofs (boot metadata, read-only, nodev, nosuid)
#   4. Mount /abox-status from virtiofs (read-write; for exit-code reporting)
#   5. Bridge /run/abox-proxy.sock <-> vsock host:5000 via socat
#   6. Exec the agent command via /abox-meta/runner.sh
#   7. Write the agent's exit code to /abox-status/exit-code
#   8. Power off cleanly so the host orchestrator unblocks

set -e

# Ensure a complete PATH — the kernel may start PID 1 with a minimal or
# empty PATH, which causes BusyBox applets in /bin to be invisible.
export PATH="/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin"

ABOX_UID=1000
ABOX_GID=1000
ABOX_TMPDIR=/run/abox-tmp

boot_fail() {
    code="$1"
    message="$2"
    echo "ERROR: $message" >&2
    mkdir -p /abox-status
    mount -t virtiofs aboxstatus /abox-status 2>/dev/null || true
    echo "$code" > /abox-status/exit-code 2>/dev/null || true
    sync 2>/dev/null || true
    exec poweroff -f
}

mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sys /sys 2>/dev/null || true
mount -t devtmpfs dev /dev 2>/dev/null || true

# Bring up the loopback interface so 127.0.0.1 is reachable (needed for
# the HTTPS egress proxy bridge on TCP port 18443).
ip link set lo up 2>/dev/null || ifconfig lo up 2>/dev/null || true

mkdir -p /run "$ABOX_TMPDIR" /workspace /abox-meta /abox-cache

# Dedicated guest scratch area for the unprivileged agent user.
# This is VM-local only: not virtiofs-backed, not under /home/abox, and
# removed with sandbox teardown. Keep it executable — Node/Python/package
# tooling occasionally runs temp helpers, and `noexec` would break those
# workflows and push callers back toward host-backed paths.
if ! grep -Fqs " $ABOX_TMPDIR tmpfs " /proc/mounts; then
    mount -t tmpfs -o mode=0700,uid="$ABOX_UID",gid="$ABOX_GID",nodev,nosuid \
        tmpfs "$ABOX_TMPDIR" 2>/dev/null || \
        boot_fail 70 "failed to mount tmpfs scratch at $ABOX_TMPDIR"
fi
chown "$ABOX_UID:$ABOX_GID" "$ABOX_TMPDIR" 2>/dev/null || true
chmod 0700 "$ABOX_TMPDIR" 2>/dev/null || true

# Workspace share — nodev and nosuid prevent the agent from creating
# device nodes or using setuid binaries on the host-backed virtiofs share.
# The workspace is a git worktree; no device files or setuid binaries should
# ever legitimately appear there. Adding these flags limits the blast radius
# of a compromised or malicious agent that writes into the worktree.
mount -t virtiofs -o nodev,nosuid workspace /workspace 2>/dev/null || \
    boot_fail 71 "failed to mount workspace virtiofs"

# Boot metadata share — mounted read-only at the guest kernel level.
# The host stages runner.sh and credentials here before boot; the guest
# should never need to write back to this share. Enforcing ro at the
# mount layer means even a root process inside the guest cannot modify
# runner.sh or staged credentials after boot, preventing a TOCTOU attack
# where a compromised guest process races to overwrite runner.sh between
# the host staging it and init.sh executing it.
# nodev and nosuid are also set: the meta share contains only plain files
# (JSON, shell scripts, PEM certificates) and should never contain devices
# or setuid binaries.
mount -t virtiofs -o ro,nodev,nosuid aboxmeta /abox-meta 2>/dev/null || \
    boot_fail 72 "failed to mount aboxmeta virtiofs"

# Durable project cache share. Most sandboxes do not request one, so the mount
# is optional unless the host explicitly staged an expectation marker.
if mount -t virtiofs -o nodev,nosuid aboxcache /abox-cache 2>/dev/null; then
    :
elif [ -f /abox-meta/expect-cache-mount ]; then
    boot_fail 73 "failed to mount required aboxcache virtiofs"
fi

# ── Inject the host-generated abox MITM CA into the guest trust store ──
# The rootfs ships with only the Mozilla CA set; the per-user abox CA is
# staged into /abox-meta/root.crt by the host orchestrator at boot time.
# Rebuild the system bundle from the immutable Mozilla source, then append
# the abox CA. This is idempotent: the rootfs is booted read-write, so a
# naive `cat >>` would duplicate the CA on repeated runs and leave stale
# CAs trusted after rotation. Rebuilding from source avoids both problems.
if [ -f /abox-meta/root.crt ]; then
    cat /usr/share/ca-certificates/mozilla/*.crt \
        > /etc/ssl/certs/ca-certificates.crt 2>/dev/null || true
    cat /abox-meta/root.crt >> /etc/ssl/certs/ca-certificates.crt
    cp /abox-meta/root.crt /etc/ssl/certs/abox-ca.pem
fi

echo
echo "==> abox guest init: online ($(awk '{print $1}' /proc/uptime)s)"
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
    # Remove stale sockets from template-restored VMs (the rootfs may
    # already contain an abox-proxy.sock from when the snapshot was taken).
    rm -f /run/abox-proxy.sock
    "$SOCAT_BIN" UNIX-LISTEN:/run/abox-proxy.sock,fork,reuseaddr \
                 VSOCK-CONNECT:2:5000 &
    SOCAT_PID=$!

    # Also bridge vsock port 5001 for the HTTPS egress proxy.
    # The host binds a per-sandbox egress proxy on vsock-<id>.sock_5001;
    # this socat exposes it as a TCP listener inside the guest so that
    # HTTPS_PROXY=http://127.0.0.1:18443 works.
    #
    # stderr is discarded because HTTP/1.1 clients close their end after
    # reading the response, which makes socat's remaining half-open write
    # return EPIPE ("Broken pipe"). That's normal close behavior, not a
    # failure — real egress errors are visible on the host via the
    # per-sandbox egress proxy's tracing output.
    "$SOCAT_BIN" TCP-LISTEN:18443,fork,reuseaddr \
                 VSOCK-CONNECT:2:5001 2>/dev/null &
    EGRESS_SOCAT_PID=$!

    # Wait for the proxy unix socket to be bound before exec'ing the agent.
    # Polling beats a fixed sleep because: (a) on a fast host both binds happen
    # in milliseconds, so we don't pay a 500ms tax on every sandbox start;
    # (b) on a heavily loaded host the fixed sleep could be too short, leading
    # to flaky "connection refused" errors. The TCP egress listener on :18443
    # is inherently bound by the time the unix socket is bound, since both
    # socat processes were spawned in immediate succession before this poll.
    i=0
    while [ ! -S /run/abox-proxy.sock ] && [ "$i" -lt 5000 ]; do
        # 5000 * 0.001s = 5s ceiling, far longer than any plausible bind delay.
        sleep 0.001
        i=$((i + 1))
    done
    if [ ! -S /run/abox-proxy.sock ]; then
        echo "WARNING: /run/abox-proxy.sock did not appear within 5s; proceeding anyway"
    else
        # The socket is created by socat (running as root). The agent runs
        # as the unprivileged abox user (uid=1000), so hand ownership to that
        # user and keep the socket private to it. Root can still connect.
        chown 1000:1000 /run/abox-proxy.sock
        chmod 0600 /run/abox-proxy.sock
    fi
else
    echo "WARNING: $SOCAT_BIN not found; proxy bridge unavailable"
fi

# Status share (writable) for reporting the agent's exit code back to host.
# nodev and nosuid are set: this share contains only the exit-code file.
mkdir -p /abox-status
mount -t virtiofs -o nodev,nosuid aboxstatus /abox-status 2>/dev/null || \
    echo "WARNING: failed to mount aboxstatus virtiofs"

if [ -f /abox-meta/runner.sh ]; then
    echo "==> running /abox-meta/runner.sh ($(awk '{print $1}' /proc/uptime)s)"
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
sync 2>/dev/null || true

# Tear down the socat bridges (best-effort).
kill "$SOCAT_PID" 2>/dev/null || true
kill "$EGRESS_SOCAT_PID" 2>/dev/null || true

echo
echo "==> abox guest init: poweroff (rc=$RC) ($(awk '{print $1}' /proc/uptime)s)"
# Use exec so poweroff replaces this shell — if poweroff somehow fails,
# PID 1 must never exit (that triggers a kernel panic).
exec poweroff -f
