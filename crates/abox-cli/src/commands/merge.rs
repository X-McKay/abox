//! `abox merge` — Merge a sandbox's branch back into the base branch.

use super::validate_task_arg;
use abox_core::sandbox::SandboxOrchestrator;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// The sandbox ID whose branch to merge.
    pub task: String,

    /// Base branch to merge into. Default: "main".
    #[arg(long, default_value = "main")]
    pub base: String,
}

pub fn execute<W: WorkspacePort, V: VmPort>(
    args: &MergeArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    validate_task_arg(&args.task)?;

    let conflicts = orchestrator.merge(&args.task, &args.base)?;

    if conflicts.is_empty() {
        println!("Successfully merged agent/{} into {}.", args.task, args.base);
    } else {
        println!("Merge failed with {} conflict(s):", conflicts.len());
        for conflict in &conflicts {
            println!("  {conflict}");
        }
        println!();
        println!("The merge was aborted. Resolve conflicts manually.");
    }

    Ok(())
}
