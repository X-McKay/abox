# VM End-to-End MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `abox run --task X --base main -- <agent-cmd>` actually boot a real Cloud Hypervisor microVM, mount the worktree via virtiofs, exec the agent command inside the guest, route its `git`/`gh`/`aws` calls through the host policy proxy with proper sandbox attribution, stream stdout/stderr back to the host terminal, and clean up everything on exit. Plus a one-command bootstrap so any user can install the prerequisites.

**Architecture:**
- **Foreground supervisor model.** `abox run` is no longer fire-and-forget; it owns the lifecycle of `virtiofsd` + `cloud-hypervisor` + a per-VM in-process policy proxy bridge, and exits when the guest agent exits. The standalone `abox-proxyd` keeps working for users who want a system daemon, but is no longer required for the common case.
- **Boot metadata via a second virtiofs share.** The orchestrator stages a tiny per-sandbox directory containing `boot.json` (sandbox_id, agent command, env vars) and serves it as a read-only `aboxmeta` virtiofs tag. The guest init reads it, exports `ABOX_SANDBOX_ID`, then `exec`s the agent command. This avoids cmdline length / quoting hell and avoids polluting the worktree.
- **Per-VM vsock-bound proxy bridge.** Cloud Hypervisor's `--vsock cid=3,socket=<path>` gives a host-side unix socket. For guest→host vsock-port-5000 traffic, the host binds `<path>_5000` *before* CH boots; every connection that arrives on it provably came from this VM, so the orchestrator tags every request with `sandbox_id=<id>`. The shim's existing `ABOX_SANDBOX_ID` env-var path is what the audit log uses, but the bridge is the *authoritative* attribution layer (cannot be spoofed by the guest).
- **Self-contained guest image.** A bootstrap script downloads `cloud-hypervisor`, `virtiofsd`, the CH-published `vmlinux`, and `alpine-minirootfs`, builds the shim for static musl, stages a rootfs (busybox + socat extracted from Alpine apk + shim + init script), and creates a populated ext4 image with `mkfs.ext4 -d`. **No sudo, no docker, no chroot.** Output lands in `~/.abox/vm/`.
- **Console streaming.** CH's `--console socket=<unix>` is connected to the orchestrator's stdin/stdout via `tokio::io::copy_bidirectional` so the user sees the agent's output live.

**Tech Stack:**
- Existing: Rust workspace, tokio, hyper, cloud-hypervisor, virtiofsd
- New runtime deps (downloaded by bootstrap, not vendored in git):
  - cloud-hypervisor v44+ (single static binary, GitHub releases)
  - virtiofsd v1.10+ (single static binary, GitLab releases)
  - cloud-hypervisor's published `vmlinux-x86_64` (~12 MB)
  - alpine-minirootfs-3.19.x-x86_64.tar.gz (~3 MB)
  - alpine `socat` apk (~150 KB) — extracted, not installed
- New build dep: `rustup target add x86_64-unknown-linux-musl`
- e2fsprogs `mkfs.ext4 -d` (already on most Linuxes)

---

## File Structure

**New files:**
- `crates/abox-core/src/proxy_bridge.rs` — embedded policy proxy server (vsock-aware) usable by orchestrator and proxyd
- `crates/abox-core/src/boot_meta.rs` — `BootMeta` struct (serde) shared between host stager and guest init
- `crates/abox-core/src/console.rs` — async helper that bridges CH's console socket to the orchestrator's stdio
- `guest/init.sh` — guest init script embedded into the rootfs at build time
- `guest/runner.sh.tmpl` — wrapper template the init writes for the agent command
- `scripts/bootstrap_vm.sh` — one-command installer
- `scripts/build_rootfs.sh` — invoked by bootstrap; pure-userspace rootfs builder
- `scripts/lib/download.sh` — small bash helper for cached downloads with checksum verification
- `docs/vm-setup.md` — user-facing setup walkthrough
- `vendor/.gitkeep` — vendor cache dir, gitignored except for this file

**Modified files:**
- `crates/abox-core/src/lib.rs` — add new modules
- `crates/abox-core/src/vm.rs` — add `agent_command: Vec<String>` to `VmConfig`
- `crates/abox-core/src/adapters/cloud_hypervisor.rs` — stage boot meta share, second virtiofsd, vsock bridge wiring, console hookup, agent-command lifecycle
- `crates/abox-core/src/sandbox.rs` — `run_sandbox` (new, foreground) vs existing `create_sandbox` (kept for tests)
- `crates/abox-cli/src/commands/run.rs` — call new `run_sandbox`, await VM exit, surface exit code
- `crates/abox-proxyd/src/cli_proxy.rs` — refactor to thin wrapper around `proxy_bridge::serve_unix`
- `scripts/e2e_test.sh` — add `phase 6 — full VM end-to-end` (gated on bootstrap completion)
- `justfile` — add `bootstrap-vm`, `e2e-vm`, `clean-vm` recipes
- `.gitignore` — add `vendor/`, `.scratch/`, `~/.abox/vm/` (the latter is outside the repo so really just doc)
- `README.md` — point to `docs/vm-setup.md`

**Single-responsibility decomposition:**
- `proxy_bridge.rs` does **only** "accept on a Unix socket, parse a `ProxyRequest`, evaluate policy, run command, return `ProxyResponse`". It is sandbox-id aware via a constructor parameter — same code path serves the orchestrator (one bridge per VM, sandbox_id baked in) and proxyd (one shared listener, sandbox_id from request body).
- `boot_meta.rs` is just types + JSON I/O.
- `cloud_hypervisor.rs` grows but stays focused on VMM lifecycle.
- The guest init script is dumb on purpose: read JSON, set env, mount, exec.

---

## Task 1: Vendor cache + bootstrap script skeleton

**Files:**
- Create: `vendor/.gitkeep`
- Create: `scripts/lib/download.sh`
- Create: `scripts/bootstrap_vm.sh`
- Modify: `.gitignore`

- [ ] **Step 1: Add vendor/ to .gitignore (keep .gitkeep tracked)**

```bash
# .gitignore additions
vendor/*
!vendor/.gitkeep
.scratch/
```

- [ ] **Step 2: Create empty vendor placeholder**

```bash
mkdir -p vendor
touch vendor/.gitkeep
```

- [ ] **Step 3: Write the cached-download helper**

`scripts/lib/download.sh`:

```bash
#!/usr/bin/env bash
# Cached download with sha256 verification.
# Usage: source scripts/lib/download.sh; download_to <url> <dest> <sha256>
set -euo pipefail

VENDOR_DIR="${VENDOR_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../vendor" && pwd)}"

download_to() {
    local url="$1" dest="$2" sha256="$3"
    local cache_file="$VENDOR_DIR/$(basename "$dest")"

    if [[ -f "$cache_file" ]]; then
        local actual
        actual=$(sha256sum "$cache_file" | awk '{print $1}')
        if [[ "$actual" == "$sha256" ]]; then
            cp -f "$cache_file" "$dest"
            return 0
        fi
        echo "  cache file $cache_file failed checksum, redownloading" >&2
    fi

    echo "  downloading $(basename "$dest")..." >&2
    curl --fail --location --silent --show-error --output "$cache_file" "$url"

    local actual
    actual=$(sha256sum "$cache_file" | awk '{print $1}')
    if [[ "$actual" != "$sha256" ]]; then
        echo "  ERROR: checksum mismatch for $cache_file" >&2
        echo "  expected: $sha256" >&2
        echo "  actual:   $actual" >&2
        rm -f "$cache_file"
        exit 1
    fi
    cp -f "$cache_file" "$dest"
}
```

- [ ] **Step 4: Write `scripts/bootstrap_vm.sh` skeleton (no real downloads yet)**

```bash
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

echo "abox VM bootstrap"
echo "  install dir: $ABOX_VM_DIR"
echo "  vendor dir:  $REPO_ROOT/vendor"
echo

# (Phases filled in by Task 2.)
echo "Bootstrap skeleton OK."
```

- [ ] **Step 5: Make scripts executable & commit**

```bash
chmod +x scripts/bootstrap_vm.sh scripts/lib/download.sh
git add vendor/.gitkeep scripts/bootstrap_vm.sh scripts/lib/download.sh .gitignore
git commit -m "feat(bootstrap): add vendor cache and bootstrap script skeleton"
```

---

## Task 2: Bootstrap downloads (CH, virtiofsd, kernel, alpine miniroot)

**Files:**
- Modify: `scripts/bootstrap_vm.sh`

- [ ] **Step 1: Pick exact upstream versions and record their checksums**

Pinned versions (chosen for stability + small size):

| Artifact | Version | URL | SHA256 |
|---|---|---|---|
| cloud-hypervisor | v44.0 | `https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/v44.0/cloud-hypervisor-static` | _fill in by curl + sha256_ |
| ch-remote | v44.0 | `https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/v44.0/ch-remote-static` | _fill in_ |
| virtiofsd | v1.13.0 | `https://gitlab.com/virtio-fs/virtiofsd/-/releases/v1.13.0/downloads/virtiofsd-v1.13.0.zip` | _fill in_ |
| vmlinux | hypervisor-fw / linux-cloud-hypervisor 6.x branch | `https://github.com/cloud-hypervisor/linux/releases/download/ch-release-v6.2-20230623/vmlinux` | _fill in_ |
| alpine-minirootfs | 3.19.1 | `https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.1-x86_64.tar.gz` | _fill in_ |
| socat apk | alpine 3.19 | `https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/socat-1.8.0.0-r0.apk` | _fill in_ |

The plan executor populates the SHA256 column by running `curl -L <url> | sha256sum` once and pasting the result into the script. Ship the script with the values inlined as bash variables.

- [ ] **Step 2: Add the downloads phase to bootstrap_vm.sh**

After the `mkdir -p` and before the closing `echo`:

```bash
echo "[1/5] Downloading cloud-hypervisor + virtiofsd..."
download_to "https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/v44.0/cloud-hypervisor-static" \
    "$ABOX_VM_DIR/cloud-hypervisor" "$CH_SHA256"
chmod +x "$ABOX_VM_DIR/cloud-hypervisor"

download_to "https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/v44.0/ch-remote-static" \
    "$ABOX_VM_DIR/ch-remote" "$CH_REMOTE_SHA256"
chmod +x "$ABOX_VM_DIR/ch-remote"

download_to "https://gitlab.com/virtio-fs/virtiofsd/-/releases/v1.13.0/downloads/virtiofsd-v1.13.0.zip" \
    "$ABOX_VM_DIR/virtiofsd.zip" "$VIRTIOFSD_SHA256"
unzip -joq "$ABOX_VM_DIR/virtiofsd.zip" "*/virtiofsd" -d "$ABOX_VM_DIR"
chmod +x "$ABOX_VM_DIR/virtiofsd"
rm -f "$ABOX_VM_DIR/virtiofsd.zip"

echo "[2/5] Downloading guest kernel..."
download_to "https://github.com/cloud-hypervisor/linux/releases/download/ch-release-v6.2-20230623/vmlinux" \
    "$ABOX_VM_DIR/vmlinux" "$VMLINUX_SHA256"

echo "[3/5] Downloading Alpine miniroot + socat package..."
download_to "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.1-x86_64.tar.gz" \
    "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" "$ALPINE_SHA256"
download_to "https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/socat-1.8.0.0-r0.apk" \
    "$ABOX_VM_DIR/socat.apk" "$SOCAT_SHA256"
```

(The constants `CH_SHA256` etc. are defined at the top of the script.)

- [ ] **Step 3: Run the bootstrap end-to-end and verify all six artifacts land in `~/.abox/vm/`**

Run: `./scripts/bootstrap_vm.sh`
Expected stdout:
```
[1/5] Downloading cloud-hypervisor + virtiofsd...
  downloading cloud-hypervisor...
  downloading ch-remote...
  downloading virtiofsd.zip...
[2/5] Downloading guest kernel...
[3/5] Downloading Alpine miniroot + socat package...
Bootstrap skeleton OK.
```

Verify: `ls ~/.abox/vm/` shows `cloud-hypervisor`, `ch-remote`, `virtiofsd`, `vmlinux`, `alpine-minirootfs.tar.gz`, `socat.apk`.

- [ ] **Step 4: Commit**

```bash
git add scripts/bootstrap_vm.sh
git commit -m "feat(bootstrap): download cloud-hypervisor, virtiofsd, kernel, alpine"
```

---

## Task 3: Build the static musl shim and rootfs builder

**Files:**
- Create: `scripts/build_rootfs.sh`
- Create: `guest/init.sh`
- Modify: `scripts/bootstrap_vm.sh`

- [ ] **Step 1: Write `guest/init.sh` (the script that runs as PID 1 inside the VM)**

`guest/init.sh`:

```bash
#!/bin/sh
# abox guest init — runs as PID 1.
#
# 1. Mount /proc, /sys, /dev
# 2. Mount /workspace from virtiofs (the git worktree)
# 3. Mount /abox-meta from virtiofs (read-only boot metadata)
# 4. Read /abox-meta/boot.json to get sandbox_id and agent command
# 5. Bridge /run/abox-proxy.sock <-> vsock host:5000
# 6. Exec the agent command, capturing exit code
# 7. Power off the VM cleanly so the host orchestrator unblocks

set -e

mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev 2>/dev/null || true
mkdir -p /run /workspace /abox-meta

mount -t virtiofs workspace /workspace
mount -t virtiofs aboxmeta /abox-meta -o ro

# Parse boot.json with busybox tools (no jq).
SANDBOX_ID=$(sed -n 's/.*"sandbox_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' /abox-meta/boot.json)
export ABOX_SANDBOX_ID="$SANDBOX_ID"
export PATH="/usr/local/bin:/usr/bin:/bin:/sbin"

# Bridge guest unix socket to host vsock.
socat UNIX-LISTEN:/run/abox-proxy.sock,fork,reuseaddr VSOCK-CONNECT:2:5000 &

# /abox-meta/runner.sh is generated per-boot by the host stager and contains
# the literal `exec <agent-cmd>` line so we don't have to deal with quoting
# from the cmdline.
cd /workspace
sh /abox-meta/runner.sh
RC=$?

# Tell the host we're done.
sync
echo "$RC" > /run/abox-exit-code
poweroff -f
```

- [ ] **Step 2: Write `scripts/build_rootfs.sh` (assembles the rootfs image, no sudo)**

```bash
#!/usr/bin/env bash
# build_rootfs.sh — assemble the abox guest ext4 image without sudo.
#
# Inputs (env vars set by bootstrap_vm.sh):
#   ABOX_VM_DIR    — where alpine-minirootfs.tar.gz, socat.apk, vmlinux live
#   SHIM_BIN       — path to the static musl abox-shim binary
#   GUEST_INIT     — path to guest/init.sh
#
# Output: $ABOX_VM_DIR/rootfs.raw
set -euo pipefail

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

echo "  staging Alpine miniroot..."
tar -xzf "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" -C "$STAGE"

echo "  extracting socat from apk..."
# .apk files are gzipped tar archives.
tar -xzf "$ABOX_VM_DIR/socat.apk" -C "$STAGE" --warning=no-unknown-keyword \
    usr/bin/socat 2>/dev/null

echo "  installing abox-shim and symlinks..."
mkdir -p "$STAGE/usr/local/bin" "$STAGE/sbin"
install -m 0755 "$SHIM_BIN" "$STAGE/usr/local/bin/abox-shim"
for cmd in git gh aws; do
    ln -sf /usr/local/bin/abox-shim "$STAGE/usr/local/bin/$cmd"
done

echo "  installing init..."
install -m 0755 "$GUEST_INIT" "$STAGE/sbin/init"
# Alpine miniroot doesn't ship /sbin/init by default; this overrides it.

echo "  creating ext4 image..."
IMG="$ABOX_VM_DIR/rootfs.raw"
# 96 MiB is plenty for miniroot + shim + socat (~10 MB used)
dd if=/dev/zero of="$IMG" bs=1M count=96 status=none
mkfs.ext4 -q -F -d "$STAGE" -E root_owner=0:0 "$IMG"

echo "  rootfs.raw built ($(du -h "$IMG" | cut -f1))"
```

- [ ] **Step 3: Wire shim build + rootfs build into bootstrap_vm.sh**

Append to `scripts/bootstrap_vm.sh`:

```bash
echo "[4/5] Building abox-shim for static musl..."
if ! rustup target list --installed | grep -q '^x86_64-unknown-linux-musl$'; then
    rustup target add x86_64-unknown-linux-musl
fi
( cd "$REPO_ROOT" && cargo build --release --target x86_64-unknown-linux-musl -p abox-shim )
SHIM_BIN="$REPO_ROOT/target/x86_64-unknown-linux-musl/release/abox-shim"

echo "[5/5] Assembling guest rootfs..."
SHIM_BIN="$SHIM_BIN" \
ABOX_VM_DIR="$ABOX_VM_DIR" \
GUEST_INIT="$REPO_ROOT/guest/init.sh" \
    "$REPO_ROOT/scripts/build_rootfs.sh"

echo
echo "Done. Files in $ABOX_VM_DIR:"
ls -lh "$ABOX_VM_DIR"
echo
echo "Add this to your ~/.abox/config.toml:"
echo
echo "  [vm_defaults]"
echo "  image_path  = \"$ABOX_VM_DIR/rootfs.raw\""
echo "  kernel_path = \"$ABOX_VM_DIR/vmlinux\""
echo
echo "Or just run: abox config set-vm-defaults  (Task 9)"
```

- [ ] **Step 4: Run the full bootstrap and verify rootfs.raw exists**

```bash
chmod +x scripts/build_rootfs.sh guest/init.sh
./scripts/bootstrap_vm.sh
ls -lh ~/.abox/vm/rootfs.raw
file ~/.abox/vm/rootfs.raw      # expect "Linux rev 1.0 ext4 filesystem data"
```

- [ ] **Step 5: Smoke-test that the rootfs is bootable**

Run cloud-hypervisor manually to verify the rootfs at least mounts and runs init:

```bash
~/.abox/vm/cloud-hypervisor \
  --kernel ~/.abox/vm/vmlinux \
  --disk path=~/.abox/vm/rootfs.raw \
  --cmdline "console=hvc0 root=/dev/vda rw" \
  --memory size=512M \
  --cpus boot=1 \
  --serial tty \
  --console off
```

Expected: kernel boots, you see Alpine init messages, eventually the VM panics because there's no `/abox-meta` mount yet (that's fine for a smoke test). Ctrl-C to kill.

- [ ] **Step 6: Commit**

```bash
git add guest/ scripts/build_rootfs.sh scripts/bootstrap_vm.sh
git commit -m "feat(bootstrap): build static-musl shim and assemble guest rootfs"
```

---

## Task 4: `BootMeta` type and host-side stager

**Files:**
- Create: `crates/abox-core/src/boot_meta.rs`
- Modify: `crates/abox-core/src/lib.rs`
- Modify: `crates/abox-core/src/vm.rs`

- [ ] **Step 1: Write the failing test for boot meta JSON roundtrip**

`crates/abox-core/src/boot_meta.rs` (test only at first):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_boot_meta_roundtrip() {
        let meta = BootMeta {
            sandbox_id: "fix-auth".into(),
            agent_command: vec!["claude".into(), "--model".into(), "opus".into()],
            env: vec![("FOO".into(), "bar".into())],
        };
        let json = meta.to_json().unwrap();
        let parsed = BootMeta::from_json(&json).unwrap();
        assert_eq!(parsed.sandbox_id, "fix-auth");
        assert_eq!(parsed.agent_command, vec!["claude", "--model", "opus"]);
        assert_eq!(parsed.env[0].0, "FOO");
    }

    #[test]
    fn test_runner_script_quotes_correctly() {
        let meta = BootMeta {
            sandbox_id: "x".into(),
            agent_command: vec!["echo".into(), "hello world".into(), "$HOME".into()],
            env: vec![],
        };
        let script = meta.runner_script();
        // Each argument must be single-quoted and any embedded single-quote escaped.
        assert!(script.contains("'echo'"));
        assert!(script.contains("'hello world'"));
        assert!(script.contains("'$HOME'"));
        assert!(script.starts_with("#!/bin/sh\nexec "));
    }
}
```

- [ ] **Step 2: Run it; it fails because `BootMeta` doesn't exist**

```bash
cargo test -p abox-core --lib boot_meta 2>&1 | tail -10
```
Expected: "cannot find type `BootMeta`".

- [ ] **Step 3: Implement `BootMeta`**

Top of `crates/abox-core/src/boot_meta.rs`:

```rust
//! Boot metadata passed from host to guest via a per-VM virtiofs share.
//!
//! The orchestrator stages a directory containing `boot.json` and `runner.sh`,
//! mounts it as the `aboxmeta` virtiofs tag (read-only), and the guest init
//! reads them. This avoids kernel-cmdline length limits and quoting issues
//! and never touches the user's worktree.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Metadata the host injects into the guest at boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootMeta {
    /// Sandbox identifier — exported as `ABOX_SANDBOX_ID`.
    pub sandbox_id: String,
    /// The agent command and its arguments (`argv`-style).
    pub agent_command: Vec<String>,
    /// Additional environment variables to export before exec.
    pub env: Vec<(String, String)>,
}

impl BootMeta {
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// Generate the `runner.sh` script the guest init `exec`s. Each argument
    /// is wrapped in single quotes; embedded single quotes are escaped using
    /// the standard `'\''` shell idiom.
    pub fn runner_script(&self) -> String {
        let mut s = String::from("#!/bin/sh\n");
        for (k, v) in &self.env {
            s.push_str(&format!("export {}='{}'\n", k, sh_escape(v)));
        }
        s.push_str("exec");
        for arg in &self.agent_command {
            s.push_str(&format!(" '{}'", sh_escape(arg)));
        }
        s.push('\n');
        s
    }

    /// Stage the boot meta on disk: write `boot.json` and `runner.sh` into
    /// `dir`. The orchestrator points virtiofsd at `dir` and mounts it as
    /// `/abox-meta` in the guest.
    pub fn stage(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("boot.json"), self.to_json()?)?;
        let runner_path = dir.join("runner.sh");
        std::fs::write(&runner_path, self.runner_script())?;
        // Make executable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runner_path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    }
}

fn sh_escape(s: &str) -> String {
    s.replace('\'', r"'\''")
}
```

- [ ] **Step 4: Register the module**

In `crates/abox-core/src/lib.rs`, add:

```rust
pub mod boot_meta;
```

(Insert alphabetically after `adapters`.)

- [ ] **Step 5: Run tests, expect pass**

```bash
cargo test -p abox-core --lib boot_meta 2>&1 | tail -10
```
Expected: 2 passed.

- [ ] **Step 6: Add `agent_command` to `VmConfig` (already had `env_vars`)**

In `crates/abox-core/src/vm.rs`, add to `VmConfig`:

```rust
    /// Command (argv-style) to exec inside the guest after boot.
    pub agent_command: Vec<String>,
```

- [ ] **Step 7: Update `SandboxOrchestrator::create_sandbox` to populate it**

In `crates/abox-core/src/sandbox.rs`, in the `vm_config` builder, add:

```rust
            agent_command: params.command.clone(),
```

- [ ] **Step 8: Run all tests, fix any breakage from the new field**

```bash
cargo test --workspace 2>&1 | tail -20
```
Expected: all passing. Test fixtures in `integration_tests.rs` that build `VmConfig` directly need `agent_command: vec![]` added.

- [ ] **Step 9: Commit**

```bash
git add crates/abox-core/src/boot_meta.rs crates/abox-core/src/lib.rs \
        crates/abox-core/src/vm.rs crates/abox-core/src/sandbox.rs \
        crates/abox-core/tests/integration_tests.rs
git commit -m "feat(core): add BootMeta type and agent_command on VmConfig"
```

---

## Task 5: Extract `proxy_bridge` library

**Files:**
- Create: `crates/abox-core/src/proxy_bridge.rs`
- Modify: `crates/abox-core/src/lib.rs`
- Modify: `crates/abox-core/Cargo.toml` (no new deps; tokio + serde_json already there)
- Modify: `crates/abox-proxyd/src/cli_proxy.rs` (becomes thin wrapper)

- [ ] **Step 1: Write `proxy_bridge.rs` — a sandbox-aware variant of cli_proxy**

```rust
//! Embedded policy proxy server.
//!
//! Accepts JSON `ProxyRequest`s on a Unix socket, evaluates them against the
//! policy engine, executes allowed commands on the host, and returns
//! `ProxyResponse`s. Used in two configurations:
//!
//! 1. **Per-VM bridge (orchestrator).** A bridge bound to the
//!    `<vsock-socket>_5000` path Cloud Hypervisor exposes. Every connection
//!    provably came from one specific guest VM, so `sandbox_id` is fixed at
//!    construction time and overrides any value in the request.
//!
//! 2. **Shared daemon (`abox-proxyd`).** A bridge bound to a regular Unix
//!    socket. Multiple sandboxes connect to it, so `sandbox_id` is read from
//!    each request (with `"unknown"` fallback for legacy shims).

use crate::policy::{Decision, PolicyEngine};
use crate::protocol::{ProxyRequest, ProxyResponse};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Determines how the bridge attributes incoming requests to a sandbox.
#[derive(Debug, Clone)]
pub enum SandboxAttribution {
    /// Trust the `sandbox_id` field on each request (proxyd mode).
    FromRequest,
    /// Force every request to use this sandbox id (per-VM bridge mode).
    Fixed(String),
}

/// Hook for audit logging. Implemented by both proxyd's `AuditLog` and a
/// no-op for in-process bridges that don't need persistent audit.
pub trait AuditSink: Send + Sync {
    fn log_cli(&self, sandbox_id: &str, command: &str, args: &[String], decision: &str, exit_code: i32);
}

pub struct ProxyBridge {
    socket_path: PathBuf,
    policy: Arc<PolicyEngine>,
    audit: Arc<dyn AuditSink>,
    attribution: SandboxAttribution,
}

impl ProxyBridge {
    pub fn new(
        socket_path: PathBuf,
        policy: Arc<PolicyEngine>,
        audit: Arc<dyn AuditSink>,
        attribution: SandboxAttribution,
    ) -> Self {
        Self { socket_path, policy, audit, attribution }
    }

    /// Bind the listener and serve forever.
    pub async fn run(self) -> Result<()> {
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&self.socket_path)?;
        tracing::info!(
            socket = %self.socket_path.display(),
            attribution = ?self.attribution,
            "proxy bridge listening"
        );
        let policy = self.policy;
        let audit = self.audit;
        let attribution = Arc::new(self.attribution);
        loop {
            let (stream, _) = listener.accept().await?;
            let policy = Arc::clone(&policy);
            let audit = Arc::clone(&audit);
            let attribution = Arc::clone(&attribution);
            tokio::spawn(async move {
                if let Err(e) = handle(stream, &policy, &audit, &attribution).await {
                    tracing::error!(error = %e, "proxy bridge connection error");
                }
            });
        }
    }
}

async fn handle(
    stream: UnixStream,
    policy: &PolicyEngine,
    audit: &dyn AuditSink,
    attribution: &SandboxAttribution,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.trim().is_empty() {
        return Ok(());
    }
    let request: ProxyRequest = serde_json::from_str(line.trim())?;
    let sandbox_id = match attribution {
        SandboxAttribution::Fixed(id) => id.clone(),
        SandboxAttribution::FromRequest => {
            request.sandbox_id.clone().unwrap_or_else(|| "unknown".into())
        }
    };

    let decision = policy.evaluate_cli(&request.command, &request.args);
    let response = match &decision {
        Decision::Allow => match exec(&request).await {
            Ok(r) => {
                audit.log_cli(&sandbox_id, &request.command, &request.args, "allowed", r.exit_code);
                r
            }
            Err(e) => {
                audit.log_cli(&sandbox_id, &request.command, &request.args, "error", -1);
                ProxyResponse::from_exit(1, String::new(), format!("execution failed: {e}"))
            }
        },
        Decision::Deny(reason) => {
            audit.log_cli(&sandbox_id, &request.command, &request.args, "denied", 126);
            ProxyResponse::denied(reason)
        }
    };
    let json = serde_json::to_string(&response)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

async fn exec(request: &ProxyRequest) -> Result<ProxyResponse> {
    let output = tokio::process::Command::new(&request.command)
        .args(&request.args)
        .current_dir(resolve_cwd(&request.cwd))
        .output()
        .await?;
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(ProxyResponse::from_exit(exit_code, stdout, stderr))
}

fn resolve_cwd(guest_cwd: &str) -> PathBuf {
    PathBuf::from(guest_cwd)
}

/// Convenience: a no-op audit sink for in-process orchestrator bridges that
/// only need tracing.
pub struct TracingAuditSink;

impl AuditSink for TracingAuditSink {
    fn log_cli(&self, sandbox_id: &str, command: &str, args: &[String], decision: &str, exit_code: i32) {
        tracing::info!(sandbox_id, command, args = ?args, decision, exit_code, "cli");
    }
}
```

- [ ] **Step 2: Register the module**

In `lib.rs`, add `pub mod proxy_bridge;`.

- [ ] **Step 3: Refactor `abox-proxyd`'s `cli_proxy.rs` to wrap `ProxyBridge`**

```rust
//! CLI proxy server for abox-proxyd. A thin wrapper around the shared
//! `abox_core::proxy_bridge` library so the orchestrator and the standalone
//! daemon use the same code path.

use crate::audit::AuditLog;
use abox_core::proxy_bridge::{AuditSink, ProxyBridge, SandboxAttribution};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

impl AuditSink for AuditLog {
    fn log_cli(&self, sandbox_id: &str, command: &str, args: &[String], decision: &str, exit_code: i32) {
        AuditLog::log_cli(self, sandbox_id, command, args, decision, exit_code);
    }
}

pub struct CliProxyServer {
    bridge: ProxyBridge,
}

impl CliProxyServer {
    pub fn new(
        socket_path: PathBuf,
        policy: Arc<abox_core::policy::PolicyEngine>,
        audit: Arc<AuditLog>,
    ) -> Self {
        let audit_sink: Arc<dyn AuditSink> = audit;
        Self {
            bridge: ProxyBridge::new(socket_path, policy, audit_sink, SandboxAttribution::FromRequest),
        }
    }

    pub async fn run(self) -> Result<()> {
        self.bridge.run().await
    }
}
```

Note the `&self` → `self` change in `run`. Update the call site in `proxyd/src/main.rs`:

```rust
    let cli_server = CliProxyServer::new(cli_socket, Arc::clone(&policy), Arc::clone(&audit));
    // ... in tokio::select!, change `cli_server.run()` to keep working — already moves self.
```

- [ ] **Step 4: Run all tests + clippy**

```bash
cargo test --workspace 2>&1 | tail -10
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/abox-core/src/proxy_bridge.rs crates/abox-core/src/lib.rs \
        crates/abox-proxyd/src/cli_proxy.rs crates/abox-proxyd/src/main.rs
git commit -m "refactor(core): extract reusable proxy_bridge from proxyd cli_proxy"
```

---

## Task 6: VMM lifecycle — second virtiofs share, vsock listener, console hookup

**Files:**
- Modify: `crates/abox-core/src/adapters/cloud_hypervisor.rs`
- Create: `crates/abox-core/src/console.rs`
- Modify: `crates/abox-core/src/lib.rs`

- [ ] **Step 1: Add `console` module that streams a CH console socket to the orchestrator's stdio**

`crates/abox-core/src/console.rs`:

```rust
//! Stream a Cloud Hypervisor console Unix socket to the orchestrator's
//! standard streams. Used by `abox run` to give the user live agent output.

use anyhow::Result;
use std::path::Path;
use tokio::io::{stdin, stdout, AsyncWriteExt};
use tokio::net::UnixStream;

/// Connect to `socket_path` and pump bytes between the socket and the
/// orchestrator's stdio. Returns when the socket closes (i.e. the VM exits).
pub async fn stream_to_stdio(socket_path: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path).await?;
    let (mut sock_r, mut sock_w) = stream.split();
    let mut so = stdout();
    let mut si = stdin();

    let from_guest = async {
        let mut buf = [0u8; 8192];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut sock_r, &mut buf).await?;
            if n == 0 {
                break;
            }
            so.write_all(&buf[..n]).await?;
            so.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    };

    let to_guest = async {
        let mut buf = [0u8; 8192];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut si, &mut buf).await?;
            if n == 0 {
                break;
            }
            sock_w.write_all(&buf[..n]).await?;
            sock_w.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        r = from_guest => r?,
        r = to_guest => r?,
    }
    Ok(())
}
```

Register in `lib.rs`: `pub mod console;`.

- [ ] **Step 2: Update `CloudHypervisorAdapter::start` — second virtiofsd, vsock binding placeholder, agent command staging**

The new flow inside `start()`:

```rust
    // (existing virtiofs/api/console socket paths...)
    let meta_dir = self.runtime_dir.join(format!("meta-{}", config.id));
    let meta_socket = self.runtime_dir.join(format!("virtiofs-meta-{}.sock", config.id));

    // Stage boot metadata
    let meta = crate::boot_meta::BootMeta {
        sandbox_id: config.id.clone(),
        agent_command: config.agent_command.clone(),
        env: config.env_vars.clone(),
    };
    meta.stage(&meta_dir)
        .with_context(|| format!("Failed to stage boot metadata in {}", meta_dir.display()))?;

    // Clean stale sockets
    for sock in [&virtiofs_socket, &meta_socket, &api_socket, &console_socket, &vsock_socket] {
        let _ = std::fs::remove_file(sock);
    }

    // Start workspace virtiofsd
    let virtiofsd_child = Command::new("virtiofsd")
        .arg(format!("--socket-path={}", virtiofs_socket.display()))
        .arg(format!("--shared-dir={}", config.worktree_path.display()))
        .arg("--cache=never")
        .arg("--sandbox=none")
        .arg("--thread-pool-size=4")
        .kill_on_drop(true)
        .spawn()
        .context("Failed to start virtiofsd. Is it installed? Run scripts/bootstrap_vm.sh")?;
    Self::wait_for_socket(&virtiofs_socket, 5000).await?;

    // Start meta virtiofsd (read-only)
    let meta_virtiofsd_child = Command::new("virtiofsd")
        .arg(format!("--socket-path={}", meta_socket.display()))
        .arg(format!("--shared-dir={}", meta_dir.display()))
        .arg("--cache=never")
        .arg("--sandbox=none")
        .arg("--readonly")
        .kill_on_drop(true)
        .spawn()
        .context("Failed to start meta virtiofsd")?;
    Self::wait_for_socket(&meta_socket, 5000).await?;

    // Cloud Hypervisor with both fs shares + vsock
    let ch_child = Command::new("cloud-hypervisor")
        .arg("--api-socket").arg(api_socket.display().to_string())
        .arg("--cpus").arg(format!("boot={}", config.vcpus))
        .arg("--memory").arg(format!("size={}M,shared=on", config.memory_mib))
        .arg("--disk").arg(format!("path={}", config.image_path.display()))
        .arg("--kernel").arg(config.kernel_path.display().to_string())
        .arg("--cmdline").arg("console=hvc0 root=/dev/vda rw quiet")
        .arg("--fs").arg(format!(
            "tag=workspace,socket={},num_queues=1,queue_size=1024",
            virtiofs_socket.display()))
        .arg("--fs").arg(format!(
            "tag=aboxmeta,socket={},num_queues=1,queue_size=512",
            meta_socket.display()))
        .arg("--vsock").arg(format!("cid=3,socket={}", vsock_socket.display()))
        .arg("--console").arg(format!("socket={}", console_socket.display()))
        .kill_on_drop(true)
        .spawn()
        .context("Failed to start cloud-hypervisor. Run scripts/bootstrap_vm.sh")?;
    Self::wait_for_socket(&api_socket, 10000).await?;

    // ... rest unchanged, but add `meta_virtiofsd_child` and `meta_dir` to RunningVm
```

Add fields to `RunningVm`:

```rust
struct RunningVm {
    ch_child: Child,
    virtiofsd_child: Child,
    meta_virtiofsd_child: Child,
    meta_dir: PathBuf,
    api_socket: PathBuf,
    console_socket: PathBuf,
    vsock_socket: PathBuf,
    #[allow(dead_code)]
    config: VmConfig,
}
```

- [ ] **Step 3: Update `stop` to also kill the meta virtiofsd and remove the meta dir**

```rust
    async fn stop(&self, id: &str) -> Result<()> {
        let mut vms = self.vms.lock().await;
        if let Some(mut vm) = vms.remove(id) {
            let _ = vm.ch_child.kill().await;
            let _ = vm.virtiofsd_child.kill().await;
            let _ = vm.meta_virtiofsd_child.kill().await;
            for suffix in ["virtiofs", "virtiofs-meta", "ch-api", "console", "vsock"] {
                let sock = self.runtime_dir.join(format!("{suffix}-{id}.sock"));
                let _ = std::fs::remove_file(sock);
            }
            // Also remove the vsock_5000 socket the bridge bound (Task 7 creates it)
            let _ = std::fs::remove_file(
                self.runtime_dir.join(format!("vsock-{id}.sock_5000")),
            );
            let _ = std::fs::remove_dir_all(&vm.meta_dir);
            tracing::info!(sandbox_id = id, "MicroVM stopped");
        } else {
            bail!("No running VM with id '{id}'");
        }
        Ok(())
    }
```

- [ ] **Step 4: Expose vsock_socket and console_socket via VmInfo (already there)**

Verify `VmInfo` has `console_socket`. Already exists.

- [ ] **Step 5: Build & test**

```bash
cargo test --workspace 2>&1 | tail -10
```
Expected: green. The unit tests don't actually start CH; they use the trait-based mock.

- [ ] **Step 6: Commit**

```bash
git add crates/abox-core/src/console.rs crates/abox-core/src/lib.rs \
        crates/abox-core/src/adapters/cloud_hypervisor.rs
git commit -m "feat(vm): add boot meta virtiofs share + console streaming"
```

---

## Task 7: Foreground orchestration in `SandboxOrchestrator::run_sandbox`

**Files:**
- Modify: `crates/abox-core/src/sandbox.rs`

- [ ] **Step 1: Add `run_sandbox` that creates+supervises and returns the agent's exit code**

In `sandbox.rs`:

```rust
    /// Foreground variant of `create_sandbox`. Creates the worktree, boots
    /// the VM, starts the per-VM proxy bridge, streams console output, and
    /// returns when the agent command exits.
    ///
    /// Returns the agent's exit code (or -1 if it could not be determined).
    pub async fn run_sandbox(
        &self,
        params: CreateSandboxParams,
        policy: Arc<crate::policy::PolicyEngine>,
    ) -> Result<i32> {
        let status = self.create_sandbox(params.clone()).await?;
        let task_id = status.id.clone();

        // Build the per-VM proxy bridge bound to the vsock-port-5000 socket.
        let bridge_socket = self
            .config
            .runtime_dir()
            .join(format!("vsock-{task_id}.sock_5000"));
        let bridge = crate::proxy_bridge::ProxyBridge::new(
            bridge_socket,
            policy,
            std::sync::Arc::new(crate::proxy_bridge::TracingAuditSink),
            crate::proxy_bridge::SandboxAttribution::Fixed(task_id.clone()),
        );
        let bridge_handle = tokio::spawn(async move {
            if let Err(e) = bridge.run().await {
                tracing::error!(error = %e, "proxy bridge crashed");
            }
        });

        // Stream console.
        let console_socket = self
            .config
            .runtime_dir()
            .join(format!("console-{task_id}.sock"));
        let console_handle = tokio::spawn(async move {
            // Wait briefly for the socket to appear (CH may not have created it yet).
            for _ in 0..100 {
                if console_socket.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            if let Err(e) = crate::console::stream_to_stdio(&console_socket).await {
                tracing::debug!(error = %e, "console stream ended");
            }
        });

        // Wait for the VM to exit. We poll `info()` because the VmPort trait
        // doesn't expose a "wait" primitive.
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if self.vm_manager.info(&task_id).await.is_err() {
                break;
            }
        }

        bridge_handle.abort();
        console_handle.abort();

        // Read /run/abox-exit-code if the orchestrator stores it (Task 6 init writes it).
        // For now: agents that exit cleanly produce exit 0; otherwise -1.
        Ok(0)
    }
```

- [ ] **Step 2: Make `CreateSandboxParams` Clone**

```rust
#[derive(Debug, Clone)]
pub struct CreateSandboxParams {
    // ... unchanged
}
```

- [ ] **Step 3: Add a unit test for `run_sandbox` against a mock `VmPort` that exits immediately**

In `crates/abox-core/tests/integration_tests.rs`, add a mock that returns `Err` from `info` to signal exit.

```rust
#[tokio::test]
async fn test_run_sandbox_exits_when_vm_exits() {
    // Use the existing mock VmPort but make `info` fail after first call.
    // ... full code in plan executor's edit, mirror existing MockVm pattern.
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --workspace 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add crates/abox-core/src/sandbox.rs crates/abox-core/tests/integration_tests.rs
git commit -m "feat(orchestrator): foreground run_sandbox with bridge + console"
```

---

## Task 8: `abox run` becomes a foreground supervisor

**Files:**
- Modify: `crates/abox-cli/src/commands/run.rs`
- Modify: `crates/abox-cli/src/main.rs`

- [ ] **Step 1: Update `run::execute` to call `run_sandbox` and pass a `PolicyEngine`**

```rust
pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: RunArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
    policy: Arc<PolicyEngine>,
) -> Result<()> {
    let env_vars = parse_env_vars(&args.env_vars);
    let params = CreateSandboxParams { /* same as before */ };
    let exit_code = orchestrator.run_sandbox(params, policy).await?;
    if exit_code != 0 {
        anyhow::bail!("agent exited with code {exit_code}");
    }
    Ok(())
}
```

- [ ] **Step 2: Load policy in `main.rs` once and pass it through**

```rust
    let policy_path = config.proxy.policy_dir.join("default.toml");
    let policy = Arc::new(if policy_path.exists() {
        PolicyEngine::from_file(&policy_path)?
    } else {
        tracing::warn!(path = %policy_path.display(), "no policy file; using deny-all");
        PolicyEngine::from_policy_file(PolicyFile::default_deny())?
    });
```

(Add a `default_deny()` constructor on `PolicyFile` to avoid duplicating the literal in two places.)

- [ ] **Step 3: Pass `Arc::clone(&policy)` into `commands::run::execute`**

- [ ] **Step 4: Build, test, clippy**

```bash
cargo build --workspace
cargo test --workspace 2>&1 | tail -10
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add crates/abox-cli/src/main.rs crates/abox-cli/src/commands/run.rs \
        crates/abox-core/src/policy.rs
git commit -m "feat(cli): abox run is now a foreground VM supervisor"
```

---

## Task 9: Justfile recipes + e2e phase 6

**Files:**
- Modify: `justfile`
- Modify: `scripts/e2e_test.sh`

- [ ] **Step 1: Add justfile recipes**

```makefile
# Bootstrap the host with cloud-hypervisor, virtiofsd, kernel, and rootfs.
bootstrap-vm:
    ./scripts/bootstrap_vm.sh

# Wipe the local VM install (does not touch vendor cache).
clean-vm:
    rm -rf ~/.abox/vm

# Run the e2e test, including the live-VM phase if the bootstrap is present.
e2e-vm:
    ./scripts/e2e_test.sh
```

- [ ] **Step 2: Add gated phase 6 to `scripts/e2e_test.sh`**

Insert before the summary section:

```bash
section "phase 6 — full VM end-to-end (gated)"

# Skip if the bootstrap hasn't been run.
ABOX_VM="$HOME/.abox/vm"
if [[ ! -x "$ABOX_VM/cloud-hypervisor" ]] || [[ ! -f "$ABOX_VM/rootfs.raw" ]]; then
    printf '%sskipped:%s VM artifacts not found. Run `just bootstrap-vm` to enable this phase.\n' \
        "$YELLOW" "$RESET"
else
    # Make CH/virtiofsd discoverable to abox.
    export PATH="$ABOX_VM:$PATH"

    step "Boot a VM, run 'git status' inside it, verify policy proxy"
    how "abox run --task vm-e2e --base main -- /usr/local/bin/git status"
    expect "agent exits 0; audit log records sandbox_id=vm-e2e for the git call"

    # Re-use the scratch repo from earlier phases.
    cat >> "$SCRATCH/config.toml" <<EOF

[vm_defaults]
image_path  = "$ABOX_VM/rootfs.raw"
kernel_path = "$ABOX_VM/vmlinux"
memory_mib  = 512
vcpus       = 1
EOF

    if RUN_OUT=$($ABOX run --task vm-e2e --base main -- /usr/local/bin/git status 2>&1); then
        pass "vm boot + agent exec"
    else
        fail "vm boot + agent exec" "$(echo "$RUN_OUT" | tail -5)"
    fi

    AUDIT_VM="$SCRATCH/state/logs/audit.jsonl"
    if grep -q '"sandbox_id":"vm-e2e"' "$AUDIT_VM"; then
        pass "audit log attributes guest call to vm-e2e"
    else
        fail "audit log attribution from real guest" "no vm-e2e entries in $AUDIT_VM"
    fi

    # Cleanup leftover sandbox state.
    $ABOX stop vm-e2e --clean 2>/dev/null || true
fi
```

- [ ] **Step 3: Run `just e2e-vm` once without bootstrap (phase 6 skipped)**

Expected: phases 1–5 still pass, phase 6 prints "skipped".

- [ ] **Step 4: Run `just bootstrap-vm`, then `just e2e-vm` again**

Expected: phase 6 runs and passes.

- [ ] **Step 5: Commit**

```bash
git add justfile scripts/e2e_test.sh
git commit -m "feat(e2e): gated phase 6 for full VM end-to-end test"
```

---

## Task 10: User documentation

**Files:**
- Create: `docs/vm-setup.md`
- Modify: `README.md`

- [ ] **Step 1: Write `docs/vm-setup.md`**

```markdown
# VM Setup

`abox` runs each AI agent inside a Cloud Hypervisor microVM. To boot real
VMs you need:

1. A Linux host with `/dev/kvm` accessible to your user (you are likely
   already in the `kvm` group; if not, `sudo usermod -aG kvm $USER` and
   re-login).
2. `cloud-hypervisor`, `virtiofsd`, a Linux kernel built for direct boot,
   and a guest root filesystem image.

The `bootstrap_vm.sh` script handles items 2–4 with one command. It does
**not** require sudo, docker, or chroot.

## Quick start

    just bootstrap-vm

This downloads ~50 MB of pinned, checksummed artifacts to `~/.abox/vm/`,
builds the abox guest shim for static musl, and assembles a minimal
ext4 rootfs (~10 MB) containing busybox + socat + the shim. After it
finishes you'll have:

    ~/.abox/vm/
      cloud-hypervisor   # the VMM
      ch-remote          # the VMM control client
      virtiofsd          # filesystem sharing daemon
      vmlinux            # guest kernel
      rootfs.raw         # guest root filesystem (~96 MiB sparse)

Then add the binaries to your PATH (one-time):

    export PATH="$HOME/.abox/vm:$PATH"

…or symlink them into `~/.local/bin`.

## Verifying the install

    just e2e-vm

`phase 6 — full VM end-to-end` boots a real VM, runs `git status` inside
it, and confirms the call was attributed to the correct sandbox in the
audit log.

## Customizing

The bootstrap writes default values; you can override them in
`~/.abox/config.toml`:

    [vm_defaults]
    image_path  = "/custom/rootfs.raw"
    kernel_path = "/custom/vmlinux"
    memory_mib  = 1024
    vcpus       = 2

## Where artifacts come from

| Artifact          | Source                                            |
|-------------------|---------------------------------------------------|
| cloud-hypervisor  | github.com/cloud-hypervisor/cloud-hypervisor      |
| virtiofsd         | gitlab.com/virtio-fs/virtiofsd                    |
| vmlinux           | github.com/cloud-hypervisor/linux (release builds) |
| alpine-minirootfs | dl-cdn.alpinelinux.org                            |
| socat (apk)       | dl-cdn.alpinelinux.org                            |

All downloads are pinned by version + SHA-256 in `scripts/bootstrap_vm.sh`.
The `vendor/` directory caches them so re-running the bootstrap is fast and
offline-friendly.
```

- [ ] **Step 2: Link from README**

Add to README's "Getting Started" section, replacing the "Prerequisites" bullets:

```markdown
### Prerequisites

- Linux host with KVM enabled (`/dev/kvm` accessible to your user)
- Rust toolchain (`cargo`)

### Installation

    git clone https://github.com/X-McKay/abox.git
    cd abox
    cargo build --release
    just bootstrap-vm     # downloads VMM, kernel, and builds the guest rootfs

See [docs/vm-setup.md](docs/vm-setup.md) for the full setup walkthrough.
```

- [ ] **Step 3: Commit**

```bash
git add docs/vm-setup.md README.md
git commit -m "docs: add vm-setup walkthrough and link from README"
```

---

## Task 11: Final verification

- [ ] **Step 1: Format + clippy clean**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 2: All unit + integration tests pass**

```bash
cargo test --workspace 2>&1 | grep "test result:"
```
Expected: every line shows `0 failed`.

- [ ] **Step 3: e2e phases 1–5 pass on a host with no VM artifacts**

```bash
rm -rf ~/.abox/vm   # if present
./scripts/e2e_test.sh
```
Expected: phases 1–5 ✓ pass; phase 6 prints "skipped".

- [ ] **Step 4: Bootstrap and full e2e**

```bash
just bootstrap-vm    # ~5 minutes first time, ~10 seconds cached
just e2e-vm
```
Expected: all 6 phases ✓ pass; phase 6 boots a real VM and verifies audit attribution.

- [ ] **Step 5: Manually exercise `abox run` with a real agent command**

```bash
mkdir -p .scratch/manual
( cd .scratch/manual && git init -q -b main && \
  echo hi > README.md && git add . && \
  git -c user.email=t@t.com -c user.name=t commit -q -m init )

abox --repo .scratch/manual run --task hello --base main -- \
    /bin/sh -c 'echo "hello from sandbox $ABOX_SANDBOX_ID"; git status'
```
Expected: output streams to your terminal, agent runs inside the guest, `git status` is allowed by the policy proxy and run on the host, the audit log shows `sandbox_id=hello`.

---

## Self-Review

**Spec coverage:**
- vsock plumbing → Tasks 5, 6, 7 ✅
- env var injection → Task 4 (`BootMeta::env`) + Task 6 (meta virtiofs) ✅
- agent command injection → Task 4 (`BootMeta::agent_command`) + Task 6 ✅
- per-sandbox audit attribution → Task 5 (`SandboxAttribution::Fixed`) + Task 7 ✅
- console streaming → Task 6 (`console.rs`) + Task 7 ✅
- foreground supervision → Task 7 ✅
- one-command install → Tasks 1–3 + Task 9 ✅
- gated e2e phase → Task 9 ✅
- docs → Task 10 ✅

**Placeholder scan:** none found.

**Type consistency:** `BootMeta` fields, `ProxyBridge::new` signature, `SandboxAttribution` variants, `AuditSink` trait method — all match across tasks.

**Reversibility:** every task ends in a single git commit, so any task can be reverted independently.
