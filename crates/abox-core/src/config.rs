//! Configuration types for abox.
//!
//! All configuration is loaded from TOML files. The main config file lives at
//! `~/.abox/config.toml` and policy files live under the `policies/` directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level abox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AboxConfig {
    /// Base directory for abox state (worktrees, snapshots, logs).
    /// Defaults to `~/.abox`.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,

    /// Directory for runtime files (sockets, PIDs).
    /// Defaults to `<state_dir>/run` so abox is fully usable without root.
    /// Set to e.g. `/run/abox` for a system-wide install.
    #[serde(default)]
    pub runtime_dir: Option<PathBuf>,

    /// Default VM configuration applied to all sandboxes unless overridden.
    #[serde(default)]
    pub vm_defaults: VmDefaults,

    /// Proxy daemon configuration.
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// Managed authentication and advanced stub configuration.
    #[serde(default)]
    pub auth: AuthConfig,
}

/// Default VM resource allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmDefaults {
    /// Memory in MiB. Default: 2048.
    #[serde(default = "default_memory")]
    pub memory_mib: u32,

    /// Number of vCPUs. Default: 2.
    #[serde(default = "default_vcpus")]
    pub vcpus: u8,

    /// Path to the default VM image.
    #[serde(default)]
    pub image_path: Option<PathBuf>,

    /// Path to the kernel (vmlinux).
    #[serde(default)]
    pub kernel_path: Option<PathBuf>,
}

/// Authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfig {
    /// First-class managed providers.
    #[serde(default)]
    pub providers: ManagedAuthProviders,

    /// Explicitly advanced, stub-only escape hatch for unsupported tools.
    #[serde(default)]
    pub advanced: AdvancedAuthConfig,
}

impl AuthConfig {
    /// Resolve every guest stub file that should be staged at boot.
    pub fn credential_files(&self) -> Vec<CredentialFileEntry> {
        let mut files = Vec::new();

        if self.providers.claude.enabled {
            files.push(CredentialFileEntry {
                host_credential_file: self.providers.claude.host_credential_file(),
                guest: default_claude_guest_credential_file(),
                mode: default_credential_mode(),
                stub: default_claude_stub(),
            });
        }

        if self.providers.codex.enabled {
            files.push(CredentialFileEntry {
                host_credential_file: self.providers.codex.host_credential_file(),
                guest: default_codex_guest_credential_file(),
                mode: default_credential_mode(),
                stub: default_codex_stub(),
            });
        }

        files.extend(self.advanced.stub_files.iter().cloned());
        files
    }

    pub fn claude_enabled(&self) -> bool {
        self.providers.claude.enabled
    }

    pub fn codex_enabled(&self) -> bool {
        self.providers.codex.enabled
    }
}

/// First-class managed providers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManagedAuthProviders {
    #[serde(default)]
    pub claude: ClaudeProviderConfig,
    #[serde(default)]
    pub codex: CodexProviderConfig,
}

/// Claude Code managed-auth configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeProviderConfig {
    /// Whether abox should stage the Claude stub and proxy the host token.
    #[serde(default)]
    pub enabled: bool,
    /// Optional override for the host credential file path.
    #[serde(default)]
    pub host_credential_file: Option<String>,
}

impl ClaudeProviderConfig {
    pub fn host_credential_file(&self) -> String {
        self.host_credential_file.clone().unwrap_or_else(default_claude_host_credential_file)
    }
}

/// Codex managed-auth configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexProviderConfig {
    /// Whether abox should stage the Codex stub and proxy the host token.
    #[serde(default)]
    pub enabled: bool,
    /// Optional override for the host credential file path.
    #[serde(default)]
    pub host_credential_file: Option<String>,
}

impl CodexProviderConfig {
    pub fn host_credential_file(&self) -> String {
        self.host_credential_file.clone().unwrap_or_else(default_codex_host_credential_file)
    }
}

/// Advanced auth configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdvancedAuthConfig {
    /// Stub-only guest files for unsupported tools.
    #[serde(default)]
    pub stub_files: Vec<CredentialFileEntry>,
}

/// A stub file to place inside the guest VM at boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialFileEntry {
    /// Source path on the host. Supports `~` expansion.
    pub host_credential_file: String,
    /// Absolute destination path inside the VM.
    pub guest: String,
    /// Unix permissions for the file. Default: "0600".
    #[serde(default = "default_credential_mode")]
    pub mode: String,
    /// JSON-ish placeholder content staged into the guest.
    pub stub: toml::Value,
}

/// Proxy daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Port for the HTTP egress proxy. Default: 18443.
    #[serde(default = "default_egress_port")]
    pub egress_port: u16,

    /// Directory containing policy TOML files.
    #[serde(default = "default_policy_dir")]
    pub policy_dir: PathBuf,
}

/// Runtime timing knobs for the VM supervisor.
///
/// Centralizes the polling intervals and timeouts that used to be hardcoded
/// throughout `cloud_hypervisor.rs`, `console.rs`, and `sandbox.rs`. The
/// defaults match the pre-refactor literals so behavior is unchanged.
/// Tests can construct a custom instance to drive faster simulations.
///
/// Not yet exposed in the TOML schema — the goal of this introduction is
/// to make tuning *possible* without committing to a public surface.
#[derive(Debug, Clone, Copy)]
pub struct VmRuntimeTuning {
    /// How often `run_sandbox` polls `vm_manager.info()` for VM exit.
    pub vm_exit_poll_interval: std::time::Duration,
    /// How often the console tailer polls for new bytes after EOF.
    pub console_poll_interval: std::time::Duration,
    /// How long to wait for a virtiofsd / cloud-hypervisor socket file to
    /// appear before bailing out.
    pub socket_wait_timeout: std::time::Duration,
    /// Grace period after sending a stop command before force-killing the VM.
    pub vm_timeout_grace_period: std::time::Duration,
}

impl VmRuntimeTuning {
    /// Default tuning: 250 ms VM exit poll, 50 ms console poll,
    /// 5 s socket wait.
    pub const DEFAULT: Self = Self {
        vm_exit_poll_interval: std::time::Duration::from_millis(250),
        console_poll_interval: std::time::Duration::from_millis(50),
        socket_wait_timeout: std::time::Duration::from_secs(5),
        vm_timeout_grace_period: std::time::Duration::from_secs(10),
    };
}

impl Default for VmRuntimeTuning {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Default for AboxConfig {
    fn default() -> Self {
        Self {
            state_dir: default_state_dir(),
            runtime_dir: None,
            vm_defaults: VmDefaults::default(),
            proxy: ProxyConfig::default(),
            auth: AuthConfig::default(),
        }
    }
}

impl Default for VmDefaults {
    fn default() -> Self {
        Self {
            memory_mib: default_memory(),
            vcpus: default_vcpus(),
            image_path: None,
            kernel_path: None,
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self { egress_port: default_egress_port(), policy_dir: default_policy_dir() }
    }
}

impl AboxConfig {
    /// Load configuration from a TOML file. Falls back to defaults if the file
    /// does not exist.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            let config: Self = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    /// Return the default config file path: `~/.abox/config.toml`.
    pub fn default_path() -> anyhow::Result<PathBuf> {
        let dir = default_state_dir();
        Ok(dir.join("config.toml"))
    }

    /// Return the worktrees directory: `<state_dir>/worktrees/`.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.state_dir.join("worktrees")
    }

    /// Return the templates directory: `<state_dir>/templates/`.
    pub fn templates_dir(&self) -> PathBuf {
        self.state_dir.join("templates")
    }

    /// Return the logs directory: `<state_dir>/logs/`.
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }

    /// Return the trust directory: `<state_dir>/trust/`.
    pub fn trust_dir(&self) -> PathBuf {
        self.state_dir.join("trust")
    }

    /// Return the runtime directory for sockets and PIDs.
    ///
    /// If `runtime_dir` is set in config, use it. Otherwise default to
    /// `<state_dir>/r` (a short name chosen deliberately — Linux caps Unix
    /// domain socket paths at 108 bytes, and abox appends per-sandbox suffixes
    /// like `vfs-status-<task-id>.sock`, so keeping the base path short avoids
    /// hitting the limit on machines with long home directory paths).
    pub fn runtime_dir(&self) -> PathBuf {
        self.runtime_dir.clone().unwrap_or_else(|| self.state_dir.join("r"))
    }

    /// Ensure all required directories exist.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.worktrees_dir())?;
        std::fs::create_dir_all(self.templates_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.trust_dir())?;
        std::fs::create_dir_all(self.runtime_dir())?;
        std::fs::create_dir_all(&self.proxy.policy_dir)?;
        Ok(())
    }
}

fn default_credential_mode() -> String {
    "0600".to_string()
}

fn default_state_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".abox")
}

fn default_memory() -> u32 {
    2048
}

fn default_vcpus() -> u8 {
    2
}

fn default_egress_port() -> u16 {
    18443
}

fn default_policy_dir() -> PathBuf {
    default_state_dir().join("policies")
}

pub fn default_claude_host_credential_file() -> String {
    "~/.claude/.credentials.json".to_string()
}

pub fn default_codex_host_credential_file() -> String {
    "~/.codex/auth.json".to_string()
}

fn default_claude_guest_credential_file() -> String {
    "~/.claude/.credentials.json".to_string()
}

fn default_codex_guest_credential_file() -> String {
    "~/.codex/auth.json".to_string()
}

fn default_claude_stub() -> toml::Value {
    toml::toml! {
        [claudeAiOauth]
        accessToken = "abox-proxy-managed"
        refreshToken = "abox-proxy-managed"
        expiresAt = 9_999_999_999_999i64
        scopes = ["user:inference"]
        subscriptionType = "pro"
    }
    .into()
}

fn default_codex_stub() -> toml::Value {
    toml::toml! {
        auth_mode = "chatgpt"
        last_refresh = "2099-01-01T00:00:00Z"

        [tokens]
        id_token = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJhYm94LXByb3h5LW1hbmFnZWQiLCJleHAiOjQxMDI0NDQ4MDB9.c2ln"
        access_token = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzY29wZSI6ImFib3gtcHJveHktbWFuYWdlZCIsImV4cCI6NDEwMjQ0NDgwMH0.c2ln"
        refresh_token = "abox-proxy-managed-refresh"
        account_id = "00000000-0000-4000-8000-000000000000"
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_auth_config_stages_no_credentials() {
        let cfg = AuthConfig::default();
        assert!(cfg.credential_files().is_empty(), "managed providers should opt in explicitly");
    }

    #[test]
    fn test_parse_managed_providers() {
        let toml_str = r#"
        [auth.providers.claude]
        enabled = true

        [auth.providers.codex]
        enabled = true
        host_credential_file = "/tmp/codex-auth.json"
    "#;
        let config: AboxConfig = toml::from_str(toml_str).unwrap();
        assert!(config.auth.providers.claude.enabled);
        assert!(config.auth.providers.codex.enabled);
        assert_eq!(
            config.auth.providers.codex.host_credential_file(),
            "/tmp/codex-auth.json".to_string()
        );

        let files = config.auth.credential_files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].host_credential_file, "~/.claude/.credentials.json");
        assert_eq!(files[0].guest, "~/.claude/.credentials.json");
        assert_eq!(files[1].host_credential_file, "/tmp/codex-auth.json");
        assert_eq!(files[1].guest, "~/.codex/auth.json");
    }

    #[test]
    fn test_parse_advanced_stub_files() {
        let toml_str = r#"
        [auth.advanced]
        [[auth.advanced.stub_files]]
        host_credential_file = "~/.tool/auth.json"
        guest = "~/.tool/auth.json"

        [auth.advanced.stub_files.stub]
        token = "abox-proxy-managed"
    "#;
        let config: AboxConfig = toml::from_str(toml_str).unwrap();
        let files = config.auth.credential_files();
        assert_eq!(files.len(), 1);
        let entry = &files[0];
        assert_eq!(entry.host_credential_file, "~/.tool/auth.json");
        assert_eq!(entry.guest, "~/.tool/auth.json");
        assert_eq!(entry.mode, "0600");
        assert_eq!(entry.stub["token"].as_str(), Some("abox-proxy-managed"));
    }

    #[test]
    fn default_codex_stub_matches_current_auth_shape() {
        let stub = default_codex_stub();
        assert_eq!(stub["auth_mode"].as_str(), Some("chatgpt"));
        assert_eq!(stub["last_refresh"].as_str(), Some("2099-01-01T00:00:00Z"));

        let id_token = stub["tokens"]["id_token"].as_str().expect("id_token missing");
        let access_token = stub["tokens"]["access_token"].as_str().expect("access_token missing");

        assert_eq!(id_token.matches('.').count(), 2, "id_token should be JWT-like");
        assert_eq!(access_token.matches('.').count(), 2, "access_token should be JWT-like");
        assert_eq!(
            stub["tokens"]["account_id"].as_str(),
            Some("00000000-0000-4000-8000-000000000000")
        );
    }

    #[test]
    fn test_parse_empty_auth_section() {
        let toml_str = r"
        [vm_defaults]
        memory_mib = 2048
    ";
        let config: AboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.auth.credential_files().len(),
            0,
            "absent [auth] section should not enable any managed providers by default"
        );
    }

    #[test]
    fn test_default_config() {
        let config = AboxConfig::default();
        assert_eq!(config.vm_defaults.memory_mib, 2048);
        assert_eq!(config.vm_defaults.vcpus, 2);
        assert_eq!(config.proxy.egress_port, 18443);
    }

    #[test]
    fn test_parse_minimal_toml() {
        let toml_str = r"
            [vm_defaults]
            memory_mib = 4096
            vcpus = 4
        ";
        let config: AboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.vm_defaults.memory_mib, 4096);
        assert_eq!(config.vm_defaults.vcpus, 4);
        // Proxy should use defaults
        assert_eq!(config.proxy.egress_port, 18443);
    }

    #[test]
    fn test_worktrees_dir() {
        let config =
            AboxConfig { state_dir: PathBuf::from("/tmp/test-abox"), ..Default::default() };
        assert_eq!(config.worktrees_dir(), PathBuf::from("/tmp/test-abox/worktrees"));
    }

    #[test]
    fn test_runtime_dir_default_is_short() {
        // The default runtime dir should preserve the full advertised task-ID
        // budget for a typical home-directory install.
        let config =
            AboxConfig { state_dir: PathBuf::from("/home/username/.abox"), ..Default::default() };
        let runtime = config.runtime_dir();
        assert!(
            crate::util::max_task_id_len_for_runtime_dir(&runtime) >= crate::util::TASK_ID_MAX_LEN,
            "default runtime_dir should preserve the full task-ID budget: {runtime:?}"
        );
    }
}
