//! Ephemeral service sidecar management.
//!
//! Inspired by Moat's service dependencies feature, this module allows
//! users to declare ephemeral services (PostgreSQL, Redis, Ollama) in
//! `.abox/project.toml` that are started alongside the sandbox VM.
//!
//! # Architecture
//!
//! Services run as Docker containers on the host, connected to the sandbox
//! VM via port forwarding. The VM's init script sets up socat bridges to
//! forward specific ports from the guest to the host Docker containers.
//!
//! # Lifecycle
//!
//! 1. Services are started before the VM boots.
//! 2. Connection URLs are injected as environment variables into the guest.
//! 3. Services are stopped and removed when the sandbox exits.
//!
//! # Supported Services
//!
//! - `postgres` — PostgreSQL database
//! - `redis` — Redis key-value store
//! - `ollama` — Local LLM inference (Ollama)
//!
//! # Configuration
//!
//! ```toml
//! [services]
//! postgres = { version = "17" }
//! redis = { version = "7" }
//! ollama = { models = ["qwen2.5-coder:7b"] }
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;

/// Configuration for a single service sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceConfig {
    /// Service version (e.g., "17" for postgres, "7" for redis).
    #[serde(default)]
    pub version: Option<String>,

    /// For Ollama: list of models to pre-pull at startup.
    #[serde(default)]
    pub models: Vec<String>,

    /// Additional environment variables to pass to the service container.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Whether to wait for the service to be ready before starting the agent.
    /// Default: true.
    #[serde(default = "default_true")]
    pub wait: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self { version: None, models: Vec::new(), env: HashMap::new(), wait: true }
    }
}

/// A running service sidecar instance.
#[derive(Debug, Clone)]
pub struct RunningService {
    /// Service name (e.g., "postgres", "redis").
    pub name: String,
    /// Docker container ID.
    pub container_id: String,
    /// Host port the service is exposed on.
    pub host_port: u16,
    /// Connection URL for the agent.
    pub connection_url: String,
    /// Environment variable name for the connection URL.
    pub env_var: String,
}

/// Built-in service definitions.
#[derive(Debug, Clone)]
pub struct ServiceDef {
    pub name: &'static str,
    pub default_version: &'static str,
    pub image_template: &'static str, // {version} is replaced
    pub default_port: u16,
    pub env_vars: &'static [(&'static str, &'static str)], // (name, value_template)
    pub connection_url_template: &'static str, // {host}, {port}, {password}
    pub env_var_name: &'static str,
    pub readiness_command: &'static [&'static str],
    pub description: &'static str,
}

pub const SERVICE_DEFS: &[ServiceDef] = &[
    ServiceDef {
        name: "postgres",
        default_version: "17",
        image_template: "postgres:{version}-alpine",
        default_port: 5432,
        env_vars: &[
            ("POSTGRES_PASSWORD", "{password}"),
            ("POSTGRES_USER", "abox"),
            ("POSTGRES_DB", "abox"),
        ],
        connection_url_template: "postgresql://abox:{password}@127.0.0.1:{port}/abox",
        env_var_name: "ABOX_POSTGRES_URL",
        readiness_command: &["pg_isready", "-h", "localhost", "-U", "abox"],
        description: "PostgreSQL relational database",
    },
    ServiceDef {
        name: "redis",
        default_version: "7",
        image_template: "redis:{version}-alpine",
        default_port: 6379,
        env_vars: &[],
        connection_url_template: "redis://127.0.0.1:{port}",
        env_var_name: "ABOX_REDIS_URL",
        readiness_command: &["redis-cli", "PING"],
        description: "Redis key-value store",
    },
    ServiceDef {
        name: "ollama",
        default_version: "latest",
        image_template: "ollama/ollama:{version}",
        default_port: 11434,
        env_vars: &[],
        connection_url_template: "http://127.0.0.1:{port}",
        env_var_name: "ABOX_OLLAMA_URL",
        readiness_command: &["ollama", "list"],
        description: "Local LLM inference (Ollama)",
    },
    ServiceDef {
        name: "mysql",
        default_version: "8",
        image_template: "mysql:{version}",
        default_port: 3306,
        env_vars: &[
            ("MYSQL_ROOT_PASSWORD", "{password}"),
            ("MYSQL_DATABASE", "abox"),
            ("MYSQL_USER", "abox"),
            ("MYSQL_PASSWORD", "{password}"),
        ],
        connection_url_template: "mysql://abox:{password}@127.0.0.1:{port}/abox",
        env_var_name: "ABOX_MYSQL_URL",
        readiness_command: &["mysqladmin", "ping", "-h", "localhost"],
        description: "MySQL relational database",
    },
];

/// Find a service definition by name.
pub fn find_service_def(name: &str) -> Option<&'static ServiceDef> {
    SERVICE_DEFS.iter().find(|s| s.name == name)
}

/// Generate a random alphanumeric password.
pub fn generate_password() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(42);
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    (0..32)
        .map(|i| {
            let idx = (seed.wrapping_add(i * 7919) as usize) % chars.len();
            chars[idx]
        })
        .collect()
}

/// Check if Docker is available on the host.
pub fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Find a free port on the host.
pub fn find_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Start a service sidecar container.
///
/// Returns the running service info including the connection URL.
pub fn start_service(
    service_name: &str,
    config: &ServiceConfig,
    sandbox_id: &str,
) -> Result<RunningService> {
    let def = find_service_def(service_name)
        .ok_or_else(|| anyhow::anyhow!("Unknown service: {service_name}"))?;

    let version = config.version.as_deref().unwrap_or(def.default_version);
    let image = def.image_template.replace("{version}", version);
    let host_port = find_free_port()?;
    let password = generate_password();

    let container_name = format!("abox-{service_name}-{sandbox_id}");

    let mut cmd = std::process::Command::new("docker");
    cmd.arg("run")
        .arg("--detach")
        .arg("--rm")
        .arg("--name")
        .arg(&container_name)
        .arg("--publish")
        .arg(format!("{host_port}:{}", def.default_port))
        .arg("--label")
        .arg(format!("abox.sandbox-id={sandbox_id}"))
        .arg("--label")
        .arg("abox.role=service");

    // Add service-specific environment variables
    for (key, value_template) in def.env_vars {
        let value = value_template.replace("{password}", &password);
        cmd.arg("--env").arg(format!("{key}={value}"));
    }

    // Add user-specified environment variables
    for (key, value) in &config.env {
        cmd.arg("--env").arg(format!("{key}={value}"));
    }

    cmd.arg(&image);

    let output = cmd
        .output()
        .with_context(|| format!("Failed to start {service_name} container"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Failed to start {service_name} container: {stderr}\n\
             Make sure Docker is running and the image '{image}' is available."
        );
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let connection_url = def
        .connection_url_template
        .replace("{password}", &password)
        .replace("{port}", &host_port.to_string())
        .replace("{host}", "127.0.0.1");

    Ok(RunningService {
        name: service_name.to_string(),
        container_id,
        host_port,
        connection_url,
        env_var: def.env_var_name.to_string(),
    })
}

/// Wait for a service to be ready by polling its readiness command.
pub fn wait_for_service_ready(
    service_name: &str,
    container_name: &str,
    timeout_secs: u64,
) -> Result<()> {
    let def = find_service_def(service_name)
        .ok_or_else(|| anyhow::anyhow!("Unknown service: {service_name}"))?;

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);

    loop {
        if start.elapsed() > timeout {
            anyhow::bail!(
                "Timed out waiting for {service_name} to be ready after {timeout_secs}s.\n\
                 Check container logs with: docker logs {container_name}"
            );
        }

        let mut cmd = std::process::Command::new("docker");
        cmd.arg("exec")
            .arg(container_name)
            .args(def.readiness_command)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if cmd.status().map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }

        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Stop and remove a service container.
pub fn stop_service(container_id: &str) -> Result<()> {
    let status = std::process::Command::new("docker")
        .arg("stop")
        .arg(container_id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Failed to stop container {container_id}"))?;

    if !status.success() {
        // Container may have already stopped — not a fatal error
        tracing::warn!(container_id, "Failed to stop service container (may already be stopped)");
    }

    Ok(())
}

/// Stop all service containers for a sandbox.
pub fn stop_sandbox_services(sandbox_id: &str) -> Result<()> {
    // Find all containers with the sandbox label
    let output = std::process::Command::new("docker")
        .arg("ps")
        .arg("--filter")
        .arg(format!("label=abox.sandbox-id={sandbox_id}"))
        .arg("--filter")
        .arg("label=abox.role=service")
        .arg("--format")
        .arg("{{.ID}}")
        .output()
        .context("Failed to list service containers")?;

    let container_ids = String::from_utf8_lossy(&output.stdout);
    for id in container_ids.lines() {
        let id = id.trim();
        if !id.is_empty() {
            if let Err(e) = stop_service(id) {
                tracing::warn!(container_id = id, error = %e, "Failed to stop service container");
            }
        }
    }

    Ok(())
}

/// Pull Ollama models after the service starts.
pub fn pull_ollama_models(container_name: &str, models: &[String]) -> Result<()> {
    for model in models {
        println!("  Pulling Ollama model: {model}...");
        let status = std::process::Command::new("docker")
            .arg("exec")
            .arg(container_name)
            .arg("ollama")
            .arg("pull")
            .arg(model)
            .status()
            .with_context(|| format!("Failed to pull Ollama model {model}"))?;

        if !status.success() {
            anyhow::bail!("Failed to pull Ollama model '{model}'");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_service_def() {
        assert!(find_service_def("postgres").is_some());
        assert!(find_service_def("redis").is_some());
        assert!(find_service_def("ollama").is_some());
        assert!(find_service_def("unknown").is_none());
    }

    #[test]
    fn test_generate_password_length() {
        let pw = generate_password();
        assert_eq!(pw.len(), 32);
        assert!(pw.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_service_def_connection_url() {
        let def = find_service_def("postgres").unwrap();
        let url = def
            .connection_url_template
            .replace("{password}", "testpass")
            .replace("{port}", "5432")
            .replace("{host}", "127.0.0.1");
        assert!(url.contains("testpass"));
        assert!(url.contains("5432"));
        assert!(url.starts_with("postgresql://"));
    }

    #[test]
    fn test_service_config_default_wait() {
        let config = ServiceConfig::default();
        assert!(config.wait);
    }
}
