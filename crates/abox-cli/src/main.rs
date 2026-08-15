//! abox: CLI for managing parallel AI agent sandboxes.

mod commands;
mod kvm;
mod msb;
mod tui;

use abox_core::adapters::git2_workspace::Git2Workspace;
use abox_core::adapters::microsandbox::MicrosandboxRuntime;
use abox_core::config::AboxConfig;
use abox_core::sandbox::SandboxOrchestrator;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "abox",
    about = "Least-privilege execution and authorization for autonomous coding agents",
    version,
    propagate_version = true
)]
struct Cli {
    /// Path to the git repository.
    #[arg(long, global = true, default_value = ".")]
    repo: PathBuf,

    /// Path to the config file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Print the capability envelope bakudo probes on dispatch and exit.
    #[arg(long)]
    capabilities: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// First-run setup wizard: checks prerequisites and configures abox.
    Init(commands::init::InitArgs),

    /// Check the environment for common setup problems.
    Doctor,

    /// Create and start a new sandbox.
    Run(commands::run::RunArgs),

    /// Manage repo-local `.abox/project.toml`.
    #[command(subcommand)]
    Project(commands::project::ProjectCommand),

    /// Manage durable repo caches and prepare flows.
    Env(commands::env::EnvArgs),

    /// List all active sandboxes.
    #[command(alias = "ls")]
    List(commands::list::ListArgs),

    /// Stop a sandbox.
    Stop(commands::stop::StopArgs),

    /// Print the host worktree path for a sandbox (for collecting results).
    Path(commands::path::PathArgs),

    /// Show file divergence across sandboxes.
    #[command(alias = "diff")]
    Divergence(commands::divergence::DivergenceArgs),

    /// Merge a sandbox's branch back into the base branch.
    Merge(commands::merge::MergeArgs),

    /// Manage the root CA for HTTPS credential injection.
    #[command(subcommand)]
    Ca(commands::ca::CaCommand),

    /// Manage credential injection rules for transparent HTTP auth.
    #[command(subcommand)]
    Grant(commands::grant::GrantAction),

    /// Manage ephemeral service sidecars (postgres, redis, ollama).
    Services(commands::services::ServicesArgs),

    /// Open the TUI dashboard.
    Tui,
    /// Verify and inspect the audit log.
    #[command(subcommand)]
    Audit(commands::audit::AuditAction),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,abox=info".into()),
        )
        .init();

    let Cli { repo, config, capabilities, command } = Cli::parse();

    // `--capabilities` must run before any config or orchestrator load so a
    // host with no abox config can still be probed by bakudo.
    if capabilities {
        return commands::capabilities::execute();
    }

    // `abox init` must run before AboxConfig::load so it remains reachable
    // when the config file is missing OR malformed. The whole point of
    // `init` is to create (or fix) the config file, so it can't require a
    // loadable one as a precondition.
    if let Some(Commands::Init(args)) = command.as_ref() {
        return commands::init::execute(args);
    }

    // Repo-local project config commands should stay reachable even when the
    // host config is missing or malformed.
    if let Some(Commands::Project(cmd)) = command.as_ref() {
        let repo_path = repo.canonicalize()?;
        return commands::project::execute(cmd, &repo_path, config.as_deref());
    }

    let config_path = config.unwrap_or_else(|| AboxConfig::default_path().unwrap_or_default());
    let config = AboxConfig::load(&config_path)?;
    config.ensure_dirs()?;
    let repo_path = repo.canonicalize()?;

    if let Some(Commands::Env(args)) = command.as_ref() {
        if commands::env::execute_without_orchestrator(args, &repo_path, &config)? {
            return Ok(());
        }
    }

    // Doctor does not need the orchestrator and must run before the policy
    // check so it works even when setup is incomplete.
    if let Some(Commands::Doctor) = command.as_ref() {
        let ok = commands::doctor::execute(&config, &repo_path)?;
        return if ok { Ok(()) } else { std::process::exit(1) };
    }

    // CA command does not need the orchestrator
    if let Some(Commands::Ca(cmd)) = command.as_ref() {
        return commands::ca::execute(cmd);
    }

    // Grant command does not need the orchestrator
    if let Some(Commands::Grant(action)) = command.as_ref() {
        let args = commands::grant::GrantArgs { action: action.clone() };
        return commands::grant::execute(&args, &config).await;
    }

    // Services command does not need the orchestrator
    if let Some(Commands::Services(args)) = command.as_ref() {
        return commands::services::execute(args, &config, &repo_path);
    }

    // TUI command
    if let Some(Commands::Tui) = command.as_ref() {
        let mut state = tui::dashboard::DashboardState::new();
        return tui::dashboard::run_dashboard(&mut state);
    }

    // Audit command does not need the orchestrator
    if let Some(Commands::Audit(action)) = command.as_ref() {
        let args = commands::audit::AuditArgs { action: action.clone() };
        return commands::audit::execute(&args, &config);
    }

    // Load the policy engine. Fail fast with an actionable message if the
    // policy file is missing — silently falling back to deny-all would make
    // every agent command fail with no visible explanation.
    let policy_path = config.proxy.policy_dir.join("default.toml");
    let policy = if policy_path.exists() {
        abox_core::policy::PolicyEngine::from_file(&policy_path)
            .with_context(|| format!("Failed to load policy from {}", policy_path.display()))?
    } else {
        anyhow::bail!(
            "No policy file found at {}\n\n\
             abox requires a policy file before it can run sandboxes.\n\
             Copy the default policy to get started:\n\n\
             \x20 cp <abox-repo>/policies/default.toml {}\n\n\
             Or run 'abox init' to set everything up automatically.",
            policy_path.display(),
            policy_path.display(),
        );
    };
    let policy = std::sync::Arc::new(policy);

    // Build the orchestrator
    let workspace = Git2Workspace::new(&repo_path, config.worktrees_dir())?;
    let runtime = MicrosandboxRuntime::new(&config)?;
    let orchestrator = SandboxOrchestrator::new(config.clone(), workspace, runtime);

    // The root CA is only consumed by `abox run` (it backs the per-sandbox
    // request broker). Loading it for read-only commands like `list`,
    // `stop`, `merge`, or `divergence` would unnecessarily couple them to the
    // CA's on-disk state — a corrupt or read-only `~/.abox/ca/` would block
    // commands that have nothing to do with the proxy. Defer the load into
    // the Run branch.
    let command = command
        .ok_or_else(|| anyhow::anyhow!("no subcommand provided (try `abox --help` for options)"))?;

    match command {
        Commands::Run(args) => {
            let ca_dir = abox_core::ca::RootCa::default_dir()?;
            let root_ca = std::sync::Arc::new(
                abox_core::ca::RootCa::load_or_generate(&ca_dir)
                    .context("Failed to load or generate root CA")?,
            );
            // Boxed: the sandbox lifecycle future is large (guest exec
            // streaming state machines).
            Box::pin(commands::run::execute(
                args,
                &repo_path,
                &orchestrator,
                std::sync::Arc::clone(&policy),
                root_ca,
            ))
            .await
        }
        Commands::Env(args) => {
            let ca_dir = abox_core::ca::RootCa::default_dir()?;
            let root_ca = std::sync::Arc::new(
                abox_core::ca::RootCa::load_or_generate(&ca_dir)
                    .context("Failed to load or generate root CA")?,
            );
            Box::pin(commands::env::execute_warm(
                &args,
                &repo_path,
                &orchestrator,
                std::sync::Arc::clone(&policy),
                root_ca,
            ))
            .await
        }
        Commands::List(ref args) => commands::list::execute(args, &orchestrator).await,
        Commands::Stop(args) => commands::stop::execute(args, &orchestrator).await,
        Commands::Divergence(ref args) => commands::divergence::execute(args, &orchestrator),
        Commands::Path(ref args) => commands::path::execute(args, &orchestrator),
        Commands::Merge(ref args) => commands::merge::execute(args, &orchestrator),
        Commands::Ca(_)
        | Commands::Grant(_)
        | Commands::Services(_)
        | Commands::Tui
        | Commands::Audit(_)
        | Commands::Init(_)
        | Commands::Doctor
        | Commands::Project(_) => unreachable!(),
    }
}
