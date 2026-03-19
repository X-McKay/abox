//! `abox list` — List all active sandboxes.

use abox_core::sandbox::SandboxOrchestrator;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;

pub async fn execute<W: WorkspacePort, V: VmPort>(
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    let sandboxes = orchestrator.list_sandboxes().await?;

    if sandboxes.is_empty() {
        println!("No active sandboxes.");
        return Ok(());
    }

    // Print a table
    println!("{:<16} {:<24} {:<10} {:<8} {:<8}", "ID", "BRANCH", "STATE", "PID", "AHEAD");
    println!("{}", "-".repeat(70));

    for s in &sandboxes {
        println!(
            "{:<16} {:<24} {:<10} {:<8} {:<8}",
            s.id, s.branch, s.vm_state, s.vm_pid, s.commits_ahead
        );
    }

    println!();
    println!("{} sandbox(es) active", sandboxes.len());

    Ok(())
}
