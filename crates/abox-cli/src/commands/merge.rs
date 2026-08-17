//! `abox merge` — Merge a sandbox's branch back into the base branch.

use super::validate_task_arg;
use abox_core::runtime::SandboxRuntimePort;
use abox_core::sandbox::SandboxOrchestrator;
use abox_core::workspace::{MergeOptions, MergeOutcome, WorkspacePort};
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// The sandbox ID whose branch to merge.
    pub task: String,

    /// Base branch to merge into. Default: "main".
    #[arg(long, default_value = "main")]
    pub base: String,

    /// Acknowledge one exact repository-relative path required by the host
    /// merge policy. Repeat for every reviewed sensitive path.
    #[arg(long = "approve-path", value_name = "PATH")]
    pub approve_paths: Vec<std::path::PathBuf>,
}

pub async fn execute<W: WorkspacePort, R: SandboxRuntimePort>(
    args: &MergeArgs,
    orchestrator: &SandboxOrchestrator<W, R>,
) -> Result<()> {
    validate_task_arg(&args.task)?;

    let options = MergeOptions::with_approved_paths(args.approve_paths.clone());
    match orchestrator.merge(&args.task, &args.base, &options).await? {
        MergeOutcome::Merged => {
            println!("Successfully merged agent/{} into {}.", args.task, args.base);
        }
        MergeOutcome::Conflicts(conflicts) => {
            println!("Merge failed with {} conflict(s):", conflicts.len());
            for conflict in &conflicts {
                println!("  {conflict}");
            }
            println!();
            println!("The merge was aborted. Resolve conflicts manually.");
        }
        MergeOutcome::Blocked(blocked) => {
            eprintln!("Merge blocked by host validation:");
            for violation in &blocked.violations {
                eprintln!("  - {violation}");
            }
            anyhow::bail!("merge was not performed");
        }
    }

    Ok(())
}
