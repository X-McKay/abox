//! `abox run` — Create and start a new sandbox.
//!
//! Creates a git worktree, boots a MicroVM, mounts the worktree via virtiofs,
//! and starts the specified agent inside the VM.

use abox_core::sandbox::{CreateSandboxParams, SandboxOrchestrator};
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Unique task identifier (e.g., "fix-auth"). Used as the sandbox name,
    /// branch name suffix, and worktree directory name.
    #[arg(long)]
    pub task: String,

    /// Base branch to fork from. Default: "main".
    #[arg(long, default_value = "main")]
    pub base: String,

    /// Restore from a snapshot template instead of booting fresh.
    #[arg(long)]
    pub template: Option<String>,

    /// Memory allocation in MiB. Overrides config default.
    #[arg(long)]
    pub memory: Option<u32>,

    /// Number of vCPUs. Overrides config default.
    #[arg(long)]
    pub cpus: Option<u8>,

    /// Unix user to run the agent as inside the VM.
    #[arg(long)]
    pub user: Option<String>,

    /// Environment variables to set (KEY=VALUE). Can be repeated.
    #[arg(long = "env", short = 'e')]
    pub env_vars: Vec<String>,

    /// The agent command to run inside the VM.
    /// Everything after `--` is treated as the command.
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: RunArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
    policy: std::sync::Arc<abox_core::policy::PolicyEngine>,
) -> Result<()> {
    let env_vars: Vec<(String, String)> = args
        .env_vars
        .iter()
        .filter_map(|s| {
            let mut parts = s.splitn(2, '=');
            let key = parts.next()?.to_string();
            let value = parts.next().unwrap_or("").to_string();
            Some((key, value))
        })
        .collect();

    let params = CreateSandboxParams {
        task_id: args.task.clone(),
        base_branch: args.base,
        template: args.template,
        memory_mib: args.memory,
        vcpus: args.cpus,
        user: args.user,
        env_vars,
        command: args.command,
    };

    println!("Sandbox '{}' starting...", args.task);
    let exit_code = orchestrator.run_sandbox(params, policy).await?;

    if exit_code == 0 {
        println!("\nSandbox '{}' exited cleanly.", args.task);
        Ok(())
    } else {
        // Surface the agent's exact exit code to the OS. We intentionally
        // bypass anyhow here: a non-zero agent exit is not an abox error —
        // it's a successful run of a failing program, and users scripting
        // `abox run` rely on the exit code matching what they would have
        // seen if they had run the command directly on the host.
        eprintln!("\nSandbox '{}' exited with code {}.", args.task, exit_code);
        std::process::exit(exit_code);
    }
}
