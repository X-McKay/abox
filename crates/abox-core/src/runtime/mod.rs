//! Runtime-neutral sandbox runtime port.
//!
//! [`SandboxRuntimePort`] is the domain boundary between the orchestrator and
//! whatever isolation runtime executes sandboxes. It expresses what abox
//! *needs* — start/stop/wait lifecycle, per-sandbox control sockets for the
//! command broker and egress proxy, and an exit-code channel — without
//! exposing hypervisor-specific concepts.
//!
//! Adapters:
//! - [`crate::adapters::microsandbox`] — the MicroSandbox runtime (default).
//! - [`crate::adapters::cloud_hypervisor_runtime`] — the legacy Cloud
//!   Hypervisor stack (transitional; see ADR-008).

pub mod images;
pub mod spec;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use spec::{
    ControlChannel, CredentialToStage, RuntimeEnvironment, RuntimeInput, RuntimeLifecycle,
    RuntimeMount, RuntimeNetworkPlan, RuntimeResources, RuntimeStart, SandboxRuntimeSpec,
    WorkspaceMount, COMMAND_BROKER_PORT, HTTPS_EGRESS_PORT,
};

use std::collections::HashMap;
use std::path::PathBuf;

/// Lifecycle states of a sandbox runtime instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    Starting,
    Running,
    Paused,
    Stopped,
}

impl std::fmt::Display for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// A running (or recently observed) sandbox runtime instance.
#[derive(Debug, Clone)]
pub struct RuntimeInstance {
    /// Sandbox/task identifier.
    pub id: String,
    /// Current state.
    pub state: RuntimeState,
    /// Host process ID backing the instance, when known.
    pub pid: Option<u32>,
}

/// The result of waiting for a sandbox to exit.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeExit {
    /// Exit code of the guest agent command, if the runtime could observe
    /// one. `None` means the sandbox terminated without reporting a code
    /// (crash before guest init, forced kill, …).
    pub exit_code: Option<i32>,
}

/// Handles a runtime exposes for legacy memory-snapshot support.
///
/// Transitional: only the Cloud Hypervisor adapter implements memory
/// checkpoints. This goes away with the legacy runtime (ADR-008).
#[derive(Debug, Clone)]
pub struct MemorySnapshotHandles {
    /// Hypervisor API socket to drive the snapshot through.
    pub api_socket: PathBuf,
    /// Filesystem-share socket file names that must be re-pinned on restore,
    /// keyed by share name.
    pub virtiofs_sockets: HashMap<String, String>,
}

/// Port (trait) for sandbox runtime lifecycle management.
///
/// Implementations are used as generic parameters (`R: SandboxRuntimePort`),
/// never as trait objects, so native async methods are fine.
#[allow(async_fn_in_trait)]
pub trait SandboxRuntimePort: Send + Sync {
    /// Start a sandbox from a runtime-neutral spec.
    async fn start(&self, spec: SandboxRuntimeSpec) -> anyhow::Result<RuntimeInstance>;

    /// Stop a sandbox gracefully.
    async fn stop(&self, id: &str) -> anyhow::Result<()>;

    /// Force-kill a sandbox that did not stop gracefully.
    async fn kill(&self, id: &str) -> anyhow::Result<()>;

    /// Get information about a sandbox instance. Errors if the sandbox is
    /// not (or no longer) managed by this runtime.
    async fn info(&self, id: &str) -> anyhow::Result<RuntimeInstance>;

    /// List all managed sandbox instances.
    async fn list(&self) -> anyhow::Result<Vec<RuntimeInstance>>;

    /// Wait until the sandbox has terminated and return its exit result.
    ///
    /// Returns `RuntimeExit { exit_code: None }` when the sandbox terminated
    /// without reporting a guest exit code, or when `id` is unknown (already
    /// reaped). The caller owns timeout enforcement.
    async fn wait(&self, id: &str) -> anyhow::Result<RuntimeExit>;

    /// Host-side Unix socket path for a guest control-channel port.
    ///
    /// The runtime routes guest connections to `guest_port` (vsock) to this
    /// per-sandbox host socket. The host side (command broker, egress proxy,
    /// service bridges) binds and serves it; sandbox attribution derives
    /// from the per-sandbox path.
    fn control_socket(&self, id: &str, guest_port: u32) -> PathBuf;

    /// Path to a file containing the guest console/log output for streaming,
    /// if the runtime exposes one.
    fn console_output(&self, id: &str) -> Option<PathBuf> {
        let _ = id;
        None
    }

    /// Pause a running sandbox (legacy memory-snapshot support).
    async fn pause(&self, id: &str) -> anyhow::Result<()> {
        let _ = id;
        anyhow::bail!("this runtime does not support pausing sandboxes")
    }

    /// Resume a paused sandbox (legacy memory-snapshot support).
    async fn resume(&self, id: &str) -> anyhow::Result<()> {
        let _ = id;
        anyhow::bail!("this runtime does not support resuming sandboxes")
    }

    /// Handles for legacy memory-snapshot creation, if supported.
    fn memory_snapshot_handles(&self, id: &str) -> Option<MemorySnapshotHandles> {
        let _ = id;
        None
    }
}
