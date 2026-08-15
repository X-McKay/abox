//! Runtime-neutral sandbox specification types.
//!
//! These types describe what the orchestrator *needs* from a sandbox runtime
//! — a workspace mount, an environment profile, resources, staged inputs,
//! control channels, and a lifecycle — without leaking any hypervisor- or
//! runtime-specific concepts (kernel paths, raw image paths, API sockets,
//! virtiofsd socket names). Adapters translate a [`SandboxRuntimeSpec`] into
//! whatever their backend requires.

use crate::project::EnvironmentProfile;
use std::path::PathBuf;

/// Guest vsock/control port used by the host command broker (`git`/`gh` shim).
pub const COMMAND_BROKER_PORT: u32 = 5000;

/// Guest vsock/control port used by the HTTPS egress proxy.
pub const HTTPS_EGRESS_PORT: u32 = 5001;

/// How the task workspace is exposed to the sandbox.
#[derive(Debug, Clone)]
pub enum WorkspaceMount {
    /// The host worktree is mounted read-write at `/workspace`.
    ReadWrite(PathBuf),
}

impl WorkspaceMount {
    /// The host path backing the workspace mount.
    pub fn host_path(&self) -> &PathBuf {
        match self {
            Self::ReadWrite(p) => p,
        }
    }
}

/// The guest environment the sandbox should provide.
#[derive(Debug, Clone)]
pub enum RuntimeEnvironment {
    /// A named abox guest profile (`base`, `node`, `python`, …). The adapter
    /// resolves the profile to its backing artifact (raw rootfs image for the
    /// legacy runtime, pinned OCI image for MicroSandbox).
    Profile(EnvironmentProfile),
}

impl RuntimeEnvironment {
    /// The environment profile this environment resolves from.
    pub fn profile(&self) -> EnvironmentProfile {
        match self {
            Self::Profile(p) => *p,
        }
    }
}

/// Resource limits for the sandbox.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeResources {
    /// Memory allocation in MiB.
    pub memory_mib: u32,
    /// Number of vCPUs.
    pub vcpus: u8,
}

/// A credential file ready to be staged into the guest.
///
/// Produced by [`crate::sandbox::stage_credential_files()`] from
/// [`crate::config::CredentialFileEntry`] entries. Carries the file content
/// (a placeholder stub — never a real secret) so the runtime adapter can
/// deliver it into the guest at boot time.
#[derive(Debug, Clone)]
pub struct CredentialToStage {
    /// Index in the credentials staging directory (maps to `credentials/<index>`).
    pub index: usize,
    /// Absolute destination path inside the guest.
    pub guest_path: String,
    /// Unix permissions (e.g., "0600").
    pub mode: String,
    /// File content to write.
    pub content: Vec<u8>,
}

/// A host file to stage read-only into the guest input directory
/// (`/abox-meta/inputs/<guest_name>`).
#[derive(Debug, Clone)]
pub struct RuntimeInput {
    /// Absolute path to the file on the host.
    pub host_path: PathBuf,
    /// File name inside the guest input directory (validated single component).
    pub guest_name: String,
}

/// An additional host directory mounted into the guest.
#[derive(Debug, Clone)]
pub struct RuntimeMount {
    /// Host directory to mount.
    pub host_path: PathBuf,
    /// Absolute mount point inside the guest.
    pub guest_path: String,
    /// Whether the guest may write through this mount.
    pub read_only: bool,
}

/// A host↔guest control channel the runtime must provide.
///
/// Each channel maps a well-known guest port to a per-sandbox host Unix
/// socket. The host side (command broker, egress proxy, service bridges)
/// binds the socket returned by
/// [`super::SandboxRuntimePort::control_socket()`]; attribution derives from
/// the per-sandbox route, never from guest-asserted identity.
#[derive(Debug, Clone)]
pub struct ControlChannel {
    /// Human-readable channel name (for diagnostics and audit context).
    pub name: String,
    /// Guest-side port for the channel.
    pub guest_port: u32,
}

/// How the sandbox's network is provisioned.
#[derive(Debug, Clone, Default)]
pub enum RuntimeNetworkPlan {
    /// The guest has no direct network path. All egress is host-mediated
    /// through abox control channels (command broker + HTTPS egress proxy),
    /// where abox policy is enforced.
    #[default]
    HostMediated,
}

/// How to start the sandbox.
#[derive(Debug, Clone, Default)]
pub enum RuntimeStart {
    /// Normal fresh start.
    #[default]
    Fresh,
    /// Restore from a prepared template directory (legacy memory-snapshot
    /// templates; only supported by runtimes that implement memory
    /// checkpoints).
    RestoreTemplate {
        /// Path to the template directory containing the snapshot files.
        template_path: PathBuf,
    },
}

/// Lifecycle intent for the sandbox.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeLifecycle {
    /// Advisory maximum runtime in seconds. The orchestrator enforces the
    /// timeout on the host; runtimes that support a native max-duration may
    /// additionally apply it as defense in depth.
    pub timeout_secs: Option<u64>,
    /// The sandbox is disposable: its state need not survive exit.
    pub ephemeral: bool,
}

/// Runtime-neutral specification for starting a sandbox.
#[derive(Debug, Clone)]
pub struct SandboxRuntimeSpec {
    /// Unique sandbox/task identifier.
    pub id: String,
    /// The task workspace mount.
    pub workspace: WorkspaceMount,
    /// The guest environment to provide.
    pub environment: RuntimeEnvironment,
    /// Resource limits.
    pub resources: RuntimeResources,
    /// Optional guest user to run the agent as.
    pub user: Option<String>,
    /// Environment variables to set for the agent.
    pub env: Vec<(String, String)>,
    /// Command (argv-style) to exec inside the guest.
    pub command: Vec<String>,
    /// Optional resolved prompt content staged for the agent
    /// (`/abox-meta/prompt.md`).
    pub resolved_prompt: Option<String>,
    /// Optional immutable prepare script content
    /// (`/abox-meta/prepare.sh`).
    pub staged_prepare_script: Option<String>,
    /// Credential placeholder files to stage into the guest.
    pub credential_files: Vec<CredentialToStage>,
    /// PEM-encoded root CA certificate the guest trust store must include
    /// (required for the host-mediated HTTPS egress path).
    pub ca_cert_pem: Option<String>,
    /// Host files staged read-only into the guest input directory.
    pub inputs: Vec<RuntimeInput>,
    /// Additional mounts (durable caches, etc.).
    pub caches: Vec<RuntimeMount>,
    /// Workspace subdirectories the guest must shadow with empty tmpfs.
    pub mount_excludes: Vec<String>,
    /// Service sidecar / host-port bridge metadata the guest needs to set up
    /// loopback listeners for.
    pub services: Vec<crate::services::GuestServiceBridge>,
    /// Control channels the runtime must route between guest ports and
    /// per-sandbox host sockets.
    pub control_channels: Vec<ControlChannel>,
    /// Network provisioning plan.
    pub network: RuntimeNetworkPlan,
    /// How to start the sandbox.
    pub start: RuntimeStart,
    /// Lifecycle intent.
    pub lifecycle: RuntimeLifecycle,
}
