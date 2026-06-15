//! Repo-local abox project configuration.

use crate::config::AboxConfig;
use anyhow::{Context, Result};
use chrono::Utc;
use git2::Repository;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::fmt::Write as _;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// The repo-local config path under a repository root.
pub const PROJECT_CONFIG_RELATIVE_PATH: &str = ".abox/project.toml";

/// Built-in bundle names supported by the simple scoped network UX.
pub const KNOWN_BUNDLES: &[&str] = &["npm-public", "pypi-public", "cargo-public"];
/// Durable per-project cache families supported by guest-native environments.
pub const KNOWN_CACHE_FAMILIES: &[&str] = &["npm", "pip", "uv", "cargo"];
/// Monotonic catalog version used in approval fingerprints.
pub const BUNDLE_CATALOG_VERSION: &str = "2026-05-09.1";

/// User-facing network modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// Only host-managed egress rules are allowed.
    Safe,
    /// Host-managed rules plus explicit repo-approved additions.
    Scoped,
    /// Host-managed rules plus broad proxy-mediated HTTPS egress.
    Open,
}

impl fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safe => write!(f, "safe"),
            Self::Scoped => write!(f, "scoped"),
            Self::Open => write!(f, "open"),
        }
    }
}

impl FromStr for NetworkMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "safe" => Ok(Self::Safe),
            "scoped" => Ok(Self::Scoped),
            "open" => Ok(Self::Open),
            other => {
                anyhow::bail!("unknown network mode {other:?}; expected safe, scoped, or open")
            }
        }
    }
}

/// Official guest environment profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentProfile {
    /// The current default guest image.
    Base,
    /// Node.js and npm focused guest image.
    Node,
    /// Python-focused guest image.
    Python,
    /// Rust-focused guest image.
    Rust,
}

impl EnvironmentProfile {
    /// Human-readable summary of the toolchain this profile guarantees.
    pub fn toolchain_summary(&self) -> &'static str {
        match self {
            Self::Base => "default guest image; no official language-specific toolchain guarantee",
            Self::Node => "node and npm",
            Self::Python => "python3, uv, and pip3",
            Self::Rust => "rustc and cargo",
        }
    }

    /// Return true when a cache family is a natural match for this profile.
    pub fn supports_cache(&self, cache: &str) -> bool {
        matches!(
            (self, cache),
            (Self::Base, _)
                | (Self::Node, "npm")
                | (Self::Python, "pip" | "uv")
                | (Self::Rust, "cargo")
        )
    }

    /// Return the official profile that best matches a cache family.
    pub fn recommended_for_cache(cache: &str) -> Option<Self> {
        match cache {
            "npm" => Some(Self::Node),
            "pip" | "uv" => Some(Self::Python),
            "cargo" => Some(Self::Rust),
            _ => None,
        }
    }

    /// Return true when this profile expects a dedicated rootfs image.
    pub fn uses_dedicated_image(&self) -> bool {
        *self != Self::Base
    }

    /// Lowercase profile name used in paths and records.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Node => "node",
            Self::Python => "python",
            Self::Rust => "rust",
        }
    }
}

impl fmt::Display for EnvironmentProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EnvironmentProfile {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "base" => Ok(Self::Base),
            "node" => Ok(Self::Node),
            "python" => Ok(Self::Python),
            "rust" => Ok(Self::Rust),
            other => anyhow::bail!(
                "unknown environment profile {other:?}; expected base, node, python, or rust"
            ),
        }
    }
}

/// Optional project metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    /// Optional stable project identity override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Repo-local network configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// The user-facing network mode.
    pub mode: NetworkMode,
    /// Built-in named bundles allowed in scoped mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bundles: Vec<String>,
    /// Exact hostnames allowed in scoped mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { mode: NetworkMode::Safe, bundles: Vec::new(), domains: Vec::new() }
    }
}

/// Optional environment reuse configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfig {
    /// Optional official guest profile to use for this repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<EnvironmentProfile>,
    /// Cache families to enable for this repo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<String>,
    /// Optional prepare script path relative to the repo root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare: Option<PathBuf>,
    /// Optional additional files that influence environment freshness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<PathBuf>,
    /// Workspace subdirectories to overlay with empty tmpfs inside the guest.
    ///
    /// Use this to prevent platform-specific dependency directories (e.g.
    /// `node_modules`, `.venv`, `target`) from leaking from a macOS host into
    /// the Linux guest. Each excluded path receives an empty tmpfs mount so
    /// the guest sees a clean directory; the host copy is untouched.
    ///
    /// Paths must be relative to the workspace root, must not start with `/`
    /// or `..`, and must not overlap with each other.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mount_excludes: Vec<String>,
}

/// Optional agent defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Optional default prompt file path relative to the repo root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_prompt_file: Option<PathBuf>,
}

/// Repo-local abox config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// Optional project metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectSection>,
    /// Repo-local network behavior.
    #[serde(default)]
    pub network: NetworkConfig,
    /// Optional environment reuse hints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentConfig>,
    /// Optional agent defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfig>,
    /// Optional ephemeral service sidecars (postgres, redis, ollama).
    ///
    /// Services are started as Docker containers on the host before the
    /// sandbox VM boots. Connection URLs are injected as environment
    /// variables (ABOX_POSTGRES_URL, ABOX_REDIS_URL, ABOX_OLLAMA_URL).
    /// All services are stopped and removed when the sandbox exits.
    ///
    /// Example:
    /// ```toml
    /// [services]
    /// postgres = { version = "17" }
    /// redis = { version = "7" }
    /// ollama = { models = ["qwen2.5-coder:7b"] }
    /// ```
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub services: std::collections::HashMap<String, crate::services::ServiceConfig>,
    /// Repo-declared host-port bridges. Each splices a guest loopback port to
    /// an existing host loopback service. Refused in `safe` network mode and
    /// audited per connection — an explicit egress-boundary exception.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_ports: Vec<crate::services::HostPortBridge>,
}

/// The normalized scoped network input used by policy compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkScope {
    /// Requested network mode.
    pub mode: NetworkMode,
    /// Built-in named bundles.
    pub bundles: Vec<String>,
    /// Explicit exact hostnames.
    pub domains: Vec<String>,
}

/// A resolved repo-local behavior model used by trust and launch decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProjectConfig {
    /// Canonical config path.
    pub config_path: PathBuf,
    /// Stable project identity.
    pub project_id: String,
    /// The normalized default network mode from repo config.
    pub default_network_mode: NetworkMode,
    /// Whether the repo declares any `[[host_ports]]` bridges. A declared
    /// host-port bridge counts as a `scoped` addition, so a `scoped` config
    /// whose only addition is a host-port bridge is not normalized to `safe`.
    pub has_host_ports: bool,
    /// Scoped bundles declared by the repo.
    pub bundles: Vec<String>,
    /// Scoped exact hostnames declared by the repo.
    pub domains: Vec<String>,
    /// Guest environment profile selected for this repo.
    pub environment_profile: EnvironmentProfile,
    /// Durable cache families enabled for this repo.
    pub caches: Vec<String>,
    /// Optional prepare script path relative to the repo root.
    pub prepare_path: Option<PathBuf>,
    /// Immutable bytes loaded during resolution for trust and later staging.
    pub prepare_bytes: Option<Vec<u8>>,
    /// Additional repo-relative inputs that affect environment freshness.
    pub watch_paths: Vec<PathBuf>,
    /// Optional default prompt file path relative to the repo root.
    pub default_prompt_path: Option<PathBuf>,
    /// Immutable bytes loaded during resolution for trust and later staging.
    pub default_prompt_bytes: Option<Vec<u8>>,
    /// Workspace subdirectories to overlay with empty tmpfs inside the guest.
    pub mount_excludes: Vec<String>,
    /// Human-readable normalization notes.
    pub notes: Vec<String>,
    /// Current approval fingerprint for this repo-owned behavior.
    pub approval_fingerprint: String,
}

/// Inspectable approval record stored under host state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// Approval schema version.
    pub version: u32,
    /// Stable project identity.
    pub project_id: String,
    /// Behavior fingerprint that was approved.
    pub approval_fingerprint: String,
    /// Approval timestamp in UTC.
    pub approved_at: String,
}

/// Inspectable environment warm-state record stored under host state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentStateRecord {
    /// Environment status schema version.
    pub version: u32,
    /// Stable project identity.
    pub project_id: String,
    /// Current environment freshness fingerprint.
    pub environment_fingerprint: String,
    /// Rootfs/image token used when the environment was warmed.
    pub rootfs_token: String,
    /// Guest profile used when the environment was warmed.
    #[serde(default)]
    pub environment_profile: Option<String>,
    /// Enabled durable cache families at warm time.
    pub caches: Vec<String>,
    /// Optional prepare script path that was executed.
    pub prepare_path: Option<String>,
    /// Warm timestamp in UTC.
    pub warmed_at: String,
}

#[derive(Clone, Copy)]
struct BundleSpec {
    hosts: &'static [&'static str],
    description: &'static str,
}

struct ApprovalFingerprintInputs<'a> {
    config_bytes: &'a [u8],
    default_network_mode: NetworkMode,
    environment_profile: EnvironmentProfile,
    bundles: &'a [String],
    domains: &'a [String],
    default_prompt_path: Option<&'a Path>,
    default_prompt_bytes: Option<&'a [u8]>,
    prepare_path: Option<&'a Path>,
    prepare_bytes: Option<&'a [u8]>,
}

impl ProjectConfig {
    /// Return the canonical repo-local config path.
    pub fn default_path(repo_root: &Path) -> PathBuf {
        repo_root.join(PROJECT_CONFIG_RELATIVE_PATH)
    }

    /// Load the repo-local config if it exists.
    pub fn load(repo_root: &Path) -> Result<Option<Self>> {
        let path = Self::default_path(repo_root);
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&content).with_context(|| format!("Parsing {}", path.display()))?;
        config.validate(repo_root)?;
        Ok(Some(config))
    }

    /// Resolve repo-owned behavior into a normalized, fingerprinted model.
    pub fn resolve(&self, repo_root: &Path) -> Result<ResolvedProjectConfig> {
        self.validate(repo_root)?;

        let config_path = Self::default_path(repo_root);
        let config_bytes = std::fs::read(&config_path)
            .with_context(|| format!("Reading {}", config_path.display()))?;

        let mut notes = Vec::new();
        let mut default_network_mode = self.network.mode;
        if self.network.mode == NetworkMode::Scoped
            && self.network.bundles.is_empty()
            && self.network.domains.is_empty()
            && self.host_ports.is_empty()
        {
            default_network_mode = NetworkMode::Safe;
            notes.push(
                "Normalized network.mode = \"scoped\" with no additions to safe.".to_string(),
            );
        }

        let prepare_path =
            self.environment.as_ref().and_then(|environment| environment.prepare.clone());
        let prepare_bytes = prepare_path
            .as_ref()
            .map(|path| read_repo_file_bytes(repo_root, path, "environment.prepare"))
            .transpose()?;
        let caches = normalized_caches(self.environment.as_ref().map(|env| &env.caches));
        let environment_profile = self
            .environment
            .as_ref()
            .and_then(|environment| environment.profile)
            .unwrap_or(EnvironmentProfile::Base);
        if environment_profile == EnvironmentProfile::Base {
            if let Some(recommended) = recommended_profile_for_caches(&caches) {
                notes.push(format!(
                    "Environment profile defaults to base; set environment.profile = \"{recommended}\" for an official {recommended} guest profile when available."
                ));
            } else if !caches.is_empty() {
                notes.push(
                    "Environment profile defaults to base; current cache mix does not map to a single official profile."
                        .to_string(),
                );
            }
        }
        let watch_paths = infer_watch_paths(self.environment.as_ref(), &caches);
        let mount_excludes =
            self.environment.as_ref().map(|env| env.mount_excludes.clone()).unwrap_or_default();

        let default_prompt_path =
            self.agent.as_ref().and_then(|agent| agent.default_prompt_file.clone());
        let default_prompt_bytes = default_prompt_path
            .as_ref()
            .map(|path| read_repo_file_bytes(repo_root, path, "agent.default_prompt_file"))
            .transpose()?;

        let project_id = derive_project_identity(repo_root, self.project.as_ref())?;
        let bundles = self.network.bundles.clone();
        let domains = self.network.domains.clone();
        let approval_inputs = ApprovalFingerprintInputs {
            config_bytes: &config_bytes,
            default_network_mode,
            environment_profile,
            bundles: &bundles,
            domains: &domains,
            default_prompt_path: default_prompt_path.as_deref(),
            default_prompt_bytes: default_prompt_bytes.as_deref(),
            prepare_path: prepare_path.as_deref(),
            prepare_bytes: prepare_bytes.as_deref(),
        };
        let approval_fingerprint = build_approval_fingerprint(&approval_inputs);

        Ok(ResolvedProjectConfig {
            config_path,
            project_id,
            default_network_mode,
            has_host_ports: !self.host_ports.is_empty(),
            bundles,
            domains,
            environment_profile,
            caches,
            prepare_path,
            prepare_bytes,
            watch_paths,
            default_prompt_path,
            default_prompt_bytes,
            mount_excludes,
            notes,
            approval_fingerprint,
        })
    }

    /// Validate the config semantically against repo-local expectations.
    pub fn validate(&self, repo_root: &Path) -> Result<()> {
        for bundle in &self.network.bundles {
            if bundle_hosts(bundle).is_none() {
                anyhow::bail!(
                    "Unknown network bundle {bundle:?}; known bundles: {}",
                    KNOWN_BUNDLES.join(", ")
                );
            }
        }

        for domain in &self.network.domains {
            validate_hostname(domain)
                .map_err(|e| anyhow::anyhow!("Invalid network domain {domain:?}: {e}"))?;
        }

        match self.network.mode {
            NetworkMode::Safe => {
                if !self.network.bundles.is_empty() || !self.network.domains.is_empty() {
                    anyhow::bail!(
                        "network.mode = \"safe\" cannot be combined with bundles or domains"
                    );
                }
            }
            NetworkMode::Scoped => {}
            NetworkMode::Open => {
                if !self.network.bundles.is_empty() || !self.network.domains.is_empty() {
                    anyhow::bail!(
                        "network.mode = \"open\" cannot be combined with bundles or domains"
                    );
                }
            }
        }

        if let Some(project) = &self.project {
            if let Some(id) = &project.id {
                crate::util::validate_task_id(id)
                    .map_err(|e| anyhow::anyhow!("invalid project.id {id:?}: {e}"))?;
            }
        }

        if let Some(environment) = &self.environment {
            let selected_profile = environment.profile.unwrap_or(EnvironmentProfile::Base);
            for cache in &environment.caches {
                if !KNOWN_CACHE_FAMILIES.iter().any(|known| known == cache) {
                    anyhow::bail!(
                        "Unknown environment cache family {cache:?}; known caches: {}",
                        KNOWN_CACHE_FAMILIES.join(", ")
                    );
                }
                if selected_profile != EnvironmentProfile::Base
                    && !selected_profile.supports_cache(cache)
                {
                    let recommended = EnvironmentProfile::recommended_for_cache(cache)
                        .map(|profile| format!(" Try environment.profile = \"{profile}\" instead."))
                        .unwrap_or_default();
                    anyhow::bail!(
                        "environment.profile = \"{selected_profile}\" does not support cache family {cache:?}.{recommended}"
                    );
                }
            }
            if selected_profile == EnvironmentProfile::Rust {
                validate_rust_profile_repo_compatibility(repo_root)?;
            }
            if let Some(prepare) = &environment.prepare {
                ensure_repo_owned_path(repo_root, prepare, "environment.prepare", true)?;
            }
            for watch in &environment.watch {
                ensure_repo_owned_path(repo_root, watch, "environment.watch", false)?;
            }
        }

        if let Some(agent) = &self.agent {
            if let Some(prompt_file) = &agent.default_prompt_file {
                ensure_repo_owned_path(repo_root, prompt_file, "agent.default_prompt_file", true)?;
            }
        }

        if let Some(environment) = &self.environment {
            validate_mount_excludes(&environment.mount_excludes)?;
        }

        // Each host-port bridge becomes a guest-side socat TCP listener, so guest
        // ports must be non-zero and unique across host-port bridges and the
        // well-known ports of declared [services] sidecars — otherwise the
        // in-guest listeners would collide at boot.
        {
            use std::collections::HashSet;
            let mut guest_ports: HashSet<u16> = HashSet::new();
            for name in self.services.keys() {
                if let Some(def) = crate::services::find_service_def(name) {
                    guest_ports.insert(def.default_port);
                }
            }
            for hp in &self.host_ports {
                if hp.guest == 0 || hp.host == 0 {
                    anyhow::bail!(
                        "[[host_ports]] entry has an invalid port 0 (guest = {}, host = {})",
                        hp.guest,
                        hp.host
                    );
                }
                if !guest_ports.insert(hp.guest) {
                    anyhow::bail!(
                        "[[host_ports]] guest port {} is declared more than once (or collides \
                         with a [services] sidecar port); each guest port maps to exactly one \
                         in-guest listener",
                        hp.guest
                    );
                }
            }
        }

        Ok(())
    }

    /// Resolve the effective network scope for a run.
    pub fn network_scope(
        &self,
        repo_root: &Path,
        override_mode: Option<NetworkMode>,
    ) -> Result<NetworkScope> {
        self.resolve(repo_root)?.effective_network_scope(override_mode)
    }
}

impl ResolvedProjectConfig {
    /// Resolve the effective network scope for a run, applying any CLI
    /// override to the repo-owned additions.
    pub fn effective_network_scope(
        &self,
        override_mode: Option<NetworkMode>,
    ) -> Result<NetworkScope> {
        let mode = override_mode.unwrap_or(self.default_network_mode);
        let scope = match mode {
            NetworkMode::Safe | NetworkMode::Open => {
                NetworkScope { mode, bundles: Vec::new(), domains: Vec::new() }
            }
            NetworkMode::Scoped => {
                NetworkScope { mode, bundles: self.bundles.clone(), domains: self.domains.clone() }
            }
        };
        // A declared host-port bridge is itself a reviewable `scoped` addition,
        // so a scoped config whose only addition is a host-port bridge is valid.
        validate_network_scope(&scope, self.has_host_ports)?;
        Ok(scope)
    }

    /// Human-readable summary of repo-owned behavior plus surfaced
    /// host-managed domains from the current host policy.
    pub fn summary_lines(&self, host_managed_domains: &[String]) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!("Project identity: {}", self.project_id));
        lines.push(format!("Repo config: {}", self.config_path.display()));
        lines.push(format!("Default network mode: {}", self.default_network_mode));
        lines.push(format!(
            "Environment profile: {} ({})",
            self.environment_profile,
            self.environment_profile.toolchain_summary()
        ));

        if !self.bundles.is_empty() {
            let bundles = self
                .bundles
                .iter()
                .map(|bundle| {
                    let description = bundle_description(bundle).unwrap_or("custom bundle");
                    format!("{bundle} ({description})")
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Scoped bundles: {bundles}"));
        }
        if !self.domains.is_empty() {
            lines.push(format!("Scoped domains: {}", self.domains.join(", ")));
        }
        if !self.caches.is_empty() {
            lines.push(format!("Durable caches: {}", self.caches.join(", ")));
        }
        if let Some(prepare_path) = &self.prepare_path {
            lines.push(format!("Prepare script: {}", prepare_path.display()));
        }
        if !self.watch_paths.is_empty() {
            let watches = self
                .watch_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Environment watch files: {watches}"));
        }
        if let Some(prompt_path) = &self.default_prompt_path {
            lines.push(format!("Default prompt file: {}", prompt_path.display()));
        }
        if !host_managed_domains.is_empty() {
            lines.push(format!("Host-managed domains: {}", host_managed_domains.join(", ")));
        }
        lines.extend(self.notes.iter().cloned());
        lines
    }

    /// Return true if this repo config enables any durable caches.
    pub fn has_durable_caches(&self) -> bool {
        !self.caches.is_empty()
    }

    /// Return true if this repo config has an immutable prepare flow.
    pub fn has_prepare_flow(&self) -> bool {
        self.prepare_bytes.is_some()
    }

    /// Return true if the current first-release environment warm flow can
    /// persist useful state for this repo.
    pub fn is_warmable(&self) -> bool {
        self.has_durable_caches() && self.has_prepare_flow()
    }

    /// Build guest environment variables for the configured durable caches.
    pub fn cache_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        for cache in &self.caches {
            match cache.as_str() {
                "npm" => {
                    vars.push(("NPM_CONFIG_CACHE".to_string(), "/abox-cache/npm".to_string()));
                    vars.push(("npm_config_cache".to_string(), "/abox-cache/npm".to_string()));
                }
                "pip" => {
                    vars.push(("PIP_CACHE_DIR".to_string(), "/abox-cache/pip".to_string()));
                }
                "uv" => {
                    vars.push(("UV_CACHE_DIR".to_string(), "/abox-cache/uv".to_string()));
                }
                "cargo" => {
                    vars.push(("CARGO_HOME".to_string(), "/abox-cache/cargo".to_string()));
                }
                _ => {}
            }
        }
        vars
    }

    /// Build the environment freshness fingerprint for durable-cache warming.
    pub fn environment_fingerprint(&self, repo_root: &Path, rootfs_token: &str) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(b"abox-project-environment-v1\n");
        hasher.update(b"project-id:");
        hasher.update(self.project_id.as_bytes());
        hasher.update(b"\nrootfs-token:");
        hasher.update(rootfs_token.as_bytes());
        hasher.update(b"\nnetwork-mode:");
        hasher.update(self.default_network_mode.to_string().as_bytes());
        hasher.update(b"\nenvironment-profile:");
        hasher.update(self.environment_profile.to_string().as_bytes());

        for bundle in &self.bundles {
            hasher.update(b"\nbundle:");
            hasher.update(bundle.as_bytes());
        }
        for domain in &self.domains {
            hasher.update(b"\ndomain:");
            hasher.update(domain.as_bytes());
        }
        for cache in &self.caches {
            hasher.update(b"\ncache:");
            hasher.update(cache.as_bytes());
        }
        if let Some(path) = &self.prepare_path {
            hasher.update(b"\nprepare-path:");
            hasher.update(path.display().to_string().as_bytes());
        }
        if let Some(bytes) = &self.prepare_bytes {
            hasher.update(b"\nprepare-bytes:");
            hasher.update(bytes);
        }

        for watch_path in &self.watch_paths {
            hasher.update(b"\nwatch-path:");
            hasher.update(watch_path.display().to_string().as_bytes());
            let absolute =
                ensure_repo_owned_path(repo_root, watch_path, "environment.watch", false)?;
            if absolute.exists() {
                let bytes = std::fs::read(&absolute)
                    .with_context(|| format!("Reading watch file {}", absolute.display()))?;
                hasher.update(b"\nwatch-bytes:");
                hasher.update(&bytes);
            } else {
                hasher.update(b"\nwatch-missing");
            }
        }

        Ok(hash_hex(hasher.finalize().as_slice()))
    }
}

/// Create the minimal starter config used by `abox project init`.
pub fn starter_config_toml() -> String {
    "[network]\nmode = \"safe\"\n".to_string()
}

/// Create starter config with an optional official guest profile.
pub fn starter_config_toml_with_profile(profile: Option<EnvironmentProfile>) -> String {
    match profile {
        None | Some(EnvironmentProfile::Base) => starter_config_toml(),
        Some(profile) => {
            format!("[network]\nmode = \"safe\"\n\n[environment]\nprofile = \"{profile}\"\n")
        }
    }
}

/// Build a network scope when no repo-local config exists.
pub fn standalone_network_scope(mode: NetworkMode) -> Result<NetworkScope> {
    let scope = NetworkScope { mode, bundles: Vec::new(), domains: Vec::new() };
    // Standalone (no repo config) has no host-port bridges to count as an addition.
    validate_network_scope(&scope, false)?;
    Ok(scope)
}

/// Return the base directory used to store project approvals.
pub fn approvals_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("trust").join("projects")
}

/// Return the base directory used to store environment warm-state records.
pub fn environments_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("env").join("projects")
}

fn project_state_component(project_id: &str) -> String {
    let raw = hash_hex(project_id.as_bytes());
    if raw.len() <= 255 {
        raw
    } else {
        // Keep state path components bounded even when the project identity is
        // a long remote URL or canonical filesystem path.
        hash_hex(&Sha256::digest(project_id.as_bytes()))
    }
}

/// Return the host cache root for a specific repo identity.
pub fn project_cache_root(state_dir: &Path, project_id: &str) -> PathBuf {
    state_dir.join("cache").join("projects").join(project_state_component(project_id))
}

/// Return the on-disk record path for a project's current environment state.
pub fn environment_record_path(state_dir: &Path, project_id: &str) -> PathBuf {
    environments_dir(state_dir).join(project_state_component(project_id)).join("status.json")
}

/// Return the on-disk record path for a specific project identity and
/// approval fingerprint.
pub fn approval_record_path(
    state_dir: &Path,
    project_id: &str,
    approval_fingerprint: &str,
) -> PathBuf {
    approvals_dir(state_dir)
        .join(project_state_component(project_id))
        .join(format!("{approval_fingerprint}.json"))
}

/// Return true if the current repo-owned behavior has already been approved.
pub fn is_approved(state_dir: &Path, resolved: &ResolvedProjectConfig) -> bool {
    approval_record_path(state_dir, &resolved.project_id, &resolved.approval_fingerprint).exists()
}

/// Persist an approval record for the current repo-owned behavior.
pub fn record_approval(state_dir: &Path, resolved: &ResolvedProjectConfig) -> Result<PathBuf> {
    let path =
        approval_record_path(state_dir, &resolved.project_id, &resolved.approval_fingerprint);
    let parent = path.parent().context("approval path has no parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("Creating {}", parent.display()))?;

    let record = ApprovalRecord {
        version: 1,
        project_id: resolved.project_id.clone(),
        approval_fingerprint: resolved.approval_fingerprint.clone(),
        approved_at: Utc::now().to_rfc3339(),
    };
    let content = serde_json::to_vec_pretty(&record).context("Serializing approval record")?;
    std::fs::write(&path, content).with_context(|| format!("Writing {}", path.display()))?;
    Ok(path)
}

/// Load an existing approval record for the current repo-owned behavior, if it
/// exists and parses successfully.
pub fn load_approval_record(
    state_dir: &Path,
    resolved: &ResolvedProjectConfig,
) -> Result<Option<ApprovalRecord>> {
    let path =
        approval_record_path(state_dir, &resolved.project_id, &resolved.approval_fingerprint);
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read(&path).with_context(|| format!("Reading {}", path.display()))?;
    let record: ApprovalRecord =
        serde_json::from_slice(&content).with_context(|| format!("Parsing {}", path.display()))?;
    Ok(Some(record))
}

/// Load the current environment state record for a repo identity, if present.
pub fn load_environment_state(
    state_dir: &Path,
    project_id: &str,
) -> Result<Option<EnvironmentStateRecord>> {
    let path = environment_record_path(state_dir, project_id);
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read(&path).with_context(|| format!("Reading {}", path.display()))?;
    let record: EnvironmentStateRecord =
        serde_json::from_slice(&content).with_context(|| format!("Parsing {}", path.display()))?;
    Ok(Some(record))
}

/// Persist the current environment warm-state record for a repo identity.
pub fn record_environment_state(
    state_dir: &Path,
    record: &EnvironmentStateRecord,
) -> Result<PathBuf> {
    let path = environment_record_path(state_dir, &record.project_id);
    let parent = path.parent().context("environment state path has no parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("Creating {}", parent.display()))?;
    let content = serde_json::to_vec_pretty(record).context("Serializing environment state")?;
    std::fs::write(&path, content).with_context(|| format!("Writing {}", path.display()))?;
    Ok(path)
}

/// Remove the current environment warm-state record for a repo identity.
pub fn clear_environment_state(state_dir: &Path, project_id: &str) -> Result<bool> {
    let path = environment_record_path(state_dir, project_id);
    if !path.exists() {
        return Ok(false);
    }

    std::fs::remove_file(&path).with_context(|| format!("Removing {}", path.display()))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(true)
}

/// Build a compact token describing the current guest rootfs image for
/// environment freshness checks.
pub fn rootfs_token(config: &AboxConfig) -> Result<String> {
    rootfs_token_for_profile(config, EnvironmentProfile::Base)
}

/// Build a compact token describing the current guest rootfs image for a
/// specific environment profile.
pub fn rootfs_token_for_profile(
    config: &AboxConfig,
    profile: EnvironmentProfile,
) -> Result<String> {
    let image_path = image_path_for_profile(config, profile);
    let inputs_path = image_path.with_file_name(format!(
        "{}.inputs",
        image_path.file_name().and_then(|name| name.to_str()).unwrap_or("rootfs.raw")
    ));
    if inputs_path.exists() {
        let inputs = std::fs::read(&inputs_path)
            .with_context(|| format!("Reading rootfs inputs {}", inputs_path.display()))?;
        return Ok(format!(
            "{}:inputs:{}",
            image_path.display(),
            hash_hex(&Sha256::digest(&inputs))
        ));
    }

    let metadata = std::fs::metadata(&image_path)
        .with_context(|| format!("Reading rootfs metadata {}", image_path.display()))?;
    Ok(format!("{}:size:{}", image_path.display(), metadata.len()))
}

/// Resolve the guest rootfs path for a selected environment profile.
pub fn image_path_for_profile(config: &AboxConfig, profile: EnvironmentProfile) -> PathBuf {
    let default_vm_dir = config.state_dir.join("vm");
    match profile {
        EnvironmentProfile::Base => config
            .vm_defaults
            .image_path
            .clone()
            .unwrap_or_else(|| default_vm_dir.join("rootfs.raw")),
        _ => default_vm_dir.join("profiles").join(profile.as_str()).join("rootfs.raw"),
    }
}

/// Resolve the guest kernel path shared by all environment profiles.
pub fn kernel_path_for_profile(config: &AboxConfig) -> PathBuf {
    let default_vm_dir = config.state_dir.join("vm");
    config.vm_defaults.kernel_path.clone().unwrap_or_else(|| default_vm_dir.join("vmlinux"))
}

/// Choose the best default branch to fork for a repo-local prepare sandbox.
pub fn default_prepare_base_branch(repo_root: &Path) -> String {
    if let Ok(repo) = Repository::discover(repo_root) {
        if let Ok(head) = repo.head() {
            if let Some(name) = head.shorthand() {
                return name.to_string();
            }
        }
    }
    "main".to_string()
}

/// Return the built-in hostnames for a named bundle.
pub fn bundle_hosts(bundle: &str) -> Option<&'static [&'static str]> {
    bundle_spec(bundle).map(|spec| spec.hosts)
}

/// Return the human-readable description for a named bundle.
pub fn bundle_description(bundle: &str) -> Option<&'static str> {
    bundle_spec(bundle).map(|spec| spec.description)
}

fn validate_network_scope(scope: &NetworkScope, allow_empty_scoped: bool) -> Result<()> {
    for bundle in &scope.bundles {
        if bundle_hosts(bundle).is_none() {
            anyhow::bail!(
                "Unknown network bundle {bundle:?}; known bundles: {}",
                KNOWN_BUNDLES.join(", ")
            );
        }
    }

    for domain in &scope.domains {
        validate_hostname(domain)
            .map_err(|e| anyhow::anyhow!("Invalid network domain {domain:?}: {e}"))?;
    }

    match scope.mode {
        NetworkMode::Safe => {
            if !scope.bundles.is_empty() || !scope.domains.is_empty() {
                anyhow::bail!("safe mode cannot be combined with bundles or domains");
            }
        }
        NetworkMode::Scoped => {
            if scope.bundles.is_empty() && scope.domains.is_empty() && !allow_empty_scoped {
                anyhow::bail!(
                    "scoped mode requires at least one bundle, domain, or host-port bridge"
                );
            }
        }
        NetworkMode::Open => {
            if !scope.bundles.is_empty() || !scope.domains.is_empty() {
                anyhow::bail!("open mode cannot be combined with bundles or domains");
            }
        }
    }

    Ok(())
}

fn derive_project_identity(repo_root: &Path, project: Option<&ProjectSection>) -> Result<String> {
    if let Some(project) = project {
        if let Some(id) = &project.id {
            return Ok(id.clone());
        }
    }

    if let Ok(repo) = Repository::discover(repo_root) {
        if let Ok(remote) = repo.find_remote("origin") {
            if let Some(url) = remote.url() {
                return Ok(normalize_remote_url(url));
            }
        }

        if let Ok(remotes) = repo.remotes() {
            for name in remotes.iter().flatten() {
                if let Ok(remote) = repo.find_remote(name) {
                    if let Some(url) = remote.url() {
                        return Ok(normalize_remote_url(url));
                    }
                }
            }
        }
    }

    repo_root
        .canonicalize()
        .with_context(|| format!("Canonicalizing repo root {}", repo_root.display()))
        .map(|path| path.display().to_string())
}

fn normalize_remote_url(url: &str) -> String {
    url.trim().trim_end_matches(".git").to_ascii_lowercase()
}

fn build_approval_fingerprint(inputs: &ApprovalFingerprintInputs<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"abox-project-approval-v1\n");
    hasher.update(b"bundle-catalog-version:");
    hasher.update(BUNDLE_CATALOG_VERSION.as_bytes());
    hasher.update(b"\nconfig-bytes:");
    hasher.update(inputs.config_bytes);
    hasher.update(b"\nresolved-network-mode:");
    hasher.update(inputs.default_network_mode.to_string().as_bytes());
    hasher.update(b"\nresolved-environment-profile:");
    hasher.update(inputs.environment_profile.to_string().as_bytes());

    let mut resolved_hosts = inputs
        .bundles
        .iter()
        .filter_map(|bundle| bundle_hosts(bundle))
        .flat_map(|hosts| hosts.iter().copied())
        .map(str::to_string)
        .collect::<Vec<_>>();
    resolved_hosts.extend(inputs.domains.iter().cloned());
    resolved_hosts.sort();
    resolved_hosts.dedup();

    for host in resolved_hosts {
        hasher.update(b"\nresolved-host:");
        hasher.update(host.as_bytes());
    }

    if let Some(path) = inputs.default_prompt_path {
        hasher.update(b"\ndefault-prompt-path:");
        hasher.update(path.display().to_string().as_bytes());
    }
    if let Some(bytes) = inputs.default_prompt_bytes {
        hasher.update(b"\ndefault-prompt-bytes:");
        hasher.update(bytes);
    }
    if let Some(path) = inputs.prepare_path {
        hasher.update(b"\nprepare-path:");
        hasher.update(path.display().to_string().as_bytes());
    }
    if let Some(bytes) = inputs.prepare_bytes {
        hasher.update(b"\nprepare-bytes:");
        hasher.update(bytes);
    }

    hash_hex(hasher.finalize().as_slice())
}

fn normalized_caches(caches: Option<&Vec<String>>) -> Vec<String> {
    let mut unique = BTreeSet::new();
    if let Some(caches) = caches {
        for cache in caches {
            unique.insert(cache.clone());
        }
    }
    unique.into_iter().collect()
}

fn infer_watch_paths(environment: Option<&EnvironmentConfig>, caches: &[String]) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();

    for cache in caches {
        match cache.as_str() {
            "npm" => {
                paths.insert(PathBuf::from("package.json"));
                paths.insert(PathBuf::from("package-lock.json"));
                paths.insert(PathBuf::from("pnpm-lock.yaml"));
                paths.insert(PathBuf::from("yarn.lock"));
            }
            "pip" => {
                paths.insert(PathBuf::from("requirements.txt"));
                paths.insert(PathBuf::from("requirements-dev.txt"));
                paths.insert(PathBuf::from("pyproject.toml"));
                paths.insert(PathBuf::from("poetry.lock"));
            }
            "uv" => {
                paths.insert(PathBuf::from("pyproject.toml"));
                paths.insert(PathBuf::from("uv.lock"));
            }
            "cargo" => {
                paths.insert(PathBuf::from("Cargo.toml"));
                paths.insert(PathBuf::from("Cargo.lock"));
            }
            _ => {}
        }
    }

    if let Some(environment) = environment {
        for watch in &environment.watch {
            paths.insert(watch.clone());
        }
    }

    paths.into_iter().collect()
}

/// Validate that mount exclusion paths are safe relative paths.
///
/// Rules:
/// - Must not be empty
/// - Must not start with `/` or `..`
/// - Must not overlap with each other (neither is a prefix of the other)
fn validate_mount_excludes(excludes: &[String]) -> Result<()> {
    for path in excludes {
        if path.is_empty() {
            anyhow::bail!("environment.mount_excludes: empty path is not allowed");
        }
        if path.starts_with('/') {
            anyhow::bail!(
                "environment.mount_excludes: path {path:?} must be relative (no leading '/')"
            );
        }
        if path.starts_with("..") {
            anyhow::bail!(
                "environment.mount_excludes: path {path:?} must not escape the workspace (no '..')"
            );
        }
        if path.contains("/../") || path.ends_with("/..") {
            anyhow::bail!(
                "environment.mount_excludes: path {path:?} must not escape the workspace (no '..')"
            );
        }
    }

    // Check for overlapping paths (one is a prefix of another)
    for (i, a) in excludes.iter().enumerate() {
        for (j, b) in excludes.iter().enumerate() {
            if i == j {
                continue;
            }
            let a_prefix = format!("{a}/");
            let b_prefix = format!("{b}/");
            if b.starts_with(&a_prefix) || a.starts_with(&b_prefix) {
                anyhow::bail!(
                    "environment.mount_excludes: paths {a:?} and {b:?} overlap; \
                     one is a prefix of the other"
                );
            }
        }
    }

    Ok(())
}

fn validate_rust_profile_repo_compatibility(repo_root: &Path) -> Result<()> {
    let cargo_toml = repo_root.join("Cargo.toml");
    if cargo_toml.exists() {
        let content = std::fs::read_to_string(&cargo_toml)
            .with_context(|| format!("Reading {}", cargo_toml.display()))?;
        let value: toml::Value = toml::from_str(&content)
            .with_context(|| format!("Parsing {}", cargo_toml.display()))?;

        let edition = value
            .get("package")
            .and_then(|package| package.get("edition"))
            .and_then(toml::Value::as_str)
            .or_else(|| {
                value
                    .get("workspace")
                    .and_then(|workspace| workspace.get("package"))
                    .and_then(|package| package.get("edition"))
                    .and_then(toml::Value::as_str)
            });

        if matches!(edition, Some("2024")) {
            anyhow::bail!(
                "environment.profile = \"rust\" currently ships cargo/rustc 1.76 and cannot warm repos that require Cargo edition 2024.\n\
                 Update the rust guest profile/toolchain or use edition 2021 for this repo."
            );
        }
    }

    let cargo_lock = repo_root.join("Cargo.lock");
    if cargo_lock.exists() {
        let content = std::fs::read_to_string(&cargo_lock)
            .with_context(|| format!("Reading {}", cargo_lock.display()))?;
        let value: toml::Value = toml::from_str(&content)
            .with_context(|| format!("Parsing {}", cargo_lock.display()))?;
        if let Some(version) = value.get("version").and_then(toml::Value::as_integer) {
            if version >= 4 {
                anyhow::bail!(
                    "environment.profile = \"rust\" currently ships cargo 1.76 and cannot warm Cargo.lock version {version}.\n\
                     Regenerate the lockfile with an older compatible cargo, remove it before warming, or update the rust guest profile/toolchain."
                );
            }
        }
    }

    Ok(())
}

fn recommended_profile_for_caches(caches: &[String]) -> Option<EnvironmentProfile> {
    let mut recommended = None;
    for cache in caches {
        let cache_profile = EnvironmentProfile::recommended_for_cache(cache)?;
        if let Some(current) = recommended {
            if current != cache_profile {
                return None;
            }
        } else {
            recommended = Some(cache_profile);
        }
    }
    recommended
}

fn read_repo_file_bytes(
    repo_root: &Path,
    relative_path: &Path,
    field_name: &str,
) -> Result<Vec<u8>> {
    let path = ensure_repo_owned_path(repo_root, relative_path, field_name, true)?;
    std::fs::read(&path)
        .with_context(|| format!("Reading {} referenced by {}", path.display(), field_name))
}

fn ensure_repo_owned_path(
    repo_root: &Path,
    relative_path: &Path,
    field_name: &str,
    require_exists: bool,
) -> Result<PathBuf> {
    anyhow::ensure!(
        !relative_path.is_absolute(),
        "{field_name} must be repo-relative, not absolute ({})",
        relative_path.display()
    );
    anyhow::ensure!(
        !relative_path
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir)),
        "{field_name} must be repo-relative, not absolute ({})",
        relative_path.display()
    );
    anyhow::ensure!(
        !relative_path.components().any(|component| matches!(component, Component::ParentDir)),
        "{field_name} must stay within repo root {} (got {})",
        repo_root.display(),
        relative_path.display()
    );

    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("Resolving repo root {}", repo_root.display()))?;
    let candidate = repo_root.join(relative_path);

    if require_exists {
        anyhow::ensure!(
            candidate.exists(),
            "{field_name} points to missing path {}",
            candidate.display()
        );
    }

    let boundary_probe = existing_boundary_probe(&candidate).unwrap_or_else(|| repo_root.clone());
    let canonical_probe = boundary_probe
        .canonicalize()
        .with_context(|| format!("Resolving {}", boundary_probe.display()))?;
    anyhow::ensure!(
        canonical_probe.starts_with(&repo_root),
        "{field_name} must stay within repo root {} (got {})",
        repo_root.display(),
        relative_path.display()
    );

    Ok(candidate)
}

fn existing_boundary_probe(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|ancestor| ancestor.exists()).map(Path::to_path_buf)
}

fn hash_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn bundle_spec(bundle: &str) -> Option<BundleSpec> {
    match bundle {
        "npm-public" => Some(BundleSpec {
            hosts: &["registry.npmjs.org"],
            description: "Public npm package registry access",
        }),
        "pypi-public" => Some(BundleSpec {
            hosts: &["pypi.org", "files.pythonhosted.org"],
            description: "Public Python package registry access",
        }),
        "cargo-public" => Some(BundleSpec {
            hosts: &["crates.io", "index.crates.io", "static.crates.io"],
            description: "Public Cargo registry access",
        }),
        _ => None,
    }
}

fn validate_hostname(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("hostname cannot be empty");
    }
    if value.len() > 253 {
        return Err("hostname is too long");
    }
    if value.contains('/')
        || value.contains(':')
        || value.contains('*')
        || value.chars().any(char::is_whitespace)
    {
        return Err("use an exact hostname only; no scheme, path, port, wildcard, or whitespace");
    }

    for label in value.split('.') {
        if label.is_empty() {
            return Err("hostname labels cannot be empty");
        }
        if label.len() > 63 {
            return Err("hostname label is too long");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("hostname labels cannot start or end with a hyphen");
        }
        if !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("hostname labels must contain only ASCII letters, digits, or hyphens");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn starter_config_is_minimal_safe_mode() {
        assert_eq!(starter_config_toml(), "[network]\nmode = \"safe\"\n");
    }

    #[test]
    fn starter_config_with_profile_adds_environment_section() {
        assert_eq!(
            starter_config_toml_with_profile(Some(EnvironmentProfile::Node)),
            "[network]\nmode = \"safe\"\n\n[environment]\nprofile = \"node\"\n"
        );
    }

    #[test]
    fn hostname_validation_rejects_urls_and_ports() {
        let err = validate_hostname("https://docs.rs").unwrap_err();
        assert!(err.contains("exact hostname"));
        let err = validate_hostname("docs.rs:443").unwrap_err();
        assert!(err.contains("exact hostname"));
    }

    #[test]
    fn safe_mode_rejects_domains() {
        let config = ProjectConfig {
            project: None,
            network: NetworkConfig {
                mode: NetworkMode::Safe,
                bundles: Vec::new(),
                domains: vec!["docs.rs".into()],
            },
            environment: None,
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(format!("{err:#}").contains("cannot be combined"));
    }

    #[test]
    fn scoped_mode_accepts_known_bundle() {
        let config = ProjectConfig {
            project: None,
            network: NetworkConfig {
                mode: NetworkMode::Scoped,
                bundles: vec!["npm-public".into()],
                domains: Vec::new(),
            },
            environment: None,
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        config.validate(Path::new(".")).unwrap();
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let config_path = ProjectConfig::default_path(repo_root);
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
        let resolved = config.resolve(repo_root).unwrap();
        let scope = resolved.effective_network_scope(None).unwrap();
        assert_eq!(scope.mode, NetworkMode::Scoped);
        assert_eq!(scope.bundles, vec!["npm-public"]);
    }

    #[test]
    fn unknown_bundle_is_rejected() {
        let config = ProjectConfig {
            project: None,
            network: NetworkConfig {
                mode: NetworkMode::Scoped,
                bundles: vec!["made-up".into()],
                domains: Vec::new(),
            },
            environment: None,
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(format!("{err:#}").contains("Unknown network bundle"));
    }

    #[test]
    fn explicit_profile_rejects_incompatible_cache_family() {
        let config = ProjectConfig {
            project: None,
            network: NetworkConfig::default(),
            environment: Some(EnvironmentConfig {
                profile: Some(EnvironmentProfile::Node),
                caches: vec!["cargo".into()],
                prepare: None,
                watch: vec![],
                mount_excludes: Vec::new(),
            }),
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(format!("{err:#}").contains("does not support cache family"));
    }

    #[test]
    fn rust_profile_rejects_edition_2024_repo() {
        let temp = tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let config = ProjectConfig {
            project: None,
            network: NetworkConfig::default(),
            environment: Some(EnvironmentConfig {
                profile: Some(EnvironmentProfile::Rust),
                caches: vec!["cargo".into()],
                prepare: None,
                watch: vec![],
                mount_excludes: Vec::new(),
            }),
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let err = config.validate(temp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("edition 2024"));
    }

    #[test]
    fn rust_profile_rejects_lockfile_v4_repo() {
        let temp = tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("Cargo.lock"), "version = 4\n").unwrap();

        let config = ProjectConfig {
            project: None,
            network: NetworkConfig::default(),
            environment: Some(EnvironmentConfig {
                profile: Some(EnvironmentProfile::Rust),
                caches: vec!["cargo".into()],
                prepare: None,
                watch: vec![],
                mount_excludes: Vec::new(),
            }),
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let err = config.validate(temp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("Cargo.lock version 4"));
    }

    #[test]
    fn scoped_without_additions_normalizes_to_safe() {
        let config = ProjectConfig {
            project: None,
            network: NetworkConfig {
                mode: NetworkMode::Scoped,
                bundles: Vec::new(),
                domains: Vec::new(),
            },
            environment: None,
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let config_path = ProjectConfig::default_path(repo_root);
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
        let resolved = config.resolve(repo_root).unwrap();

        assert_eq!(resolved.default_network_mode, NetworkMode::Safe);
        assert!(resolved.notes.iter().any(|note| note.contains("Normalized")));
    }

    #[test]
    fn project_identity_prefers_origin_remote() {
        let temp = tempdir().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        repo.remote("origin", "git@github.com:OpenAI/abox.git").unwrap();

        let config = ProjectConfig::default();
        let config_path = ProjectConfig::default_path(temp.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

        let resolved = config.resolve(temp.path()).unwrap();
        assert_eq!(resolved.project_id, "git@github.com:openai/abox");
    }

    #[test]
    fn short_project_ids_keep_existing_state_path_component_format() {
        let temp = tempdir().unwrap();
        let project_id = "repo";

        let cache_root = project_cache_root(temp.path(), project_id);
        let cache_component =
            cache_root.file_name().and_then(|name| name.to_str()).expect("cache component");
        assert_eq!(cache_component, hash_hex(project_id.as_bytes()));
    }

    #[test]
    fn long_project_ids_use_hashed_state_path_components() {
        let temp = tempdir().unwrap();
        let project_id = format!(
            "ssh://git@github.enterprise.example.com/org/{}/repo-with-extra-depth.git",
            "very-long-segment/".repeat(24)
        );

        let cache_root = project_cache_root(temp.path(), &project_id);
        let env_record = environment_record_path(temp.path(), &project_id);
        let approval_record = approval_record_path(temp.path(), &project_id, "fingerprint");

        let cache_component =
            cache_root.file_name().and_then(|name| name.to_str()).expect("cache component");
        let env_component = env_record
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .expect("env component");
        let approval_component = approval_record
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .expect("approval component");

        assert_eq!(cache_component.len(), 64);
        assert_eq!(env_component.len(), 64);
        assert_eq!(approval_component.len(), 64);
        assert_ne!(cache_component, hash_hex(project_id.as_bytes()));
    }

    #[test]
    fn approval_fingerprint_changes_when_prompt_changes() {
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let prompt_path = repo_root.join(".abox/prompt.md");
        std::fs::create_dir_all(prompt_path.parent().unwrap()).unwrap();
        std::fs::write(&prompt_path, "first prompt\n").unwrap();

        let config = ProjectConfig {
            project: None,
            network: NetworkConfig::default(),
            environment: None,
            agent: Some(AgentConfig {
                default_prompt_file: Some(PathBuf::from(".abox/prompt.md")),
            }),
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };
        let config_path = ProjectConfig::default_path(repo_root);
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

        let first = config.resolve(repo_root).unwrap();
        std::fs::write(&prompt_path, "second prompt\n").unwrap();
        let second = config.resolve(repo_root).unwrap();

        assert_ne!(first.approval_fingerprint, second.approval_fingerprint);
    }

    #[test]
    fn prompt_file_must_stay_within_repo_root() {
        let temp = tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(repo_root.join(".abox")).expect("create .abox");
        std::fs::write(temp.path().join("outside-secret.txt"), "secret\n").expect("write secret");

        let config = ProjectConfig {
            project: None,
            network: NetworkConfig::default(),
            environment: None,
            agent: Some(AgentConfig {
                default_prompt_file: Some(PathBuf::from("../outside-secret.txt")),
            }),
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let err = config.validate(&repo_root).expect_err("prompt path should be rejected");
        assert!(format!("{err:#}").contains("must stay within repo root"));
    }

    #[test]
    fn prepare_script_symlink_cannot_escape_repo_root() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let temp = tempdir().expect("tempdir");
            let repo_root = temp.path().join("repo");
            let outside = temp.path().join("outside-prepare.sh");
            std::fs::create_dir_all(repo_root.join(".abox")).expect("create .abox");
            std::fs::write(&outside, "#!/bin/sh\necho outside\n").expect("write prepare");
            symlink(&outside, repo_root.join(".abox/prepare.sh")).expect("create symlink");

            let config = ProjectConfig {
                project: None,
                network: NetworkConfig::default(),
                environment: Some(EnvironmentConfig {
                    profile: None,
                    caches: vec!["npm".into()],
                    prepare: Some(PathBuf::from(".abox/prepare.sh")),
                    watch: vec![],
                    mount_excludes: Vec::new(),
                }),
                agent: None,
                services: std::collections::HashMap::new(),
                host_ports: vec![],
            };

            let err = config.validate(&repo_root).expect_err("symlink escape should be rejected");
            assert!(format!("{err:#}").contains("must stay within repo root"));
        }
    }

    #[test]
    fn watch_entries_must_stay_within_repo_root() {
        let temp = tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("create repo");

        let config = ProjectConfig {
            project: None,
            network: NetworkConfig::default(),
            environment: Some(EnvironmentConfig {
                profile: None,
                caches: vec!["npm".into()],
                prepare: None,
                watch: vec![PathBuf::from("../outside-lock.json")],
                mount_excludes: Vec::new(),
            }),
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };

        let err = config.validate(&repo_root).expect_err("watch escape should be rejected");
        assert!(format!("{err:#}").contains("must stay within repo root"));
    }

    #[test]
    fn environment_fingerprint_changes_when_watch_file_changes() {
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let prepare_path = repo_root.join(".abox/prepare.sh");
        let watch_path = repo_root.join("package-lock.json");
        std::fs::create_dir_all(prepare_path.parent().unwrap()).unwrap();
        std::fs::write(&prepare_path, "#!/bin/sh\necho preparing\n").unwrap();
        std::fs::write(&watch_path, "{ \"lock\": 1 }\n").unwrap();

        let config = ProjectConfig {
            project: None,
            network: NetworkConfig::default(),
            environment: Some(EnvironmentConfig {
                profile: None,
                caches: vec!["npm".into()],
                prepare: Some(PathBuf::from(".abox/prepare.sh")),
                watch: vec![PathBuf::from("package-lock.json")],
                mount_excludes: Vec::new(),
            }),
            agent: None,
            services: std::collections::HashMap::new(),
            host_ports: vec![],
        };
        let config_path = ProjectConfig::default_path(repo_root);
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

        let first = config.resolve(repo_root).unwrap();
        let first_fingerprint = first.environment_fingerprint(repo_root, "rootfs-token").unwrap();

        std::fs::write(&watch_path, "{ \"lock\": 2 }\n").unwrap();
        let second = config.resolve(repo_root).unwrap();
        let second_fingerprint = second.environment_fingerprint(repo_root, "rootfs-token").unwrap();

        assert_ne!(first_fingerprint, second_fingerprint);
    }

    #[test]
    fn cache_env_vars_include_expected_paths() {
        let resolved = ResolvedProjectConfig {
            config_path: PathBuf::from(".abox/project.toml"),
            project_id: "repo".into(),
            default_network_mode: NetworkMode::Safe,
            has_host_ports: false,
            bundles: vec![],
            domains: vec![],
            environment_profile: EnvironmentProfile::Base,
            caches: vec!["npm".into(), "cargo".into()],
            prepare_path: None,
            prepare_bytes: None,
            watch_paths: vec![],
            default_prompt_path: None,
            default_prompt_bytes: None,
            mount_excludes: vec![],
            notes: vec![],
            approval_fingerprint: "abc".into(),
        };

        let vars = resolved.cache_env_vars();
        assert!(vars.contains(&("NPM_CONFIG_CACHE".into(), "/abox-cache/npm".into())));
        assert!(vars.contains(&("npm_config_cache".into(), "/abox-cache/npm".into())));
        assert!(vars.contains(&("CARGO_HOME".into(), "/abox-cache/cargo".into())));
    }

    #[test]
    fn rootfs_token_prefers_inputs_stamp_over_mutable_image_mtime() {
        let temp = tempdir().unwrap();
        let vm_dir = temp.path().join("vm");
        std::fs::create_dir_all(&vm_dir).unwrap();
        let image_path = vm_dir.join("rootfs.raw");
        let inputs_path = vm_dir.join("rootfs.raw.inputs");
        std::fs::write(&image_path, b"rootfs-bytes").unwrap();
        std::fs::write(&inputs_path, b"stable-inputs").unwrap();

        let config = crate::config::AboxConfig {
            state_dir: temp.path().to_path_buf(),
            vm_defaults: crate::config::VmDefaults {
                memory_mib: 2048,
                vcpus: 2,
                image_path: Some(image_path.clone()),
                kernel_path: None,
            },
            ..Default::default()
        };

        let first = rootfs_token(&config).unwrap();
        std::fs::write(&image_path, b"rootfs-bytes-mutated-at-runtime").unwrap();
        let second = rootfs_token(&config).unwrap();

        assert_eq!(first, second);
        assert!(first.contains(":inputs:"));
    }

    #[test]
    fn profile_image_path_uses_profiles_subdir_for_non_base_profiles() {
        let temp = tempdir().unwrap();
        let config = crate::config::AboxConfig {
            state_dir: temp.path().to_path_buf(),
            ..Default::default()
        };

        assert_eq!(
            image_path_for_profile(&config, EnvironmentProfile::Python),
            temp.path().join("vm/profiles/python/rootfs.raw")
        );
    }

    #[test]
    fn parses_host_ports_section() {
        let toml = r"
[[host_ports]]
guest = 4000
host = 4000
";
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.host_ports.len(), 1);
        assert_eq!(cfg.host_ports[0].guest, 4000);
        assert_eq!(cfg.host_ports[0].host, 4000);
    }

    #[test]
    fn rejects_duplicate_host_port_guest() {
        let toml = r"
[[host_ports]]
guest = 4000
host = 4000

[[host_ports]]
guest = 4000
host = 5000
";
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        let err = cfg.validate(std::path::Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("4000"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_host_port_zero() {
        let toml = r"
[[host_ports]]
guest = 0
host = 4000
";
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        assert!(cfg.validate(std::path::Path::new(".")).is_err());
    }

    #[test]
    fn scoped_with_only_host_ports_stays_scoped() {
        // A scoped config whose only addition is a host-port bridge must NOT be
        // normalized to safe, and must resolve to an effective scoped scope
        // (otherwise the gated, version-controlled bridge would be unusable).
        let toml = r"
[network]
mode = 'scoped'

[[host_ports]]
guest = 4000
host = 4000
";
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        let tmp = tempdir().unwrap();
        let cfg_path = ProjectConfig::default_path(tmp.path());
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, toml).unwrap();

        let resolved = cfg.resolve(tmp.path()).unwrap();
        assert_eq!(resolved.default_network_mode, NetworkMode::Scoped);
        assert!(resolved.has_host_ports);
        let scope = resolved.effective_network_scope(None).unwrap();
        assert_eq!(scope.mode, NetworkMode::Scoped);
    }

    #[test]
    fn scoped_without_additions_still_normalizes_to_safe() {
        let toml = r"
[network]
mode = 'scoped'
";
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        let tmp = tempdir().unwrap();
        let cfg_path = ProjectConfig::default_path(tmp.path());
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, toml).unwrap();

        let resolved = cfg.resolve(tmp.path()).unwrap();
        assert_eq!(resolved.default_network_mode, NetworkMode::Safe);
    }

    #[test]
    fn accepts_distinct_host_port_guests() {
        let toml = r"
[[host_ports]]
guest = 4000
host = 4000

[[host_ports]]
guest = 4001
host = 5000
";
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        cfg.validate(std::path::Path::new(".")).unwrap();
    }

    #[test]
    fn approval_records_round_trip() {
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let config = ProjectConfig::default();
        let config_path = ProjectConfig::default_path(repo_root);
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

        let resolved = config.resolve(repo_root).unwrap();
        assert!(!is_approved(repo_root, &resolved));

        let record_path = record_approval(repo_root, &resolved).unwrap();
        assert!(record_path.exists());
        assert!(is_approved(repo_root, &resolved));

        let record = load_approval_record(repo_root, &resolved).unwrap().unwrap();
        assert_eq!(record.project_id, resolved.project_id);
        assert_eq!(record.approval_fingerprint, resolved.approval_fingerprint);
    }
}
