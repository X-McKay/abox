//! Sandbox orchestrator.
//!
//! Coordinates the workspace manager, VM manager, and proxy daemon to provide
//! a unified interface for sandbox lifecycle management. This is the main
//! application-layer service that the CLI and TUI call into.

use crate::config::{AboxConfig, CredentialFileEntry, VmRuntimeTuning};
use crate::project::{image_path_for_profile, kernel_path_for_profile, EnvironmentProfile};
use crate::vm::{CredentialToStage, VmConfig, VmInfo, VmPort, VmState};
use crate::workspace::{DivergenceEntry, WorkspacePort, WorktreeInfo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    /// Optional resolved prompt content staged into guest boot metadata.
    pub resolved_prompt: Option<String>,
    /// Optional repo-scoped host cache root mounted at `/abox-cache`.
    pub cache_mount_dir: Option<PathBuf>,
    /// Optional immutable prepare script content staged as `/abox-meta/prepare.sh`.
    pub staged_prepare_script: Option<String>,
    /// Guest environment profile that selects the rootfs image.
    pub environment_profile: EnvironmentProfile,
    /// Kill the sandbox after this many seconds (exit code 124).
    pub timeout_secs: Option<u64>,
    /// Automatically remove the sandbox (worktree + branch) after exit.
    pub ephemeral: bool,
    /// PEM-encoded root CA certificate to inject into the guest trust store.
    /// Set by `run_sandbox` from the loaded `RootCa`; `None` for tests or
    /// callers that don't need MITM proxy support.
    pub ca_cert_pem: Option<String>,
    /// Workspace subdirectories to overlay with empty tmpfs inside the guest.
    /// Sourced from `ResolvedProjectConfig.mount_excludes` and passed through
    /// to `VmConfig` and ultimately `BootMeta`.
    pub mount_excludes: Vec<String>,
    /// Ephemeral service sidecars already started on the host. `run_sandbox`
    /// spawns a host→guest vsock bridge for each and stages guest metadata so
    /// the agent can reach them; teardown stops the containers. Empty for the
    /// common case (no services), in which nothing changes.
    pub service_bridges: Vec<crate::services::ServiceBridge>,
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

/// Resolve guest stub-file config entries into staged credential payloads.
///
/// For each [`CredentialFileEntry`]:
/// - Expands `~` in the host path.
/// - If the host file does not exist, logs a warning and skips.
///   Host-file existence is the proxy of "user is logged in"; staging a stub
///   for a tool the user is not logged into would mislead the guest into
///   believing credentials are available when the host-side proxy has nothing
///   to inject.
/// - Serializes the TOML stub value to JSON.
pub fn stage_credential_files(entries: &[CredentialFileEntry]) -> Vec<CredentialToStage> {
    let mut result = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let guest_expanded = match crate::boot_meta::expand_guest_path(&entry.guest) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    guest_path = %entry.guest,
                    error = %e,
                    "Invalid guest path in credential_files; skipping entry"
                );
                continue;
            }
        };
        let host_path = crate::policy::expand_tilde(&entry.host_credential_file);
        let path = std::path::Path::new(&host_path);
        if !path.exists() {
            tracing::warn!(
                host_path = %host_path,
                guest_path = %entry.guest,
                "No host credential file for this stub-bearing entry; agent will \
                 start without this credential and may fail at first API call. \
                 Log in to the tool on the host, or disable the provider in \
                 ~/.abox/config.toml if intentional."
            );
            continue;
        }

        let json_value = toml_to_json(&entry.stub);
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
            guest_path: guest_expanded.clone(),
            mode: entry.mode.clone(),
            content,
        });
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

    pub fn config(&self) -> &AboxConfig {
        &self.config
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
        crate::util::validate_task_id_for_runtime_dir(&params.task_id, &self.config.runtime_dir())
            .map_err(anyhow::Error::msg)?;
        for (key, _) in &params.env_vars {
            crate::util::validate_env_key(key)
                .map_err(|e| anyhow::anyhow!("invalid environment variable key {key:?}: {e}"))?;
        }

        // Step 1: Create the git worktree
        let t_worktree = std::time::Instant::now();
        let worktree_path = self
            .workspace
            .create_worktree(&params.task_id, &params.base_branch)
            .with_context(|| format!("Failed to create worktree for '{}'", params.task_id))?;

        tracing::info!(
            task_id = %params.task_id,
            worktree = %worktree_path.display(),
            elapsed_ms = t_worktree.elapsed().as_millis() as u64,
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
        if params.resolved_prompt.is_some() {
            env_vars.push(("ABOX_PROMPT_FILE".to_string(), "/abox-meta/prompt.md".to_string()));
        }
        // Node.js uses its own embedded CA bundle; tell it to also trust the
        // abox root CA so the TLS-terminating MITM proxy is accepted.
        env_vars
            .push(("NODE_EXTRA_CA_CERTS".to_string(), "/etc/ssl/certs/abox-ca.pem".to_string()));

        // Step 3: Build VM config.
        // Resolve image and kernel paths: prefer explicit config values, then
        // fall back to the standard bootstrap location (~/.abox/vm/). Fail fast
        // with an actionable message rather than attempting to start with a
        // non-existent path and producing a cryptic OS error.
        let image_path = image_path_for_profile(&self.config, params.environment_profile);
        let kernel_path = kernel_path_for_profile(&self.config);
        if !image_path.exists() {
            // Roll back the worktree we just created before returning the error.
            let _ = self.workspace.remove_worktree(&params.task_id, true);
            if params.environment_profile == EnvironmentProfile::Base {
                anyhow::bail!(
                    "VM rootfs image not found at {}\n\n\
                     Run 'abox init' or 'just bootstrap-vm' to download and assemble\n\
                     the VM stack, then try again.",
                    image_path.display()
                );
            }
            anyhow::bail!(
                "VM rootfs image for profile '{}' not found at {}\n\n\
                 Install or build the '{}' guest profile under ~/.abox/vm/profiles/{}/, then try again.",
                params.environment_profile,
                image_path.display(),
                params.environment_profile,
                params.environment_profile,
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
        let credential_files = stage_credential_files(&self.config.auth.credential_files());

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
            resolved_prompt: params.resolved_prompt,
            cache_mount_dir: params.cache_mount_dir,
            staged_prepare_script: params.staged_prepare_script,
            start_mode,
            credential_files,
            ca_cert_pem: params.ca_cert_pem.clone(),
            mount_excludes: params.mount_excludes.clone(),
            services: params.service_bridges.iter().map(crate::services::ServiceBridge::guest).collect(),
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
        // Capture service bridges before `params` is consumed; used below to
        // spawn host→guest forwarders and to tear down containers at exit.
        let service_bridges = params.service_bridges.clone();
        // Inject the root CA PEM so the guest trust store includes it.
        let mut params = params;
        params.ca_cert_pem = Some(root_ca.cert_pem.clone());
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
            std::sync::Arc::clone(&audit_sink),
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
        // Wrap bypass_tls in an Arc so each per-connection / per-request task
        // does a cheap pointer clone instead of cloning the underlying Vec.
        // The pattern list is set at policy load time and never changes.
        let bypass_tls: std::sync::Arc<[String]> = policy.bypass_tls_patterns().to_vec().into();
        let egress_task_id = task_id.clone();
        let egress_audit = std::sync::Arc::clone(&audit_sink);
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
                let bypass_tls = std::sync::Arc::clone(&bypass_tls);
                let sandbox_id = egress_task_id.clone();
                let audit = std::sync::Arc::clone(&egress_audit);

                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let policy = policy.clone();
                    let root_ca = root_ca.clone();
                    let bypass_tls = std::sync::Arc::clone(&bypass_tls);
                    let sandbox_id = sandbox_id.clone();
                    let audit = std::sync::Arc::clone(&audit);

                    let service = hyper::service::service_fn(move |req| {
                        let policy = policy.clone();
                        let root_ca = std::sync::Arc::clone(&root_ca);
                        let bypass_tls = std::sync::Arc::clone(&bypass_tls);
                        let sandbox_id = sandbox_id.clone();
                        let audit = std::sync::Arc::clone(&audit);
                        async move {
                            crate::egress::handle_request(
                                req,
                                &policy,
                                root_ca,
                                &bypass_tls,
                                move |domain: &str, decision: &str, status_code: i32| {
                                    // Reuse the shared AuditSink used by the
                                    // CLI proxy bridge so egress entries land
                                    // in the same buffered audit.jsonl file.
                                    audit.log_egress(&sandbox_id, domain, decision, status_code);
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

        // Spawn a host→guest bridge for each service sidecar. Each binds the
        // Cloud Hypervisor vsock backend socket for the service's vsock port
        // and forwards to the container's published host port. No services →
        // no bridges, so the common path is unaffected.
        let mut service_bridge_handles = Vec::new();
        for bridge in &service_bridges {
            let socket = self
                .config
                .runtime_dir()
                .join(format!("vsock-{task_id}.sock_{}", bridge.vsock_port));
            let host_port = bridge.host_port;
            let name = bridge.name.clone();
            service_bridge_handles.push(tokio::spawn(async move {
                if let Err(e) = crate::services::serve_service_bridge(socket, host_port).await {
                    tracing::error!(service = %name, error = %e, "service bridge crashed");
                }
            }));
        }

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

        // Wait for VM exit. The adapter's wait_for_exit() polls the
        // child process handle directly (5 ms try_wait on the real
        // adapter, 10 ms info()-based fallback for mocks), replacing
        // the previous 250 ms info() poll loop.
        let tuning = VmRuntimeTuning::DEFAULT;

        let wait_future = self.vm_manager.wait_for_exit(&task_id);

        let timed_out = if let Some(secs) = timeout_secs {
            match tokio::time::timeout(std::time::Duration::from_secs(secs), wait_future).await {
                Ok(_) => false,
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
                    let grace = self.vm_manager.wait_for_exit(&task_id);
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
            let _ = wait_future.await;
            false
        };

        bridge_handle.abort();
        egress_handle.abort();
        for handle in service_bridge_handles {
            handle.abort();
        }
        // Stop and remove any service sidecars started for this sandbox. Keyed
        // by the sandbox label so it also reaps containers the bridge list
        // might have missed. No-op when no services were started.
        if !service_bridges.is_empty() {
            if let Err(e) = crate::services::stop_sandbox_services(&task_id) {
                tracing::warn!(task_id = %task_id, error = %e, "Failed to stop service sidecars");
            }
        }
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
    fn stage_expands_tilde_in_guest_path() {
        let expanded = crate::boot_meta::expand_guest_path("~/.claude/.credentials.json").unwrap();
        assert_eq!(expanded, "/home/abox/.claude/.credentials.json");
    }

    #[test]
    fn stage_rejects_invalid_guest_path() {
        let result = crate::boot_meta::expand_guest_path("foo/bar");
        assert!(result.is_err());
    }

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
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"real-creds-on-host").unwrap();

        let entries = vec![CredentialFileEntry {
            host_credential_file: tmp.path().to_str().unwrap().to_string(),
            guest: "/.claude/.credentials.json".into(),
            mode: "0600".into(),
            stub: toml::toml! {
                [claudeAiOauth]
                accessToken = "stub-token"
            }
            .into(),
        }];

        let staged = stage_credential_files(&entries);
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].index, 0);
        assert_eq!(staged[0].guest_path, "/.claude/.credentials.json");
        assert_eq!(staged[0].mode, "0600");

        // Content should be the JSON-serialized stub, NOT the host file content.
        // (Stubs satisfy local credential checks; the proxy injects the real token.)
        let json: serde_json::Value = serde_json::from_slice(&staged[0].content).unwrap();
        assert_eq!(json["claudeAiOauth"]["accessToken"], "stub-token");
    }

    #[test]
    fn stage_credential_files_stub_skipped_when_host_missing() {
        let entries = vec![CredentialFileEntry {
            host_credential_file: "/this/path/definitely/does/not/exist".into(),
            guest: "/.claude/.credentials.json".into(),
            mode: "0600".into(),
            stub: toml::toml! { key = "val" }.into(),
        }];

        let staged = stage_credential_files(&entries);
        assert!(
            staged.is_empty(),
            "stub should be skipped when host file is missing (user not logged in)"
        );
    }

    #[test]
    fn stage_credential_files_multiple_stubs_keep_indices() {
        let first = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(first.path(), b"first-real").unwrap();
        let second = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(second.path(), b"second-real").unwrap();

        let entries = vec![
            CredentialFileEntry {
                host_credential_file: first.path().to_str().unwrap().to_string(),
                guest: "/a".into(),
                mode: "0600".into(),
                stub: toml::toml! { token = "alpha" }.into(),
            },
            CredentialFileEntry {
                host_credential_file: second.path().to_str().unwrap().to_string(),
                guest: "/b".into(),
                mode: "0400".into(),
                stub: toml::toml! { token = "beta" }.into(),
            },
        ];

        let staged = stage_credential_files(&entries);
        assert_eq!(staged.len(), 2);
        assert_eq!(staged[0].index, 0);
        assert_eq!(staged[0].guest_path, "/a");
        assert_eq!(staged[0].mode, "0600");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&staged[0].content).unwrap()["token"],
            "alpha"
        );
        assert_eq!(staged[1].index, 1);
        assert_eq!(staged[1].guest_path, "/b");
        assert_eq!(staged[1].mode, "0400");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&staged[1].content).unwrap()["token"],
            "beta"
        );
    }
}
