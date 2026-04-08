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
}

impl VmRuntimeTuning {
    /// Default tuning: 250 ms VM exit poll, 50 ms console poll,
    /// 5 s socket wait.
    pub const DEFAULT: Self = Self {
        vm_exit_poll_interval: std::time::Duration::from_millis(250),
        console_poll_interval: std::time::Duration::from_millis(50),
        socket_wait_timeout: std::time::Duration::from_secs(5),
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

    /// Return the runtime directory for sockets and PIDs.
    ///
    /// If `runtime_dir` is set in config, use it. Otherwise default to
    /// `<state_dir>/run`, which is always writable for the current user.
    pub fn runtime_dir(&self) -> PathBuf {
        self.runtime_dir.clone().unwrap_or_else(|| self.state_dir.join("run"))
    }

    /// Ensure all required directories exist.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.worktrees_dir())?;
        std::fs::create_dir_all(self.templates_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.runtime_dir())?;
        std::fs::create_dir_all(&self.proxy.policy_dir)?;
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
