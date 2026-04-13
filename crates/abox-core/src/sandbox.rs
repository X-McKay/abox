//! Sandbox orchestrator.
//!
//! Coordinates the workspace manager, VM manager, and proxy daemon to provide
//! a unified interface for sandbox lifecycle management. This is the main
//! application-layer service that the CLI and TUI call into.

use crate::config::{AboxConfig, CredentialFileEntry, VmRuntimeTuning};
use crate::vm::{CredentialToStage, VmConfig, VmInfo, VmPort, VmState};
use crate::workspace::{DivergenceEntry, WorkspacePort, WorktreeInfo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Parameters for creating a new sandbox.
#[derive(Debug, Clone)]
pub struct CreateSandboxParams {
    /// Unique task/sandbox identifier (e.g., "fix-auth").
    pub task_id: String,
    /// Base branch to fork from (default: "main").
    pub base_branch: String,
    /// Optional template to restore from instead of booting fresh.
    pub template: Option<String>,
    /// Memory override in MiB.
    pub memory_mib: Option<u32>,
    /// vCPU override.
    pub vcpus: Option<u8>,
    /// Unix user to run the agent as inside the VM.
    pub user: Option<String>,
    /// Environment variables to set inside the VM.
    pub env_vars: Vec<(String, String)>,
    /// Command to execute inside the VM (the agent).
    pub command: Vec<String>,
    /// Kill the sandbox after this many seconds (exit code 124).
    pub timeout_secs: Option<u64>,
    /// Automatically remove the sandbox (worktree + branch) after exit.
    pub ephemeral: bool,
}

/// Full sandbox status combining workspace and VM info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxStatus {
    pub id: String,
    pub branch: String,
    pub worktree_path: String,
    pub vm_state: String,
    pub vm_pid: u32,
    pub commits_ahead: usize,
    /// Port allocated for this sandbox's egress proxy listener.
    #[serde(default)]
    pub egress_port: u16,
}

/// Convert a TOML value to a JSON value (for stub credential generation).
pub fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let map = table.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect();
            serde_json::Value::Object(map)
        }
    }
}

/// Resolve credential file config entries into staged credential payloads.
///
/// For each [`CredentialFileEntry`]:
/// - Expands `~` in the host path.
/// - If the host file does not exist, logs a debug message and skips.
/// - If `stub` is set, serializes the TOML stub value to JSON.
/// - Otherwise, copies the host file content as-is.
pub fn stage_credential_files(entries: &[CredentialFileEntry]) -> Vec<CredentialToStage> {
    let mut result = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let host_path = crate::policy::expand_tilde(&entry.host);

        if let Some(ref stub) = entry.stub {
            // Stub mode: serialize TOML value to JSON.
            let json_value = toml_to_json(stub);
            let content = match serde_json::to_string_pretty(&json_value) {
                Ok(s) => s.into_bytes(),
                Err(e) => {
                    tracing::warn!(
                        index,
                        guest_path = %entry.guest,
                        error = %e,
                        "Failed to serialize credential stub to JSON; skipping"
                    );
                    continue;
                }
            };
            result.push(CredentialToStage {
                index,
                guest_path: entry.guest.clone(),
                mode: entry.mode.clone(),
                content,
            });
        } else {
            // Copy mode: read from host file.
            let path = std::path::Path::new(&host_path);
            if !path.exists() {
                tracing::debug!(
                    host_path = %host_path,
                    guest_path = %entry.guest,
                    "Host credential file does not exist; skipping"
                );
                continue;
            }
            match std::fs::read(path) {
                Ok(content) => {
                    result.push(CredentialToStage {
                        index,
                        guest_path: entry.guest.clone(),
                        mode: entry.mode.clone(),
                        content,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        host_path = %host_path,
                        guest_path = %entry.guest,
                        error = %e,
                        "Failed to read host credential file; skipping"
                    );
                }
            }
        }
    }
    result
}

/// The sandbox orchestrator. This is the main entry point for all operations.
pub struct SandboxOrchestrator<W: WorkspacePort, V: VmPort> {
    config: AboxConfig,
    workspace: W,
    vm_manager: V,
}

impl<W: WorkspacePort, V: VmPort> SandboxOrchestrator<W, V> {
    pub fn new(config: AboxConfig, workspace: W, vm_manager: V) -> Self {
        Self { config, workspace, vm_manager }
    }

    /// Runtime directory (where sockets, console logs, and detached
    /// supervisor PID files live). Exposed so CLI commands like
    /// `abox run --detach` can write per-sandbox state next to the rest.
    pub fn runtime_dir(&self) -> std::path::PathBuf {
        self.config.runtime_dir()
    }

    /// Create and start a new sandbox.
    ///
    /// This performs the full lifecycle:
    /// 1. Create a git worktree on a new branch
    /// 2. Start virtiofsd + Cloud Hypervisor VM
    /// 3. The VM boots, mounts the worktree at /workspace, and runs the agent
    pub async fn create_sandbox(&self, params: CreateSandboxParams) -> Result<SandboxStatus> {
        // Step 1: Create the git worktree
        let worktree_path = self
            .workspace
            .create_worktree(&params.task_id, &params.base_branch)
            .with_context(|| format!("Failed to create worktree for '{}'", params.task_id))?;

        tracing::info!(
            task_id = %params.task_id,
            worktree = %worktree_path.display(),
            "Worktree created"
        );

        // Step 2: Determine start mode (fresh boot or restore from template).
        let start_mode = match &params.template {
            Some(name) => {
                let template_path = self.config.templates_dir().join(name);
                anyhow::ensure!(template_path.exists(), "template '{name}' not found");
                crate::vm::StartMode::Restore { template_path }
            }
            None => crate::vm::StartMode::Fresh,
        };

        // Inject HTTPS_PROXY env vars so the guest routes HTTPS through the
        // per-sandbox egress proxy. The guest's init.sh bridges vsock port
        // 5001 to a local TCP listener at 127.0.0.1:18443 via socat.
        let mut env_vars = params.env_vars;
        let proxy_url = "http://127.0.0.1:18443".to_string();
        env_vars.push(("HTTPS_PROXY".to_string(), proxy_url.clone()));
        env_vars.push(("https_proxy".to_string(), proxy_url));
        // Node.js uses its own embedded CA bundle; tell it to also trust the
        // abox root CA so the TLS-terminating MITM proxy is accepted.
        env_vars.push((
            "NODE_EXTRA_CA_CERTS".to_string(),
            "/etc/ssl/certs/abox-ca.pem".to_string(),
        ));

        // Step 3: Build VM config.
        // Resolve image and kernel paths: prefer explicit config values, then
        // fall back to the standard bootstrap location (~/.abox/vm/). Fail fast
        // with an actionable message rather than attempting to start with a
        // non-existent path and producing a cryptic OS error.
        let default_vm_dir = self.config.state_dir.join("vm");
        let image_path = self
            .config
            .vm_defaults
            .image_path
            .clone()
            .unwrap_or_else(|| default_vm_dir.join("rootfs.raw"));
        let kernel_path = self
            .config
            .vm_defaults
            .kernel_path
            .clone()
            .unwrap_or_else(|| default_vm_dir.join("vmlinux"));
        if !image_path.exists() {
            // Roll back the worktree we just created before returning the error.
            let _ = self.workspace.remove_worktree(&params.task_id, true);
            anyhow::bail!(
                "VM rootfs image not found at {}\n\n\
                 Run 'abox init' or 'just bootstrap-vm' to download and assemble\n\
                 the VM stack, then try again.",
                image_path.display()
            );
        }
        if !kernel_path.exists() {
            let _ = self.workspace.remove_worktree(&params.task_id, true);
            anyhow::bail!(
                "VM kernel not found at {}\n\n\
                 Run 'abox init' or 'just bootstrap-vm' to download and assemble\n\
                 the VM stack, then try again.",
                kernel_path.display()
            );
        }

        // Resolve credential files from config so they can be staged into the guest.
        let credential_files = stage_credential_files(&self.config.guest.credential_files);

        let vm_config = VmConfig {
            id: params.task_id.clone(),
            worktree_path: worktree_path.clone(),
            image_path,
            kernel_path,
            memory_mib: params.memory_mib.unwrap_or(self.config.vm_defaults.memory_mib),
            vcpus: params.vcpus.unwrap_or(self.config.vm_defaults.vcpus),
            user: params.user,
            env_vars,
            agent_command: params.command.clone(),
            proxy_port: 0, // unused; egress proxy now routes through vsock
            start_mode,
            credential_files,
        };

        // Step 4: Start the VM (or restore from snapshot). If this fails, roll back the worktree we just
        // created so the user is not left with orphaned state.
        let vm_info = match self.vm_manager.start(vm_config).await {
            Ok(info) => info,
            Err(start_err) => {
                tracing::warn!(
                    task_id = %params.task_id,
                    error = %start_err,
                    "VM start failed; rolling back worktree"
                );
                if let Err(cleanup_err) = self.workspace.remove_worktree(&params.task_id, true) {
                    tracing::error!(
                        task_id = %params.task_id,
                        error = %cleanup_err,
                        "Worktree rollback failed; manual cleanup required"
                    );
                }
                return Err(
                    start_err.context(format!("Failed to start VM for '{}'", params.task_id))
                );
            }
        };

        Ok(SandboxStatus {
            id: params.task_id.clone(),
            branch: format!("agent/{}", params.task_id),
            worktree_path: worktree_path.display().to_string(),
            vm_state: vm_info.state.to_string(),
            vm_pid: vm_info.pid,
            commits_ahead: 0,
            egress_port: 0, // unused; egress proxy now routes through vsock
        })
    }

    /// Stop a sandbox and optionally clean up.
    ///
    /// If the VM is already stopped (or never started — e.g. previous `run`
    /// failed at VM boot), this still proceeds with worktree cleanup when
    /// `clean` is true. This guarantees there is always a CLI path to recover
    /// from orphaned state.
    pub async fn stop_sandbox(&self, task_id: &str, clean: bool) -> Result<()> {
        match self.vm_manager.stop(task_id).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    task_id,
                    error = %e,
                    "VM stop returned error (likely already stopped); continuing"
                );
            }
        }

        if clean {
            self.workspace
                .remove_worktree(task_id, true)
                .with_context(|| format!("Failed to remove worktree '{task_id}'"))?;
        }

        Ok(())
    }

    /// List all active sandboxes.
    pub async fn list_sandboxes(&self) -> Result<Vec<SandboxStatus>> {
        let worktrees = self.workspace.list_worktrees()?;
        let vms = self.vm_manager.list().await?;

        let mut statuses = Vec::new();

        for wt in &worktrees {
            let vm_info = vms.iter().find(|v| v.id == wt.sandbox_id);
            let (vm_state, vm_pid) = match vm_info {
                Some(v) => (v.state.to_string(), v.pid),
                None => (VmState::Stopped.to_string(), 0),
            };

            statuses.push(SandboxStatus {
                id: wt.sandbox_id.clone(),
                branch: wt.branch.clone(),
                worktree_path: wt.path.display().to_string(),
                vm_state,
                vm_pid,
                commits_ahead: wt.commits_ahead,
                egress_port: 0, // not tracked for listed sandboxes
            });
        }

        Ok(statuses)
    }

    /// Get the divergence matrix.
    pub fn divergence(&self, base_branch: &str) -> Result<Vec<DivergenceEntry>> {
        self.workspace.compute_divergence(base_branch)
    }

    /// Merge a sandbox's branch back into the base branch.
    pub fn merge(&self, task_id: &str, base_branch: &str) -> Result<Vec<String>> {
        self.workspace.merge_branch(task_id, base_branch)
    }

    /// Get workspace info for a specific sandbox.
    pub fn worktree_info(&self, task_id: &str) -> Result<Option<WorktreeInfo>> {
        let worktrees = self.workspace.list_worktrees()?;
        Ok(worktrees.into_iter().find(|w| w.sandbox_id == task_id))
    }

    /// Pause a sandbox VM (for snapshotting).
    pub async fn pause_sandbox(&self, task_id: &str) -> Result<()> {
        self.vm_manager.pause(task_id).await
    }

    /// Resume a paused sandbox VM.
    pub async fn resume_sandbox(&self, task_id: &str) -> Result<()> {
        self.vm_manager.resume(task_id).await
    }

    /// Get VM info for a specific sandbox.
    pub async fn vm_info(&self, task_id: &str) -> Result<VmInfo> {
        self.vm_manager.info(task_id).await
    }

    /// Foreground variant of `create_sandbox`.
    ///
    /// Creates the worktree, boots the VM, starts a per-VM proxy bridge
    /// bound to `<runtime>/vsock-<id>.sock_5000` (the path Cloud Hypervisor
    /// exposes for guest vsock-port-5000 traffic), streams the guest
    /// console to the orchestrator's stdio, polls the VM until it exits,
    /// and tears everything down.
    ///
    /// Returns the agent's exit code. The current MVP returns 0 on clean
    /// VM exit; structured exit-code propagation from the guest is a
    /// follow-up.
    pub async fn run_sandbox(
        &self,
        params: CreateSandboxParams,
        policy: std::sync::Arc<crate::policy::PolicyEngine>,
        root_ca: std::sync::Arc<crate::ca::RootCa>,
    ) -> Result<i32> {
        let timeout_secs = params.timeout_secs;
        let ephemeral = params.ephemeral;
        let status = self.create_sandbox(params).await?;
        let task_id = status.id.clone();
        let worktree_path = std::path::PathBuf::from(&status.worktree_path);

        // Spawn the per-VM proxy bridge bound to vsock-<id>.sock_5000.
        let bridge_socket = self.config.runtime_dir().join(format!("vsock-{task_id}.sock_5000"));
        // Use a file-based audit sink so guest requests appear in the same
        // audit JSONL file used by abox-proxyd. Fall back to TracingAuditSink
        // if the log file can't be opened.
        let audit_path = self.config.logs_dir().join("audit.jsonl");
        let audit_sink: std::sync::Arc<dyn crate::proxy_bridge::AuditSink> =
            match crate::proxy_bridge::FileAuditSink::open(&audit_path) {
                Ok(sink) => std::sync::Arc::new(sink),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %audit_path.display(),
                        "Could not open audit log; falling back to tracing-only"
                    );
                    std::sync::Arc::new(crate::proxy_bridge::TracingAuditSink)
                }
            };
        // Map guest /workspace → host worktree so that the shim's CWD
        // (which is /workspace inside the VM) resolves to the real path.
        let bridge = crate::proxy_bridge::ProxyBridge::new(
            bridge_socket,
            std::sync::Arc::clone(&policy),
            audit_sink,
            crate::proxy_bridge::SandboxAttribution::Fixed(task_id.clone()),
        )
        .with_cwd_map("/workspace", worktree_path);
        let bridge_handle = tokio::spawn(async move {
            if let Err(e) = bridge.run().await {
                tracing::error!(error = %e, "proxy bridge crashed");
            }
        });

        // Spawn the per-sandbox egress proxy bound to the vsock-bridged Unix
        // socket. Cloud Hypervisor routes guest vsock port 5001 traffic to
        // `vsock-<id>.sock_5001` — the same pattern used for the CLI proxy
        // bridge on port 5000.
        let egress_socket = self.config.runtime_dir().join(format!("vsock-{task_id}.sock_5001"));
        let egress_policy = std::sync::Arc::clone(&policy);
        let egress_ca = std::sync::Arc::clone(&root_ca);
        let bypass_tls = policy.bypass_tls_patterns().to_vec();
        let egress_task_id = task_id.clone();
        let egress_audit_path = self.config.logs_dir().join("audit.jsonl");
        let egress_handle = tokio::spawn(async move {
            // Remove any stale socket from a previous run.
            let _ = std::fs::remove_file(&egress_socket);
            let listener = match tokio::net::UnixListener::bind(&egress_socket) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(
                        socket = %egress_socket.display(),
                        error = %e,
                        "Failed to bind egress proxy listener"
                    );
                    return;
                }
            };
            tracing::info!(
                socket = %egress_socket.display(),
                task_id = %egress_task_id,
                "Per-sandbox egress proxy listening (vsock port 5001)"
            );

            loop {
                let (stream, _peer_addr) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!(error = %e, "Egress accept error");
                        continue;
                    }
                };
                let policy = std::sync::Arc::clone(&egress_policy);
                let root_ca = std::sync::Arc::clone(&egress_ca);
                let bypass_tls = bypass_tls.clone();
                let sandbox_id = egress_task_id.clone();
                let audit_path = egress_audit_path.clone();

                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let policy = policy.clone();
                    let root_ca = root_ca.clone();
                    let bypass_tls = bypass_tls.clone();
                    let sandbox_id = sandbox_id.clone();
                    let audit_path = audit_path.clone();

                    let service = hyper::service::service_fn(move |req| {
                        let policy = policy.clone();
                        let root_ca = std::sync::Arc::clone(&root_ca);
                        let bypass_tls = bypass_tls.clone();
                        let sandbox_id = sandbox_id.clone();
                        let audit_path = audit_path.clone();
                        async move {
                            crate::egress::handle_request(
                                req,
                                &policy,
                                root_ca,
                                &bypass_tls,
                                move |domain: &str, decision: &str, status_code: i32| {
                                    // Best-effort audit logging via a per-entry
                                    // file append — avoids pulling in the full
                                    // AuditLog type from abox-proxyd.
                                    let entry = serde_json::json!({
                                        "timestamp": chrono::Utc::now().to_rfc3339(),
                                        "sandbox_id": sandbox_id,
                                        "request_type": "egress",
                                        "target": domain,
                                        "detail": "",
                                        "decision": decision,
                                        "result_code": status_code,
                                    });
                                    if let Ok(line) = serde_json::to_string(&entry) {
                                        use std::io::Write;
                                        if let Ok(mut f) = std::fs::OpenOptions::new()
                                            .create(true)
                                            .append(true)
                                            .open(&audit_path)
                                        {
                                            let _ = writeln!(f, "{line}");
                                        }
                                    }
                                },
                            )
                            .await
                        }
                    });

                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(io, service)
                        .with_upgrades()
                        .await
                    {
                        tracing::debug!(error = %e, "Egress proxy connection error");
                    }
                });
            }
        });

        // Spawn the console streamer with a shutdown notify so it can drain
        // the last bytes of guest output gracefully when the VM exits,
        // instead of being abort()'d mid-read and dropping the trailing
        // poweroff banner on slow systems.
        let console_log = self.config.runtime_dir().join(format!("console-{task_id}.log"));
        let console_shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
        let console_shutdown_for_task = console_shutdown.clone();
        let console_handle = tokio::spawn(async move {
            if let Err(e) = Box::pin(crate::console::tail_to_stdout_until(
                &console_log,
                console_shutdown_for_task,
            ))
            .await
            {
                tracing::debug!(error = %e, "console stream ended");
            }
        });

        // Poll for VM exit. The trait doesn't expose a "wait" primitive,
        // so we poll `info` until it errors out (which the adapter does
        // when the VM is no longer in its registry — i.e. after it has
        // been removed from the map). Interval is centralized in
        // VmRuntimeTuning so tests can tighten it.
        let tuning = VmRuntimeTuning::DEFAULT;

        let poll_future = async {
            loop {
                tokio::time::sleep(tuning.vm_exit_poll_interval).await;
                if self.vm_manager.info(&task_id).await.is_err() {
                    break;
                }
            }
        };

        let timed_out = if let Some(secs) = timeout_secs {
            match tokio::time::timeout(std::time::Duration::from_secs(secs), poll_future).await {
                Ok(()) => false,
                Err(_elapsed) => {
                    tracing::warn!(task_id = %task_id, secs, "sandbox timed out");
                    // Graceful shutdown first.
                    if let Err(e) = self.vm_manager.stop(&task_id).await {
                        tracing::warn!(
                            task_id = %task_id,
                            error = %e,
                            "Graceful shutdown after timeout failed"
                        );
                    }
                    // Wait up to 10s for the VM to actually exit.
                    let grace = async {
                        loop {
                            tokio::time::sleep(tuning.vm_exit_poll_interval).await;
                            if self.vm_manager.info(&task_id).await.is_err() {
                                break;
                            }
                        }
                    };
                    if tokio::time::timeout(tuning.vm_timeout_grace_period, grace).await.is_err() {
                        tracing::warn!(
                            task_id = %task_id,
                            "VM did not exit within grace period; force-killing"
                        );
                        // Force kill — best effort. The existing stop() on
                        // Cloud Hypervisor calls `shutdown-vmm` which is
                        // already forceful; a second call is our best bet.
                        let _ = self.vm_manager.stop(&task_id).await;
                    }
                    true
                }
            }
        } else {
            poll_future.await;
            false
        };

        bridge_handle.abort();
        egress_handle.abort();
        // Signal the console tailer to drain and exit. Wait briefly for it
        // to finish before we move on; if it stays stuck (shouldn't happen
        // in practice), the JoinHandle is dropped and the task is cancelled.
        console_shutdown.notify_one();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), console_handle).await;

        // Determine exit code: 124 on timeout, otherwise read from guest.
        let exit_code = if timed_out {
            124
        } else {
            // Read the exit code the guest wrote into /abox-status/exit-code.
            let exit_code_opt = self
                .vm_manager
                .status_dir(&task_id)
                .and_then(|d| crate::adapters::cloud_hypervisor::read_exit_code(&d));

            // Tear down the status dir now that we've read (or failed to read) it.
            if let Some(sd) = self.vm_manager.status_dir(&task_id) {
                let _ = std::fs::remove_dir_all(&sd);
            }

            if let Some(code) = exit_code_opt {
                code
            } else {
                // The guest never wrote an exit code — the VM died before
                // init.sh got that far (kernel panic, missing rootfs,
                // virtiofs failure, etc.). Roll back the worktree like a
                // failed VM start would, since this run produced nothing.
                eprintln!(
                    "abox: sandbox '{task_id}' did not report an exit code; \
                     rolling back worktree (the VM may have crashed before \
                     guest init ran -- check the console log)"
                );
                tracing::warn!(
                    task_id = %task_id,
                    "Guest did not write an exit code; rolling back worktree"
                );
                if let Err(e) = self.workspace.remove_worktree(&task_id, true) {
                    tracing::error!(
                        task_id = %task_id,
                        error = %e,
                        "Worktree rollback after silent VM failure also failed"
                    );
                }
                1
            }
        };

        // Ephemeral mode: clean up worktree + branch regardless of exit code.
        if ephemeral {
            tracing::info!(task_id = %task_id, "ephemeral mode: cleaning up sandbox");
            if let Err(e) = self.stop_sandbox(&task_id, /*clean=*/ true).await {
                tracing::error!(
                    task_id = %task_id,
                    error = %e,
                    "Ephemeral cleanup failed"
                );
            }
        }

        Ok(exit_code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CredentialFileEntry;

    #[test]
    fn toml_to_json_string() {
        let v = toml::Value::String("hello".into());
        assert_eq!(toml_to_json(&v), serde_json::json!("hello"));
    }

    #[test]
    fn toml_to_json_integer() {
        let v = toml::Value::Integer(42);
        assert_eq!(toml_to_json(&v), serde_json::json!(42));
    }

    #[test]
    fn toml_to_json_bool() {
        let v = toml::Value::Boolean(true);
        assert_eq!(toml_to_json(&v), serde_json::json!(true));
    }

    #[test]
    fn toml_to_json_nested_table() {
        let toml_str = r#"
            [claudeAiOauth]
            accessToken = "abox-proxy-managed"
            expiresAt = 9999999999999
            scopes = ["user:inference"]
        "#;
        let val: toml::Value = toml::from_str(toml_str).unwrap();
        let json = toml_to_json(&val);
        assert_eq!(json["claudeAiOauth"]["accessToken"], "abox-proxy-managed");
        assert_eq!(json["claudeAiOauth"]["expiresAt"], 9_999_999_999_999i64);
        assert_eq!(json["claudeAiOauth"]["scopes"][0], "user:inference");
    }

    #[test]
    fn stage_credential_files_with_stub() {
        let entries = vec![CredentialFileEntry {
            host: "/nonexistent/host/file".into(),
            guest: "/.claude/.credentials.json".into(),
            mode: "0600".into(),
            stub: Some(
                toml::toml! {
                    [claudeAiOauth]
                    accessToken = "stub-token"
                }
                .into(),
            ),
        }];

        let staged = stage_credential_files(&entries);
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].index, 0);
        assert_eq!(staged[0].guest_path, "/.claude/.credentials.json");
        assert_eq!(staged[0].mode, "0600");

        // Content should be valid JSON with the stub data.
        let json: serde_json::Value = serde_json::from_slice(&staged[0].content).unwrap();
        assert_eq!(json["claudeAiOauth"]["accessToken"], "stub-token");
    }

    #[test]
    fn stage_credential_files_missing_host_skipped() {
        let entries = vec![CredentialFileEntry {
            host: "/this/path/does/not/exist".into(),
            guest: "/root/.config/creds".into(),
            mode: "0600".into(),
            stub: None,
        }];

        let staged = stage_credential_files(&entries);
        assert!(staged.is_empty(), "missing host file should be skipped");
    }

    #[test]
    fn stage_credential_files_copy_mode() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"secret-content").unwrap();

        let entries = vec![CredentialFileEntry {
            host: tmp.path().to_str().unwrap().to_string(),
            guest: "/root/.secret".into(),
            mode: "0400".into(),
            stub: None,
        }];

        let staged = stage_credential_files(&entries);
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].content, b"secret-content");
        assert_eq!(staged[0].mode, "0400");
    }

    #[test]
    fn stage_credential_files_mixed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"real-cred").unwrap();

        let entries = vec![
            CredentialFileEntry {
                host: "/missing".into(),
                guest: "/a".into(),
                mode: "0600".into(),
                stub: None,
            },
            CredentialFileEntry {
                host: tmp.path().to_str().unwrap().to_string(),
                guest: "/b".into(),
                mode: "0600".into(),
                stub: None,
            },
            CredentialFileEntry {
                host: "/also-missing".into(),
                guest: "/c".into(),
                mode: "0600".into(),
                stub: Some(toml::toml! { key = "val" }.into()),
            },
        ];

        let staged = stage_credential_files(&entries);
        // Entry 0 (missing, no stub) skipped; entry 1 (real file) included;
        // entry 2 (stub) included regardless of missing host file.
        assert_eq!(staged.len(), 2);
        assert_eq!(staged[0].index, 1);
        assert_eq!(staged[0].guest_path, "/b");
        assert_eq!(staged[1].index, 2);
        assert_eq!(staged[1].guest_path, "/c");
    }
}
