//! CLI proxy handler.
//!
//! Listens on a Unix socket (bridged from VSock) for JSON requests from
//! `abox-shim` inside the VM. Each request specifies a command and arguments.
//! The handler evaluates the request against the policy engine, and if allowed,
//! executes the real command on the host and returns the result.
//!
//! # Protocol
//!
//! Uses [`abox_core::protocol::ProxyRequest`] and [`abox_core::protocol::ProxyResponse`]
//! for the JSON-over-Unix-socket wire format.

use crate::audit::AuditLog;
use abox_core::policy::{Decision, PolicyEngine};
use abox_core::protocol::{ProxyRequest, ProxyResponse};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// The CLI proxy server. Owns the socket path, policy engine, and audit log.
pub struct CliProxyServer {
    socket_path: PathBuf,
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLog>,
}

impl CliProxyServer {
    pub fn new(socket_path: PathBuf, policy: Arc<PolicyEngine>, audit: Arc<AuditLog>) -> Self {
        Self { socket_path, policy, audit }
    }

    /// Start listening for connections. Runs until cancelled.
    pub async fn run(&self) -> Result<()> {
        // Clean up stale socket from a previous run
        let _ = std::fs::remove_file(&self.socket_path);

        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        tracing::info!(socket = %self.socket_path.display(), "CLI proxy listening");

        loop {
            let (stream, _) = listener.accept().await?;
            let policy = Arc::clone(&self.policy);
            let audit = Arc::clone(&self.audit);

            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, &policy, &audit).await {
                    tracing::error!(error = %e, "CLI proxy connection error");
                }
            });
        }
    }
}

/// Handle a single connection from the guest shim.
async fn handle_connection(
    stream: UnixStream,
    policy: &PolicyEngine,
    audit: &AuditLog,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Read exactly one JSON line
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.trim().is_empty() {
        return Ok(());
    }

    let request: ProxyRequest = serde_json::from_str(line.trim())?;
    tracing::debug!(command = %request.command, args = ?request.args, "CLI proxy request");

    // Evaluate policy and build response
    let decision = policy.evaluate_cli(&request.command, &request.args);
    let response = match &decision {
        Decision::Allow => execute_and_respond(&request, audit).await,
        Decision::Deny(reason) => {
            audit.log_cli("unknown", &request.command, &request.args, "denied", 126);
            tracing::warn!(command = %request.command, reason = %reason, "CLI request denied");
            ProxyResponse::denied(reason)
        }
    };

    // Send response as a single JSON line
    let json = serde_json::to_string(&response)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;

    Ok(())
}

/// Execute an allowed command on the host and build a response.
async fn execute_and_respond(request: &ProxyRequest, audit: &AuditLog) -> ProxyResponse {
    match execute_command(request).await {
        Ok(response) => {
            audit.log_cli(
                "unknown",
                &request.command,
                &request.args,
                "allowed",
                response.exit_code,
            );
            response
        }
        Err(e) => {
            audit.log_cli("unknown", &request.command, &request.args, "error", -1);
            tracing::error!(command = %request.command, error = %e, "Command execution failed");
            ProxyResponse::from_exit(1, String::new(), format!("execution failed: {e}"))
        }
    }
}

/// Execute a command on the host and capture its output.
async fn execute_command(request: &ProxyRequest) -> Result<ProxyResponse> {
    let output = tokio::process::Command::new(&request.command)
        .args(&request.args)
        .current_dir(resolve_cwd(&request.cwd))
        .output()
        .await?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(ProxyResponse::from_exit(exit_code, stdout, stderr))
}

/// Resolve the guest CWD to a host path.
///
/// The guest's `/workspace` is mounted via virtiofs from the host's worktree
/// directory. In a full implementation, this maps `/workspace/...` to the
/// actual worktree path based on the sandbox ID (derived from the socket path).
/// For now, passes through as-is since virtiofsd handles the mapping.
fn resolve_cwd(guest_cwd: &str) -> PathBuf {
    PathBuf::from(guest_cwd)
}
