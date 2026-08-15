//! `abox template` — Manage VM snapshot templates.
//!
//! **Deprecated with the legacy runtime (ADR-008):** templates are memory
//! snapshots, a Cloud Hypervisor capability the MicroSandbox runtime does
//! not implement (`create` fails with a clear error there). Environment
//! warming (`abox env warm`) is the replacement for fast starts.

use super::validate_task_arg;
use abox_core::config::AboxConfig;
use abox_core::runtime::SandboxRuntimePort;
use abox_core::sandbox::SandboxOrchestrator;
use abox_core::snapshot::SnapshotManager;
use abox_core::util::format_size;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct TemplateArgs {
    #[command(subcommand)]
    pub action: TemplateAction,
}

#[derive(Debug, Subcommand)]
pub enum TemplateAction {
    /// List available templates.
    List,

    /// Create a template from a running sandbox.
    Create {
        /// Name for the new template.
        #[arg(long)]
        name: String,
        /// Sandbox ID to snapshot.
        #[arg(long)]
        from: String,
    },

    /// Delete a template.
    Delete {
        /// Template name to delete.
        name: String,
    },
}

/// Execute a template subcommand that does not require the orchestrator
/// (List, Delete). Returns `Ok(true)` if the command was handled.
pub fn execute_without_orchestrator(args: &TemplateArgs, config: &AboxConfig) -> Result<bool> {
    let snap_mgr = SnapshotManager::new(
        config.templates_dir(),
        config.runtime_dir(),
        config.state_dir.clone(),
    )?;

    match &args.action {
        TemplateAction::List => {
            let templates = snap_mgr.list_templates()?;
            if templates.is_empty() {
                println!("No templates available.");
                println!();
                println!("Create one with: abox template create --name <name> --from <sandbox>");
            } else {
                println!("{:<20} {:<12} PATH", "NAME", "SIZE");
                println!("{}", "-".repeat(60));
                for t in &templates {
                    println!(
                        "{:<20} {:<12} {}",
                        t.name,
                        format_size(t.size_bytes),
                        t.path.display()
                    );
                }
            }
            Ok(true)
        }
        TemplateAction::Delete { name } => {
            snap_mgr.delete_template(name)?;
            println!("Template '{name}' deleted.");
            Ok(true)
        }
        TemplateAction::Create { .. } => Ok(false),
    }
}

/// Execute `template create`, which requires the orchestrator to pause/resume
/// the source sandbox and look up its API socket.
pub async fn execute_create<W: WorkspacePort, R: SandboxRuntimePort>(
    name: &str,
    from: &str,
    orchestrator: &SandboxOrchestrator<W, R>,
    config: &AboxConfig,
) -> Result<()> {
    validate_task_arg(from)?;

    let snap_mgr = SnapshotManager::new(
        config.templates_dir(),
        config.runtime_dir(),
        config.state_dir.clone(),
    )?;

    // Verify the sandbox exists, then get the runtime's snapshot handles
    // (hypervisor API socket + filesystem-share socket names to re-pin).
    orchestrator.runtime_info(from).await?;
    let handles = orchestrator
        .memory_snapshot_handles(from)
        .ok_or_else(|| anyhow::anyhow!("this runtime does not support memory snapshots"))?;

    // Pause the sandbox so the snapshot is consistent.
    orchestrator.pause_sandbox(from).await?;

    // Create the snapshot. On failure, attempt to resume the sandbox so we
    // don't leave it stuck in the paused state.
    match snap_mgr.create_snapshot(&handles.api_socket, name, handles.virtiofs_sockets).await {
        Ok(_path) => {
            orchestrator.resume_sandbox(from).await?;
            println!("Template '{name}' created from sandbox '{from}'");
            Ok(())
        }
        Err(e) => {
            let _ = orchestrator.resume_sandbox(from).await;
            Err(e)
        }
    }
}
