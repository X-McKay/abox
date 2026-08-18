//! Configuration types for abox.
//!
//! All configuration is loaded from TOML files. The main config file lives at
//! `~/.abox/config.toml` and policy files live under the `policies/` directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level abox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AboxConfig {
    /// Base directory for abox state (worktrees, logs, trust records).
    /// Defaults to `~/.abox`.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,

    /// Directory for runtime files (sockets, PIDs).
    /// Defaults to `<state_dir>/run` so abox is fully usable without root.
    /// Set to e.g. `/run/abox` for a system-wide install.
    #[serde(default)]
    pub runtime_dir: Option<PathBuf>,

    /// Default resources applied to all sandboxes unless overridden.
    #[serde(default)]
    pub sandbox_defaults: SandboxDefaults,

    /// Proxy daemon configuration.
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// Managed authentication and advanced stub configuration.
    #[serde(default)]
    pub auth: AuthConfig,

    /// Guest OCI image configuration for the MicroSandbox runtime.
    #[serde(default)]
    pub images: ImagesConfig,

    /// Host-owned validation applied before agent branches are merged.
    ///
    /// This configuration deliberately lives in `~/.abox/config.toml`, not
    /// `.abox/project.toml`: an agent can edit files in its worktree, so
    /// repository-owned configuration cannot be a merge security boundary.
    #[serde(default)]
    pub merge: MergeConfig,
}

/// `[merge]` section — host-owned integration behavior.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergeConfig {
    /// Rules evaluated before `abox merge` checks out or integrates an agent
    /// branch.
    #[serde(default)]
    pub validation: MergeValidationConfig,
}

/// `[merge.validation]` section — optional rules for changes produced by an
/// agent branch.
///
/// Empty/default rules preserve the historical merge behavior. Patterns are
/// interpreted as repository-relative globs and compiled by the workspace
/// adapter before they are used.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergeValidationConfig {
    /// Repository-relative globs that are never allowed to be merged.
    #[serde(default)]
    pub deny_patterns: Vec<String>,

    /// Repository-relative globs that require an exact `--approve-path`
    /// acknowledgement before merging.
    #[serde(default)]
    pub require_review_paths: Vec<String>,

    /// Reject a path whose mode changes to executable.
    #[serde(default)]
    pub deny_new_executables: bool,

    /// Reject an incoming blob larger than this many KiB. `None` disables the
    /// limit.
    #[serde(default)]
    pub max_file_size_kib: Option<u64>,
}

/// `[images]` section — guest OCI image behavior for the MicroSandbox
/// runtime.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImagesConfig {
    /// Development escape hatch: per-profile image reference overrides,
    /// e.g. `overrides = { node = "localhost:5000/dev-guest:latest" }`.
    /// Host-owned only; repo config can never choose an image.
    #[serde(default)]
    pub overrides: std::collections::HashMap<String, String>,
}

/// Default sandbox resource allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxDefaults {
    /// Memory in MiB. Default: 2048.
    #[serde(default = "default_memory")]
    pub memory_mib: u32,

    /// Number of vCPUs. Default: 2.
    #[serde(default = "default_vcpus")]
    pub vcpus: u8,
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

/// A stub file to place inside the guest at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialFileEntry {
    /// Source path on the host. Supports `~` expansion.
    pub host_credential_file: String,
    /// Absolute destination path inside the guest.
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

impl Default for AboxConfig {
    fn default() -> Self {
        Self {
            state_dir: default_state_dir(),
            runtime_dir: None,
            sandbox_defaults: SandboxDefaults::default(),
            proxy: ProxyConfig::default(),
            auth: AuthConfig::default(),
            images: ImagesConfig::default(),
            merge: MergeConfig::default(),
        }
    }
}

impl Default for SandboxDefaults {
    fn default() -> Self {
        Self { memory_mib: default_memory(), vcpus: default_vcpus() }
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
    /// like `msb-<task-id>.sock_5000`, so keeping the base path short avoids
    /// hitting the limit on machines with long home directory paths).
    pub fn runtime_dir(&self) -> PathBuf {
        self.runtime_dir.clone().unwrap_or_else(|| self.state_dir.join("r"))
    }

    /// Ensure all required directories exist.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.worktrees_dir())?;
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
        [sandbox_defaults]
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
        assert_eq!(config.sandbox_defaults.memory_mib, 2048);
        assert_eq!(config.sandbox_defaults.vcpus, 2);
        assert_eq!(config.proxy.egress_port, 18443);
    }

    #[test]
    fn test_parse_minimal_toml() {
        let toml_str = r"
            [sandbox_defaults]
            memory_mib = 4096
            vcpus = 4
        ";
        let config: AboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.sandbox_defaults.memory_mib, 4096);
        assert_eq!(config.sandbox_defaults.vcpus, 4);
        // Proxy should use defaults
        assert_eq!(config.proxy.egress_port, 18443);
    }

    #[test]
    fn merge_validation_is_host_config_and_defaults_to_no_rules() {
        let toml_str = r#"
            [merge.validation]
            deny_patterns = [".github/**"]
            require_review_paths = ["Cargo.toml"]
            deny_new_executables = true
            max_file_size_kib = 512
        "#;
        let config: AboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.merge.validation.deny_patterns, [".github/**"]);
        assert_eq!(config.merge.validation.require_review_paths, ["Cargo.toml"]);
        assert!(config.merge.validation.deny_new_executables);
        assert_eq!(config.merge.validation.max_file_size_kib, Some(512));
        assert_eq!(AboxConfig::default().merge.validation.max_file_size_kib, None);
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
