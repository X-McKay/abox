//! `abox snapshot` — User-facing workspace snapshot management.
//!
//! **Deprecated with the legacy runtime (ADR-008):** memory snapshots are a
//! Cloud Hypervisor capability. The MicroSandbox runtime does not implement
//! memory checkpoints — `create`/`restore` fail with a clear error there —
//! and the durable task model is `git + workspace + warmed environment`
//! (`abox env warm`) instead. This command is removed together with the
//! legacy backend.
//!
//! Exposes Cloud Hypervisor's VM snapshot capabilities through a friendly CLI.
//! Snapshots capture the full VM state (memory + disk) at a point in time and
//! can be used to restore a sandbox to a known-good state.
//!
//! This is distinct from `abox template`, which creates reusable base images.
//! Snapshots are per-sandbox checkpoints; templates are shared starting points.
//!
//! # Commands
//!
//! - `abox snapshot list` — list all templates (snapshots)
//! - `abox snapshot create --name <name> --from <sandbox>` — snapshot a running sandbox
//! - `abox snapshot restore <name> --as <sandbox>` — restore a snapshot as a new sandbox
//! - `abox snapshot delete <name>` — delete a snapshot
//! - `abox snapshot prune [--keep N]` — remove oldest snapshots, keeping N most recent

use super::validate_task_arg;
use abox_core::config::AboxConfig;
use abox_core::project::EnvironmentProfile;
use abox_core::runtime::SandboxRuntimePort;
use abox_core::sandbox::{CreateSandboxParams, SandboxOrchestrator};
use abox_core::snapshot::{validate_template_name, SnapshotManager};
use abox_core::util::format_size;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    pub action: SnapshotAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SnapshotAction {
    /// List all available snapshots.
    List,

    /// Create a snapshot of a running sandbox.
    Create {
        /// Name for the new snapshot.
        #[arg(long)]
        name: String,
        /// Sandbox ID to snapshot (must be running).
        #[arg(long)]
        from: String,
    },

    /// Restore a snapshot as a new sandbox.
    ///
    /// The restored sandbox will have the same worktree state as when the
    /// snapshot was taken. The original snapshot is preserved.
    Restore {
        /// Snapshot name to restore.
        name: String,
        /// New sandbox ID for the restored sandbox.
        #[arg(long)]
        r#as: String,
    },

    /// Delete a snapshot.
    Delete {
        /// Snapshot name to delete.
        name: String,
    },

    /// Remove oldest snapshots, keeping the N most recent.
    Prune {
        /// Number of snapshots to keep (default: 5).
        #[arg(long, default_value = "5")]
        keep: usize,
        /// Preview what would be deleted without actually deleting.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Execute snapshot subcommands that do not require the orchestrator.
/// Returns `Ok(true)` if the command was handled.
pub fn execute_without_orchestrator(args: &SnapshotArgs, config: &AboxConfig) -> Result<bool> {
    let snap_mgr = SnapshotManager::new(
        config.templates_dir(),
        config.runtime_dir(),
        config.state_dir.clone(),
    )?;

    match &args.action {
        SnapshotAction::List => {
            list_snapshots(&snap_mgr)?;
            Ok(true)
        }
        SnapshotAction::Delete { name } => {
            validate_template_name(name)?;
            snap_mgr.delete_template(name)?;
            println!("Snapshot '{name}' deleted.");
            Ok(true)
        }
        SnapshotAction::Prune { keep, dry_run } => {
            prune_snapshots(&snap_mgr, *keep, *dry_run)?;
            Ok(true)
        }
        // Create and Restore require the orchestrator
        SnapshotAction::Create { .. } | SnapshotAction::Restore { .. } => Ok(false),
    }
}

/// Execute snapshot create/restore, which require the orchestrator.
pub async fn execute_with_orchestrator<W: WorkspacePort, R: SandboxRuntimePort>(
    args: &SnapshotArgs,
    orchestrator: &SandboxOrchestrator<W, R>,
    config: &AboxConfig,
) -> Result<()> {
    let snap_mgr = SnapshotManager::new(
        config.templates_dir(),
        config.runtime_dir(),
        config.state_dir.clone(),
    )?;

    match &args.action {
        SnapshotAction::Create { name, from } => {
            validate_template_name(name)?;
            validate_task_arg(from)?;
            create_snapshot(name, from, orchestrator, &snap_mgr).await
        }
        SnapshotAction::Restore { name, r#as } => {
            validate_template_name(name)?;
            validate_task_arg(r#as)?;
            restore_snapshot(name, r#as, orchestrator, &snap_mgr).await
        }
        _ => unreachable!("non-orchestrator actions should have been handled earlier"),
    }
}

fn list_snapshots(snap_mgr: &SnapshotManager) -> Result<()> {
    let snapshots = snap_mgr.list_templates()?;

    if snapshots.is_empty() {
        println!("No snapshots available.");
        println!();
        println!("Create one with:");
        println!("  abox snapshot create --name <name> --from <running-sandbox>");
        return Ok(());
    }

    println!("{:<24} {:<12} PATH", "NAME", "SIZE");
    println!("{}", "-".repeat(70));
    let mut total_bytes = 0u64;
    for snap in &snapshots {
        total_bytes += snap.size_bytes;
        println!("{:<24} {:<12} {}", snap.name, format_size(snap.size_bytes), snap.path.display());
    }
    println!();
    println!("{} snapshot(s), {} total on disk", snapshots.len(), format_size(total_bytes));
    println!();
    println!("Restore with: abox snapshot restore <name> --as <new-sandbox-id>");
    println!("Delete with:  abox snapshot delete <name>");

    Ok(())
}

async fn create_snapshot<W: WorkspacePort, R: SandboxRuntimePort>(
    name: &str,
    from: &str,
    orchestrator: &SandboxOrchestrator<W, R>,
    snap_mgr: &SnapshotManager,
) -> Result<()> {
    println!("Creating snapshot '{name}' from sandbox '{from}'...");

    // Verify the sandbox exists, then get the runtime's snapshot handles
    // (hypervisor API socket + filesystem-share socket names to re-pin).
    orchestrator.runtime_info(from).await?;
    let handles = orchestrator
        .memory_snapshot_handles(from)
        .ok_or_else(|| anyhow::anyhow!("this runtime does not support memory snapshots"))?;

    // Pause the sandbox so the snapshot is consistent.
    println!("  Pausing sandbox...");
    orchestrator.pause_sandbox(from).await?;

    // Create the snapshot. On failure, attempt to resume the sandbox so we
    // don't leave it stuck in the paused state.
    match snap_mgr.create_snapshot(&handles.api_socket, name, handles.virtiofs_sockets).await {
        Ok(path) => {
            orchestrator.resume_sandbox(from).await?;
            let size = abox_core::snapshot::dir_size(&path).unwrap_or(0);
            println!("  Resuming sandbox...");
            println!();
            println!("Snapshot '{}' created ({}) from sandbox '{}'", name, format_size(size), from);
            println!("Restore with: abox snapshot restore {name} --as <new-sandbox-id>");
            Ok(())
        }
        Err(e) => {
            let _ = orchestrator.resume_sandbox(from).await;
            Err(e)
        }
    }
}

async fn restore_snapshot<W: WorkspacePort, R: SandboxRuntimePort>(
    name: &str,
    sandbox_id: &str,
    orchestrator: &SandboxOrchestrator<W, R>,
    snap_mgr: &SnapshotManager,
) -> Result<()> {
    // Verify the snapshot exists before doing any work.
    if !snap_mgr.list_templates()?.iter().any(|t| t.name == name) {
        anyhow::bail!("Snapshot '{name}' not found. List with: abox snapshot list");
    }

    println!("Restoring snapshot '{name}' as sandbox '{sandbox_id}'...");
    println!();
    println!("Note: the restored sandbox resumes the VM state from when '{name}' was taken.");
    println!();

    // Restore through the orchestrator's proven path (the same one used by
    // `abox run --template`): it recreates the snapshot's virtiofsd backends,
    // keeps the VM process alive, and registers it in the manager so it shows
    // up in `abox list` and can be attached to. A fresh worktree is created for
    // the new sandbox id; the resumed guest reconnects its virtiofs shares to
    // it. The agent command is a placeholder — the snapshot resumes from memory
    // and does not re-run the guest init/runner.
    let params = CreateSandboxParams {
        task_id: sandbox_id.to_string(),
        base_branch: "main".to_string(),
        template: Some(name.to_string()),
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: Vec::new(),
        command: vec!["true".to_string()],
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: Vec::new(),
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    };

    orchestrator.create_sandbox(params).await?;

    println!("Snapshot '{name}' restored as sandbox '{sandbox_id}'.");
    println!("See it with:  abox list");
    println!("Attach with:  abox attach {sandbox_id}");

    Ok(())
}

fn prune_snapshots(snap_mgr: &SnapshotManager, keep: usize, dry_run: bool) -> Result<()> {
    let mut snapshots = snap_mgr.list_templates()?;

    if snapshots.is_empty() {
        println!("No snapshots to prune.");
        return Ok(());
    }

    // Sort oldest-first by on-disk modification time so "keep the N most
    // recent" is correct regardless of the user-chosen snapshot names (names
    // are arbitrary, so lexicographic order is not chronological). Snapshots
    // with no readable mtime sort oldest.
    snapshots.sort_by_key(|s| s.modified.unwrap_or(std::time::UNIX_EPOCH));

    let total = snapshots.len();
    if total <= keep {
        println!("Nothing to prune ({total} snapshots, keeping {keep}).");
        return Ok(());
    }

    let to_delete: Vec<_> = snapshots.iter().take(total - keep).collect();
    let to_keep: Vec<_> = snapshots.iter().skip(total - keep).collect();

    println!("Snapshots to keep ({}):", to_keep.len());
    for snap in &to_keep {
        println!("  {} ({})", snap.name, format_size(snap.size_bytes));
    }
    println!();
    println!("Snapshots to delete ({}):", to_delete.len());
    for snap in &to_delete {
        println!("  {} ({})", snap.name, format_size(snap.size_bytes));
    }
    println!();

    if dry_run {
        println!("Dry run — no changes made. Remove --dry-run to actually delete.");
        return Ok(());
    }

    let mut deleted = 0;
    let mut failed = 0;
    for snap in &to_delete {
        print!("  Deleting '{}'... ", snap.name);
        match snap_mgr.delete_template(&snap.name) {
            Ok(()) => {
                println!("done");
                deleted += 1;
            }
            Err(e) => {
                println!("error: {e}");
                failed += 1;
            }
        }
    }

    println!();
    if failed > 0 {
        println!("Pruned {deleted} snapshot(s) ({failed} failed).");
        anyhow::bail!("Failed to delete {failed} snapshot(s)");
    }
    println!("Pruned {deleted} snapshot(s).");

    Ok(())
}
