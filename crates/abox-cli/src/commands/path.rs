//! `abox path` — Print the host worktree path for a sandbox.
//!
//! The worktree is the bind-mounted `/workspace`, so this is the supported way
//! to collect what an agent wrote without hardcoding `~/.abox/worktrees/<task>`.

use abox_core::runtime::SandboxRuntimePort;
use abox_core::sandbox::SandboxOrchestrator;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct PathArgs {
    /// The task/sandbox identifier.
    pub task: String,
}

pub fn execute<W: WorkspacePort, R: SandboxRuntimePort>(
    args: &PathArgs,
    orchestrator: &SandboxOrchestrator<W, R>,
) -> Result<()> {
    match orchestrator.worktree_info(&args.task)? {
        Some(info) => {
            println!("{}", info.path.display());
            Ok(())
        }
        None => anyhow::bail!("No sandbox named '{}'.", args.task),
    }
}
