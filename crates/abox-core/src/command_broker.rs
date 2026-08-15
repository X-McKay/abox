//! The host command broker.
//!
//! Accepts JSON [`ProxyRequest`] lines on a Unix socket, evaluates them
//! against a [`PolicyEngine`], executes allowed commands on the host, and
//! writes JSON [`ProxyResponse`] back. Used in two configurations:
//!
//! 1. **Per-sandbox broker (orchestrator).** The broker binds the sandbox's
//!    command-broker control socket ([`crate::runtime`] port 5000): the
//!    runtime routes exactly one guest's vsock traffic there, so every
//!    connection provably came from that sandbox.
//!    [`SandboxAttribution::Fixed`] is used and the request's own
//!    `sandbox_id` field is ignored.
//! 2. **Shared daemon (`abox-proxyd`).** The broker binds a regular Unix
//!    socket and uses [`SandboxAttribution::FromRequest`], so different
//!    sandboxes can share one daemon and the audit log uses whatever id
//!    each request supplies (with `"unknown"` fallback when absent).

use crate::policy::{Decision, PolicyEngine};
use crate::protocol::{ProxyRequest, ProxyResponse};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// How the broker attributes incoming requests to a sandbox identifier.
#[derive(Debug, Clone)]
pub enum SandboxAttribution {
    /// Trust the `sandbox_id` field on each request (proxyd mode).
    /// Falls back to `"unknown"` when the field is absent.
    FromRequest,
    /// Force every request handled on this socket to use this id
    /// (per-sandbox broker mode — the socket itself proves provenance).
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

    /// Record an HTTPS egress request. Default impl emits a tracing event
    /// so callers that only care about CLI auditing don't need to
    /// implement this.
    fn log_egress(&self, sandbox_id: &str, domain: &str, decision: &str, status_code: i32) {
        tracing::info!(
            sandbox_id = %sandbox_id,
            domain = %domain,
            decision = %decision,
            status_code,
            "egress"
        );
    }

    /// Record a host-port bridge event. Default impl emits a tracing event so
    /// sinks that don't persist it still surface the boundary crossing.
    fn log_host_port(&self, sandbox_id: &str, event: &str, guest_port: u16, host_port: u16) {
        tracing::info!(
            sandbox_id = %sandbox_id,
            event = %event,
            guest_port,
            host_port,
            "host-port"
        );
    }
}

/// A configured but not-yet-running proxy bridge.
pub struct CommandBroker {
    socket_path: PathBuf,
    policy: Arc<PolicyEngine>,
    audit: Arc<dyn AuditSink>,
    attribution: SandboxAttribution,
    /// Optional: map a guest path prefix to the host worktree root.
    ///
    /// In per-VM mode (`SandboxAttribution::Fixed`), this is treated as a
    /// hard boundary: only guest CWDs rooted at `guest_prefix` are accepted,
    /// the translated host path is canonicalized, and the final result must
    /// remain inside `host_prefix`.
    ///
    /// In shared-daemon mode (`SandboxAttribution::FromRequest`), the bridge
    /// does not enforce this boundary and the request CWD is passed through.
    cwd_map: Option<(String, PathBuf)>,
}

impl CommandBroker {
    /// Construct a new bridge. Use [`run`] to bind the socket and serve.
    pub fn new(
        socket_path: PathBuf,
        policy: Arc<PolicyEngine>,
        audit: Arc<dyn AuditSink>,
        attribution: SandboxAttribution,
    ) -> Self {
        Self { socket_path, policy, audit, attribution, cwd_map: None }
    }

    /// Set the guest-prefix to host-worktree mapping used for per-VM CWD
    /// boundary enforcement.
    pub fn with_cwd_map(mut self, guest_prefix: impl Into<String>, host_root: PathBuf) -> Self {
        self.cwd_map = Some((guest_prefix.into(), host_root));
        self
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
        let cwd_map = self.cwd_map.map(Arc::new);
        loop {
            let (stream, _) = listener.accept().await?;
            let policy = Arc::clone(&policy);
            let audit = Arc::clone(&audit);
            let attribution = Arc::clone(&attribution);
            let cwd_map = cwd_map.clone();
            tokio::spawn(async move {
                if let Err(e) = handle(
                    stream,
                    &policy,
                    audit.as_ref(),
                    attribution.as_ref(),
                    cwd_map.as_deref(),
                )
                .await
                {
                    tracing::error!(error = %e, "proxy bridge connection error");
                }
            });
        }
    }
}

/// Serve one connection. Handles any number of newline-framed
/// request/response exchanges: one-shot clients (the shim over a Unix
/// socket) close after the first response, while the persistent guest
/// bridge under the MicroSandbox runtime keeps a single uplink connection
/// alive and multiplexes sequential shim invocations over it.
async fn handle(
    stream: UnixStream,
    policy: &PolicyEngine,
    audit: &dyn AuditSink,
    attribution: &SandboxAttribution,
    cwd_map: Option<&(String, PathBuf)>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        handle_one(line.trim(), &mut writer, policy, audit, attribution, cwd_map).await?;
    }
}

async fn handle_one(
    line: &str,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    policy: &PolicyEngine,
    audit: &dyn AuditSink,
    attribution: &SandboxAttribution,
    cwd_map: Option<&(String, PathBuf)>,
) -> Result<()> {
    let mut request: ProxyRequest = serde_json::from_str(line)?;

    let sandbox_id = match attribution {
        SandboxAttribution::Fixed(id) => id.clone(),
        SandboxAttribution::FromRequest => {
            request.sandbox_id.clone().unwrap_or_else(|| "unknown".to_string())
        }
    };

    let resolved_cwd = match resolve_request_cwd(&request.cwd, attribution, cwd_map) {
        Ok(path) => path,
        Err(reason) => {
            audit.log_cli(&sandbox_id, &request.command, &request.args, "denied", 126);
            tracing::warn!(
                sandbox_id = %sandbox_id,
                command = %request.command,
                cwd = %request.cwd,
                reason = %reason,
                "request denied before execution"
            );
            let json = serde_json::to_string(&ProxyResponse::denied(&reason))?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            return Ok(());
        }
    };
    request.cwd = resolved_cwd.display().to_string();

    tracing::debug!(
        sandbox_id = %sandbox_id,
        command = %request.command,
        args = ?request.args,
        cwd = %request.cwd,
        "proxy bridge request"
    );

    let decision = policy.evaluate_cli(&request.command, &request.args);
    let forward_ssh = policy.forward_ssh_agent(&request.command);
    let response = match &decision {
        Decision::Allow => match exec(&request, forward_ssh).await {
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
    writer.flush().await?;
    Ok(())
}

async fn exec(request: &ProxyRequest, forward_ssh: bool) -> Result<ProxyResponse> {
    let mut cmd = tokio::process::Command::new(&request.command);
    cmd.args(&request.args).current_dir(resolve_cwd(&request.cwd));

    // SSH agent forwarding (S3): if the matched policy opted in, pass the
    // host's SSH_AUTH_SOCK through so guest tools (e.g. `git push` to an
    // SSH remote) can reach the host's running ssh-agent. Otherwise,
    // explicitly remove SSH_AUTH_SOCK from the child env so a child
    // cannot accidentally inherit it from a parent process that happens
    // to have it set. The unset is the safer default.
    if forward_ssh {
        if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
            cmd.env("SSH_AUTH_SOCK", sock);
        }
    } else {
        cmd.env_remove("SSH_AUTH_SOCK");
    }

    let output = cmd.output().await?;
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(ProxyResponse::from_exit(exit_code, stdout, stderr))
}

fn resolve_cwd(guest_cwd: &str) -> PathBuf {
    PathBuf::from(guest_cwd)
}

fn resolve_request_cwd(
    guest_cwd: &str,
    attribution: &SandboxAttribution,
    cwd_map: Option<&(String, PathBuf)>,
) -> std::result::Result<PathBuf, String> {
    match attribution {
        SandboxAttribution::FromRequest => Ok(resolve_cwd(guest_cwd)),
        SandboxAttribution::Fixed(_) => {
            let (guest_prefix, host_root) = cwd_map.ok_or_else(|| {
                "per-VM proxy bridge is missing its worktree boundary map".to_string()
            })?;
            let prefix = guest_prefix.as_str();
            let trailing = format!("{prefix}/");
            if guest_cwd != prefix && !guest_cwd.starts_with(&trailing) {
                return Err(format!(
                    "cwd '{guest_cwd}' is outside the sandbox worktree; expected '{prefix}' or a subdirectory"
                ));
            }

            let canonical_root = host_root.canonicalize().map_err(|e| {
                format!("sandbox worktree root '{}' is not accessible: {e}", host_root.display())
            })?;
            let suffix = guest_cwd.trim_start_matches(prefix).trim_start_matches('/');
            let translated = if suffix.is_empty() {
                canonical_root.clone()
            } else {
                canonical_root.join(suffix)
            };
            let canonical_cwd = translated.canonicalize().map_err(|e| {
                format!("cwd '{guest_cwd}' could not be resolved inside the sandbox worktree: {e}")
            })?;

            if canonical_cwd == canonical_root || canonical_cwd.starts_with(&canonical_root) {
                Ok(canonical_cwd)
            } else {
                Err(format!("cwd '{guest_cwd}' escapes the sandbox worktree"))
            }
        }
    }
}

/// A no-op audit sink that emits each request through `tracing` only.
/// Used by the orchestrator's per-VM bridge — the persistent on-disk
/// audit log lives in `abox-proxyd::audit::AuditLog`, which has its own
/// `AuditSink` impl.
pub struct TracingAuditSink;

/// A file-based audit sink for the per-VM proxy bridge.
///
/// Delegates to the shared [`crate::audit::AuditChainWriter`] so guest requests
/// are written to the same hash-chained, verifiable `audit.jsonl` as requests
/// through `abox-proxyd` — and can be checked with `abox audit verify`.
pub struct FileAuditSink {
    writer: crate::audit::AuditChainWriter,
}

impl FileAuditSink {
    /// Open (or create + append to) the given path as a hash-chained audit log.
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        Ok(Self { writer: crate::audit::AuditChainWriter::open(path)? })
    }
}

impl AuditSink for FileAuditSink {
    fn log_cli(
        &self,
        sandbox_id: &str,
        command: &str,
        args: &[String],
        decision: &str,
        exit_code: i32,
    ) {
        self.writer.log_cli(sandbox_id, command, args, decision, exit_code);
        // Also emit through tracing for visibility.
        tracing::info!(
            sandbox_id = %sandbox_id,
            command = %command,
            args = ?args,
            decision = %decision,
            exit_code,
            "cli"
        );
    }

    fn log_egress(&self, sandbox_id: &str, domain: &str, decision: &str, status_code: i32) {
        self.writer.log_egress(sandbox_id, domain, decision, status_code);
        tracing::info!(
            sandbox_id = %sandbox_id,
            domain = %domain,
            decision = %decision,
            status_code,
            "egress"
        );
    }

    fn log_host_port(&self, sandbox_id: &str, event: &str, guest_port: u16, host_port: u16) {
        self.writer.log_host_port(sandbox_id, event, guest_port, host_port);
        tracing::info!(
            sandbox_id = %sandbox_id,
            event = %event,
            guest_port,
            host_port,
            "host-port"
        );
    }
}

/// The shared chained writer is itself an [`AuditSink`], so the `abox-proxyd`
/// daemon can plug it straight into a [`CommandBroker`] without a wrapper. Both
/// CLI and egress entries are written to the chain (the trait's default
/// `log_egress` only traces). Inherent methods are named explicitly to avoid
/// recursing into these trait methods.
impl AuditSink for crate::audit::AuditChainWriter {
    fn log_cli(
        &self,
        sandbox_id: &str,
        command: &str,
        args: &[String],
        decision: &str,
        exit_code: i32,
    ) {
        crate::audit::AuditChainWriter::log_cli(
            self, sandbox_id, command, args, decision, exit_code,
        );
    }

    fn log_egress(&self, sandbox_id: &str, domain: &str, decision: &str, status_code: i32) {
        crate::audit::AuditChainWriter::log_egress(self, sandbox_id, domain, decision, status_code);
    }

    fn log_host_port(&self, sandbox_id: &str, event: &str, guest_port: u16, host_port: u16) {
        crate::audit::AuditChainWriter::log_host_port(
            self, sandbox_id, event, guest_port, host_port,
        );
    }
}

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
                bypass_tls: vec![],
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

    fn fixed_cwd_map(root: &std::path::Path) -> (String, PathBuf) {
        ("/workspace".to_string(), root.to_path_buf())
    }

    #[test]
    fn fixed_cwd_resolves_workspace_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let resolved = resolve_request_cwd(
            "/workspace",
            &SandboxAttribution::Fixed("task-a".into()),
            Some(&fixed_cwd_map(tmp.path())),
        )
        .unwrap();

        assert_eq!(resolved, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn fixed_cwd_resolves_nested_workspace_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let resolved = resolve_request_cwd(
            "/workspace/src",
            &SandboxAttribution::Fixed("task-a".into()),
            Some(&fixed_cwd_map(tmp.path())),
        )
        .unwrap();

        assert_eq!(resolved, nested.canonicalize().unwrap());
    }

    #[test]
    fn fixed_cwd_denies_unmapped_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = resolve_request_cwd(
            "/tmp",
            &SandboxAttribution::Fixed("task-a".into()),
            Some(&fixed_cwd_map(tmp.path())),
        )
        .unwrap_err();

        assert!(err.contains("outside the sandbox worktree"), "unexpected error: {err}");
    }

    #[test]
    fn fixed_cwd_denies_symlink_escape() {
        let tmp = tempfile::TempDir::new().unwrap();
        let outside = tmp.path().join("outside");
        let worktree = tmp.path().join("worktree");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::os::unix::fs::symlink(&outside, worktree.join("escape")).unwrap();

        let err = resolve_request_cwd(
            "/workspace/escape",
            &SandboxAttribution::Fixed("task-a".into()),
            Some(&fixed_cwd_map(&worktree)),
        )
        .unwrap_err();

        assert!(err.contains("escapes the sandbox worktree"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn fixed_attribution_overrides_request_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("bridge.sock");
        let worktree = tmp.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let sink = CountingSink::new();
        let bridge = CommandBroker::new(
            socket.clone(),
            allow_all_engine(),
            sink.clone() as Arc<dyn AuditSink>,
            SandboxAttribution::Fixed("authoritative-id".into()),
        )
        .with_cwd_map("/workspace", worktree.clone());
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
            cwd: "/workspace".into(),
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
    async fn fixed_attribution_denies_cwd_outside_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("bridge.sock");
        let worktree = tmp.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let sink = CountingSink::new();
        let bridge = CommandBroker::new(
            socket.clone(),
            allow_all_engine(),
            sink.clone() as Arc<dyn AuditSink>,
            SandboxAttribution::Fixed("authoritative-id".into()),
        )
        .with_cwd_map("/workspace", worktree);
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
            sandbox_id: Some("client-claimed-id".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, json.as_bytes()).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"\n").await.unwrap();
        tokio::io::AsyncWriteExt::shutdown(&mut stream).await.unwrap();

        let mut buf = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut stream, &mut buf).await.unwrap();
        let response: ProxyResponse = serde_json::from_str(buf.trim()).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        server.abort();

        assert_eq!(response.exit_code, 126);
        assert!(response.stderr.contains("outside the sandbox worktree"));

        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, "denied");
    }

    #[tokio::test]
    async fn from_request_attribution_uses_request_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("bridge.sock");
        let sink = CountingSink::new();
        let bridge = CommandBroker::new(
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
        let bridge = CommandBroker::new(
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
