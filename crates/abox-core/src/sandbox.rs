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
                .unwrap_or_else(|| PathBuf::from("/var/lib/agentbox/images/base.raw")),
            kernel_path: self
                .config
                .vm_defaults
                .kernel_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("/var/lib/agentbox/kernel/vmlinux")),
            memory_mib: params.memory_mib.unwrap_or(self.config.vm_defaults.memory_mib),
            vcpus: params.vcpus.unwrap_or(self.config.vm_defaults.vcpus),
            user: params.user,
            env_vars: params.env_vars,
            proxy_port: self.config.proxy.egress_port,
        };

        // Step 3: Start the VM
        let vm_info = self
            .vm_manager
            .start(vm_config)
            .await
            .with_context(|| format!("Failed to start VM for '{}'", params.task_id))?;

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
    pub async fn stop_sandbox(&self, task_id: &str, clean: bool) -> Result<()> {
        // Stop the VM
        self.vm_manager
            .stop(task_id)
            .await
            .with_context(|| format!("Failed to stop VM '{}'", task_id))?;

        // Optionally remove the worktree and branch
        if clean {
            self.workspace
                .remove_worktree(task_id, true)
                .with_context(|| format!("Failed to remove worktree '{}'", task_id))?;
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
}
