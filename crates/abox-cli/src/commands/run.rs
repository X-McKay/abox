//! `abox run` — Create and start a new sandbox.
//!
//! Creates a git worktree, boots a MicroVM, mounts the worktree via virtiofs,
//! and starts the specified agent inside the VM.

use super::env::ensure_warm_environment_for_run;
use super::validate_task_arg_for_runtime_dir;
use abox_core::config::AboxConfig;
use abox_core::project::{
    is_approved, project_cache_root, record_approval, standalone_network_scope, EnvironmentProfile,
    NetworkMode, ProjectConfig, ResolvedProjectConfig,
};
use abox_core::sandbox::{CreateSandboxParams, SandboxOrchestrator};
use abox_core::util::validate_env_key;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

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

    /// Override the effective repo network mode for this run.
    #[arg(long, value_enum)]
    pub network: Option<RunNetworkMode>,

    /// Inline prompt content. Only known managed agents (claude, codex) can
    /// consume a prompt; for any other `--` command use `--input-file` instead.
    #[arg(long, conflicts_with = "prompt_file")]
    pub prompt: Option<String>,

    /// Load prompt content from a file on the host. Only known managed agents
    /// (claude, codex) can consume it; for any other `--` command use
    /// `--input-file` instead.
    #[arg(long, conflicts_with = "prompt")]
    pub prompt_file: Option<PathBuf>,

    /// Stage an arbitrary host file into `/abox-meta/inputs/` (read-only) for
    /// any `--` command. Format: `<hostpath>[:<guestname>]`. Repeatable.
    /// The guest sees `ABOX_INPUT_DIR=/abox-meta/inputs`, plus
    /// `ABOX_INPUT_FILE` when exactly one is given.
    #[arg(long = "input-file")]
    pub input_files: Vec<String>,

    /// Kill sandbox after N seconds (exit code 124, like GNU timeout).
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Auto-remove sandbox (worktree + branch) after exit.
    #[arg(long)]
    pub ephemeral: bool,

    /// Skip automatic guest-native environment refresh before launch.
    ///
    /// When a repo config defines durable caches plus a prepare flow, `abox run`
    /// normally refreshes stale or missing warm state automatically. Use this
    /// flag to bypass that refresh for a single run.
    #[arg(long)]
    pub no_warm: bool,

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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RunNetworkMode {
    Safe,
    Scoped,
    Open,
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

/// A parsed `--input-file` argument: a host file plus the name it will take
/// inside `/abox-meta/inputs/` in the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InputFileSpec {
    host_path: PathBuf,
    guest_name: String,
}

/// Validate a guest-side input file name. Must be a single safe path component
/// so it cannot escape `/abox-meta/inputs/` or collide with reserved meta files.
fn validate_guest_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        anyhow::bail!("{name:?} is not a valid file name");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        anyhow::bail!("{name:?} may contain only ASCII letters, digits, '.', '_', '-'");
    }
    Ok(())
}

/// Parse `<hostpath>[:<guestname>]`. The guest name is taken after the last
/// `:` when that suffix is a plain file name (no `/`); otherwise the whole
/// string is the host path and the guest name is the host file's basename.
///
/// If a `:` is present and the suffix after it is non-empty but contains a
/// `/`, it is treated as an invalid explicit guest name (path traversal attempt)
/// rather than falling back to basename derivation.
fn parse_input_file_arg(s: &str) -> Result<InputFileSpec> {
    let (host_str, guest_name) = match s.rsplit_once(':') {
        Some((host, name)) if !name.is_empty() && !name.contains('/') => {
            (host.to_string(), name.to_string())
        }
        Some((_host, name)) if !name.is_empty() => {
            // Non-empty suffix that contains '/' — reject immediately rather
            // than silently deriving a basename, since the user clearly tried
            // to specify a guest path and that is not allowed.
            anyhow::bail!(
                "--input-file {s:?}: guest name {name:?} must be a plain file name, not a path"
            );
        }
        _ => {
            let derived =
                Path::new(s).file_name().and_then(|n| n.to_str()).map(str::to_string).ok_or_else(
                    || {
                        anyhow::anyhow!(
                            "--input-file {s:?}: cannot derive a guest file name; \
                         specify one as <hostpath>:<name>"
                        )
                    },
                )?;
            (s.to_string(), derived)
        }
    };
    validate_guest_name(&guest_name).map_err(|e| anyhow::anyhow!("--input-file {s:?}: {e}"))?;
    Ok(InputFileSpec { host_path: PathBuf::from(host_str), guest_name })
}

/// Per-file and total size budget for staged `--input-file` inputs.
const MAX_INPUT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INPUT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Validate parsed `--input-file` specs (each must exist, be a regular file,
/// stay within the size budget, and map to a unique guest name) and return the
/// staged-file descriptors. Pure except for reading host file metadata.
fn resolve_input_files(specs: &[InputFileSpec]) -> Result<Vec<abox_core::vm::InputFile>> {
    let mut input_total: u64 = 0;
    let mut input_files: Vec<abox_core::vm::InputFile> = Vec::with_capacity(specs.len());
    for spec in specs {
        let meta = std::fs::metadata(&spec.host_path).with_context(|| {
            format!("--input-file: cannot read host file {}", spec.host_path.display())
        })?;
        if !meta.is_file() {
            anyhow::bail!(
                "--input-file {}: not a regular file (directories and special files \
                 cannot be staged)",
                spec.host_path.display()
            );
        }
        if meta.len() > MAX_INPUT_FILE_BYTES {
            anyhow::bail!(
                "--input-file {}: {} bytes exceeds the {} byte per-file limit",
                spec.host_path.display(),
                meta.len(),
                MAX_INPUT_FILE_BYTES
            );
        }
        input_total += meta.len();
        if input_total > MAX_INPUT_TOTAL_BYTES {
            anyhow::bail!(
                "--input-file: total staged input exceeds the {MAX_INPUT_TOTAL_BYTES} byte limit"
            );
        }
        if input_files.iter().any(|f| f.guest_name == spec.guest_name) {
            anyhow::bail!(
                "--input-file: two inputs both stage to /abox-meta/inputs/{}; \
                 give one an explicit name as <hostpath>:<name>",
                spec.guest_name
            );
        }
        input_files.push(abox_core::vm::InputFile {
            host_path: spec.host_path.clone(),
            guest_name: spec.guest_name.clone(),
        });
    }
    Ok(input_files)
}

/// Refuse a host-port bridge whose guest port collides with the in-guest HTTPS
/// egress proxy listener. Both run as `socat TCP-LISTEN:<port>` inside the
/// guest, so a collision would make one silently fail to bind.
fn ensure_host_ports_no_egress_collision(
    host_ports: &[abox_core::services::HostPortBridge],
    egress_port: u16,
) -> Result<()> {
    if let Some(hp) = host_ports.iter().find(|hp| hp.guest == egress_port) {
        anyhow::bail!(
            "[[host_ports]] guest port {} collides with the in-guest HTTPS egress proxy \
             listener (proxy.egress_port = {egress_port}); choose a different guest port.",
            hp.guest
        );
    }
    Ok(())
}

/// Refuse `[[host_ports]]` with a `--template` restore. A restored VM resumes
/// an already-booted guest and does not re-run guest init, so newly staged
/// `/abox-meta/services` host-port lines are never read and the in-guest
/// listener is never created (mirrors how `[services]` rejects `--template`).
fn ensure_host_ports_not_templated(
    host_ports: &[abox_core::services::HostPortBridge],
    template: Option<&str>,
) -> Result<()> {
    if template.is_some() && !host_ports.is_empty() {
        anyhow::bail!(
            "[[host_ports]] is not supported with --template restores.\n\
             A restored VM does not re-run guest init, so the in-guest port \
             listener is never created. Remove --template for this run, or remove \
             [[host_ports]] from .abox/project.toml."
        );
    }
    Ok(())
}

/// Start the project's declared service sidecars and return their host→guest
/// bridges, injecting each connection URL into `env_vars`.
///
/// Returns an empty vec when no services are declared. Services are not
/// supported alongside `--template` restores (the bridges/env are not captured
/// in a snapshot); that combination is rejected. On any failure after a
/// container has started, all of this sandbox's containers are torn down so we
/// don't leak them.
fn start_project_services(
    services: &std::collections::HashMap<String, abox_core::services::ServiceConfig>,
    task_id: &str,
    template: Option<&str>,
    env_vars: &mut Vec<(String, String)>,
) -> Result<Vec<abox_core::services::ServiceBridge>> {
    use abox_core::services::{
        docker_available, plan_service_bridge, start_service, stop_sandbox_services,
        wait_for_service_ready, RunningService,
    };

    if services.is_empty() {
        return Ok(Vec::new());
    }
    if template.is_some() {
        anyhow::bail!(
            "Service sidecars are not supported with --template restores.\n\
             Remove --template for this run, or remove [services] from .abox/project.toml."
        );
    }
    if !docker_available() {
        anyhow::bail!(
            "This repo declares [services] in .abox/project.toml, but Docker is not available.\n\
             Install Docker and ensure it is running, or remove the services."
        );
    }

    // Deterministic ordering so vsock-port assignment is stable across runs.
    let mut names: Vec<&String> = services.keys().collect();
    names.sort();

    let mut bridges = Vec::new();
    let result = (|| -> Result<()> {
        for (index, name) in names.iter().enumerate() {
            let cfg = &services[*name];
            println!("Starting service sidecar '{name}'...");
            let running: RunningService = start_service(name, cfg, task_id)?;
            if cfg.wait {
                let container = format!("abox-{name}-{task_id}");
                wait_for_service_ready(name, &container, 30)?;
            }
            if name.as_str() == "ollama" && !cfg.models.is_empty() {
                abox_core::services::pull_ollama_models(
                    &format!("abox-{name}-{task_id}"),
                    &cfg.models,
                )?;
            }
            let bridge = plan_service_bridge(&running, index);
            println!("  {} → {} (guest 127.0.0.1:{})", name, bridge.env_var, bridge.guest_port);
            env_vars.push((bridge.env_var.clone(), bridge.guest_url.clone()));
            bridges.push(bridge);
        }
        Ok(())
    })();

    if let Err(e) = result {
        let _ = stop_sandbox_services(task_id);
        return Err(e.context("Failed to start service sidecars; started containers were removed"));
    }

    Ok(bridges)
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

/// Refuse `[[host_ports]]` unless the effective network mode permits an
/// unmediated path to a host service. `safe` means "only the host-managed
/// surface", which a host-port bridge is not.
fn ensure_host_ports_allowed(
    host_ports: &[abox_core::services::HostPortBridge],
    mode: NetworkMode,
) -> Result<()> {
    if !host_ports.is_empty() && mode == NetworkMode::Safe {
        anyhow::bail!(
            "[[host_ports]] requires network mode 'scoped' or 'open', but the \
             effective mode is 'safe'.\n\n\
             A host-port bridge gives the sandbox an unmediated path to a host \
             service, so it is refused in 'safe' mode. Set network.mode = \
             \"scoped\" in .abox/project.toml (or pass --network scoped) to enable it."
        );
    }
    Ok(())
}

pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: RunArgs,
    repo_root: &Path,
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
    let mut env_vars: Vec<(String, String)> =
        args.env_vars.iter().map(|s| parse_env_var(s)).collect::<Result<Vec<_>>>()?;

    // Resolve --input-file specs into staged-file descriptors (existence,
    // regular-file, size budget, and guest-name uniqueness validated within),
    // then expose them to any command via /abox-meta/inputs.
    let input_specs: Vec<InputFileSpec> =
        args.input_files.iter().map(|s| parse_input_file_arg(s)).collect::<Result<_>>()?;
    let input_files = resolve_input_files(&input_specs)?;
    if !input_files.is_empty() {
        env_vars.push(("ABOX_INPUT_DIR".to_string(), "/abox-meta/inputs".to_string()));
        if let [only] = input_files.as_slice() {
            env_vars.push((
                "ABOX_INPUT_FILE".to_string(),
                format!("/abox-meta/inputs/{}", only.guest_name),
            ));
        }
    }

    ensure_managed_agent_ready(&args.command, orchestrator.config())?;

    let requested_network = args.network.map(Into::into);
    let project_config = ProjectConfig::load(repo_root)?;
    let project_services = project_config.as_ref().map(|c| c.services.clone()).unwrap_or_default();
    let project_host_ports =
        project_config.as_ref().map(|c| c.host_ports.clone()).unwrap_or_default();
    let resolved_project = match project_config {
        Some(config) => Some(config.resolve(repo_root)?),
        None => None,
    };
    if let Some(resolved) = resolved_project.as_ref() {
        ensure_project_trusted(
            resolved,
            policy.as_ref(),
            &orchestrator.config().state_dir,
            requested_network,
        )?;
        if args.template.is_some() && resolved.has_durable_caches() {
            anyhow::bail!(
                "Template restore is not yet supported with repo-managed durable caches.\n\n\
                 Remove `--template` for this run, or remove [environment].caches from \
                 .abox/project.toml for this repo."
            );
        }
    }

    let network_scope = match (resolved_project.as_ref(), requested_network) {
        (Some(resolved), override_mode) => Some(resolved.effective_network_scope(override_mode)?),
        (None, Some(mode)) => Some(standalone_network_scope(mode)?),
        (None, None) => None,
    };
    let effective_mode = network_scope.as_ref().map_or(NetworkMode::Safe, |s| s.mode);
    ensure_host_ports_allowed(&project_host_ports, effective_mode)?;
    ensure_host_ports_no_egress_collision(
        &project_host_ports,
        orchestrator.config().proxy.egress_port,
    )?;
    let policy = if let Some(scope) = network_scope.as_ref() {
        println!("Network mode: {}", scope.mode);
        std::sync::Arc::new(policy.as_ref().with_network_scope(scope.clone())?)
    } else {
        policy
    };

    let cache_mount_dir = resolved_project.as_ref().and_then(|resolved| {
        if resolved.has_durable_caches() {
            env_vars.extend(resolved.cache_env_vars());
            Some(project_cache_root(&orchestrator.config().state_dir, &resolved.project_id))
        } else {
            None
        }
    });

    let resolved_prompt = resolve_prompt_input(&args, resolved_project.as_ref())?;
    let command = adapt_command_for_prompt(&args.command, resolved_prompt.as_deref())?;

    if args.detach {
        return spawn_detached(&args, orchestrator);
    }

    if args.no_warm {
        if resolved_project.as_ref().is_some_and(ResolvedProjectConfig::is_warmable) {
            println!("Skipping guest-native environment refresh (--no-warm).");
        }
    } else {
        ensure_warm_environment_for_run(
            repo_root,
            resolved_project.as_ref(),
            orchestrator,
            std::sync::Arc::clone(&policy),
            std::sync::Arc::clone(&root_ca),
        )
        .await?;
    }

    // Start any declared service sidecars (postgres/redis/ollama/…), inject
    // their connection URLs as env vars, and build host→guest bridges. The
    // orchestrator tears the containers down when the sandbox exits.
    let service_bridges = start_project_services(
        &project_services,
        &args.task,
        args.template.as_deref(),
        &mut env_vars,
    )?;
    ensure_host_ports_not_templated(&project_host_ports, args.template.as_deref())?;
    let host_port_bridges =
        abox_core::services::plan_host_port_bridges(&project_host_ports, service_bridges.len());

    let params = CreateSandboxParams {
        task_id: args.task.clone(),
        base_branch: args.base,
        template: args.template,
        memory_mib: args.memory,
        vcpus: args.cpus,
        user: args.user,
        env_vars,
        command,
        resolved_prompt,
        cache_mount_dir,
        staged_prepare_script: None,
        environment_profile: resolved_project
            .as_ref()
            .map_or(EnvironmentProfile::Base, |resolved| resolved.environment_profile),
        timeout_secs: args.timeout,
        ephemeral: args.ephemeral,
        ca_cert_pem: None, // Populated by run_sandbox from the loaded RootCa.
        mount_excludes: resolved_project
            .as_ref()
            .map_or_else(Vec::new, |resolved| resolved.mount_excludes.clone()),
        service_bridges,
        host_port_bridges,
        input_files,
    };

    println!("Sandbox '{}' starting...", args.task);
    // The orchestrator tears the sidecars down when the sandbox exits cleanly,
    // but if `run_sandbox` fails before reaching that teardown (e.g. a
    // virtiofsd/Cloud Hypervisor startup error or a missing VM artifact) the
    // already-started containers would leak. Tear them down on the error path.
    // `stop_sandbox_services` is idempotent (stops by sandbox label), so this is
    // safe even if some teardown already happened.
    let exit_code = match orchestrator.run_sandbox(params, policy, root_ca).await {
        Ok(code) => code,
        Err(e) => {
            if !project_services.is_empty() {
                let _ = abox_core::services::stop_sandbox_services(&args.task);
            }
            return Err(e);
        }
    };

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

impl From<RunNetworkMode> for NetworkMode {
    fn from(value: RunNetworkMode) -> Self {
        match value {
            RunNetworkMode::Safe => Self::Safe,
            RunNetworkMode::Scoped => Self::Scoped,
            RunNetworkMode::Open => Self::Open,
        }
    }
}

fn resolve_prompt_input(
    args: &RunArgs,
    resolved_project: Option<&ResolvedProjectConfig>,
) -> Result<Option<String>> {
    if let Some(prompt) = &args.prompt {
        return Ok(Some(prompt.clone()));
    }

    if let Some(prompt_file) = &args.prompt_file {
        let content = std::fs::read_to_string(prompt_file)
            .with_context(|| format!("Reading prompt file {}", prompt_file.display()))?;
        return Ok(Some(content));
    }

    if let Some(resolved_project) = resolved_project {
        if let Some(bytes) = &resolved_project.default_prompt_bytes {
            let prompt = String::from_utf8(bytes.clone()).context(
                "Repo default prompt file is not valid UTF-8; prompt inputs must be UTF-8 text",
            )?;
            return Ok(Some(prompt));
        }
    }

    Ok(None)
}

fn adapt_command_for_prompt(command: &[String], prompt: Option<&str>) -> Result<Vec<String>> {
    let Some(_prompt) = prompt else {
        return Ok(command.to_vec());
    };

    match command {
        [single] if single == "codex" => {
            Ok(vec!["sh".into(), "-lc".into(), "cat \"$ABOX_PROMPT_FILE\" | codex exec -".into()])
        }
        [single] if single == "claude" => Ok(vec![
            "sh".into(),
            "-lc".into(),
            "PROMPT=$(cat \"$ABOX_PROMPT_FILE\")\nexec claude -p \"$PROMPT\"".into(),
        ]),
        [single, ..] if single == "codex" || single == "claude" => anyhow::bail!(
            "Prompt input currently supports only bare `{single}` commands.\n\n\
             Remove extra `{single}` arguments or omit --prompt/--prompt-file for this run."
        ),
        [other, ..] => anyhow::bail!(
            "Prompt input is only supported for known managed agents right now.\n\n\
             Command {other:?} cannot consume --prompt/--prompt-file yet."
        ),
        [] => anyhow::bail!("no agent command provided"),
    }
}

fn ensure_project_trusted(
    resolved: &ResolvedProjectConfig,
    policy: &abox_core::policy::PolicyEngine,
    state_dir: &Path,
    requested_network: Option<NetworkMode>,
) -> Result<()> {
    if is_approved(state_dir, resolved) {
        return Ok(());
    }

    let mut summary_lines = resolved.summary_lines(&policy.managed_egress_domains());
    if let Some(mode) = requested_network {
        if mode != resolved.default_network_mode {
            summary_lines.push(format!("Current run overrides network mode to: {mode}"));
        }
    }

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Repo-owned abox behavior is not yet trusted for this fingerprint.\n\n\
             Run `abox project explain` to review it, then `abox project trust` to approve it."
        );
    }

    eprintln!("Repo-owned abox behavior is not yet trusted:");
    for line in summary_lines {
        eprintln!("  {line}");
    }
    eprint!("Trust this repo config and continue? [y/N]: ");
    std::io::stderr().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let accepted = matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if !accepted {
        anyhow::bail!(
            "Launch cancelled. Run `abox project trust` after reviewing the repo config."
        );
    }

    let record_path = record_approval(state_dir, resolved)?;
    eprintln!("Trusted current repo behavior.");
    eprintln!("Approval record: {}", record_path.display());
    Ok(())
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
        adapt_command_for_prompt, ensure_host_ports_allowed, ensure_host_ports_no_egress_collision,
        ensure_host_ports_not_templated, ensure_managed_agent_ready, parse_env_var,
        parse_input_file_arg, resolve_input_files, resolve_prompt_input, selected_managed_agent,
        strip_detach_flag, InputFileSpec, RunArgs, MAX_INPUT_FILE_BYTES,
    };
    use abox_core::config::AboxConfig;
    use abox_core::project::{EnvironmentProfile, NetworkMode, ResolvedProjectConfig};
    use std::path::PathBuf;

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

    #[test]
    fn test_strip_detach_preserves_no_warm_flag() {
        let raw = vec![
            "run".to_string(),
            "--task".to_string(),
            "x".to_string(),
            "--no-warm".to_string(),
            "--detach".to_string(),
            "--".to_string(),
            "codex".to_string(),
        ];
        assert_eq!(strip_detach_flag(&raw), vec!["run", "--task", "x", "--no-warm", "--", "codex"]);
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

    #[test]
    fn adapt_prompt_for_bare_codex_uses_exec_stdin() {
        let adapted = adapt_command_for_prompt(&["codex".into()], Some("prompt")).unwrap();
        assert_eq!(adapted, vec!["sh", "-lc", "cat \"$ABOX_PROMPT_FILE\" | codex exec -"]);
    }

    #[test]
    fn adapt_prompt_for_bare_claude_uses_print_mode() {
        let adapted = adapt_command_for_prompt(&["claude".into()], Some("prompt")).unwrap();
        assert_eq!(
            adapted,
            vec!["sh", "-lc", "PROMPT=$(cat \"$ABOX_PROMPT_FILE\")\nexec claude -p \"$PROMPT\"",]
        );
    }

    #[test]
    fn adapt_prompt_rejects_extra_managed_agent_args() {
        let err = adapt_command_for_prompt(&["codex".into(), "--model".into()], Some("prompt"))
            .unwrap_err();
        assert!(format!("{err:#}").contains("bare `codex`"));
    }

    #[test]
    fn resolve_prompt_uses_repo_default_when_cli_prompt_missing() {
        let args = RunArgs {
            task: "x".into(),
            base: "main".into(),
            template: None,
            memory: None,
            cpus: None,
            user: None,
            env_vars: vec![],
            network: None,
            prompt: None,
            prompt_file: None,
            input_files: vec![],
            timeout: None,
            ephemeral: false,
            no_warm: false,
            detach: false,
            command: vec!["codex".into()],
        };
        let resolved = ResolvedProjectConfig {
            config_path: PathBuf::from(".abox/project.toml"),
            project_id: "repo".into(),
            default_network_mode: NetworkMode::Safe,
            has_host_ports: false,
            bundles: vec![],
            domains: vec![],
            environment_profile: EnvironmentProfile::Base,
            caches: vec![],
            prepare_path: None,
            prepare_bytes: None,
            watch_paths: vec![],
            default_prompt_path: Some(PathBuf::from(".abox/prompt.md")),
            default_prompt_bytes: Some(b"hello from repo prompt".to_vec()),
            mount_excludes: vec![],
            notes: vec![],
            approval_fingerprint: "abc".into(),
        };

        let prompt = resolve_prompt_input(&args, Some(&resolved)).unwrap();
        assert_eq!(prompt.as_deref(), Some("hello from repo prompt"));
    }

    #[test]
    fn input_file_derives_guest_name_from_basename() {
        let spec = parse_input_file_arg("/tmp/data/bundle.json").unwrap();
        assert_eq!(spec.host_path, std::path::PathBuf::from("/tmp/data/bundle.json"));
        assert_eq!(spec.guest_name, "bundle.json");
    }

    #[test]
    fn input_file_accepts_explicit_guest_name() {
        let spec = parse_input_file_arg("/tmp/x.json:task.json").unwrap();
        assert_eq!(spec.host_path, std::path::PathBuf::from("/tmp/x.json"));
        assert_eq!(spec.guest_name, "task.json");
    }

    #[test]
    fn input_file_rejects_traversal_guest_name() {
        assert!(parse_input_file_arg("/tmp/x.json:..").is_err());
        assert!(parse_input_file_arg("/tmp/x.json:a/b").is_err());
        assert!(parse_input_file_arg("/tmp/x.json:.").is_err());
    }

    #[test]
    fn host_ports_refused_in_safe_mode() {
        use abox_core::project::NetworkMode;
        use abox_core::services::HostPortBridge;
        let hp = vec![HostPortBridge { guest: 4000, host: 4000 }];
        assert!(ensure_host_ports_allowed(&hp, NetworkMode::Safe).is_err());
        assert!(ensure_host_ports_allowed(&hp, NetworkMode::Scoped).is_ok());
        assert!(ensure_host_ports_allowed(&[], NetworkMode::Safe).is_ok());
    }

    #[test]
    fn host_ports_rejected_with_template() {
        use abox_core::services::HostPortBridge;
        let hp = vec![HostPortBridge { guest: 4000, host: 4000 }];
        assert!(ensure_host_ports_not_templated(&hp, Some("snap")).is_err());
        assert!(ensure_host_ports_not_templated(&hp, None).is_ok());
        assert!(ensure_host_ports_not_templated(&[], Some("snap")).is_ok());
    }

    #[test]
    fn host_ports_reject_egress_port_collision() {
        use abox_core::services::HostPortBridge;
        let hp = vec![HostPortBridge { guest: 18443, host: 4000 }];
        assert!(ensure_host_ports_no_egress_collision(&hp, 18443).is_err());
        assert!(ensure_host_ports_no_egress_collision(&hp, 28443).is_ok());
        assert!(ensure_host_ports_no_egress_collision(&[], 18443).is_ok());
    }

    #[test]
    fn resolve_input_files_validates_and_dedups() {
        let dir = std::env::temp_dir().join(format!("abox-rif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        std::fs::write(&a, b"hello").unwrap();

        // Happy path: one regular file resolves.
        let ok = resolve_input_files(&[InputFileSpec {
            host_path: a.clone(),
            guest_name: "a.txt".into(),
        }])
        .unwrap();
        assert_eq!(ok.len(), 1);

        // A directory is rejected as not a regular file.
        let subdir = dir.join("sub");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(resolve_input_files(&[InputFileSpec {
            host_path: subdir.clone(),
            guest_name: "sub".into(),
        }])
        .is_err());

        // A missing file is rejected.
        assert!(resolve_input_files(&[InputFileSpec {
            host_path: dir.join("missing"),
            guest_name: "missing".into(),
        }])
        .is_err());

        // Two specs mapping to the same guest name collide.
        let b = dir.join("b.txt");
        std::fs::write(&b, b"world").unwrap();
        let dup = resolve_input_files(&[
            InputFileSpec { host_path: a.clone(), guest_name: "same".into() },
            InputFileSpec { host_path: b.clone(), guest_name: "same".into() },
        ]);
        assert!(dup.is_err());

        // Per-file size cap (use a sparse file so we don't write 64 MiB).
        let big = dir.join("big.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_INPUT_FILE_BYTES + 1).unwrap();
        assert!(resolve_input_files(&[InputFileSpec {
            host_path: big.clone(),
            guest_name: "big.bin".into(),
        }])
        .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
