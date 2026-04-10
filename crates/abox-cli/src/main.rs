//! abox: CLI for managing parallel AI agent sandboxes.

mod commands;
mod tui;

use abox_core::adapters::cloud_hypervisor::CloudHypervisorAdapter;
use abox_core::adapters::git2_workspace::Git2Workspace;
use abox_core::config::AboxConfig;
use abox_core::sandbox::SandboxOrchestrator;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "abox",
    about = "Parallel AI Agent Sandboxing with MicroVMs",
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

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create and start a new sandbox.
    Run(commands::run::RunArgs),

    /// List all active sandboxes.
    #[command(alias = "ls")]
    List,

    /// Attach to a sandbox's console.
    Attach(commands::attach::AttachArgs),

    /// Stop a sandbox.
    Stop(commands::stop::StopArgs),

    /// Show file divergence across sandboxes.
    #[command(alias = "diff")]
    Divergence(commands::divergence::DivergenceArgs),

    /// Merge a sandbox's branch back into the base branch.
    Merge(commands::merge::MergeArgs),

    /// Manage VM snapshot templates.
    Template(commands::template::TemplateArgs),

    /// Manage the root CA for HTTPS credential injection.
    #[command(subcommand)]
    Ca(commands::ca::CaCommand),

    /// Open the TUI dashboard.
    Tui,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,abox=info".into()),
        )
        .init();

    let cli = Cli::parse();

    let config_path = cli.config.unwrap_or_else(|| AboxConfig::default_path().unwrap_or_default());
    let config = AboxConfig::load(&config_path)?;
    config.ensure_dirs()?;

    // Template command does not need the orchestrator
    if let Commands::Template(args) = cli.command {
        return commands::template::execute(args, &config);
    }

    // CA command does not need the orchestrator
    if let Commands::Ca(cmd) = cli.command {
        return commands::ca::execute(cmd);
    }

    // TUI command
    if let Commands::Tui = cli.command {
        let mut state = tui::dashboard::DashboardState::new();
        return tui::dashboard::run_dashboard(&mut state);
    }

    // Load the policy engine. If no policy file exists, fall back to a
    // hard deny-all policy with a warning.
    let policy_path = config.proxy.policy_dir.join("default.toml");
    let policy = if policy_path.exists() {
        abox_core::policy::PolicyEngine::from_file(&policy_path)
            .with_context(|| format!("Failed to load policy from {}", policy_path.display()))?
    } else {
        tracing::warn!(
            path = %policy_path.display(),
            "No policy file found, using deny-all defaults"
        );
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "deny".to_string(),
            default_egress_action: "deny".to_string(),
            bypass_tls: vec![],
        })?
    };
    let policy = std::sync::Arc::new(policy);

    // Build the orchestrator
    let repo_path = cli.repo.canonicalize()?;
    let workspace = Git2Workspace::new(&repo_path, config.worktrees_dir())?;
    let vm_manager = CloudHypervisorAdapter::new(config.runtime_dir())?;
    let orchestrator = SandboxOrchestrator::new(config.clone(), workspace, vm_manager);

    match cli.command {
        Commands::Run(args) => {
            commands::run::execute(args, &orchestrator, std::sync::Arc::clone(&policy)).await
        }
        Commands::List => commands::list::execute(&orchestrator).await,
        Commands::Attach(args) => commands::attach::execute(args, &orchestrator).await,
        Commands::Stop(args) => commands::stop::execute(args, &orchestrator).await,
        Commands::Divergence(ref args) => commands::divergence::execute(args, &orchestrator),
        Commands::Merge(ref args) => commands::merge::execute(args, &orchestrator),
        Commands::Template(_) | Commands::Ca(_) | Commands::Tui => unreachable!(),
    }
}
