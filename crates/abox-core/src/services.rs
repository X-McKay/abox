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
use std::path::PathBuf;
use std::process::Stdio;

/// Base guest vsock port for service bridges.
///
/// Ports 5000 (CLI proxy) and 5001 (egress proxy) are already in use, so
/// service bridges start at 5100. Each service gets `BASE + index`.
pub const SERVICE_VSOCK_BASE: u32 = 5100;

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

/// A planned host↔guest bridge for one running service.
///
/// The host runs the service as a Docker container published on
/// `127.0.0.1:host_port`. Inside the microVM, `127.0.0.1` is the guest's own
/// loopback, so the container is unreachable directly. To connect them, the
/// orchestrator binds a host listener at `vsock-<id>.sock_<vsock_port>` (Cloud
/// Hypervisor routes the guest's vsock port `vsock_port` there) that forwards
/// to `127.0.0.1:host_port`, and the guest's init script runs
/// `socat TCP-LISTEN:<guest_port> VSOCK-CONNECT:2:<vsock_port>`. The connection
/// URL handed to the agent therefore points at `127.0.0.1:<guest_port>`.
#[derive(Debug, Clone)]
pub struct ServiceBridge {
    pub name: String,
    /// Docker-published port on the host loopback.
    pub host_port: u16,
    /// Port the guest listens on (and that appears in the connection URL).
    pub guest_port: u16,
    /// vsock port tunneling host↔guest for this service.
    pub vsock_port: u32,
    /// Environment variable carrying the connection URL.
    pub env_var: String,
    /// Connection URL rewritten to use the guest port.
    pub guest_url: String,
    /// Docker container ID, for teardown.
    pub container_id: String,
}

/// The guest-visible subset of a [`ServiceBridge`], staged into boot metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuestServiceBridge {
    pub name: String,
    pub guest_port: u16,
    pub vsock_port: u32,
}

/// A repo-declared bridge from a guest loopback port to an existing host
/// loopback service. Unlike `[services]` sidecars, abox launches nothing —
/// it splices the guest port to a port the operator already runs on the host.
///
/// This is an explicit hole in the egress boundary: it is refused in `safe`
/// network mode and every connection through it is written to the audit log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostPortBridge {
    /// Port the agent connects to inside the guest (`127.0.0.1:<guest>`).
    pub guest: u16,
    /// Existing host loopback port to splice to (`127.0.0.1:<host>`).
    pub host: u16,
}

impl ServiceBridge {
    /// Project the host-side bridge to the guest-visible subset.
    pub fn guest(&self) -> GuestServiceBridge {
        GuestServiceBridge {
            name: self.name.clone(),
            guest_port: self.guest_port,
            vsock_port: self.vsock_port,
        }
    }
}

/// Plan a [`ServiceBridge`] for a running service at a given bridge index.
///
/// The guest port defaults to the service's well-known port (e.g. 5432 for
/// Postgres) so connection URLs look conventional; the vsock port is allocated
/// from [`SERVICE_VSOCK_BASE`]. The connection URL is rewritten from the random
/// host port to the stable guest port.
pub fn plan_service_bridge(running: &RunningService, index: usize) -> ServiceBridge {
    let guest_port = find_service_def(&running.name).map_or(running.host_port, |d| d.default_port);
    let vsock_port = SERVICE_VSOCK_BASE + index as u32;
    let guest_url = running
        .connection_url
        .replace(&format!("127.0.0.1:{}", running.host_port), &format!("127.0.0.1:{guest_port}"));
    ServiceBridge {
        name: running.name.clone(),
        host_port: running.host_port,
        guest_port,
        vsock_port,
        env_var: running.env_var.clone(),
        guest_url,
        container_id: running.container_id.clone(),
    }
}

/// Run a host-side bridge: accept connections on a Unix socket (the Cloud
/// Hypervisor vsock backend path) and splice each to `127.0.0.1:host_port`.
///
/// Loops until the listener errors or the task is aborted (on sandbox
/// teardown). Each accepted connection is handled on its own task.
pub async fn serve_service_bridge(socket_path: PathBuf, host_port: u16) -> Result<()> {
    use tokio::net::{TcpStream, UnixListener};

    // Cloud Hypervisor creates the `_<port>` socket lazily; remove any stale
    // one from a previous run before binding.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Binding service bridge socket {}", socket_path.display()))?;
    tracing::info!(socket = %socket_path.display(), host_port, "Service bridge listening");

    loop {
        let (mut guest, _) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "Service bridge accept error");
                // Back off so a persistent error (e.g. EMFILE) doesn't spin the CPU.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        tokio::spawn(async move {
            match TcpStream::connect(("127.0.0.1", host_port)).await {
                Ok(mut upstream) => {
                    if let Err(e) = tokio::io::copy_bidirectional(&mut guest, &mut upstream).await {
                        tracing::debug!(error = %e, host_port, "Service bridge copy ended");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, host_port, "Service bridge could not reach container");
                }
            }
        });
    }
}

/// A planned host-port bridge with its allocated vsock port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPortPlan {
    pub guest_port: u16,
    pub host_port: u16,
    pub vsock_port: u32,
}

impl HostPortPlan {
    /// Project to the guest-visible bridge line written into `/abox-meta/services`.
    pub fn guest(&self) -> GuestServiceBridge {
        GuestServiceBridge {
            name: format!("hostport-{}", self.host_port),
            guest_port: self.guest_port,
            vsock_port: self.vsock_port,
        }
    }
}

/// Plan host-port bridges, allocating vsock ports immediately after the
/// `service_count` sidecar bridges so the two ranges never collide.
pub fn plan_host_port_bridges(
    bridges: &[HostPortBridge],
    service_count: usize,
) -> Vec<HostPortPlan> {
    bridges
        .iter()
        .enumerate()
        .map(|(i, b)| HostPortPlan {
            guest_port: b.guest,
            host_port: b.host,
            vsock_port: SERVICE_VSOCK_BASE + (service_count + i) as u32,
        })
        .collect()
}

/// Like [`serve_service_bridge`], but for an operator-declared host port:
/// logs an audit entry at setup and on every accepted connection, since this
/// bypasses the egress proxy and is a deliberate boundary exception.
pub async fn serve_host_port_bridge(
    socket_path: PathBuf,
    guest_port: u16,
    host_port: u16,
    sandbox_id: String,
    audit: std::sync::Arc<dyn crate::proxy_bridge::AuditSink>,
) -> Result<()> {
    use tokio::net::{TcpStream, UnixListener};

    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Binding host-port bridge socket {}", socket_path.display()))?;
    audit.log_host_port(&sandbox_id, "host-port-bridge", guest_port, host_port);
    tracing::info!(socket = %socket_path.display(), guest_port, host_port, "Host-port bridge listening");

    loop {
        let (mut guest, _) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "Host-port bridge accept error");
                // Back off so a persistent error (e.g. EMFILE) doesn't spin the CPU.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        audit.log_host_port(&sandbox_id, "host-port-connect", guest_port, host_port);
        tokio::spawn(async move {
            match TcpStream::connect(("127.0.0.1", host_port)).await {
                Ok(mut upstream) => {
                    if let Err(e) = tokio::io::copy_bidirectional(&mut guest, &mut upstream).await {
                        tracing::debug!(error = %e, host_port, "Host-port bridge copy ended");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, host_port, "Host-port bridge could not reach host service");
                }
            }
        });
    }
}

/// Built-in service definitions.
#[derive(Debug, Clone)]
pub struct ServiceDef {
    pub name: &'static str,
    pub default_version: &'static str,
    pub image_template: &'static str, // {version} is replaced
    pub default_port: u16,
    pub env_vars: &'static [(&'static str, &'static str)], // (name, value_template)
    pub connection_url_template: &'static str,             // {host}, {port}, {password}
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

/// Generate a cryptographically random alphanumeric password.
///
/// Uses the OS CSPRNG. Returns an error rather than falling back to a weak
/// source — a predictable database password would be a real (if narrow)
/// vulnerability. To avoid modulo bias we draw extra bytes and reject those in
/// the biased tail of the byte range.
pub fn generate_password() -> Result<String> {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    const TARGET_LEN: usize = 32;
    // Largest multiple of CHARS.len() that fits in a byte; bytes >= this are
    // rejected to keep the distribution uniform.
    let limit = (256 / CHARS.len()) * CHARS.len();
    let mut out = String::with_capacity(TARGET_LEN);
    let mut buf = [0u8; 64];
    while out.len() < TARGET_LEN {
        crate::util::secure_random_bytes(&mut buf)
            .map_err(|e| anyhow::anyhow!("Cannot generate service password: {e}"))?;
        for &b in &buf {
            if (b as usize) < limit {
                out.push(CHARS[b as usize % CHARS.len()] as char);
                if out.len() == TARGET_LEN {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Check if Docker is available on the host.
pub fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Find a free port on the host.
pub fn find_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Build the Docker `--publish` argument binding the host port to loopback
/// only.
///
/// The guest reaches the service through a vsock->`127.0.0.1` splice (see the
/// module docs), so the published port must never be exposed on the host's
/// external interfaces. A bare `host:container` spec makes Docker bind
/// `0.0.0.0`, which would leak the (password-protected) sidecar to the host
/// LAN; the explicit `127.0.0.1:` prefix keeps it on loopback.
fn loopback_publish_spec(host_port: u16, container_port: u16) -> String {
    format!("127.0.0.1:{host_port}:{container_port}")
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
    let password = generate_password()?;

    let container_name = format!("abox-{service_name}-{sandbox_id}");

    let mut cmd = std::process::Command::new("docker");
    cmd.arg("run")
        .arg("--detach")
        .arg("--rm")
        .arg("--name")
        .arg(&container_name)
        .arg("--publish")
        .arg(loopback_publish_spec(host_port, def.default_port))
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

    let output =
        cmd.output().with_context(|| format!("Failed to start {service_name} container"))?;

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

        // Detect a container that died during startup instead of polling a
        // dead container until the full timeout elapses.
        if !container_is_running(container_name) {
            anyhow::bail!(
                "{service_name} container '{container_name}' exited during startup.\n\
                 Check logs with: docker logs {container_name}"
            );
        }

        let mut cmd = std::process::Command::new("docker");
        cmd.arg("exec")
            .arg(container_name)
            .args(def.readiness_command)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if cmd.status().is_ok_and(|s| s.success()) {
            return Ok(());
        }

        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Return whether a container is currently in the `running` state.
fn container_is_running(container_name: &str) -> bool {
    std::process::Command::new("docker")
        .arg("inspect")
        .arg("--format")
        .arg("{{.State.Running}}")
        .arg(container_name)
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// Stop and remove a service container.
///
/// Uses `docker rm --force`, which stops a running container and removes it in
/// one step. This also reaps a container that has already exited but lingers
/// (e.g. if its `--rm` auto-removal did not fire), avoiding orphans.
pub fn stop_service(container_id: &str) -> Result<()> {
    let status = std::process::Command::new("docker")
        .arg("rm")
        .arg("--force")
        .arg(container_id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Failed to remove container {container_id}"))?;

    if !status.success() {
        // Container may have already been removed — not a fatal error.
        tracing::warn!(container_id, "Failed to remove service container (may already be gone)");
    }

    Ok(())
}

/// Stop all service containers for a sandbox.
pub fn stop_sandbox_services(sandbox_id: &str) -> Result<()> {
    // Find all containers with the sandbox label. `--all` so that a container
    // which has already exited (e.g. crashed) is still found and force-removed,
    // rather than being left behind because `docker ps` only lists running ones.
    let output = std::process::Command::new("docker")
        .arg("ps")
        .arg("--all")
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
    fn test_loopback_publish_spec_binds_loopback_only() {
        // Must always bind 127.0.0.1 so the sidecar is never reachable from the
        // host LAN. A bare "host:container" spec would let Docker bind 0.0.0.0.
        let spec = loopback_publish_spec(49201, 5432);
        assert_eq!(spec, "127.0.0.1:49201:5432");
        assert!(spec.starts_with("127.0.0.1:"));
    }

    #[test]
    fn test_find_service_def() {
        assert!(find_service_def("postgres").is_some());
        assert!(find_service_def("redis").is_some());
        assert!(find_service_def("ollama").is_some());
        assert!(find_service_def("unknown").is_none());
    }

    #[test]
    fn test_generate_password_length() {
        let pw = generate_password().unwrap();
        assert_eq!(pw.len(), 32);
        assert!(pw.chars().all(char::is_alphanumeric));
    }

    #[test]
    fn test_generate_password_is_random() {
        // Two passwords must differ; a constant would indicate a broken RNG.
        assert_ne!(generate_password().unwrap(), generate_password().unwrap());
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

    #[test]
    fn test_plan_service_bridge_rewrites_url_to_guest_port() {
        let running = RunningService {
            name: "postgres".into(),
            container_id: "abc123".into(),
            host_port: 49201,
            connection_url: "postgresql://abox:pw@127.0.0.1:49201/abox".into(),
            env_var: "ABOX_POSTGRES_URL".into(),
        };
        let bridge = plan_service_bridge(&running, 0);
        assert_eq!(bridge.guest_port, 5432); // postgres default
        assert_eq!(bridge.vsock_port, SERVICE_VSOCK_BASE);
        assert_eq!(bridge.guest_url, "postgresql://abox:pw@127.0.0.1:5432/abox");
        assert_eq!(bridge.guest().vsock_port, SERVICE_VSOCK_BASE);
    }

    #[test]
    fn host_port_plans_allocate_vsock_after_services() {
        let cfg = vec![
            HostPortBridge { guest: 4000, host: 4000 },
            HostPortBridge { guest: 8080, host: 9000 },
        ];
        // Two sidecar services already occupy SERVICE_VSOCK_BASE + 0..=1.
        let plans = plan_host_port_bridges(&cfg, 2);
        assert_eq!(plans[0].vsock_port, SERVICE_VSOCK_BASE + 2);
        assert_eq!(plans[1].vsock_port, SERVICE_VSOCK_BASE + 3);
        assert_eq!(plans[0].guest().name, "hostport-4000");
        assert_eq!(plans[1].guest().guest_port, 8080);
    }

    #[tokio::test]
    async fn test_serve_service_bridge_forwards_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // A fake "container": a TCP echo server on the host loopback.
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = upstream.accept().await {
                let mut buf = [0u8; 5];
                let _ = s.read_exact(&mut buf).await;
                let _ = s.write_all(&buf).await; // echo
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("vsock-test.sock_5100");
        let sock2 = sock.clone();
        let bridge = tokio::spawn(async move {
            let _ = serve_service_bridge(sock2, host_port).await;
        });

        // Give the bridge a moment to bind.
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut out = [0u8; 5];
        client.read_exact(&mut out).await.unwrap();
        assert_eq!(&out, b"hello");
        bridge.abort();
    }

    #[tokio::test]
    async fn host_port_bridge_audits_each_connection() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // A fake host service: a TCP echo server on the host loopback.
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = upstream.accept().await {
                let mut buf = [0u8; 5];
                let _ = s.read_exact(&mut buf).await;
                let _ = s.write_all(&buf).await; // echo
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("vsock-test.sock_5200");
        let audit_path = dir.path().join("audit.jsonl");
        let audit: std::sync::Arc<dyn crate::proxy_bridge::AuditSink> =
            std::sync::Arc::new(crate::proxy_bridge::FileAuditSink::open(&audit_path).unwrap());

        let sock2 = sock.clone();
        let bridge = tokio::spawn(async move {
            let _ = serve_host_port_bridge(sock2, 4000, host_port, "audit-task".to_string(), audit)
                .await;
        });

        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut out = [0u8; 5];
        client.read_exact(&mut out).await.unwrap();
        assert_eq!(&out, b"hello");

        // The connect event is appended synchronously on accept (before
        // forwarding); a short pause covers filesystem flush on slow CI.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        bridge.abort();

        let log = std::fs::read_to_string(&audit_path).unwrap();
        assert!(log.contains("host-port-bridge"), "setup event missing:\n{log}");
        assert!(log.contains("host-port-connect"), "per-connection event missing:\n{log}");
        assert!(log.contains("guest:4000->host:"), "port mapping missing:\n{log}");
    }
}
