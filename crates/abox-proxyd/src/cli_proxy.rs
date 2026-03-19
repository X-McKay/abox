//! CLI proxy handler.
//!
//! Listens on a Unix socket (bridged from VSock) for JSON requests from
//! abox-shim inside the VM. Each request specifies a command and arguments.
//! The handler evaluates the request against the policy engine, and if allowed,
//! executes the real command on the host and returns the result.

use crate::audit::AuditLog;
use abox_core::policy::{Decision, PolicyEngine};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Request from the guest shim (mirrors abox-shim's ProxyRequest).
#[derive(Debug, Deserialize)]
pub struct CliRequest {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
}

/// Response sent back to the guest shim.
#[derive(Debug, Serialize)]
pub struct CliResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The CLI proxy server.
pub struct CliProxyServer {
    socket_path: PathBuf,
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLog>,
}

impl CliProxyServer {
    pub fn new(socket_path: PathBuf, policy: Arc<PolicyEngine>, audit: Arc<AuditLog>) -> Self {
        Self { socket_path, policy, audit }
    }

    /// Start listening for connections. This runs forever.
    pub async fn run(&self) -> Result<()> {
        // Clean up stale socket
        let _ = std::fs::remove_file(&self.socket_path);

        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        tracing::info!(
            socket = %self.socket_path.display(),
            "CLI proxy listening"
        );

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

async fn handle_connection(
    stream: UnixStream,
    policy: &PolicyEngine,
    audit: &AuditLog,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    if line.trim().is_empty() {
        return Ok(());
    }

    let request: CliRequest = serde_json::from_str(line.trim())?;

    tracing::debug!(
        command = %request.command,
        args = ?request.args,
        "CLI proxy request"
    );

    // Evaluate the policy
    let decision = policy.evaluate_cli(&request.command, &request.args);

    let response = match &decision {
        Decision::Allow => {
            // Execute the real command on the host
            let output = execute_command(&request).await;
            match output {
                Ok((exit_code, stdout, stderr)) => {
                    // Sandbox ID is derived from the socket connection context.
                    // For now we use "unknown" — in production, each sandbox gets
                    // its own socket path, so the ID is embedded in the path.
                    audit.log_cli("unknown", &request.command, &request.args, "allowed", exit_code);
                    CliResponse { exit_code, stdout, stderr }
                }
                Err(e) => {
                    audit.log_cli("unknown", &request.command, &request.args, "allowed", -1);
                    CliResponse {
                        exit_code: 1,
                        stdout: String::new(),
                        stderr: format!("Failed to execute command: {}", e),
                    }
                }
            }
        }
        Decision::Deny(reason) => {
            audit.log_cli("unknown", &request.command, &request.args, "denied", 126);
            tracing::warn!(
                command = %request.command,
                reason = %reason,
                "CLI request denied"
            );
            CliResponse {
                exit_code: 126,
                stdout: String::new(),
                stderr: format!("abox-proxyd: denied: {}\n", reason),
            }
        }
    };

    let response_json = serde_json::to_string(&response)?;
    writer.write_all(response_json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;

    Ok(())
}

/// Execute a command on the host and capture its output.
async fn execute_command(request: &CliRequest) -> Result<(i32, String, String)> {
    let output = tokio::process::Command::new(&request.command)
        .args(&request.args)
        .current_dir(resolve_cwd(&request.cwd))
        .output()
        .await?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok((exit_code, stdout, stderr))
}

/// Resolve the CWD from the guest to the host.
/// The guest's /workspace maps to the host's worktree path.
/// For now, we just use the host's current directory.
fn resolve_cwd(guest_cwd: &str) -> PathBuf {
    // TODO: Map /workspace -> actual worktree path based on sandbox ID
    PathBuf::from(guest_cwd)
}
