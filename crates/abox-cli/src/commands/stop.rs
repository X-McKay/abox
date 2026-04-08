//! `abox stop` — Stop a sandbox and optionally clean up.

use abox_core::sandbox::SandboxOrchestrator;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct StopArgs {
    /// The sandbox ID to stop.
    pub task: String,

    /// Also remove the worktree and delete the branch.
    #[arg(long)]
    pub clean: bool,
}

pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: StopArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    orchestrator.stop_sandbox(&args.task, args.clean).await?;

    if args.clean {
        println!("Sandbox '{}' stopped and cleaned up.", args.task);
    } else {
        println!("Sandbox '{}' stopped. Worktree preserved. Use --clean to remove.", args.task);
    }

    Ok(())
}
