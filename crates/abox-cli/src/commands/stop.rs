//! `abox stop` — Stop a sandbox and optionally clean up.

use super::validate_task_arg;
use abox_core::runtime::SandboxRuntimePort;
use abox_core::sandbox::SandboxOrchestrator;
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

pub async fn execute<W: WorkspacePort, R: SandboxRuntimePort>(
    args: StopArgs,
    orchestrator: &SandboxOrchestrator<W, R>,
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
                // The pid file can outlive its supervisor (crash before
                // cleanup), and the OS may have recycled the PID. Verify the
                // process is really this task's supervisor before signaling
                // (issue #38); a dead or foreign PID means the file is stale.
                if pid_is_task_supervisor(pid, &args.task) {
                    // Send SIGTERM via /bin/kill to avoid pulling in
                    // libc/nix for a single syscall.
                    let _ = std::process::Command::new("kill")
                        .args(["-TERM", &pid.to_string()])
                        .status();
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
                } else {
                    eprintln!(
                        "Stale pid file for '{}' (pid {pid} is not its supervisor); removing.",
                        args.task
                    );
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

/// Check that `pid` is currently running as the detached-run supervisor for
/// `task`, by inspecting its command line via `ps` (portable across Linux
/// and macOS; avoids a libc/procfs dependency for one lookup). A dead PID
/// yields no output and fails the check.
fn pid_is_task_supervisor(pid: i32, task: &str) -> bool {
    let output =
        std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "args="]).output();
    match output {
        Ok(out) if out.status.success() => {
            cmdline_is_task_supervisor(String::from_utf8_lossy(&out.stdout).trim(), task)
        }
        _ => false,
    }
}

/// Decide whether a process command line belongs to the detached-run
/// supervisor for `task` — i.e. an `abox run` whose `--task` option (before
/// any `--` delimiter) names exactly this task. Guards `abox stop` against
/// signaling a recycled PID recorded in a stale pid file (issue #38).
fn cmdline_is_task_supervisor(cmdline: &str, task: &str) -> bool {
    let mut tokens = cmdline.split_whitespace();
    let Some(argv0) = tokens.next() else {
        return false;
    };
    if std::path::Path::new(argv0).file_name().is_none_or(|n| n != "abox") {
        return false;
    }
    let mut expect_task_value = false;
    for tok in tokens {
        if tok == "--" {
            // Guest-command region: anything past here is not an abox option.
            return false;
        }
        if expect_task_value {
            return tok == task;
        }
        if tok == "--task" {
            expect_task_value = true;
        } else if let Some(value) = tok.strip_prefix("--task=") {
            return value == task;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::cmdline_is_task_supervisor;

    #[test]
    fn test_supervisor_match_space_form() {
        assert!(cmdline_is_task_supervisor(
            "/home/u/.cargo/bin/abox run --task x -- sleep 60",
            "x"
        ));
    }

    #[test]
    fn test_supervisor_match_equals_form() {
        assert!(cmdline_is_task_supervisor("/usr/local/bin/abox run --task=x --ephemeral", "x"));
    }

    #[test]
    fn test_supervisor_rejects_other_task() {
        assert!(!cmdline_is_task_supervisor("/usr/local/bin/abox run --task y -- sleep 60", "x"));
    }

    #[test]
    fn test_supervisor_rejects_non_abox_binary() {
        assert!(!cmdline_is_task_supervisor("/usr/bin/python run --task x", "x"));
    }

    #[test]
    fn test_supervisor_rejects_unrelated_process() {
        assert!(!cmdline_is_task_supervisor("nginx: worker process", "x"));
    }

    #[test]
    fn test_supervisor_ignores_task_flag_after_delimiter() {
        // `--task x` appearing only in the guest command must not match.
        assert!(!cmdline_is_task_supervisor(
            "/usr/local/bin/abox run --task other -- mytool --task x",
            "x"
        ));
    }

    #[test]
    fn test_supervisor_rejects_empty_cmdline() {
        assert!(!cmdline_is_task_supervisor("", "x"));
    }
}
