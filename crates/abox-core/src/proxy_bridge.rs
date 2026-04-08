//! Embedded credential-proxy server.
//!
//! Accepts JSON [`ProxyRequest`] lines on a Unix socket, evaluates them
//! against a [`PolicyEngine`], executes allowed commands on the host, and
//! writes JSON [`ProxyResponse`] back. Used in two configurations:
//!
//! 1. **Per-VM bridge (orchestrator).** The bridge binds the
//!    `<vsock-socket>_5000` path Cloud Hypervisor exposes for the guest's
//!    vsock port 5000. Every connection that arrives there provably came
//!    from one specific guest VM, so [`SandboxAttribution::Fixed`] is used
//!    and the request's own `sandbox_id` field is ignored.
//! 2. **Shared daemon (`abox-proxyd`).** The bridge binds a regular Unix
//!    socket and uses [`SandboxAttribution::FromRequest`], so different
//!    sandboxes can share one daemon and the audit log uses whatever id
//!    each request supplies (with `"unknown"` fallback for legacy shims).

use crate::policy::{Decision, PolicyEngine};
use crate::protocol::{ProxyRequest, ProxyResponse};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// How the bridge attributes incoming requests to a sandbox identifier.
#[derive(Debug, Clone)]
pub enum SandboxAttribution {
    /// Trust the `sandbox_id` field on each request (proxyd mode).
    /// Falls back to `"unknown"` when the field is absent.
    FromRequest,
    /// Force every request handled on this socket to use this id
    /// (per-VM bridge mode — the socket itself proves provenance).
    Fixed(String),
}

/// Hook for audit logging. Implemented by both `abox-proxyd::audit::AuditLog`
/// and the orchestrator's [`TracingAuditSink`] (which only logs to the
/// `tracing` subscriber, no on-disk file).
pub trait AuditSink: Send + Sync {
    fn log_cli(
        &self,
        sandbox_id: &str,
        command: &str,
        args: &[String],
        decision: &str,
        exit_code: i32,
    );
}

/// A configured but not-yet-running proxy bridge.
pub struct ProxyBridge {
    socket_path: PathBuf,
    policy: Arc<PolicyEngine>,
    audit: Arc<dyn AuditSink>,
    attribution: SandboxAttribution,
}

impl ProxyBridge {
    /// Construct a new bridge. Use [`run`] to bind the socket and serve.
    pub fn new(
        socket_path: PathBuf,
        policy: Arc<PolicyEngine>,
        audit: Arc<dyn AuditSink>,
        attribution: SandboxAttribution,
    ) -> Self {
        Self { socket_path, policy, audit, attribution }
    }

    /// Bind the listener and serve forever. Removes any stale socket file
    /// at the same path first. Spawns one tokio task per accepted
    /// connection.
    pub async fn run(self) -> Result<()> {
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&self.socket_path)?;
        tracing::info!(
            socket = %self.socket_path.display(),
            attribution = ?self.attribution,
            "proxy bridge listening"
        );
        let policy = self.policy;
        let audit = self.audit;
        let attribution = Arc::new(self.attribution);
        loop {
            let (stream, _) = listener.accept().await?;
            let policy = Arc::clone(&policy);
            let audit = Arc::clone(&audit);
            let attribution = Arc::clone(&attribution);
            tokio::spawn(async move {
                if let Err(e) = handle(stream, &policy, audit.as_ref(), attribution.as_ref()).await
                {
                    tracing::error!(error = %e, "proxy bridge connection error");
                }
            });
        }
    }
}

async fn handle(
    stream: UnixStream,
    policy: &PolicyEngine,
    audit: &dyn AuditSink,
    attribution: &SandboxAttribution,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.trim().is_empty() {
        return Ok(());
    }
    let request: ProxyRequest = serde_json::from_str(line.trim())?;

    let sandbox_id = match attribution {
        SandboxAttribution::Fixed(id) => id.clone(),
        SandboxAttribution::FromRequest => {
            request.sandbox_id.clone().unwrap_or_else(|| "unknown".to_string())
        }
    };

    tracing::debug!(
        sandbox_id = %sandbox_id,
        command = %request.command,
        args = ?request.args,
        "proxy bridge request"
    );

    let decision = policy.evaluate_cli(&request.command, &request.args);
    let response = match &decision {
        Decision::Allow => match exec(&request).await {
            Ok(r) => {
                audit.log_cli(&sandbox_id, &request.command, &request.args, "allowed", r.exit_code);
                r
            }
            Err(e) => {
                audit.log_cli(&sandbox_id, &request.command, &request.args, "error", -1);
                tracing::error!(
                    sandbox_id = %sandbox_id,
                    command = %request.command,
                    error = %e,
                    "command execution failed"
                );
                ProxyResponse::from_exit(1, String::new(), format!("execution failed: {e}"))
            }
        },
        Decision::Deny(reason) => {
            audit.log_cli(&sandbox_id, &request.command, &request.args, "denied", 126);
            tracing::warn!(
                sandbox_id = %sandbox_id,
                command = %request.command,
                reason = %reason,
                "request denied"
            );
            ProxyResponse::denied(reason)
        }
    };

    let json = serde_json::to_string(&response)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

async fn exec(request: &ProxyRequest) -> Result<ProxyResponse> {
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

fn resolve_cwd(guest_cwd: &str) -> PathBuf {
    PathBuf::from(guest_cwd)
}

/// A no-op audit sink that emits each request through `tracing` only.
/// Used by the orchestrator's per-VM bridge — the persistent on-disk
/// audit log lives in `abox-proxyd::audit::AuditLog`, which has its own
/// `AuditSink` impl.
pub struct TracingAuditSink;

impl AuditSink for TracingAuditSink {
    fn log_cli(
        &self,
        sandbox_id: &str,
        command: &str,
        args: &[String],
        decision: &str,
        exit_code: i32,
    ) {
        tracing::info!(
            sandbox_id = %sandbox_id,
            command = %command,
            args = ?args,
            decision = %decision,
            exit_code,
            "cli"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PolicyFile;

    fn allow_all_engine() -> Arc<PolicyEngine> {
        Arc::new(
            PolicyEngine::from_policy_file(PolicyFile {
                cli: vec![],
                egress: vec![],
                default_cli_action: "allow".to_string(),
                default_egress_action: "deny".to_string(),
            })
            .unwrap(),
        )
    }

    struct CountingSink {
        calls: std::sync::Mutex<Vec<(String, String, String, i32)>>,
    }

    impl CountingSink {
        fn new() -> Arc<Self> {
            Arc::new(Self { calls: std::sync::Mutex::new(Vec::new()) })
        }
    }

    impl AuditSink for CountingSink {
        fn log_cli(
            &self,
            sandbox_id: &str,
            command: &str,
            _args: &[String],
            decision: &str,
            exit_code: i32,
        ) {
            self.calls.lock().unwrap().push((
                sandbox_id.to_string(),
                command.to_string(),
                decision.to_string(),
                exit_code,
            ));
        }
    }

    #[tokio::test]
    async fn fixed_attribution_overrides_request_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("bridge.sock");
        let sink = CountingSink::new();
        let bridge = ProxyBridge::new(
            socket.clone(),
            allow_all_engine(),
            sink.clone() as Arc<dyn AuditSink>,
            SandboxAttribution::Fixed("authoritative-id".into()),
        );
        let server = tokio::spawn(bridge.run());

        // Wait for the socket to appear.
        for _ in 0..40 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        // Connect and send a request whose sandbox_id field is a *different*
        // value to prove Fixed wins.
        let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let req = ProxyRequest {
            command: "true".into(),
            args: vec![],
            cwd: "/tmp".into(),
            sandbox_id: Some("client-claimed-id".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, json.as_bytes()).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"\n").await.unwrap();
        tokio::io::AsyncWriteExt::shutdown(&mut stream).await.unwrap();

        // Read the response so we know the handler completed.
        let mut buf = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut stream, &mut buf).await.unwrap();

        // Give the audit logger a moment.
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        server.abort();

        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "authoritative-id"); // NOT "client-claimed-id"
        assert_eq!(calls[0].1, "true");
        assert_eq!(calls[0].2, "allowed");
    }

    #[tokio::test]
    async fn from_request_attribution_uses_request_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("bridge.sock");
        let sink = CountingSink::new();
        let bridge = ProxyBridge::new(
            socket.clone(),
            allow_all_engine(),
            sink.clone() as Arc<dyn AuditSink>,
            SandboxAttribution::FromRequest,
        );
        let server = tokio::spawn(bridge.run());
        for _ in 0..40 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let req = ProxyRequest {
            command: "true".into(),
            args: vec![],
            cwd: "/tmp".into(),
            sandbox_id: Some("from-client".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, json.as_bytes()).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"\n").await.unwrap();
        tokio::io::AsyncWriteExt::shutdown(&mut stream).await.unwrap();

        let mut buf = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut stream, &mut buf).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        server.abort();

        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "from-client");
    }

    #[tokio::test]
    async fn from_request_falls_back_to_unknown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("bridge.sock");
        let sink = CountingSink::new();
        let bridge = ProxyBridge::new(
            socket.clone(),
            allow_all_engine(),
            sink.clone() as Arc<dyn AuditSink>,
            SandboxAttribution::FromRequest,
        );
        let server = tokio::spawn(bridge.run());
        for _ in 0..40 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        // Legacy shim: no sandbox_id field at all.
        let json = r#"{"command":"true","args":[],"cwd":"/tmp"}"#;
        tokio::io::AsyncWriteExt::write_all(&mut stream, json.as_bytes()).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"\n").await.unwrap();
        tokio::io::AsyncWriteExt::shutdown(&mut stream).await.unwrap();

        let mut buf = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut stream, &mut buf).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        server.abort();

        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "unknown");
    }
}
