//! Sandbox orchestrator.
//!
//! Coordinates the workspace manager, VM manager, and proxy daemon to provide
//! a unified interface for sandbox lifecycle management. This is the main
//! application-layer service that the CLI and TUI call into.

use crate::config::AboxConfig;
use crate::vm::{VmConfig, VmInfo, VmPort, VmState};
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

        // Step 2: Build VM config
        let vm_config = VmConfig {
            id: params.task_id.clone(),
            worktree_path: worktree_path.clone(),
            image_path: self
                .config
                .vm_defaults
                .image_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("/var/lib/abox/images/base.raw")),
            kernel_path: self
                .config
                .vm_defaults
                .kernel_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("/var/lib/abox/kernel/vmlinux")),
            memory_mib: params.memory_mib.unwrap_or(self.config.vm_defaults.memory_mib),
            vcpus: params.vcpus.unwrap_or(self.config.vm_defaults.vcpus),
            user: params.user,
            env_vars: params.env_vars,
            agent_command: params.command.clone(),
            proxy_port: self.config.proxy.egress_port,
        };

        // Step 3: Start the VM. If this fails, roll back the worktree we just
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
    ) -> Result<i32> {
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
            policy,
            audit_sink,
            crate::proxy_bridge::SandboxAttribution::Fixed(task_id.clone()),
        )
        .with_cwd_map("/workspace", worktree_path);
        let bridge_handle = tokio::spawn(async move {
            if let Err(e) = bridge.run().await {
                tracing::error!(error = %e, "proxy bridge crashed");
            }
        });

        // Spawn the console streamer.
        let console_log = self.config.runtime_dir().join(format!("console-{task_id}.log"));
        let console_handle = tokio::spawn(async move {
            if let Err(e) = Box::pin(crate::console::tail_to_stdout(&console_log)).await {
                tracing::debug!(error = %e, "console stream ended");
            }
        });

        // Poll for VM exit. The trait doesn't expose a "wait" primitive,
        // so we poll `info` until it errors out (which the adapter does
        // when the VM is no longer in its registry — i.e. after it has
        // been removed from the map).
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if self.vm_manager.info(&task_id).await.is_err() {
                break;
            }
        }

        bridge_handle.abort();
        console_handle.abort();

        // Read the exit code the guest wrote into /abox-status/exit-code.
        // If the status dir isn't available (in-memory adapter) or the file
        // is missing/malformed, fall back to 0.
        let exit_code = self
            .vm_manager
            .status_dir(&task_id)
            .and_then(|d| crate::adapters::cloud_hypervisor::read_exit_code(&d))
            .unwrap_or(0);

        // Tear down the status dir now that we've read it.
        if let Some(sd) = self.vm_manager.status_dir(&task_id) {
            let _ = std::fs::remove_dir_all(&sd);
        }

        Ok(exit_code)
    }
}
