//! Runtime-neutral sandbox specification types.
//!
//! These types describe what the orchestrator *needs* from a sandbox runtime
//! — a workspace mount, an environment profile, resources, staged inputs,
//! control channels, and a lifecycle — without leaking runtime
//! implementation details. The adapter translates a [`SandboxRuntimeSpec`]
//! into whatever its backend requires.

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
    /// resolves the profile to its pinned OCI image
    /// (see [`super::images::ImageManifest`]).
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
///
/// Compiled from abox's user-facing network modes by
/// [`crate::policy::compile_runtime_network_plan`]. The runtime adapter
/// translates the plan mechanically and never widens it — `safe`/`scoped`
/// semantics and the "open ≠ unrestricted" invariant are decided here, not
/// by runtime defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RuntimeNetworkPlan {
    /// The guest has no direct network path. All egress is host-mediated
    /// through abox control channels (command broker + HTTPS egress proxy),
    /// where abox policy is enforced. Used by `safe` mode.
    #[default]
    HostMediated,
    /// Native guest networking compiled from abox intent, for traffic that
    /// does not cooperate with the proxy environment. The host-mediated
    /// proxy channel remains active (and preferred — `HTTPS_PROXY` stays
    /// set) for managed/credential domains and auditing.
    Native(NativeNetworkPlan),
}

/// A compiled native network plan.
///
/// Invariants the adapter must implement (and tests assert):
/// - loopback, private ranges, link-local, cloud metadata, multicast, and
///   the host itself are always denied — `open` is public-internet only;
/// - egress is limited to TCP 443 plus DNS to the gateway;
/// - ingress is denied entirely;
/// - allowed hostnames are DNS-pinned and SNI-verified by the runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeNetworkPlan {
    /// `true` for `open` mode: allow all public destinations. `false` for
    /// `scoped`: only `allowed_hosts` are reachable directly.
    pub allow_public: bool,
    /// Exact hostnames allowed for direct egress (`scoped` mode: resolved
    /// bundles plus explicitly approved domains).
    pub allowed_hosts: Vec<String>,
}

/// A credential rule delegated to the runtime's native secret substitution
/// (see [`crate::policy::CredentialExecutionStrategy`]). The real value
/// stays on the host and is referenced by source environment variable —
/// never passed by value — so it cannot persist in runtime state. The guest
/// sees only a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSecretSpec {
    /// Guest environment variable the placeholder is exposed as.
    pub env_var: String,
    /// Host environment variable holding the real value (source reference).
    pub source_env_var: String,
    /// The only destination host the substituted value may be sent to.
    pub allowed_host: String,
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
    /// Credential rules delegated to native runtime secret substitution
    /// (only meaningful with a [`RuntimeNetworkPlan::Native`] plan).
    pub native_secrets: Vec<NativeSecretSpec>,
    /// Lifecycle intent.
    pub lifecycle: RuntimeLifecycle,
}
