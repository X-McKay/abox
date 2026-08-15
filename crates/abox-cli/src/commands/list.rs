//! `abox list` — List all active sandboxes.

use abox_core::runtime::SandboxRuntimePort;
use abox_core::sandbox::{SandboxOrchestrator, SandboxStatus};
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Emit machine-readable JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// Stable JSON contract for one sandbox. Field names are a supported API.
#[derive(Debug, Serialize)]
pub struct ListItem {
    pub id: String,
    pub branch: String,
    pub state: String,
    pub pid: u32,
    pub ahead: usize,
    pub worktree_path: String,
}

impl From<&SandboxStatus> for ListItem {
    fn from(s: &SandboxStatus) -> Self {
        Self {
            id: s.id.clone(),
            branch: s.branch.clone(),
            state: s.vm_state.clone(),
            pid: s.vm_pid,
            ahead: s.commits_ahead,
            worktree_path: s.worktree_path.clone(),
        }
    }
}

pub async fn execute<W: WorkspacePort, R: SandboxRuntimePort>(
    args: &ListArgs,
    orchestrator: &SandboxOrchestrator<W, R>,
) -> Result<()> {
    let sandboxes = orchestrator.list_sandboxes().await?;

    if args.json {
        let items: Vec<ListItem> = sandboxes.iter().map(ListItem::from).collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if sandboxes.is_empty() {
        println!("No active sandboxes.");
        return Ok(());
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use abox_core::sandbox::SandboxStatus;

    #[test]
    fn list_json_has_stable_fields() {
        let s = SandboxStatus {
            id: "t".into(),
            branch: "agent/t".into(),
            worktree_path: "/w/t".into(),
            vm_state: "running".into(),
            vm_pid: 42,
            commits_ahead: 3,
        };
        let item = ListItem::from(&s);
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["id"], "t");
        assert_eq!(json["state"], "running");
        assert_eq!(json["pid"], 42);
        assert_eq!(json["ahead"], 3);
        assert_eq!(json["worktree_path"], "/w/t");
    }
}
