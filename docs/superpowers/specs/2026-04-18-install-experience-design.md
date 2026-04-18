# Install & First-Run Experience Design

## Context

abox is a sandbox runtime that boots Cloud Hypervisor microVMs with git-worktree isolation for AI coding agents. The current "zero to first sandbox" flow requires 10-12 minutes from absolute zero: install Rust, build from source, run `abox init` (downloads ~60 MB of artifacts, builds a 768 MiB rootfs locally, generates a CA keypair), then manually configure credentials. This friction is the primary barrier to adoption.

The release infrastructure is largely built — `install.sh` (curl-pipe installer), `release.yml` (GitHub Actions), and `release.sh` (local orchestration) exist. The `vm-assets` job in `release.yml` already builds and bundles a rootfs into a tarball (`release.yml:64-67`). `init.rs` already embeds the default policy via `include_str!` (`init.rs:237`). `bootstrap_vm.sh --yes` already auto-installs the musl target (`bootstrap_vm.sh:196-199`). No release has been published yet.

Two blockers prevent a working curl-pipe install today:
1. **CA trust is baked into the rootfs at build time** (`build_rootfs.sh:173-177`). A CI-built rootfs trusts the CI machine's CA, not the end user's CA, breaking MITM credential injection.
2. **VM binaries are resolved from PATH** (`cloud_hypervisor.rs:249,299,359`). `install.sh` places them in `~/.abox/vm/` but the runtime has no way to find them there.

This spec designs the full install experience across three phases: P1 ships a working release, P2 polishes solo-dev UX, P3 expands platform support.

## Target: 2 minutes from zero to first sandbox

```
curl -fsSL .../install.sh | bash       # ~90s: download binary + pre-built VM assets (~140 MB)
abox init                               # ~10s: KVM check, CA gen, credential wizard, config
abox run --task hello -- echo hello     # ~290ms: boot VM, run command, clean up
```

---

## P1: Ship v0.1.0

**Goal:** `curl ... | bash && abox init && abox run --task hello -- echo hello` works on any Linux x86_64 machine with KVM. No Rust, fakeroot, or npm on host.

### 1.1 Decouple CA trust from rootfs build

**Files:** `scripts/build_rootfs.sh`, `guest/init.sh`, `crates/abox-core/src/sandbox.rs`

**Problem:** `build_rootfs.sh:173-177` bakes `$HOME/.abox/ca/root.crt` into the rootfs at build time. `sandbox.rs:217-218` sets `NODE_EXTRA_CA_CERTS=/etc/ssl/certs/abox-ca.pem`, assuming the CA is pre-baked. A CI-built rootfs trusts the CI machine's CA, not the user's.

**Fix:** Stop baking the CA into the rootfs. Instead, inject it at boot via the existing `aboxmeta` virtiofs share (already mounted read-only at `/abox-meta` by `guest/init.sh:34`):

1. **`build_rootfs.sh`:** Remove the CA-baking block (lines 172-177). The rootfs ships with only Mozilla CAs in the system trust store.
2. **Orchestrator (`sandbox.rs`):** Stage the user's `root.crt` into the per-sandbox `meta_dir` alongside `runner.sh` and credentials. The meta virtiofsd already serves this directory.
3. **`guest/init.sh`:** After mounting `aboxmeta`, append `/abox-meta/root.crt` to `/etc/ssl/certs/ca-certificates.crt` and copy it to `/etc/ssl/certs/abox-ca.pem`. This runs before the agent command, so the trust store is ready.
4. **`sandbox.rs:218`:** `NODE_EXTRA_CA_CERTS=/etc/ssl/certs/abox-ca.pem` remains correct — the file just arrives at boot instead of build time.

**For source builders:** `build_rootfs.sh` no longer needs `~/.abox/ca/root.crt` to exist at build time, removing an ordering dependency. Source builders and CI produce identical rootfs images.

**Effort:** S | **Impact:** Critical blocker — without this, no pre-built rootfs can work.

### 1.2 Resolve VM binaries from config, not PATH

**Files:** `crates/abox-core/src/adapters/cloud_hypervisor.rs`, `crates/abox-core/src/config.rs`

**Problem:** `cloud_hypervisor.rs` uses `Command::new("virtiofsd")` (line 249), `Command::new("cloud-hypervisor")` (line 299), and `Command::new("ch-remote")` (line 359) — pure PATH-based resolution. `install.sh` puts these in `~/.abox/vm/` but never adds that to PATH. The `--from-bundle` path in `bootstrap_vm.sh` exits at line 116 before creating `~/.local/bin` symlinks.

**Fix:** Resolve binaries from `state_dir/vm/` (i.e., `~/.abox/vm/`) in Rust:

1. **`config.rs`:** Add optional path fields to `VmDefaults`:
   ```rust
   pub cloud_hypervisor_path: Option<PathBuf>,
   pub virtiofsd_path: Option<PathBuf>,
   pub ch_remote_path: Option<PathBuf>,
   ```
   Defaults: `None` (fall back to auto-detection).

2. **`cloud_hypervisor.rs`:** Replace bare `Command::new("virtiofsd")` etc. with a resolution function:
   - If config field is set, use it
   - Else check `state_dir/vm/<binary>` (covers `install.sh` users)
   - Else fall back to PATH lookup (covers source builders who symlinked to `~/.local/bin`)
   - Fail with an actionable message if not found anywhere

3. **`doctor.rs`:** Update the "cloud-hypervisor on PATH" check to use the same resolution logic.

**Effort:** S | **Impact:** Critical blocker — without this, `install.sh` users can't boot VMs.

### 1.3 Release pipeline smoke test

**Files:** `.github/workflows/release.yml`, `scripts/release.sh`

**Problem:** `release.yml` only triggers on tag push (line 3). `release.sh` creates a local commit and tag but does not push (line 19 comment, lines 362-363 print manual push instructions). There's no way to test the full install flow before going live.

**Fix:** Add a staging smoke path:

1. **`release.yml`:** Add a `workflow_dispatch` trigger with an optional `dry_run` input. When `dry_run` is true, build artifacts and upload them as workflow artifacts (not a GitHub Release). This lets you test the full CI pipeline without publishing.
2. **Local staging test:** After `release.sh --dry`, manually build the release binary and tarball locally, serve them from a temp HTTP server (`python3 -m http.server`), and run `install.sh` against `ABOX_VERSION=v0.1.0 BASE_URL=http://localhost:8000 bash install.sh` (requires adding `BASE_URL` override to `install.sh`).

**Effort:** S | **Impact:** Prevents shipping a broken release. Catches artifact-name mismatches before the tag is public.

### 1.4 KVM error diagnostics

**Files:** `crates/abox-cli/src/commands/init.rs`, `crates/abox-cli/src/commands/doctor.rs`

Create a single shared `kvm_diagnostics()` function (in a new `crates/abox-cli/src/kvm.rs` or similar) used by both `init.rs:77` and `doctor.rs:119`. The existing checks are simple "does `/dev/kvm` exist and is it accessible" — enhance to detect *why* it's missing:

| Condition | Detection | Remediation |
|-----------|-----------|-------------|
| No CPU virt extensions | `/proc/cpuinfo` lacks `vmx`/`svm` | "Enable VT-x/AMD-V in BIOS, or use bare-metal" |
| KVM module not loaded | `/dev/kvm` missing but flags present | `sudo modprobe kvm_intel` / `kvm_amd` |
| Permission denied | `/dev/kvm` exists, open returns EACCES | `sudo usermod -aG kvm $USER && newgrp kvm` |
| Inside container | `/proc/1/cgroup` shows container ns | "Use `--device /dev/kvm` when launching container" |
| Nested virt disabled | Inside VM, no virt extensions | "Enable nested virtualization on host hypervisor" |
| WSL2 | `/proc/version` contains `microsoft` | "Set `nestedVirtualization=true` in `.wslconfig`" |

**Effort:** S | **Impact:** Turns a cryptic "KVM not available" into actionable guidance per environment.

### 1.5 `abox init` without source tree

**Files:** `crates/abox-cli/src/commands/init.rs`

When installed via `install.sh`, there's no source tree. The init flow adapts:

- Detect that VM assets already exist at `~/.abox/vm/` (placed by `install.sh`) — skip bootstrap entirely. No need to find `scripts/bootstrap_vm.sh`.
- Generate CA keypair directly via `RootCa::load_or_generate()` from abox-core (already a library function — stop shelling out to `cargo run --example ca_init`)
- Default policy is already embedded via `include_str!` at `init.rs:237` — this path works as-is for binary installs
- Ensure `config.toml` generation wires `image_path` and `kernel_path` to `~/.abox/vm/` paths

Net result: `abox init` for binary-install users = KVM check + CA gen + credential detection + config write. ~5 seconds, no network.

**Effort:** S | **Impact:** Makes `abox init` work cleanly without Rust/source tree.

### 1.6 Credential detection in `abox init`

**Files:** `crates/abox-cli/src/commands/init.rs`

**Corrected scope:** The credential wizard as originally designed was modeled against the wrong config surfaces. Here's what actually exists:

- **Guest credential staging** uses `[[guest.credential_files]]` in `config.toml` (`config.rs:58-61`), not `[[credentials]]`
- **Claude staging already exists by default** — `GuestConfig::default()` includes `~/.claude/.credentials.json` (`config.rs:67-72`), and missing files are safely skipped (`sandbox.rs:92-109`)
- **HTTP egress injection** (GITHUB_TOKEN, GOOGLE_API_KEY, OPENAI_API_KEY) is policy-driven via `[[egress]]` rules in `policies/default.toml:96-137` — these already work out of the box with no config changes
- **There is no `[proxy.http_egress]` config schema** — `ProxyConfig` only has `egress_port` and `policy_dir` (`config.rs:92-101`)
- **SSH agent forwarding** is policy-driven per-command (`default.toml:18` for git, `default.toml:55` for gh) — not a config concern

**What the wizard actually needs to do for P1:**
1. Detect if `~/.codex/auth.json` exists on host. If so, prompt to add a `[[guest.credential_files]]` entry for it (Codex is not in the default config, unlike Claude)
2. Print a status summary: "Claude credentials: found / Codex: found / GITHUB_TOKEN: set / GOOGLE_API_KEY: not set" — so users know what's active
3. With `--yes`: auto-approve Codex entry if detected

This is a much narrower, more accurate scope than the original design.

**Effort:** S | **Impact:** Codex users get zero-config credential forwarding. All users get visibility into what's active.

### 1.7 Guard aarch64 in install.sh

**Files:** `scripts/install.sh`

**Problem:** `install.sh:21` accepts aarch64 and maps it to `aarch64-unknown-linux-gnu`, but `release.yml` doesn't build aarch64 (commented out), `bootstrap_vm.sh` explicitly rejects it (line 77), and `build_rootfs.sh` hard-codes x86_64 Alpine URLs (lines 62, 66, 96, 110).

**Fix:** Add a guard after architecture detection:

```bash
if [[ "$TARGET" == "aarch64-unknown-linux-gnu" ]]; then
    echo "ERROR: aarch64 support is not yet available." >&2
    echo "See https://github.com/X-McKay/abox/issues/XX for tracking." >&2
    exit 1
fi
```

**Effort:** XS | **Impact:** Prevents confusing failure when aarch64 users hit a missing release asset.

### 1.8 Cut v0.1.0

**Depends on:** All above steps merged.

1. Run the staging smoke test (1.3) to verify the full flow
2. Run `release.sh v0.1.0 --dry` to review the diff
3. Run `release.sh v0.1.0` to commit + tag
4. Push: `git push origin main --tags`
5. Verify `release.yml` produces correct artifacts
6. Test `install.sh` against the live release on a clean machine

**Effort:** S | **Impact:** Unblocks the entire curl-pipe install path.

### 1.9 One-line install summary

```bash
curl -fsSL https://raw.githubusercontent.com/X-McKay/abox/main/scripts/install.sh | bash
abox init
cd ~/my-project && abox run --task hello -- echo hello
```

**Total: ~2 minutes** (down from 10-12). Host dependencies: none beyond `curl` and `tar`.

---

## P2: Polish Solo-Dev UX

**Goal:** First 10 minutes after install feel guided. Source-builders hit fewer sharp edges.

### 2.1 `abox update` command

**Files:** New `crates/abox-cli/src/commands/update.rs`

Self-update mechanism for binary-installed users:

1. Query GitHub API for latest release
2. Compare against compiled-in version (`env!("CARGO_PKG_VERSION")`)
3. If newer: show changelog excerpt, ask to proceed
4. Download new binary + VM assets tarball + SHA256SUMS to temp dir
5. Verify checksums
6. Atomic binary replacement: write new binary to a temp file in the same directory (same filesystem), then `rename()` over the old binary
7. Extract updated VM assets to `~/.abox/vm/`
8. Preserve user config: `config.toml`, `policies/`, `ca/` are never overwritten

**Flags:** `--check` (report only), `--yes` (skip confirmation), `--version` (pin/rollback)

**Edge cases:**
- Source-build detected (binary not under `~/.abox/bin/`): suggest `git pull && cargo build --release`
- Install-location detection: read `std::env::current_exe()` to find the running binary's actual path

**Effort:** M | **Impact:** Users stay current without re-running the full install flow.

### 2.2 `abox demo` — first-run tutorial

**Files:** New `crates/abox-cli/src/commands/demo.rs`

After `abox init`, print: "Try `abox demo` for a guided walkthrough."

Creates a temporary git repo and runs a scripted sequence:

1. **Boot test:** `abox run --task demo-boot -- echo "Hello from inside a microVM!"` — proves sandbox works
2. **Workspace isolation:** `abox run --task demo-fs -- sh -c 'ls /workspace/'` — shows virtiofs mount
3. **Credential test:** `abox run --task demo-creds -- git remote -v` (if git creds configured) — proves credential forwarding
4. **Policy demo:** Shows a blocked request to illustrate policy enforcement
5. Clean up temp repo

Each step prints an explanation. Total: ~2-3 seconds (4 fast VM boots).

**Effort:** M | **Impact:** Builds confidence that the setup works correctly, teaches key concepts.

### 2.3 Credential auto-detection improvements

**Files:** `crates/abox-cli/src/commands/init.rs`, new `creds.rs` subcommand

Enhance the P1 credential detection:

- **SSH agent:** Detect `SSH_AUTH_SOCK`, confirm it's active
- **AWS credential chain:** Check `~/.aws/credentials`, `AWS_ACCESS_KEY_ID`, AWS SSO cache
- **OAuth token freshness:** Detect expired Claude/Codex tokens, prompt to re-auth on host
- **`abox creds`:** New subcommand showing configured credential files, policy egress rules with their env-var status, and overall readiness

**Effort:** M | **Impact:** Covers more credential sources, catches stale tokens before they cause confusing sandbox failures.

### 2.4 Source-builder quality of life

**Files:** `scripts/bootstrap_vm.sh`, new `crates/abox-cli/src/commands/rootfs.rs`

- **`abox rootfs rebuild`:** First-class command wrapping `build_rootfs.sh` for users customizing the guest image.
- **Stale rootfs warning:** `abox run` checks `rootfs.raw.inputs` hash — if stale, prints: "Guest init.sh or shim has changed. Run `abox rootfs rebuild` to update."

Note: musl auto-install is already handled by `bootstrap_vm.sh --yes` (`bootstrap_vm.sh:196-199`).

**Effort:** S per item, M total | **Impact:** Makes rootfs customization discoverable.

### 2.5 PATH and environment handling

**Files:** `crates/abox-cli/src/commands/init.rs`

- **Shell snippet:** `abox init` offers to append `export PATH="$HOME/.abox/bin:$PATH"` to `~/.bashrc` / `~/.zshrc` (with confirmation). Same pattern as `rustup`.
- **`abox env`:** Prints shell-evaluable setup: `eval "$(abox env)"` — exports `PATH`, `ABOX_HOME`. Useful in CI scripts and dotfiles.

**Effort:** S | **Impact:** Eliminates the "`abox: command not found`" post-install surprise.

---

## P3: Platform Expansion

**Goal:** Break the KVM-only, x86_64-only, Linux-only constraints.

### 3.1 VMM abstraction layer

**Files:** `crates/abox-core/src/adapters/cloud_hypervisor.rs` (refactor), new `crates/abox-core/src/vmm/` module

The guest side (rootfs, init.sh, shim, proxy) is VMM-agnostic. The VMM-specific surface is narrow:

1. VM config generation (CH JSON API)
2. VM lifecycle (ch-remote over Unix socket)
3. Device wiring (virtiofs, vsock, console)
4. Binary paths (cloud-hypervisor, virtiofsd)

Extract a `VmmBackend` trait:

```rust
pub trait VmmBackend: Send + Sync {
    fn name(&self) -> &str;
    fn launch(&self, config: &SandboxConfig) -> Result<VmHandle>;
    fn shutdown(&self, handle: &VmHandle) -> Result<()>;
    fn is_available(&self) -> Result<BackendStatus>;
}

pub enum BackendStatus {
    Ready,
    Degraded { reason: String },
    Unavailable { reason: String, remediation: String },
}
```

`config.toml` gains `vmm.backend`: `"cloud-hypervisor"` (default), `"qemu"`, or `"auto"` (prefers CH/KVM > QEMU/KVM > QEMU/HVF > QEMU/TCG).

**Effort:** M | **Impact:** Enables all subsequent platform work without guest-side changes.

### 3.2 QEMU backend

**Files:** New `crates/abox-core/src/vmm/qemu.rs`

Implements `VmmBackend` for QEMU with accelerator auto-detection:

| Host | Accelerator | Performance |
|------|-------------|-------------|
| Linux + KVM | `qemu -accel kvm` | Near-native |
| Linux, no KVM | `qemu -accel tcg` | ~10-50x slower |
| macOS Intel | `qemu -accel hvf` | Near-native |
| macOS Apple Silicon | `qemu -accel hvf` | Near-native (aarch64 guest) |

Device mapping:
- virtiofs: `vhost-user-fs-pci` (requires virtiofsd — same binary)
- vsock: `vhost-vsock-pci` (Linux), or TCP-over-virtio-net fallback (macOS)
- Lifecycle: QMP (QEMU Machine Protocol) over Unix socket replaces ch-remote

QEMU is available via system package managers (`apt install qemu-system-x86`, `brew install qemu`). Unlike CH, we don't bundle it — detect and require it. `abox doctor` gains a QEMU check when the QEMU backend is selected.

**Effort:** L | **Impact:** Unlocks macOS, no-KVM Linux, and QEMU TCG as universal fallback.

### 3.3 aarch64 support

**Depends on:** 3.2 for cross-arch. Native ARM can proceed independently.

**Native ARM (aarch64 host + aarch64 guest):**
- Fill placeholder SHA256 checksums in `bootstrap_vm.sh` for ARM builds of CH, virtiofsd, kernel
- Uncomment aarch64 build in `release.yml`
- Build aarch64 rootfs — requires parameterizing `build_rootfs.sh` which currently hard-codes x86_64 Alpine package URLs (`build_rootfs.sh:62,66,96,110`) and x86_64 Alpine signing key path
- Parameterize `bootstrap_vm.sh` Alpine miniroot URL for aarch64
- Test on ARM hardware (Graviton, Ampere)
- **Effort:** M-L — more than "fill checksums and uncomment CI"; rootfs and bootstrap scripts need arch parameterization

**Cross-arch (x86_64 <-> aarch64):**
- Only via QEMU TCG (software emulation, very slow)
- Niche use case, minimal additional work once QEMU backend exists
- **Effort:** S (after 3.2)

### 3.4 macOS support

**Depends on:** 3.2 (QEMU backend)

- **virtiofsd on macOS:** Linux-specific (uses FUSE_DEV). Alternatives: QEMU's built-in virtiofs, or 9p filesystem sharing (`-virtfs`, simpler but ~2-5x slower for heavy I/O). 9p is the pragmatic first choice — correctness first, optimize later if profiling shows it matters.
- **vsock on macOS:** No vhost-vsock. Fall back to TCP-over-virtio-net for proxy bridge. Slightly higher latency, functionally identical.
- **`install.sh` cross-platform:** Add `Darwin` detection, serve macOS binaries + VM assets. Same `curl ... | bash` everywhere.
- **PATH handling:** macOS defaults differ (`/usr/local/bin`, `/opt/homebrew/bin`). `abox init` detects platform and adjusts.

**Effort:** L | **Impact:** Opens abox to the macOS developer population. virtiofs/vsock alternatives are the main engineering challenge.

### 3.5 CI and headless mode

- **`abox init --headless`:** Combines `--yes` with environment-variable credential auto-detection. No prompts, no wizard, no demo suggestion. Configure and go.
- **Docker image:** `docker run --device /dev/kvm ghcr.io/x-mckay/abox:latest run --task ci -- echo hello` — pre-installed binary + VM assets. Users bring KVM via `--device`.
- **GitHub Action:** `uses: x-mckay/abox-action@v1` — setup step for self-hosted runners with KVM, or any runner with QEMU TCG fallback (slower but works).

**Effort:** M (headless), M (Docker), S (Action) | **Impact:** Enables abox in CI pipelines and automated workflows.

---

## Summary: Effort vs. Impact

| Item | Phase | Effort | Impact |
|------|-------|--------|--------|
| Decouple CA from rootfs | P1 | S | Critical blocker — enables pre-built rootfs |
| Resolve VM binaries from config | P1 | S | Critical blocker — enables install.sh users |
| Release pipeline smoke test | P1 | S | Prevents shipping broken release |
| KVM error diagnostics | P1 | S | Actionable errors per environment |
| `abox init` without source tree | P1 | S | Binary install works cleanly |
| Credential detection (narrow) | P1 | S | Codex detection + status summary |
| Guard aarch64 in install.sh | P1 | XS | Prevents confusing aarch64 failures |
| Cut v0.1.0 | P1 | S | Unblocks curl-pipe install |
| `abox update` | P2 | M | Self-update without reinstall |
| `abox demo` tutorial | P2 | M | Guided first-run experience |
| Credential improvements | P2 | M | More sources, freshness checks |
| Source-builder QoL | P2 | M | Rootfs rebuild, stale warning |
| PATH handling | P2 | S | Eliminates command-not-found |
| VMM abstraction | P3 | M | Enables all platform work |
| QEMU backend | P3 | L | macOS + no-KVM fallback |
| aarch64 support | P3 | M-L | ARM — requires arch parameterization |
| macOS support | P3 | L | macOS developer audience |
| CI/headless mode | P3 | M | Automated workflows |

## Verification

### P1 verification
1. On a clean Ubuntu without Rust: `curl ... | bash && abox init && abox run --task hello -- echo hello` completes in <3 minutes
2. On a machine without KVM: `abox init` prints a specific, actionable error message (not generic)
3. With Codex credentials on host: init detects them and adds `[[guest.credential_files]]` entry
4. `abox doctor` passes all checks after curl-pipe install + init (rootfs freshness check warns as expected for released binaries — `doctor.rs:327`)
5. Source builders: `cargo build && abox init` still works (no regression)

### P2 verification
1. `abox update --check` reports installed version correctly
2. `abox demo` runs scripted sandbox operations and prints explanations
3. `abox creds` shows credential files and policy egress rule status
4. `abox env` outputs valid shell that puts abox on PATH

### P3 verification
1. On Linux without KVM: `abox run` with QEMU/TCG backend completes (slower but functional)
2. On macOS with QEMU+HVF: `abox run --task hello -- echo hello` works
3. `install.sh` on macOS: downloads correct platform binaries
4. `install.sh` on aarch64: exits with clear "not yet supported" message
5. `abox init --headless` in a Docker container with `--device /dev/kvm`: zero prompts, fully configured

## Critical files

- `scripts/build_rootfs.sh` — remove CA baking (lines 172-177)
- `guest/init.sh` — add boot-time CA injection from `/abox-meta/root.crt`
- `crates/abox-core/src/sandbox.rs` — stage CA into meta_dir
- `crates/abox-core/src/adapters/cloud_hypervisor.rs` — resolve binaries from config/state_dir
- `crates/abox-core/src/config.rs` — add VM binary path fields
- `crates/abox-cli/src/commands/init.rs` — source-tree-free init, credential detection, KVM diagnostics
- `crates/abox-cli/src/commands/doctor.rs` — shared KVM diagnostics, binary resolution checks
- `.github/workflows/release.yml` — add workflow_dispatch smoke test trigger
- `scripts/install.sh` — aarch64 guard, optional BASE_URL override
- `scripts/release.sh` — verify existing flow works end-to-end
- `policies/default.toml` — already correct, no changes needed
- `templates/config.example.toml` — document new VM binary path fields
