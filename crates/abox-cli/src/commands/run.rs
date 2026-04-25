//! `abox run` — Create and start a new sandbox.
//!
//! Creates a git worktree, boots a MicroVM, mounts the worktree via virtiofs,
//! and starts the specified agent inside the VM.

use super::validate_task_arg_for_runtime_dir;
use abox_core::config::AboxConfig;
use abox_core::sandbox::{CreateSandboxParams, SandboxOrchestrator};
use abox_core::util::validate_env_key;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Unique task identifier (e.g., "fix-auth"). Used as the sandbox name,
    /// branch name suffix, and worktree directory name.
    ///
    /// Must contain only ASCII letters, digits, hyphens, underscores, and
    /// dots. Must not start or end with a dot, contain consecutive dots, or
    /// exceed 64 characters. On very deep `runtime_dir` layouts, the effective
    /// limit may be lower because abox embeds the task ID in Unix socket paths.
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

    /// Environment variables to set inside the sandbox (KEY=VALUE).
    ///
    /// KEY must be a valid POSIX shell identifier: start with an ASCII letter
    /// or underscore, followed by ASCII letters, digits, or underscores only.
    /// Invalid keys are rejected immediately with a clear error.
    ///
    /// Can be repeated: `--env FOO=bar --env BAZ=qux`.
    #[arg(long = "env", short = 'e')]
    pub env_vars: Vec<String>,

    /// Kill sandbox after N seconds (exit code 124, like GNU timeout).
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Auto-remove sandbox (worktree + branch) after exit.
    #[arg(long)]
    pub ephemeral: bool,

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

/// Parse and validate a single `KEY=VALUE` environment variable string.
///
/// Returns `(key, value)` on success or a descriptive error. The key is
/// validated against POSIX shell identifier rules via [`validate_env_key`].
/// This is the single parsing entry point for `--env` arguments.
fn parse_env_var(s: &str) -> Result<(String, String)> {
    let mut parts = s.splitn(2, '=');
    let key = parts.next().unwrap_or("").to_string();
    let value = parts.next().unwrap_or("").to_string();

    validate_env_key(&key).map_err(|e| anyhow::anyhow!("--env {s:?}: {e}"))?;

    Ok((key, value))
}

fn selected_managed_agent(command: &[String]) -> Option<&str> {
    command.first().map(String::as_str).and_then(|cmd| match cmd {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        _ => None,
    })
}

fn ensure_managed_agent_ready(command: &[String], config: &AboxConfig) -> Result<()> {
    let Some(agent) = selected_managed_agent(command) else {
        return Ok(());
    };

    let (enabled, host_credential_file, provider_label) = match agent {
        "claude" => (
            config.auth.providers.claude.enabled,
            config.auth.providers.claude.host_credential_file(),
            "Claude Code",
        ),
        "codex" => (
            config.auth.providers.codex.enabled,
            config.auth.providers.codex.host_credential_file(),
            "Codex",
        ),
        _ => return Ok(()),
    };

    let expanded = abox_core::policy::expand_tilde(&host_credential_file);
    let config_path = AboxConfig::default_path()
        .map_or_else(|_| "~/.abox/config.toml".to_string(), |p| p.display().to_string());

    if !enabled {
        anyhow::bail!(
            "{provider_label} is not enabled for managed auth.\n\n\
             Enable it under [auth.providers.{agent}] in {config_path}, then re-run `abox init` \
             or edit the config manually."
        );
    }

    if !std::path::Path::new(&expanded).exists() {
        anyhow::bail!(
            "{provider_label} is enabled, but host credentials were not found at {expanded}.\n\n\
             Log in to {provider_label} on the host, or disable [auth.providers.{agent}] in \
             {config_path} if you do not want abox to manage it."
        );
    }

    Ok(())
}

pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: RunArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
    policy: std::sync::Arc<abox_core::policy::PolicyEngine>,
    root_ca: std::sync::Arc<abox_core::ca::RootCa>,
) -> Result<()> {
    // ── Input validation (trust boundary) ────────────────────────────────
    // Validate the task ID before it is used as a branch name, worktree
    // directory, socket path, log file name, or PID file name.
    validate_task_arg_for_runtime_dir(&args.task, &orchestrator.runtime_dir())
        .map_err(|e| anyhow::anyhow!("--task {:?}: {e}", args.task))?;

    // Parse and validate every --env KEY=VALUE argument. Fail fast on the
    // first invalid key so the user gets a clear error before any VM work.
    let env_vars: Vec<(String, String)> =
        args.env_vars.iter().map(|s| parse_env_var(s)).collect::<Result<Vec<_>>>()?;

    ensure_managed_agent_ready(&args.command, orchestrator.config())?;

    if args.detach {
        return spawn_detached(&args, orchestrator);
    }

    let params = CreateSandboxParams {
        task_id: args.task.clone(),
        base_branch: args.base,
        template: args.template,
        memory_mib: args.memory,
        vcpus: args.cpus,
        user: args.user,
        env_vars,
        command: args.command,
        timeout_secs: args.timeout,
        ephemeral: args.ephemeral,
        ca_cert_pem: None, // Populated by run_sandbox from the loaded RootCa.
    };

    println!("Sandbox '{}' starting...", args.task);
    let exit_code = orchestrator.run_sandbox(params, policy, root_ca).await?;

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
///
/// Note: task ID validation is performed before reaching this function, so
/// the task string is already known-safe when used to construct file paths.
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
    use super::{
        ensure_managed_agent_ready, parse_env_var, selected_managed_agent, strip_detach_flag,
    };
    use abox_core::config::AboxConfig;

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

    // ── parse_env_var tests ───────────────────────────────────────────────

    #[test]
    fn parse_env_var_accepts_valid_key() {
        let (k, v) = parse_env_var("FOO=bar").unwrap();
        assert_eq!(k, "FOO");
        assert_eq!(v, "bar");
    }

    #[test]
    fn parse_env_var_accepts_empty_value() {
        let (k, v) = parse_env_var("FOO=").unwrap();
        assert_eq!(k, "FOO");
        assert_eq!(v, "");
    }

    #[test]
    fn parse_env_var_accepts_value_with_equals() {
        // Value may itself contain '=' — only the first '=' is the separator.
        let (k, v) = parse_env_var("FOO=a=b=c").unwrap();
        assert_eq!(k, "FOO");
        assert_eq!(v, "a=b=c");
    }

    #[test]
    fn parse_env_var_rejects_digit_start_key() {
        assert!(parse_env_var("1FOO=bar").is_err());
    }

    #[test]
    fn parse_env_var_rejects_hyphen_in_key() {
        assert!(parse_env_var("FOO-BAR=baz").is_err());
    }

    #[test]
    fn parse_env_var_rejects_shell_injection_key() {
        // A crafted key that would break `export <key>='value'` shell syntax.
        // Note: injection in the *value* is safe because sh_escape() wraps
        // values in single quotes. Only the *key* is interpolated unescaped.
        assert!(parse_env_var("FOO$(cmd)=bar").is_err()); // $ in key
        assert!(parse_env_var("FOO BAR=baz").is_err()); // space in key
        assert!(parse_env_var("FOO;evil=bar").is_err()); // semicolon in key
        assert!(parse_env_var("=value").is_err()); // empty key
    }

    #[test]
    fn parse_env_var_rejects_empty_key() {
        assert!(parse_env_var("=value").is_err());
    }

    #[test]
    fn selected_managed_agent_identifies_supported_agents() {
        assert_eq!(selected_managed_agent(&["claude".into()]), Some("claude"));
        assert_eq!(selected_managed_agent(&["codex".into(), "--quiet".into()]), Some("codex"));
        assert_eq!(selected_managed_agent(&["echo".into(), "hi".into()]), None);
    }

    #[test]
    fn ensure_managed_agent_ready_allows_arbitrary_commands() {
        let config = AboxConfig::default();
        assert!(ensure_managed_agent_ready(&["echo".into(), "hi".into()], &config).is_ok());
    }

    #[test]
    fn ensure_managed_agent_ready_rejects_disabled_provider() {
        let config = AboxConfig::default();
        let err = ensure_managed_agent_ready(&["claude".into()], &config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Claude Code is not enabled"));
    }

    #[test]
    fn ensure_managed_agent_ready_rejects_missing_host_credentials() {
        let mut config = AboxConfig::default();
        config.auth.providers.claude.enabled = true;
        config.auth.providers.claude.host_credential_file =
            Some("/definitely/missing/abox-claude-auth.json".into());

        let err = ensure_managed_agent_ready(&["claude".into()], &config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("host credentials were not found"));
    }

    #[test]
    fn ensure_managed_agent_ready_accepts_present_host_credentials() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut config = AboxConfig::default();
        config.auth.providers.codex.enabled = true;
        config.auth.providers.codex.host_credential_file =
            Some(tmp.path().to_string_lossy().to_string());

        assert!(ensure_managed_agent_ready(&["codex".into()], &config).is_ok());
    }
}
