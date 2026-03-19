//! VM lifecycle management port (trait).
//!
//! Defines the domain interface for MicroVM operations. The adapter
//! implementation lives in `adapters::cloud_hypervisor`.

use std::path::PathBuf;

/// Configuration for creating a new sandbox VM.
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// Unique sandbox identifier.
    pub id: String,
    /// Path to the git worktree on the host (will be mounted at /workspace).
    pub worktree_path: PathBuf,
    /// Path to the root filesystem image.
    pub image_path: PathBuf,
    /// Path to the kernel binary (vmlinux).
    pub kernel_path: PathBuf,
    /// Memory allocation in MiB.
    pub memory_mib: u32,
    /// Number of vCPUs.
    pub vcpus: u8,
    /// Optional Unix user to run the agent as inside the VM.
    pub user: Option<String>,
    /// Environment variables to set inside the VM.
    pub env_vars: Vec<(String, String)>,
    /// Port of the HTTP egress proxy on the host.
    pub proxy_port: u16,
}

/// Information about a running or stopped VM.
#[derive(Debug, Clone)]
pub struct VmInfo {
    /// Unique sandbox identifier.
    pub id: String,
    /// Process ID of the Cloud Hypervisor process.
    pub pid: u32,
    /// Current state.
    pub state: VmState,
    /// Path to the Cloud Hypervisor API socket.
    pub api_socket: PathBuf,
    /// Path to the console socket (for `abox attach`).
    pub console_socket: PathBuf,
}

/// VM lifecycle states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmState {
    Starting,
    Running,
    Paused,
    Stopped,
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// Port (trait) for VM lifecycle management.
#[allow(async_fn_in_trait)]
pub trait VmPort: Send + Sync {
    /// Start a new VM with the given configuration.
    async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo>;

    /// Stop a running VM gracefully.
    async fn stop(&self, id: &str) -> anyhow::Result<()>;

    /// Pause a running VM (for snapshotting).
    async fn pause(&self, id: &str) -> anyhow::Result<()>;

    /// Resume a paused VM.
    async fn resume(&self, id: &str) -> anyhow::Result<()>;

    /// Get information about a VM.
    async fn info(&self, id: &str) -> anyhow::Result<VmInfo>;

    /// List all managed VMs.
    async fn list(&self) -> anyhow::Result<Vec<VmInfo>>;
}
