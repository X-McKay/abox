//! VM lifecycle management port (trait).
//!
//! Defines the domain interface for MicroVM operations. The adapter
//! implementation lives in `adapters::cloud_hypervisor`.

use std::path::PathBuf;

/// How to start a VM — fresh boot or restore from snapshot.
#[derive(Debug, Clone, Default)]
pub enum StartMode {
    /// Normal boot from kernel + disk image.
    #[default]
    Fresh,
    /// Restore from a snapshot template directory.
    Restore {
        /// Path to the template directory containing the snapshot files.
        template_path: PathBuf,
    },
}

/// A credential file ready to be written into the boot metadata directory.
///
/// Produced by [`crate::sandbox::stage_credential_files()`] from
/// [`crate::config::CredentialFileEntry`] entries. Carries the file content
/// so the VM adapter can write it into the meta directory at boot time.
#[derive(Debug, Clone)]
pub struct CredentialToStage {
    /// Index in the credentials directory (maps to `credentials/<index>`).
    pub index: usize,
    /// Absolute destination path inside the guest VM.
    pub guest_path: String,
    /// Unix permissions (e.g., "0600").
    pub mode: String,
    /// File content to write.
    pub content: Vec<u8>,
}

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
    /// Command (argv-style) to exec inside the guest after boot.
    pub agent_command: Vec<String>,
    /// How to start the VM: fresh boot or restore from snapshot.
    #[allow(dead_code)]
    pub start_mode: StartMode,
    /// Credential files to stage in the boot metadata directory.
    pub credential_files: Vec<CredentialToStage>,
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

    /// Return the path to the status directory for a given sandbox id.
    ///
    /// Adapters that back onto a real hypervisor (e.g. Cloud Hypervisor)
    /// stage a read-write virtiofs share here and point the guest at it so
    /// guest init can write `exit-code` before poweroff. In-memory / mock
    /// adapters don't have a status dir and return `None`.
    fn status_dir(&self, _id: &str) -> Option<PathBuf> {
        None
    }
}
