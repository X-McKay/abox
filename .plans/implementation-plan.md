# agentbox: Detailed Rust Implementation Plan

> A lightweight, self-hosted tool for running parallel AI coding agents in hardware-isolated MicroVMs, with git worktree integration, dual-layer credential proxying, and a central management interface.

**Language:** Rust (Cargo workspace, Hexagonal Architecture)
**Isolation:** KVM hardware virtualization via Cloud Hypervisor
**Filesystem Sharing:** `virtiofs` for live, bidirectional worktree mounting
**Credential Model:** Dual-layer proxy (CLI shim over VSock + HTTP egress proxy)
**Agent Model:** Agent runs *inside* the VM with full, unmodified shell access

---

## Table of Contents

1. [Design Decisions and Rationale](#1-design-decisions-and-rationale)
2. [High-Level Architecture](#2-high-level-architecture)
3. [Project Structure](#3-project-structure)
4. [Phase 1: Workspace Manager](#4-phase-1-workspace-manager)
5. [Phase 2: VM Lifecycle Manager](#5-phase-2-vm-lifecycle-manager)
6. [Phase 3: Dual-Layer Credential Proxy](#6-phase-3-dual-layer-credential-proxy)
7. [Phase 4: CLI and TUI](#7-phase-4-cli-and-tui)
8. [Phase 5: Snapshot and Template System](#8-phase-5-snapshot-and-template-system)
9. [Phase 6: Guest Image Builder](#9-phase-6-guest-image-builder)
10. [Testing Strategy](#10-testing-strategy)
11. [Crate Dependency Map](#11-crate-dependency-map)
12. [Open Decisions](#12-open-decisions)
13. [References](#13-references)

---

## 1. Design Decisions and Rationale

This section records the key architectural decisions, the alternatives considered, and the reasoning behind each choice.

### 1.1 Agent Inside the VM (not outside)

The most consequential decision is that **the AI agent process runs inside the MicroVM**, not on the host. In the alternative model, the agent runs on the host and reaches into the VM via an MCP server or API to execute commands and read files. We rejected that model for three reasons.

First, it adds an enormous amount of latency and complexity. Every `ls`, every file read, every shell command must be serialized, sent over a socket, executed, and the result sent back. This re-implements the agent's native tool interface with extra steps. Second, it breaks interactive agents. Claude Code, for example, expects a real terminal with tab completion, `SIGINT` handling, and PTY support. Proxying that through MCP is fragile. Third, it creates a larger attack surface on the host: the MCP server itself becomes a privileged process that can execute arbitrary commands.

By running the agent inside the VM, the agent gets a real Linux environment with native shell access. It does not know it is sandboxed. The sandbox boundary is enforced by KVM hardware virtualization, which is invisible to the agent. The only things that cross the VM boundary are credential proxy requests (outbound) and filesystem I/O (via virtiofs).

### 1.2 Cloud Hypervisor (not Firecracker)

Both Cloud Hypervisor and Firecracker are Rust-based VMMs that run on KVM. Both are built on the `rust-vmm` crate ecosystem. The critical difference is that **Cloud Hypervisor supports `virtiofs`** (shared host directories), while Firecracker explicitly does not [1]. Firecracker's maintainers have stated that the attack surface of filesystem virtualization is not justified for their use cases [2].

For `agentbox`, live filesystem sharing is essential. The git worktree on the host must be directly visible inside the VM at `/workspace`, and changes made by the agent must be immediately visible on the host for divergence tracking. Without `virtiofs`, the alternatives are copying files in and out via VSock (slow, complex), using a block device image (requires rebuild on every change), or running NFS/SSHFS (adds network complexity). All of these defeat the purpose of the "agent inside" model.

Cloud Hypervisor's trade-off is a slightly larger device model (it supports virtio-fs, virtio-balloon, vfio, etc.), which increases the attack surface compared to Firecracker's minimal 4-device model. However, Cloud Hypervisor still provides full KVM hardware isolation, and the `virtiofsd` daemon runs in its own sandbox (namespace or chroot) [3].

| Feature | Cloud Hypervisor | Firecracker |
|---|---|---|
| virtiofs (shared directories) | **Yes** | No |
| VSock | Yes | Yes |
| Snapshot/Restore | Yes | Yes |
| Rust-based | Yes | Yes |
| KVM required | Yes | Yes |
| Device model size | Medium (~10 devices) | Minimal (4 devices) |
| Jailer / Sandboxing | Landlock | Jailer (chroot + seccomp) |

### 1.3 virtiofs for Worktree Mounting

`virtiofs` is a VIRTIO-defined shared filesystem protocol. It uses a host-side daemon (`virtiofsd`) that serves a directory over a vhost-user socket. The VMM (Cloud Hypervisor) connects the guest's virtio-fs driver to this socket. Inside the guest, the shared directory is mounted with `mount -t virtiofs <tag> /workspace` [3].

The key properties that make this ideal for `agentbox`:

- **Bidirectional:** Changes on the host are immediately visible in the guest, and vice versa. There is no sync delay.
- **POSIX-compliant:** The agent sees a normal filesystem with correct permissions, symlinks, and inotify support.
- **No network required:** The data path is a Unix socket between `virtiofsd` and Cloud Hypervisor, not TCP/IP.
- **Cache control:** We use `--cache=never` to minimize host memory footprint, which is important when running many VMs [3].

### 1.4 Dual-Layer Credential Proxy

Credentials must never exist inside the VM. We achieve this with two complementary proxy layers:

**Layer 1 (CLI Proxy / Airlock Pattern):** For CLI tools that rely on file-based credentials (`git` needing `~/.ssh/id_rsa`, `aws` needing `~/.aws/credentials`), we replace the binary inside the VM with a shim. The shim forwards the command and arguments over VSock to a host-side daemon, which validates the request against a TOML policy and executes the real binary with the host's credentials. This is inspired by the Airlock project [4].

**Layer 2 (HTTP Egress Proxy):** For API keys injected into HTTP headers (`Authorization: Bearer sk-...`, `x-api-key: ...`), we run a forward HTTP proxy on the host and set `HTTPS_PROXY` inside the VM. When the agent's code makes an HTTP request to a known API endpoint (e.g., `api.anthropic.com`), the proxy intercepts the request and injects the real API key. The agent never sees the key. This is inspired by OneCLI [5] and Docker Sandboxes [6].

### 1.5 Running as a Specific User (IAM Identity Mapping)

Each sandbox can be configured to run its processes as a specific Unix user inside the VM. On the host side, the credential proxy daemon maps each sandbox ID to a set of credentials (e.g., an AWS IAM role ARN). When the shim forwards an `aws` command, the proxy daemon assumes the mapped IAM role via STS before executing the command. This allows fine-grained, per-agent IAM policies without the agent ever seeing the credentials.

---

## 2. High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        HOST MACHINE                             │
│                                                                 │
│  ┌──────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
│  │ abox CLI  │  │  abox TUI    │  │   Git Repository         │  │
│  │ (clap)   │  │  (ratatui)   │  │   ├── .git/              │  │
│  └────┬─────┘  └──────┬───────┘  │   ├── worktrees/         │  │
│       │               │          │   │   ├── task-1/ ──────────────┐
│       └───────┬───────┘          │   │   ├── task-2/ ────────────┐ │
│               │                  │   │   └── task-3/ ──────────┐ │ │
│               ▼                  └──────────────────────────┘  │ │ │
│  ┌────────────────────────┐                                    │ │ │
│  │      abox-core          │                                    │ │ │
│  │  ┌──────────────────┐  │                                    │ │ │
│  │  │ Workspace Manager │  │                                    │ │ │
│  │  │ (git2)           │  │                                    │ │ │
│  │  └──────────────────┘  │                                    │ │ │
│  │  ┌──────────────────┐  │                                    │ │ │
│  │  │ VM Manager        │  │                                    │ │ │
│  │  │ (cloud-hypervisor)│  │                                    │ │ │
│  │  └──────────────────┘  │                                    │ │ │
│  └────────────────────────┘                                    │ │ │
│                                                                │ │ │
│  ┌────────────────────────┐                                    │ │ │
│  │    abox-proxyd          │                                    │ │ │
│  │  ┌──────────────────┐  │                                    │ │ │
│  │  │ CLI Proxy (VSock) │  │  ◄── Policy Engine (TOML)         │ │ │
│  │  └──────────────────┘  │                                    │ │ │
│  │  ┌──────────────────┐  │                                    │ │ │
│  │  │ HTTP Egress Proxy │  │  ◄── Credential Store             │ │ │
│  │  └──────────────────┘  │                                    │ │ │
│  └────────────────────────┘                                    │ │ │
│                                                                │ │ │
│  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐ │ │ │
│  │ virtiofsd        │  │ virtiofsd        │  │ virtiofsd      │ │ │ │
│  │ (task-3 wt)     │  │ (task-2 wt)     │  │ (task-1 wt)   │ │ │ │
│  └────────┬────────┘  └────────┬────────┘  └───────┬────────┘ │ │ │
│           │                    │                    │          │ │ │
│ ══════════╪════════════════════╪════════════════════╪══════════╪═╪═╪═
│    KVM    │                    │                    │          │ │ │
│  ┌────────▼────────┐  ┌───────▼─────────┐  ┌──────▼───────┐  │ │ │
│  │  MicroVM task-3  │  │  MicroVM task-2  │  │ MicroVM task-1│  │ │ │
│  │                 │  │                 │  │              │  │ │ │
│  │ /workspace ◄────┼──┼─────────────────┼──┼──────────────┼──┘ │ │
│  │   (virtiofs)    │  │ /workspace ◄────┼──┼──────────────┼────┘ │
│  │                 │  │   (virtiofs)    │  │ /workspace ◄─┼──────┘
│  │ ┌─────────────┐ │  │                 │  │  (virtiofs)  │
│  │ │ Claude Code  │ │  │ ┌─────────────┐ │  │              │
│  │ │ (unmodified) │ │  │ │ Custom Agent │ │  │ ┌──────────┐ │
│  │ └─────────────┘ │  │ └─────────────┘ │  │ │ Cursor   │ │
│  │ ┌─────────────┐ │  │ ┌─────────────┐ │  │ └──────────┘ │
│  │ │ abox-shim   │ │  │ │ abox-shim   │ │  │ ┌──────────┐ │
│  │ │ (git, aws)  │ │  │ │ (git, aws)  │ │  │ │ abox-shim│ │
│  │ └──────┬──────┘ │  │ └──────┬──────┘ │  │ └────┬─────┘ │
│  └────────┼────────┘  └────────┼────────┘  └──────┼───────┘
│           │  VSock             │  VSock            │  VSock
│           └────────────────────┴──────────────────┘
│                        │
│                        ▼
│              ┌──────────────────┐
│              │   abox-proxyd    │
│              │  (host daemon)   │
│              └──────────────────┘
└─────────────────────────────────────────────────────────────────┘
```

---

## 3. Project Structure

```text
agentbox/
├── Cargo.toml                    # Workspace root
├── Cargo.lock
├── .github/
│   └── workflows/
│       ├── ci.yml                # Lint, test (unit only, no KVM)
│       └── integration.yml       # E2E tests on KVM-enabled runner
├── .pre-commit-config.yaml       # Pre-commit hooks (rustfmt, clippy)
├── crates/
│   ├── abox-cli/                 # Binary: CLI + TUI
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs           # Entry point, clap dispatch
│   │       ├── commands/
│   │       │   ├── run.rs        # `abox run` — create sandbox + start agent
│   │       │   ├── list.rs       # `abox list` — show active sandboxes
│   │       │   ├── attach.rs     # `abox attach` — drop into sandbox terminal
│   │       │   ├── stop.rs       # `abox stop` — graceful shutdown
│   │       │   ├── template.rs   # `abox template build|list`
│   │       │   └── merge.rs      # `abox merge` — merge agent's branch
│   │       └── tui/
│   │           ├── app.rs        # Ratatui app state
│   │           ├── dashboard.rs  # Main dashboard view
│   │           ├── divergence.rs # Divergence matrix widget
│   │           └── logs.rs       # Per-sandbox log viewer
│   │
│   ├── abox-core/                # Library: domain logic
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs         # TOML config parsing
│   │       ├── workspace.rs      # Git worktree management (port)
│   │       ├── vm.rs             # VM lifecycle management (port)
│   │       ├── snapshot.rs       # Snapshot create/restore logic
│   │       ├── sandbox.rs        # Sandbox aggregate (workspace + VM + proxy)
│   │       ├── state.rs          # In-memory state store (active sandboxes)
│   │       └── adapters/
│   │           ├── cloud_hypervisor.rs  # Cloud Hypervisor adapter
│   │           ├── git2_workspace.rs    # git2-based workspace adapter
│   │           └── process.rs           # Process spawning utilities
│   │
│   ├── abox-proxyd/              # Binary: host-side credential proxy daemon
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs           # Daemon entry point
│   │       ├── cli_proxy.rs      # VSock listener for CLI shim requests
│   │       ├── egress_proxy.rs   # HTTP forward proxy with credential injection
│   │       ├── policy.rs         # TOML policy engine
│   │       ├── credential_store.rs # Reads host credentials (env vars, files)
│   │       └── audit.rs          # Structured audit logging
│   │
│   └── abox-shim/                # Binary: guest-side credential shim
│       ├── Cargo.toml            # Target: x86_64-unknown-linux-musl (static)
│       └── src/
│           └── main.rs           # Intercepts CLI calls, forwards over VSock
│
├── policies/                     # Default TOML policy templates
│   ├── git.toml
│   ├── aws.toml
│   ├── egress.toml
│   └── README.md
│
├── templates/                    # Guest image build scripts
│   ├── base/
│   │   ├── build.sh              # Creates the base rootfs (Alpine + tools)
│   │   └── init.sh               # Guest init script (mounts virtiofs, starts sshd)
│   └── agents/
│       ├── claude-code.sh        # Installs Claude Code into base image
│       └── custom-python.sh      # Installs Python agent framework
│
└── tests/
    ├── unit/                     # Unit tests (no KVM)
    └── e2e/                      # Integration tests (requires KVM)
```

### 3.1 Workspace `Cargo.toml`

```toml
[workspace]
members = [
    "crates/abox-cli",
    "crates/abox-core",
    "crates/abox-proxyd",
    "crates/abox-shim",
]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
repository = "https://github.com/<org>/agentbox"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
```

---

## 4. Phase 1: Workspace Manager

**Goal:** Manage git worktrees so that each sandbox gets its own isolated branch and working directory, all backed by the same repository.

**Purpose:** Git worktrees allow multiple checked-out copies of a repository to coexist on disk, each on a different branch, sharing the same `.git` object store. This is far more efficient than full clones (no duplicate objects) and enables the divergence matrix (comparing branches).

### 4.1 Port Definition (Domain Interface)

Following Hexagonal Architecture, we define the port (trait) first, then implement the adapter.

```rust
// abox-core/src/workspace.rs

use std::path::PathBuf;
use anyhow::Result;

/// Represents the status of a file in a worktree relative to the base branch.
#[derive(Debug, Clone, PartialEq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed(String), // old path
}

/// A single entry in the divergence report.
#[derive(Debug, Clone)]
pub struct DivergenceEntry {
    pub file_path: String,
    pub sandbox_id: String,
    pub status: FileStatus,
}

/// The port (trait) for workspace management. Decoupled from git2.
pub trait WorkspacePort: Send + Sync {
    /// Create a new worktree on a new branch forked from `base_branch`.
    /// Returns the absolute path to the worktree directory.
    fn create_worktree(&self, sandbox_id: &str, base_branch: &str) -> Result<PathBuf>;

    /// Remove a worktree and optionally delete its branch.
    fn remove_worktree(&self, sandbox_id: &str, delete_branch: bool) -> Result<()>;

    /// List all active worktrees managed by agentbox.
    fn list_worktrees(&self) -> Result<Vec<String>>;

    /// Compute the divergence matrix: which files are changed in which worktrees.
    /// This is the data source for the TUI's divergence view.
    fn compute_divergence(&self, base_branch: &str) -> Result<Vec<DivergenceEntry>>;

    /// Merge a sandbox's branch back into the base branch.
    /// Returns a list of conflicting files, if any.
    fn merge_branch(&self, sandbox_id: &str, base_branch: &str) -> Result<Vec<String>>;
}
```

### 4.2 Adapter Implementation (git2)

```rust
// abox-core/src/adapters/git2_workspace.rs

use crate::workspace::{WorkspacePort, DivergenceEntry, FileStatus};
use git2::{Repository, Diff, DiffOptions, StatusOptions};
use std::path::{Path, PathBuf};
use anyhow::{Result, Context};

pub struct Git2Workspace {
    repo_path: PathBuf,
    worktree_base: PathBuf, // e.g., ~/.agentbox/worktrees/
}

impl Git2Workspace {
    pub fn new(repo_path: impl AsRef<Path>) -> Result<Self> {
        let worktree_base = dirs::home_dir()
            .context("No home directory")?
            .join(".agentbox")
            .join("worktrees");
        std::fs::create_dir_all(&worktree_base)?;

        Ok(Self {
            repo_path: repo_path.as_ref().to_path_buf(),
            worktree_base,
        })
    }
}

impl WorkspacePort for Git2Workspace {
    fn create_worktree(&self, sandbox_id: &str, base_branch: &str) -> Result<PathBuf> {
        let repo = Repository::open(&self.repo_path)?;
        let branch_name = format!("agent/{}", sandbox_id);

        // Resolve the base branch to a commit
        let base_ref = repo.revparse_single(base_branch)?;
        let commit = base_ref.peel_to_commit()?;

        // Create the new branch
        repo.branch(&branch_name, &commit, false)
            .context("Failed to create branch")?;

        // Create the worktree directory
        let wt_path = self.worktree_base.join(sandbox_id);
        std::fs::create_dir_all(&wt_path)?;

        // Add the worktree, checking out the new branch
        let mut opts = git2::WorktreeAddOptions::new();
        let reference = repo.find_branch(&branch_name, git2::BranchType::Local)?;
        opts.reference(Some(&reference.into_reference()));
        repo.worktree(sandbox_id, &wt_path, Some(&mut opts))?;

        tracing::info!(sandbox_id, branch = %branch_name, path = %wt_path.display(),
            "Created worktree");

        Ok(wt_path)
    }

    fn compute_divergence(&self, base_branch: &str) -> Result<Vec<DivergenceEntry>> {
        let repo = Repository::open(&self.repo_path)?;
        let base_commit = repo.revparse_single(base_branch)?.peel_to_commit()?;
        let base_tree = base_commit.tree()?;

        let mut entries = Vec::new();

        // Iterate over all worktrees
        for wt_name in repo.worktrees()?.iter() {
            let wt_name = wt_name.unwrap_or_default();
            if !wt_name.starts_with("agent/") && !self.worktree_base.join(wt_name).exists() {
                continue; // Skip non-agentbox worktrees
            }

            // Get the worktree's HEAD commit
            let wt = repo.find_worktree(wt_name)?;
            let wt_repo = Repository::open_from_worktree(&wt)?;
            let head = wt_repo.head()?.peel_to_commit()?;
            let head_tree = head.tree()?;

            // Diff the base tree against the worktree's HEAD tree
            let diff = repo.diff_tree_to_tree(
                Some(&base_tree),
                Some(&head_tree),
                None,
            )?;

            for delta in diff.deltas() {
                let status = match delta.status() {
                    git2::Delta::Added => FileStatus::Added,
                    git2::Delta::Modified => FileStatus::Modified,
                    git2::Delta::Deleted => FileStatus::Deleted,
                    git2::Delta::Renamed => {
                        let old = delta.old_file().path()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();
                        FileStatus::Renamed(old)
                    }
                    _ => continue,
                };

                let file_path = delta.new_file().path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                entries.push(DivergenceEntry {
                    file_path,
                    sandbox_id: wt_name.to_string(),
                    status,
                });
            }
        }

        Ok(entries)
    }

    fn remove_worktree(&self, sandbox_id: &str, delete_branch: bool) -> Result<()> {
        let repo = Repository::open(&self.repo_path)?;

        // Prune the worktree
        let wt = repo.find_worktree(sandbox_id)?;
        wt.prune(Some(
            git2::WorktreePruneOptions::new()
                .working_tree(true)
                .valid(true)
                .locked(false),
        ))?;

        // Remove the directory
        let wt_path = self.worktree_base.join(sandbox_id);
        if wt_path.exists() {
            std::fs::remove_dir_all(&wt_path)?;
        }

        // Optionally delete the branch
        if delete_branch {
            let branch_name = format!("agent/{}", sandbox_id);
            let mut branch = repo.find_branch(&branch_name, git2::BranchType::Local)?;
            branch.delete()?;
        }

        Ok(())
    }

    fn list_worktrees(&self) -> Result<Vec<String>> {
        let repo = Repository::open(&self.repo_path)?;
        let names: Vec<String> = repo.worktrees()?
            .iter()
            .filter_map(|n| n.map(|s| s.to_string()))
            .collect();
        Ok(names)
    }

    fn merge_branch(&self, sandbox_id: &str, base_branch: &str) -> Result<Vec<String>> {
        // For the initial implementation, we shell out to `git merge` because
        // git2's merge API is notoriously difficult to use correctly.
        let branch_name = format!("agent/{}", sandbox_id);
        let output = std::process::Command::new("git")
            .args(["merge", "--no-ff", &branch_name])
            .current_dir(&self.repo_path)
            .output()?;

        if output.status.success() {
            Ok(vec![]) // No conflicts
        } else {
            // Parse conflict list from stderr
            let stderr = String::from_utf8_lossy(&output.stderr);
            let conflicts: Vec<String> = stderr.lines()
                .filter(|l| l.contains("CONFLICT"))
                .map(|l| l.to_string())
                .collect();
            Ok(conflicts)
        }
    }
}
```

---

## 5. Phase 2: VM Lifecycle Manager

**Goal:** Start a Cloud Hypervisor MicroVM with the worktree mounted via `virtiofs`, manage its lifecycle, and provide terminal access.

### 5.1 Port Definition

```rust
// abox-core/src/vm.rs

use std::path::PathBuf;
use anyhow::Result;

/// Configuration for a new sandbox VM.
#[derive(Debug, Clone)]
pub struct VmConfig {
    pub id: String,
    pub worktree_path: PathBuf,
    pub image_path: PathBuf,
    pub memory_mib: u32,
    pub vcpus: u8,
    pub user: Option<String>,         // Unix user inside the VM
    pub env_vars: Vec<(String, String)>, // Environment variables to set
    pub proxy_port: u16,              // Port of the egress proxy on the host
}

/// Information about a running VM.
#[derive(Debug, Clone)]
pub struct VmInfo {
    pub id: String,
    pub pid: u32,
    pub state: VmState,
    pub api_socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VmState {
    Starting,
    Running,
    Paused,
    Stopped,
}

/// The port (trait) for VM lifecycle management.
pub trait VmPort: Send + Sync {
    /// Start a new VM with the given configuration.
    async fn start(&self, config: VmConfig) -> Result<VmInfo>;

    /// Stop a running VM gracefully.
    async fn stop(&self, id: &str) -> Result<()>;

    /// Pause a running VM (for snapshotting).
    async fn pause(&self, id: &str) -> Result<()>;

    /// Resume a paused VM.
    async fn resume(&self, id: &str) -> Result<()>;

    /// Get the current state of a VM.
    async fn info(&self, id: &str) -> Result<VmInfo>;

    /// List all running VMs.
    async fn list(&self) -> Result<Vec<VmInfo>>;
}
```

### 5.2 Cloud Hypervisor Adapter

```rust
// abox-core/src/adapters/cloud_hypervisor.rs

use crate::vm::{VmConfig, VmInfo, VmPort, VmState};
use tokio::process::{Command, Child};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use anyhow::{Result, Context, bail};

/// Manages Cloud Hypervisor and virtiofsd processes.
pub struct CloudHypervisorAdapter {
    /// Base directory for runtime files (sockets, PIDs).
    runtime_dir: PathBuf,
    /// Track running VMs: id -> (ch_process, virtiofsd_process)
    vms: Arc<Mutex<HashMap<String, RunningVm>>>,
}

struct RunningVm {
    ch_child: Child,
    virtiofsd_child: Child,
    api_socket: PathBuf,
    config: VmConfig,
}

impl CloudHypervisorAdapter {
    pub fn new() -> Result<Self> {
        let runtime_dir = PathBuf::from("/run/agentbox");
        std::fs::create_dir_all(&runtime_dir)?;
        Ok(Self {
            runtime_dir,
            vms: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Wait for a Unix socket to appear on disk (with timeout).
    async fn wait_for_socket(path: &std::path::Path, timeout_ms: u64) -> Result<()> {
        let start = std::time::Instant::now();
        loop {
            if path.exists() {
                return Ok(());
            }
            if start.elapsed().as_millis() > timeout_ms as u128 {
                bail!("Timed out waiting for socket: {}", path.display());
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

impl VmPort for CloudHypervisorAdapter {
    async fn start(&self, config: VmConfig) -> Result<VmInfo> {
        let virtiofs_socket = self.runtime_dir.join(format!("virtiofs-{}.sock", config.id));
        let api_socket = self.runtime_dir.join(format!("ch-api-{}.sock", config.id));
        let console_socket = self.runtime_dir.join(format!("console-{}.sock", config.id));

        // ── Step 1: Start virtiofsd ──
        // virtiofsd serves the git worktree to the VM via the vhost-user protocol.
        // --sandbox=namespace puts virtiofsd in its own mount/pid namespace.
        // --cache=never avoids consuming host page cache (important at scale).
        let virtiofsd_child = Command::new("virtiofsd")
            .arg(format!("--socket-path={}", virtiofs_socket.display()))
            .arg(format!("--shared-dir={}", config.worktree_path.display()))
            .arg("--cache=never")
            .arg("--sandbox=namespace")
            .arg("--thread-pool-size=4")
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start virtiofsd")?;

        Self::wait_for_socket(&virtiofs_socket, 5000).await
            .context("virtiofsd socket did not appear")?;

        // ── Step 2: Start Cloud Hypervisor ──
        // --memory shared=on is REQUIRED for virtiofs (enables shared memory mapping).
        // --fs connects the virtiofsd socket as a virtio-fs device with tag "workspace".
        // --console tty connects the VM's serial console to a PTY for `abox attach`.
        // --vsock allows the guest shim to communicate with the host proxy daemon.
        let ch_child = Command::new("cloud-hypervisor")
            .arg("--api-socket").arg(api_socket.display().to_string())
            .arg("--cpus").arg(format!("boot={}", config.vcpus))
            .arg("--memory").arg(format!("size={}M,shared=on", config.memory_mib))
            .arg("--disk").arg(format!("path={}", config.image_path.display()))
            .arg("--kernel").arg("/usr/share/agentbox/vmlinux")
            .arg("--cmdline").arg("console=hvc0 root=/dev/vda1 rw")
            .arg("--fs").arg(format!(
                "tag=workspace,socket={},num_queues=1,queue_size=1024",
                virtiofs_socket.display()
            ))
            .arg("--vsock").arg(format!(
                "cid=3,socket={}/vsock-{}.sock",
                self.runtime_dir.display(), config.id
            ))
            .arg("--console").arg(format!("socket={}", console_socket.display()))
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start cloud-hypervisor")?;

        Self::wait_for_socket(&api_socket, 10000).await
            .context("Cloud Hypervisor API socket did not appear")?;

        let pid = ch_child.id().unwrap_or(0);

        let running = RunningVm {
            ch_child,
            virtiofsd_child,
            api_socket: api_socket.clone(),
            config: config.clone(),
        };

        self.vms.lock().await.insert(config.id.clone(), running);

        Ok(VmInfo {
            id: config.id,
            pid,
            state: VmState::Running,
            api_socket,
        })
    }

    async fn stop(&self, id: &str) -> Result<()> {
        let mut vms = self.vms.lock().await;
        if let Some(mut vm) = vms.remove(id) {
            // Send shutdown via Cloud Hypervisor API
            let client = reqwest::Client::new();
            let _ = client.put(format!(
                "http://localhost/api/v1/vm.shutdown"
            ))
            // In practice, use a Unix socket HTTP client here
            .send()
            .await;

            // Ensure processes are killed
            let _ = vm.ch_child.kill().await;
            let _ = vm.virtiofsd_child.kill().await;

            // Clean up sockets
            let runtime = &self.runtime_dir;
            let _ = std::fs::remove_file(runtime.join(format!("virtiofs-{}.sock", id)));
            let _ = std::fs::remove_file(runtime.join(format!("ch-api-{}.sock", id)));
            let _ = std::fs::remove_file(runtime.join(format!("vsock-{}.sock", id)));
            let _ = std::fs::remove_file(runtime.join(format!("console-{}.sock", id)));
        }
        Ok(())
    }

    async fn pause(&self, id: &str) -> Result<()> {
        // PUT to /api/v1/vm.pause via the API socket
        todo!("Implement via Cloud Hypervisor REST API")
    }

    async fn resume(&self, id: &str) -> Result<()> {
        // PUT to /api/v1/vm.resume via the API socket
        todo!("Implement via Cloud Hypervisor REST API")
    }

    async fn info(&self, id: &str) -> Result<VmInfo> {
        let vms = self.vms.lock().await;
        let vm = vms.get(id).context("VM not found")?;
        Ok(VmInfo {
            id: id.to_string(),
            pid: vm.ch_child.id().unwrap_or(0),
            state: VmState::Running,
            api_socket: vm.api_socket.clone(),
        })
    }

    async fn list(&self) -> Result<Vec<VmInfo>> {
        let vms = self.vms.lock().await;
        Ok(vms.iter().map(|(id, vm)| VmInfo {
            id: id.clone(),
            pid: vm.ch_child.id().unwrap_or(0),
            state: VmState::Running,
            api_socket: vm.api_socket.clone(),
        }).collect())
    }
}
```

### 5.3 Guest Init Script

The guest's init system must mount the `virtiofs` tag and configure the environment. This script is baked into the rootfs image.

```bash
#!/bin/bash
# templates/base/init.sh — Runs as part of the guest's boot sequence

set -euo pipefail

# Mount the host's git worktree at /workspace
mkdir -p /workspace
mount -t virtiofs workspace /workspace

# Configure the egress proxy (the host's IP is the default gateway)
HOST_IP=$(ip route | grep default | awk '{print $3}')
export HTTPS_PROXY="http://${HOST_IP}:18443"
export HTTP_PROXY="http://${HOST_IP}:18080"
export NO_PROXY="localhost,127.0.0.1"

# Write proxy config so all user sessions inherit it
cat > /etc/profile.d/agentbox-proxy.sh << EOF
export HTTPS_PROXY="http://${HOST_IP}:18443"
export HTTP_PROXY="http://${HOST_IP}:18080"
export NO_PROXY="localhost,127.0.0.1"
EOF

# Start SSH daemon for `abox attach`
/usr/sbin/sshd -D &

# Signal readiness to the host via VSock
echo '{"status":"ready"}' | socat - VSOCK-CONNECT:2:1234

# Keep the init process alive
wait
```

---

## 6. Phase 3: Dual-Layer Credential Proxy

### 6.1 Layer 1: CLI Proxy (Airlock Pattern)

**Feature:** When the agent (or the agent's code) runs `git push` inside the VM, the command is intercepted by a shim binary that forwards the request to the host, where it is executed with the host's SSH keys.

**Purpose:** File-based credentials (SSH keys, AWS credentials files, GPG keys) never enter the VM. The shim is a thin RPC client; the host daemon is the RPC server with access to the real credentials.

**Rationale:** This is the only safe way to handle credentials that are stored as files on disk. Environment variables can be read by any process in the VM (via `/proc/*/environ`), so they are not suitable for secrets. The shim pattern ensures the secret material exists only in the host daemon's memory.

#### 6.1.1 Guest Shim (`abox-shim`)

The shim is compiled as a static binary (`x86_64-unknown-linux-musl`) and installed in the guest image at paths like `/usr/local/bin/git`, `/usr/local/bin/aws`, `/usr/local/bin/ssh`. It determines which command it is proxying by inspecting `argv[0]`.

```rust
// abox-shim/src/main.rs

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

/// The shim communicates with the host daemon using a simple JSON-lines protocol
/// over a Unix socket (which is backed by VSock on the host side).

#[derive(serde::Serialize)]
struct Request {
    /// The command name (e.g., "git", "aws", "ssh")
    command: String,
    /// The arguments passed to the command
    args: Vec<String>,
    /// The current working directory (relative to /workspace)
    cwd: String,
    /// Environment variables that may be relevant (e.g., GIT_AUTHOR_NAME)
    env: Vec<(String, String)>,
}

#[derive(serde::Deserialize)]
struct Response {
    /// Exit code of the command on the host
    exit_code: i32,
    /// Stdout from the command
    stdout: String,
    /// Stderr from the command
    stderr: String,
}

fn main() -> ExitCode {
    // Determine which command we are proxying from argv[0]
    let args: Vec<String> = env::args().collect();
    let invoked_as = std::path::Path::new(&args[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/workspace".to_string());

    // Collect relevant environment variables
    let relevant_env: Vec<(String, String)> = env::vars()
        .filter(|(k, _)| {
            k.starts_with("GIT_") || k.starts_with("AWS_") || k == "HOME" || k == "USER"
        })
        .collect();

    let request = Request {
        command: invoked_as,
        args: args[1..].to_vec(),
        cwd,
        env: relevant_env,
    };

    // Connect to the host proxy daemon via the VSock-backed Unix socket
    let mut stream = match UnixStream::connect("/run/agentbox/proxy.sock") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agentbox-shim: failed to connect to proxy: {}", e);
            return ExitCode::from(127);
        }
    };

    // Send the request as a single JSON line
    let payload = serde_json::to_string(&request).unwrap();
    if let Err(e) = writeln!(stream, "{}", payload) {
        eprintln!("agentbox-shim: failed to send request: {}", e);
        return ExitCode::from(127);
    }

    // Read the response
    let reader = BufReader::new(&stream);
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if let Ok(resp) = serde_json::from_str::<Response>(&line) {
                    // Write stdout and stderr to the appropriate file descriptors
                    if !resp.stdout.is_empty() {
                        print!("{}", resp.stdout);
                    }
                    if !resp.stderr.is_empty() {
                        eprint!("{}", resp.stderr);
                    }
                    return ExitCode::from(resp.exit_code as u8);
                }
            }
            Err(_) => break,
        }
    }

    ExitCode::from(1)
}
```

#### 6.1.2 Host Daemon CLI Proxy Handler

```rust
// abox-proxyd/src/cli_proxy.rs

use crate::policy::PolicyEngine;
use crate::audit::AuditLog;
use tokio::net::UnixListener;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use std::process::Stdio;
use anyhow::Result;

#[derive(serde::Deserialize)]
struct Request {
    command: String,
    args: Vec<String>,
    cwd: String,
    env: Vec<(String, String)>,
}

#[derive(serde::Serialize)]
struct Response {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

pub async fn serve_cli_proxy(
    socket_path: &str,
    policy: PolicyEngine,
    audit: AuditLog,
) -> Result<()> {
    let listener = UnixListener::bind(socket_path)?;
    tracing::info!(socket = socket_path, "CLI proxy listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let policy = policy.clone();
        let audit = audit.clone();

        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let req: Request = match serde_json::from_str(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Invalid request: {}", e);
                        line.clear();
                        continue;
                    }
                };

                // ── Policy Check ──
                let decision = policy.evaluate(&req.command, &req.args);
                audit.log(&req.command, &req.args, &decision).await;

                let response = if decision.allowed {
                    // Execute the real command on the host
                    execute_on_host(&req).await
                } else {
                    Response {
                        exit_code: 126,
                        stdout: String::new(),
                        stderr: format!(
                            "agentbox: command denied by policy: {} {}\nReason: {}\n",
                            req.command,
                            req.args.join(" "),
                            decision.reason
                        ),
                    }
                };

                let resp_json = serde_json::to_string(&response).unwrap();
                let _ = writer.write_all(resp_json.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;

                line.clear();
            }
        });
    }
}

async fn execute_on_host(req: &Request) -> Response {
    // Map the guest's /workspace path to the host's worktree path
    // This mapping is maintained by the sandbox manager
    let host_cwd = req.cwd.replace("/workspace", "/actual/host/worktree/path");

    let result = tokio::process::Command::new(&req.command)
        .args(&req.args)
        .current_dir(&host_cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match result {
        Ok(output) => Response {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(e) => Response {
            exit_code: 127,
            stdout: String::new(),
            stderr: format!("agentbox: failed to execute {}: {}\n", req.command, e),
        },
    }
}
```

#### 6.1.3 Policy Engine and TOML Configuration

```rust
// abox-proxyd/src/policy.rs

use serde::Deserialize;
use regex::Regex;
use std::path::Path;
use anyhow::Result;

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyFile {
    pub command: String,
    pub allow: AllowRules,
    #[serde(default)]
    pub deny: DenyRules,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AllowRules {
    pub subcommands: Vec<String>,
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DenyRules {
    #[serde(default)]
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub reason: String,
}

#[derive(Clone)]
pub struct PolicyEngine {
    policies: Vec<PolicyFile>,
}

impl PolicyEngine {
    /// Load all .toml files from a directory.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let mut policies = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().map(|e| e == "toml").unwrap_or(false) {
                let content = std::fs::read_to_string(entry.path())?;
                let policy: PolicyFile = toml::from_str(&content)?;
                policies.push(policy);
            }
        }
        Ok(Self { policies })
    }

    pub fn evaluate(&self, command: &str, args: &[String]) -> PolicyDecision {
        // Find the policy for this command
        let policy = match self.policies.iter().find(|p| p.command == command) {
            Some(p) => p,
            None => return PolicyDecision {
                allowed: false,
                reason: format!("No policy defined for command '{}'", command),
            },
        };

        // Check if the subcommand is in the allowlist
        if let Some(subcommand) = args.first() {
            if !policy.allow.subcommands.contains(subcommand) {
                return PolicyDecision {
                    allowed: false,
                    reason: format!(
                        "Subcommand '{}' is not in the allowlist for '{}'",
                        subcommand, command
                    ),
                };
            }
        }

        // Check deny patterns
        let full_args = args.join(" ");
        for pattern in &policy.deny.patterns {
            if let Ok(re) = Regex::new(pattern) {
                if re.is_match(&full_args) {
                    return PolicyDecision {
                        allowed: false,
                        reason: format!("Matched deny pattern: {}", pattern),
                    };
                }
            }
        }

        PolicyDecision {
            allowed: true,
            reason: "Allowed by policy".to_string(),
        }
    }
}
```

**Example Policy: `policies/git.toml`**

```toml
command = "git"

[allow]
subcommands = [
    "status", "log", "diff", "show",    # Read-only operations
    "add", "commit", "branch",           # Local write operations
    "push", "pull", "fetch",             # Remote operations (use host's SSH key)
    "checkout", "switch", "merge",       # Branch operations
    "stash", "rebase",                   # History operations
]

[deny]
patterns = [
    ".*--force.*",                        # No force pushes
    ".*push.*--delete.*",                 # No remote branch deletion
    ".*config\\s+--global.*",             # No global config changes
    ".*remote\\s+(add|remove|set-url).*", # No remote manipulation
]
```

**Example Policy: `policies/aws.toml`**

```toml
command = "aws"

[allow]
subcommands = [
    "s3",           # S3 access
    "sts",          # Token operations
    "ecr",          # Container registry
    "logs",         # CloudWatch logs (read)
]

[deny]
patterns = [
    ".*iam.*",                    # No IAM changes
    ".*ec2\\s+(run|terminate).*", # No EC2 instance management
    ".*s3.*rm\\s+s3://.*--recursive.*", # No recursive S3 deletes
]
```

### 6.2 Layer 2: HTTP Egress Proxy

**Feature:** API keys for services like Anthropic, OpenAI, and AWS are injected into outbound HTTP requests by a forward proxy running on the host. The agent never sees the actual key values.

**Purpose:** Many AI agents make HTTP requests to LLM APIs. The API keys for these services are high-value secrets. Rather than placing them in environment variables inside the VM (where any process can read them from `/proc`), we inject them at the network boundary.

**Implementation:** We use `hyper` to build a standard HTTP forward proxy. The VM's environment has `HTTPS_PROXY` set to point to this proxy. When the agent's HTTP client issues a `CONNECT` request, the proxy inspects the destination, and for known API endpoints, it injects the appropriate credential header.

```rust
// abox-proxyd/src/egress_proxy.rs

use hyper::{Body, Request, Response, StatusCode};
use hyper::server::conn::Http;
use hyper::service::service_fn;
use tokio::net::TcpListener;
use std::collections::HashMap;
use std::sync::Arc;
use anyhow::Result;

/// Maps a hostname to the header name and value to inject.
#[derive(Debug, Clone)]
pub struct EgressRule {
    pub host: String,
    pub header_name: String,
    pub header_value: String, // Loaded from host env or credential store
}

#[derive(Clone)]
pub struct EgressProxy {
    rules: Arc<Vec<EgressRule>>,
}

impl EgressProxy {
    /// Load egress rules from a TOML config and resolve credential values.
    pub fn from_config(config_path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(config_path)?;
        let config: EgressConfig = toml::from_str(&content)?;

        let rules: Vec<EgressRule> = config.rules.into_iter().map(|r| {
            // Resolve the credential value from the host's environment
            let value = if r.value_from_env.is_some() {
                std::env::var(r.value_from_env.as_ref().unwrap()).unwrap_or_default()
            } else {
                r.value.unwrap_or_default()
            };

            EgressRule {
                host: r.host,
                header_name: r.header_name,
                header_value: value,
            }
        }).collect();

        Ok(Self { rules: Arc::new(rules) })
    }

    /// Handle a proxy request: inspect destination, inject credentials, forward.
    async fn handle(&self, mut req: Request<Body>) -> Result<Response<Body>> {
        let uri = req.uri().clone();
        let host = uri.host().unwrap_or_default();

        // Check if any egress rule matches this host
        for rule in self.rules.iter() {
            if host == rule.host || host.ends_with(&format!(".{}", rule.host)) {
                req.headers_mut().insert(
                    hyper::header::HeaderName::from_bytes(rule.header_name.as_bytes())?,
                    hyper::header::HeaderValue::from_str(&rule.header_value)?,
                );
                tracing::debug!(host, header = %rule.header_name, "Injected credential");
            }
        }

        // Forward the request to the actual destination
        let client = hyper::Client::builder()
            .build::<_, Body>(hyper_tls::HttpsConnector::new());

        match client.request(req).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                tracing::error!("Upstream error: {}", e);
                Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::from(format!("Proxy error: {}", e)))?)
            }
        }
    }

    pub async fn serve(self, port: u16) -> Result<()> {
        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(port, "Egress proxy listening");

        loop {
            let (stream, _) = listener.accept().await?;
            let proxy = self.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req| {
                    let proxy = proxy.clone();
                    async move { proxy.handle(req).await }
                });
                if let Err(e) = Http::new().serve_connection(stream, service).await {
                    tracing::error!("Connection error: {}", e);
                }
            });
        }
    }
}

#[derive(serde::Deserialize)]
struct EgressConfig {
    rules: Vec<EgressRuleConfig>,
}

#[derive(serde::Deserialize)]
struct EgressRuleConfig {
    host: String,
    header_name: String,
    value: Option<String>,
    value_from_env: Option<String>,
}
```

**Example Configuration: `policies/egress.toml`**

```toml
[[rules]]
host = "api.anthropic.com"
header_name = "x-api-key"
value_from_env = "ANTHROPIC_API_KEY"

[[rules]]
host = "api.openai.com"
header_name = "Authorization"
# The proxy prepends "Bearer " automatically if the header is "Authorization"
value_from_env = "OPENAI_API_KEY"

[[rules]]
host = "api.github.com"
header_name = "Authorization"
value_from_env = "GITHUB_TOKEN"
```

### 6.3 Audit Logging

Every proxied request is logged with a structured audit trail.

```rust
// abox-proxyd/src/audit.rs

use chrono::Utc;
use serde::Serialize;
use tokio::sync::mpsc;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub sandbox_id: String,
    pub layer: String,        // "cli" or "egress"
    pub command: String,
    pub args: Vec<String>,
    pub allowed: bool,
    pub reason: String,
}

#[derive(Clone)]
pub struct AuditLog {
    tx: mpsc::UnboundedSender<AuditEntry>,
}

impl AuditLog {
    pub fn new(log_path: PathBuf) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<AuditEntry>();

        // Background writer
        tokio::spawn(async move {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true).append(true)
                .open(&log_path).await.unwrap();

            while let Some(entry) = rx.recv().await {
                let line = serde_json::to_string(&entry).unwrap();
                tokio::io::AsyncWriteExt::write_all(&mut file, line.as_bytes()).await.ok();
                tokio::io::AsyncWriteExt::write_all(&mut file, b"\n").await.ok();
            }
        });

        Self { tx }
    }

    pub async fn log(&self, command: &str, args: &[String], decision: &crate::policy::PolicyDecision) {
        let entry = AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            sandbox_id: "TODO".to_string(), // Populated from connection context
            layer: "cli".to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            allowed: decision.allowed,
            reason: decision.reason.clone(),
        };
        let _ = self.tx.send(entry);
    }
}
```

---

## 7. Phase 4: CLI and TUI

### 7.1 CLI (`abox-cli`)

The CLI is the primary user interface. It uses `clap` for argument parsing and dispatches to the core library.

```rust
// abox-cli/src/main.rs

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "abox", about = "Parallel AI Agent Sandboxing", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a new sandboxed agent session
    Run {
        /// A short identifier for this task (used for branch naming)
        #[arg(short, long)]
        task: String,

        /// The git repository to work on
        #[arg(short, long, default_value = ".")]
        repo: String,

        /// The base branch to fork from
        #[arg(short, long, default_value = "main")]
        base: String,

        /// Memory allocation in MiB
        #[arg(short, long, default_value = "2048")]
        memory: u32,

        /// Number of vCPUs
        #[arg(long, default_value = "2")]
        cpus: u8,

        /// The agent command to run inside the sandbox
        /// Example: abox run --task fix-auth -- claude --print "Fix the auth bug"
        #[arg(last = true)]
        agent_cmd: Vec<String>,
    },

    /// List all active sandboxes
    #[command(alias = "ls")]
    List,

    /// Attach to a running sandbox's terminal
    Attach {
        /// The task ID of the sandbox to attach to
        task: String,
    },

    /// Stop a running sandbox
    Stop {
        /// The task ID of the sandbox to stop
        task: String,
        /// Also delete the worktree and branch
        #[arg(long)]
        clean: bool,
    },

    /// Show the divergence matrix (which files are changed in which sandboxes)
    Divergence {
        /// The base branch to compare against
        #[arg(short, long, default_value = "main")]
        base: String,
    },

    /// Merge a sandbox's branch back into the base branch
    Merge {
        /// The task ID of the sandbox to merge
        task: String,
        /// The target branch to merge into
        #[arg(short, long, default_value = "main")]
        base: String,
    },

    /// Open the interactive TUI dashboard
    Tui,

    /// Manage VM templates (base images with pre-installed tools)
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
}

#[derive(Subcommand)]
enum TemplateAction {
    /// Build a new template from a Dockerfile-like script
    Build {
        /// Name of the template
        name: String,
        /// Path to the build script
        script: String,
    },
    /// List available templates
    List,
    /// Create a snapshot of a running sandbox as a new template
    Snapshot {
        /// The task ID of the running sandbox
        task: String,
        /// Name for the new template
        name: String,
    },
}
```

**Example Usage:**

```bash
# Start 3 parallel agents working on different tasks
abox run --task fix-auth -- claude --print "Fix the authentication bug in src/auth.rs"
abox run --task add-tests -- claude --print "Add unit tests for the payment module"
abox run --task refactor-db -- claude --print "Refactor the database layer to use connection pooling"

# Check what files each agent is modifying
abox divergence

# Output:
# ┌──────────────────────┬────────────┬────────────┬──────────────┐
# │ File                 │ fix-auth   │ add-tests  │ refactor-db  │
# ├──────────────────────┼────────────┼────────────┼──────────────┤
# │ src/auth.rs          │ Modified   │ .          │ .            │
# │ src/db/pool.rs       │ .          │ .          │ Modified     │
# │ src/db/connection.rs │ .          │ .          │ Deleted      │
# │ tests/payment_test.rs│ .          │ Added      │ .            │
# │ src/auth.rs          │ Modified   │ .          │ .            │ ← No conflict
# │ Cargo.toml           │ Modified   │ .          │ Modified     │ ← CONFLICT!
# └──────────────────────┴────────────┴────────────┴──────────────┘

# Attach to a sandbox to see what the agent is doing
abox attach fix-auth

# Merge completed work
abox merge fix-auth

# Stop and clean up
abox stop refactor-db --clean
```

### 7.2 TUI Dashboard (`ratatui`)

The TUI provides a real-time dashboard with three panels:

1. **Sandbox List:** Shows all active sandboxes with their status, resource usage, and the agent command running inside.
2. **Divergence Matrix:** A live-updating table showing which files are modified in which sandboxes, with conflict highlighting.
3. **Audit Log:** A scrollable log of all proxied credential requests, showing what the agents are doing with external services.

```rust
// abox-cli/src/tui/dashboard.rs

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};
use crate::tui::app::AppState;

pub fn render(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),       // Title bar
            Constraint::Percentage(30),  // Sandbox list
            Constraint::Percentage(40),  // Divergence matrix
            Constraint::Percentage(30),  // Audit log
        ])
        .split(frame.area());

    // ── Title Bar ──
    let title = Paragraph::new(format!(
        " agentbox — {} active sandboxes",
        state.sandboxes.len()
    ))
    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(title, chunks[0]);

    // ── Sandbox List ──
    let sandbox_rows: Vec<Row> = state.sandboxes.iter().map(|sb| {
        let status_style = match sb.state.as_str() {
            "running" => Style::default().fg(Color::Green),
            "paused" => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Red),
        };
        Row::new(vec![
            Cell::from(sb.id.clone()),
            Cell::from(sb.state.clone()).style(status_style),
            Cell::from(format!("{}MB", sb.memory_mib)),
            Cell::from(sb.agent_cmd.clone()),
            Cell::from(sb.uptime.clone()),
        ])
    }).collect();

    let sandbox_table = Table::new(
        sandbox_rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(10),
            Constraint::Percentage(10),
            Constraint::Percentage(45),
            Constraint::Percentage(20),
        ],
    )
    .header(Row::new(vec!["ID", "State", "Memory", "Agent", "Uptime"])
        .style(Style::default().add_modifier(Modifier::BOLD)))
    .block(Block::default().title("Sandboxes").borders(Borders::ALL));
    frame.render_widget(sandbox_table, chunks[1]);

    // ── Divergence Matrix ──
    // (Rendered similarly, with conflict rows highlighted in red)

    // ── Audit Log ──
    // (Scrollable list of recent audit entries)
}
```

---

## 8. Phase 5: Snapshot and Template System

**Feature:** Pre-boot a VM with all tools installed (Python, Node.js, Claude Code, the abox-shim), snapshot its memory and device state, and use that snapshot to fork new VMs in sub-second time.

**Purpose:** A cold boot of a Linux VM takes 1-2 seconds. For a responsive developer experience, we need sandbox creation to feel instant. Cloud Hypervisor's snapshot/restore feature allows us to resume a VM from a memory image, skipping the entire kernel boot and init sequence.

**Rationale:** This is the same technique used by AWS Lambda (via Firecracker) and ForgeVM [7]. The key insight is that the snapshot captures the *entire* VM state: memory contents, CPU registers, device queues. When restored, the VM resumes execution from exactly where it was paused. The guest does not know it was ever stopped.

### 8.1 Template Build Flow

```
abox template build claude-agent ./templates/agents/claude-code.sh
         │
         ▼
┌─────────────────────┐
│ 1. Cold-boot base   │  (~1-2 seconds)
│    Alpine VM         │
└─────────┬───────────┘
          ▼
┌─────────────────────┐
│ 2. Run build script │  (installs Node.js, Claude Code, abox-shim)
│    inside VM         │
└─────────┬───────────┘
          ▼
┌─────────────────────┐
│ 3. Pause VM          │
│    (CH API: vm.pause)│
└─────────┬───────────┘
          ▼
┌─────────────────────┐
│ 4. Snapshot to disk  │  (memory.bin + vm-state.json)
│    (CH API:          │
│     vm.snapshot)     │
└─────────┬───────────┘
          ▼
┌─────────────────────┐
│ 5. Store in          │
│    ~/.agentbox/      │
│    templates/        │
│    claude-agent/     │
└─────────────────────┘
```

### 8.2 Snapshot Restore Implementation

```rust
// abox-core/src/snapshot.rs

use std::path::{Path, PathBuf};
use anyhow::{Result, Context};
use tokio::process::Command;

pub struct SnapshotManager {
    template_dir: PathBuf, // ~/.agentbox/templates/
}

impl SnapshotManager {
    pub fn new() -> Result<Self> {
        let template_dir = dirs::home_dir()
            .context("No home dir")?
            .join(".agentbox")
            .join("templates");
        std::fs::create_dir_all(&template_dir)?;
        Ok(Self { template_dir })
    }

    /// Create a snapshot of a running (paused) VM.
    pub async fn create_snapshot(&self, api_socket: &Path, template_name: &str) -> Result<PathBuf> {
        let snap_dir = self.template_dir.join(template_name);
        std::fs::create_dir_all(&snap_dir)?;

        // Use ch-remote to trigger the snapshot via Cloud Hypervisor's API
        let status = Command::new("ch-remote")
            .arg("--api-socket").arg(api_socket.display().to_string())
            .arg("snapshot")
            .arg(format!("file://{}", snap_dir.display()))
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!("Snapshot creation failed");
        }

        tracing::info!(template = template_name, path = %snap_dir.display(), "Snapshot created");
        Ok(snap_dir)
    }

    /// Restore a VM from a snapshot. Returns the API socket path.
    pub async fn restore_from_snapshot(
        &self,
        template_name: &str,
        sandbox_id: &str,
    ) -> Result<PathBuf> {
        let snap_dir = self.template_dir.join(template_name);
        let api_socket = PathBuf::from(format!("/run/agentbox/ch-api-{}.sock", sandbox_id));

        // Cloud Hypervisor restore: starts a new VMM process that loads the snapshot
        let _child = Command::new("cloud-hypervisor")
            .arg("--api-socket").arg(api_socket.display().to_string())
            .arg("--restore").arg(format!("source_url=file://{}", snap_dir.display()))
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start cloud-hypervisor in restore mode")?;

        // Wait for the API socket to appear
        crate::adapters::cloud_hypervisor::CloudHypervisorAdapter::wait_for_socket(
            &api_socket, 5000
        ).await?;

        // Resume the VM (it was paused when the snapshot was taken)
        let status = Command::new("ch-remote")
            .arg("--api-socket").arg(api_socket.display().to_string())
            .arg("resume")
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!("VM resume failed");
        }

        Ok(api_socket)
    }

    /// List available templates.
    pub fn list_templates(&self) -> Result<Vec<String>> {
        let mut templates = Vec::new();
        for entry in std::fs::read_dir(&self.template_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    templates.push(name.to_string());
                }
            }
        }
        Ok(templates)
    }
}
```

---

## 9. Phase 6: Guest Image Builder

**Feature:** A script-based system for building the base rootfs image that all VMs boot from.

**Purpose:** The rootfs must contain the Linux userspace, the `abox-shim` binary, SSH server, and the init script that mounts `virtiofs` and configures the proxy environment.

```bash
#!/bin/bash
# templates/base/build.sh — Creates the base Alpine rootfs image

set -euo pipefail

IMAGE_SIZE="2G"
IMAGE_PATH="./base-rootfs.raw"
MOUNT_DIR="/tmp/agentbox-rootfs"

# Create a raw disk image
truncate -s $IMAGE_SIZE $IMAGE_PATH
mkfs.ext4 $IMAGE_PATH

# Mount and install Alpine
mkdir -p $MOUNT_DIR
mount -o loop $IMAGE_PATH $MOUNT_DIR

# Bootstrap Alpine Linux
apk --root $MOUNT_DIR --initdb add alpine-base openssh-server bash curl

# Install the abox-shim (pre-compiled static binary)
cp ./target/x86_64-unknown-linux-musl/release/abox-shim $MOUNT_DIR/usr/local/bin/abox-shim
chmod +x $MOUNT_DIR/usr/local/bin/abox-shim

# Create symlinks for proxied commands
ln -sf /usr/local/bin/abox-shim $MOUNT_DIR/usr/local/bin/git
ln -sf /usr/local/bin/abox-shim $MOUNT_DIR/usr/local/bin/aws
ln -sf /usr/local/bin/abox-shim $MOUNT_DIR/usr/local/bin/ssh

# Install the init script
cp ./templates/base/init.sh $MOUNT_DIR/etc/init.d/agentbox
chmod +x $MOUNT_DIR/etc/init.d/agentbox

# Configure SSH (key-based auth only, no passwords)
sed -i 's/#PermitRootLogin.*/PermitRootLogin prohibit-password/' \
    $MOUNT_DIR/etc/ssh/sshd_config
sed -i 's/#PasswordAuthentication.*/PasswordAuthentication no/' \
    $MOUNT_DIR/etc/ssh/sshd_config

# Generate host keys
ssh-keygen -A -f $MOUNT_DIR

# Clean up
umount $MOUNT_DIR
rmdir $MOUNT_DIR

echo "Base rootfs image created at $IMAGE_PATH"
```

---

## 10. Testing Strategy

### 10.1 Unit Tests (No KVM Required)

The Hexagonal Architecture ensures that all domain logic can be tested without any virtualization infrastructure.

| Component | Testable Without KVM | Test Strategy |
|---|---|---|
| `PolicyEngine` | Yes | Feed mock commands, assert allow/deny decisions |
| `WorkspacePort` (git2 adapter) | Yes | Create temp git repos, test worktree creation/divergence |
| `EgressProxy` rule matching | Yes | Unit test the host/header matching logic |
| `AuditLog` | Yes | Assert structured log entries are written correctly |
| Config parsing | Yes | Parse sample TOML files, assert correct structures |

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_allows_git_push() {
        let engine = PolicyEngine::from_dir(Path::new("../policies")).unwrap();
        let decision = engine.evaluate("git", &["push".into(), "origin".into(), "main".into()]);
        assert!(decision.allowed);
    }

    #[test]
    fn test_policy_denies_force_push() {
        let engine = PolicyEngine::from_dir(Path::new("../policies")).unwrap();
        let decision = engine.evaluate("git", &["push".into(), "--force".into()]);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("deny pattern"));
    }

    #[test]
    fn test_policy_denies_unknown_command() {
        let engine = PolicyEngine::from_dir(Path::new("../policies")).unwrap();
        let decision = engine.evaluate("rm", &["-rf".into(), "/".into()]);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("No policy defined"));
    }
}
```

### 10.2 Integration Tests (Requires KVM)

These tests run on a CI runner with `/dev/kvm` access (e.g., a bare-metal GitHub Actions runner or a self-hosted runner).

```rust
#[tokio::test]
#[ignore] // Only runs on KVM-enabled hosts
async fn test_full_sandbox_lifecycle() {
    // 1. Create a temp git repo with a file
    let repo_dir = tempdir().unwrap();
    init_test_repo(&repo_dir);

    // 2. Create a worktree
    let ws = Git2Workspace::new(&repo_dir).unwrap();
    let wt_path = ws.create_worktree("e2e-test", "main").unwrap();

    // 3. Start a VM with the worktree mounted
    let adapter = CloudHypervisorAdapter::new().unwrap();
    let vm = adapter.start(VmConfig {
        id: "e2e-test".into(),
        worktree_path: wt_path.clone(),
        image_path: PathBuf::from("./templates/base-rootfs.raw"),
        memory_mib: 512,
        vcpus: 1,
        user: None,
        env_vars: vec![],
        proxy_port: 18443,
    }).await.unwrap();

    // 4. Execute a command inside the VM that writes to /workspace
    // (via SSH or VSock exec)
    ssh_exec(&vm, "echo 'hello from vm' > /workspace/test.txt").await;

    // 5. Verify the file appears on the host via virtiofs
    let content = std::fs::read_to_string(wt_path.join("test.txt")).unwrap();
    assert_eq!(content.trim(), "hello from vm");

    // 6. Verify divergence detection
    let divergence = ws.compute_divergence("main").unwrap();
    assert!(divergence.iter().any(|e| e.file_path == "test.txt"));

    // 7. Cleanup
    adapter.stop("e2e-test").await.unwrap();
    ws.remove_worktree("e2e-test", true).unwrap();
}
```

### 10.3 GitHub Actions CI

```yaml
# .github/workflows/ci.yml
name: CI
on: [push, pull_request]

jobs:
  lint-and-unit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --check
      - run: cargo clippy -- -D warnings
      - run: cargo test --workspace --exclude abox-shim

  integration:
    runs-on: [self-hosted, kvm]  # Requires a KVM-enabled runner
    needs: lint-and-unit
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --workspace -- --ignored
```

---

## 11. Crate Dependency Map

```text
abox-cli
├── abox-core (workspace, vm, snapshot, config)
├── clap (CLI parsing)
├── ratatui (TUI)
├── crossterm (terminal backend)
└── tokio

abox-core
├── git2 (workspace adapter)
├── tokio (async process management)
├── serde + toml (config)
├── tracing (structured logging)
├── reqwest (Cloud Hypervisor API client, Unix socket)
├── dirs (home directory resolution)
└── thiserror / anyhow

abox-proxyd
├── abox-core (policy types, config)
├── tokio (async networking)
├── hyper + hyper-tls (egress proxy)
├── serde + serde_json (JSON-lines protocol)
├── regex (deny pattern matching)
├── toml (policy files)
├── chrono (audit timestamps)
└── tracing

abox-shim (MINIMAL dependencies — static musl binary)
├── serde + serde_json (request/response serialization)
└── (no tokio, no async — synchronous for minimal binary size)
```

---

## 12. Open Decisions

The following decisions are deferred to the implementation phase, where hands-on experimentation will inform the best choice.

| Decision | Options | Leaning | Rationale |
|---|---|---|---|
| VSock vs SSH for `abox attach` | VSock serial console / SSH into VM | SSH | SSH is well-understood, works with existing terminal multiplexers, and Cloud Hypervisor supports a console socket for serial access as a fallback. |
| State persistence | In-memory only / SQLite / Redis | SQLite | A single `~/.agentbox/state.db` file is simple, requires no external services, and survives host reboots. Redis is overkill for a local tool. |
| Guest init system | Custom PID 1 / OpenRC / systemd | OpenRC (Alpine) | Alpine's OpenRC is lightweight and well-understood. A custom PID 1 (like ForgeVM's agent) is faster but harder to debug. |
| Egress proxy: CONNECT vs full MITM | Forward proxy (CONNECT tunnel) / Full MITM with CA cert | CONNECT first | The CONNECT approach is simpler and doesn't require injecting a CA cert. It works for header injection. Fall back to full MITM only if we need to modify request bodies. |
| Cloud Hypervisor API client | `ch-remote` CLI / Direct REST over Unix socket | Direct REST | Shelling out to `ch-remote` is simpler initially but adds a process spawn per API call. The `cloud-hypervisor-client` Rust crate provides direct access. |
| TUI framework | `ratatui` / `gpui-component` | `ratatui` for v1 | `ratatui` is mature and terminal-native. A `gpui-component` desktop UI could be a v2 enhancement for richer visualization. |

---

## 13. References

[1]: https://github.com/firecracker-microvm/firecracker/issues/889 "Firecracker Issue #889: Share filesystem between guest and host"
[2]: https://github.com/firecracker-microvm/firecracker/issues/1180 "Firecracker Issue #1180: Host Filesystem Sharing"
[3]: https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/docs/fs.md "Cloud Hypervisor: How to use virtio-fs"
[4]: https://github.com/calebfaruki/airlock "Airlock: Secure credential proxy for AI agents"
[5]: https://github.com/onecli/onecli "OneCLI: Vault for AI Agents in Rust"
[6]: https://docs.docker.com/ai/sandboxes/architecture/ "Docker Sandboxes Architecture"
[7]: https://dev.to/adwitiya/how-i-built-sandboxes-that-boot-in-28ms-using-firecracker-snapshots-i0k "ForgeVM: 28ms sandbox boots with Firecracker snapshots"
[8]: https://github.com/coder/mux "Coder Mux: Terminal multiplexer for AI agents"
[9]: https://github.com/adammiribyan/zeroboot "Zeroboot: Instant VM cloning"
