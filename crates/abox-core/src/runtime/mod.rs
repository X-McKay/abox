//! The sandbox runtime port.
//!
//! [`SandboxRuntimePort`] is the domain boundary between the orchestrator and
//! the isolation runtime that executes sandboxes. It expresses what abox
//! *needs* — start/stop/wait lifecycle, per-sandbox control sockets for the
//! command broker and request broker, and an exit-code channel — without
//! exposing runtime implementation details. [`crate::adapters::microsandbox`]
//! implements it; the same contract doubles as the qualification suite for
//! runtime upgrades (see `docs/runtime-upgrades.md`).

pub mod images;
pub mod spec;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use spec::{
    ControlChannel, CredentialToStage, RuntimeEnvironment, RuntimeInput, RuntimeLifecycle,
    RuntimeMount, RuntimeNetworkPlan, RuntimeResources, SandboxRuntimeSpec, WorkspaceMount,
    COMMAND_BROKER_PORT, HTTPS_EGRESS_PORT,
};

use std::path::PathBuf;

/// Lifecycle states of a sandbox runtime instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    Starting,
    Running,
    Stopped,
}

impl std::fmt::Display for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
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
    /// per-sandbox host socket. The host side (command broker, request
    /// broker, service bridges) binds and serves it; sandbox attribution
    /// derives from the per-sandbox path.
    fn control_socket(&self, id: &str, guest_port: u32) -> PathBuf;
}
