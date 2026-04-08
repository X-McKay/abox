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

    /// Detach after launching the sandbox instead of blocking on the agent.
    ///
    /// The supervisor is re-exec'd as a background process; its stdout/stderr
    /// (the agent's console output + boot banner) are appended to
    /// `<runtime>/console-<task>.log`, and the supervisor's PID is written
    /// to `<runtime>/run-<task>.pid` so `abox stop <task>` can tear it down.
    #[arg(long)]
    pub detach: bool,

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
    if args.detach {
        return spawn_detached(&args, orchestrator);
    }

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

/// Re-exec the current binary without `--detach`, redirecting stdout/err
/// to a per-sandbox console log, then return so the original `abox run`
/// invocation completes immediately.
///
/// We use a re-exec strategy (rather than daemon(3)-style double-fork)
/// because it's debuggable: `ps` shows a real `abox run` process, and the
/// supervisor's argv reflects exactly what the user typed minus the
/// `--detach` flag.
fn spawn_detached<W: WorkspacePort, V: VmPort>(
    args: &RunArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    let runtime = orchestrator.runtime_dir();
    std::fs::create_dir_all(&runtime)?;
    let console_log = runtime.join(format!("console-{}.log", args.task));
    let pid_file = runtime.join(format!("run-{}.pid", args.task));

    // Re-exec the same binary; strip `--detach` from the new argv so the
    // child runs in foreground mode and supervises the VM normally.
    let exe = std::env::current_exe()?;
    let raw_argv: Vec<String> = std::env::args().skip(1).collect();
    let child_argv = strip_detach_flag(&raw_argv);

    let log = std::fs::OpenOptions::new().create(true).append(true).open(&console_log)?;
    let log_err = log.try_clone()?;

    use std::os::unix::process::CommandExt as _;
    let child = std::process::Command::new(&exe)
        .args(&child_argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0) // become its own process group so the parent
        // shell exiting doesn't take it down
        .spawn()?;

    let child_pid = child.id();
    std::fs::write(&pid_file, format!("{child_pid}\n"))?;
    println!("Sandbox '{}' detached (pid {child_pid}).", args.task);
    println!("  logs: {}", console_log.display());
    println!("  stop: abox stop {}", args.task);

    // Intentionally do NOT await the child — we want the CLI to return
    // immediately. Forgetting the Child handle prevents Drop from sending
    // SIGKILL on early exit paths.
    std::mem::forget(child);
    Ok(())
}

/// Remove every occurrence of `--detach` from an argv list.
fn strip_detach_flag(argv: &[String]) -> Vec<String> {
    argv.iter().filter(|a| a.as_str() != "--detach").cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::strip_detach_flag;

    #[test]
    fn test_strip_detach_in_middle() {
        let raw = vec![
            "run".to_string(),
            "--task".to_string(),
            "x".to_string(),
            "--detach".to_string(),
            "--".to_string(),
            "claude".to_string(),
        ];
        let stripped = strip_detach_flag(&raw);
        assert_eq!(stripped, vec!["run", "--task", "x", "--", "claude"]);
        assert!(!stripped.iter().any(|a| a == "--detach"));
    }

    #[test]
    fn test_strip_detach_absent_is_noop() {
        let raw = vec!["run".to_string(), "--task".to_string(), "x".to_string(), "--".to_string()];
        assert_eq!(strip_detach_flag(&raw), raw);
    }

    #[test]
    fn test_strip_detach_at_start() {
        let raw = vec!["--detach".to_string(), "run".to_string(), "--task".to_string()];
        assert_eq!(strip_detach_flag(&raw), vec!["run", "--task"]);
    }
}
