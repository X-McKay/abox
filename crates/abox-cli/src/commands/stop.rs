//! `abox stop` — Stop a sandbox and optionally clean up.

use super::validate_task_arg;
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
    validate_task_arg(&args.task)?;

    // If this sandbox was launched with `abox run --detach`, kill the
    // supervisor process first so it tears down its own VM. The
    // orchestrator's stop_sandbox below will then clean up any residual
    // state. If the pid file is missing or the process is already gone,
    // fall through to the orchestrator's own stop path — `--clean` still
    // removes the worktree even if no VM is registered.
    let pid_file = orchestrator.runtime_dir().join(format!("run-{}.pid", args.task));
    if pid_file.exists() {
        if let Ok(s) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = s.trim().parse::<i32>() {
                // Send SIGTERM via /bin/kill to avoid pulling in libc/nix
                // for a single syscall.
                let _ =
                    std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
                // Best-effort wait for the supervisor to clean up.
                for _ in 0..30 {
                    let still_alive = std::process::Command::new("kill")
                        .args(["-0", &pid.to_string()])
                        .status()
                        .is_ok_and(|s| s.success());
                    if !still_alive {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        let _ = std::fs::remove_file(&pid_file);
    }

    orchestrator.stop_sandbox(&args.task, args.clean).await?;

    if args.clean {
        println!("Sandbox '{}' stopped and cleaned up.", args.task);
    } else {
        println!("Sandbox '{}' stopped. Worktree preserved. Use --clean to remove.", args.task);
    }

    Ok(())
}
