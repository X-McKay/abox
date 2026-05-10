//! `abox project` — manage repo-local abox config.

use abox_core::config::AboxConfig;
use abox_core::policy::PolicyEngine;
use abox_core::project::{
    approvals_dir, is_approved, load_approval_record, record_approval,
    starter_config_toml_with_profile, EnvironmentConfig, EnvironmentProfile, ProjectConfig,
    ResolvedProjectConfig,
};
use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Subcommand)]
pub enum ProjectCommand {
    /// Create a minimal repo-local `.abox/project.toml`.
    Init(ProjectInitArgs),
    /// Validate `.abox/project.toml` for this repo.
    Validate,
    /// Approve the current repo-owned behavior fingerprint.
    Trust,
    /// Explain the current repo-owned behavior and trust status.
    Explain,
    /// Set or clear the repo's official guest profile.
    SetProfile {
        #[arg(value_enum)]
        profile: ProjectProfileArg,
    },
}

#[derive(Debug, Clone, Args)]
pub struct ProjectInitArgs {
    /// Optional official guest profile to write into the starter config.
    #[arg(long, value_enum)]
    pub profile: Option<ProjectProfileArg>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProjectProfileArg {
    Base,
    Node,
    Python,
    Rust,
}

pub fn execute(
    cmd: &ProjectCommand,
    repo_root: &Path,
    config_override: Option<&Path>,
) -> Result<()> {
    match cmd {
        ProjectCommand::Init(args) => init(repo_root, args.profile.map(Into::into)),
        ProjectCommand::Validate => validate(repo_root),
        ProjectCommand::Trust => trust(repo_root, config_override),
        ProjectCommand::Explain => explain(repo_root, config_override),
        ProjectCommand::SetProfile { profile } => set_profile(repo_root, (*profile).into()),
    }
}

fn init(repo_root: &Path, profile: Option<EnvironmentProfile>) -> Result<()> {
    let path = ProjectConfig::default_path(repo_root);
    if path.exists() {
        anyhow::bail!("Project config already exists at {}", path.display());
    }

    let parent = path.parent().context("project config path has no parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("Creating {}", parent.display()))?;
    std::fs::write(&path, starter_config_toml_with_profile(profile))
        .with_context(|| format!("Writing {}", path.display()))?;

    println!("Created {}", path.display());
    println!("Starter config uses network mode: safe");
    if let Some(profile) = profile.filter(|profile| *profile != EnvironmentProfile::Base) {
        println!("Starter config uses environment profile: {profile}");
    }
    Ok(())
}

fn validate(repo_root: &Path) -> Result<()> {
    let resolved = load_resolved_project(repo_root)?;
    println!("Valid: {}", resolved.config_path.display());
    println!("Default network mode: {}", resolved.default_network_mode);
    if !resolved.notes.is_empty() {
        for note in resolved.notes {
            println!("Note: {note}");
        }
    }
    Ok(())
}

fn trust(repo_root: &Path, config_override: Option<&Path>) -> Result<()> {
    let resolved = load_resolved_project(repo_root)?;
    let (host_config, policy) = load_host_context(config_override);
    let policy_domains =
        policy.as_ref().map_or_else(Vec::new, PolicyEngine::managed_egress_domains);

    print_summary(&resolved, &policy_domains, &host_config.state_dir)?;
    let path = record_approval(&host_config.state_dir, &resolved)?;
    println!("Trusted current repo behavior.");
    println!("Approval record: {}", path.display());
    Ok(())
}

fn explain(repo_root: &Path, config_override: Option<&Path>) -> Result<()> {
    let resolved = load_resolved_project(repo_root)?;
    let (host_config, policy) = load_host_context(config_override);
    let policy_domains =
        policy.as_ref().map_or_else(Vec::new, PolicyEngine::managed_egress_domains);
    print_summary(&resolved, &policy_domains, &host_config.state_dir)
}

fn load_resolved_project(repo_root: &Path) -> Result<ResolvedProjectConfig> {
    let path = ProjectConfig::default_path(repo_root);
    let config = ProjectConfig::load(repo_root)?;
    let Some(config) = config else {
        anyhow::bail!("No project config found at {}", path.display());
    };
    config.resolve(repo_root)
}

fn load_editable_project(repo_root: &Path) -> Result<ProjectConfig> {
    let path = ProjectConfig::default_path(repo_root);
    ProjectConfig::load(repo_root)?
        .ok_or_else(|| anyhow::anyhow!("No project config found at {}", path.display()))
}

fn write_project_config(repo_root: &Path, config: &ProjectConfig) -> Result<()> {
    let path = ProjectConfig::default_path(repo_root);
    let parent = path.parent().context("project config path has no parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("Creating {}", parent.display()))?;
    let content = toml::to_string_pretty(config).context("Serializing project config")?;
    std::fs::write(&path, content).with_context(|| format!("Writing {}", path.display()))
}

fn set_profile(repo_root: &Path, profile: EnvironmentProfile) -> Result<()> {
    let mut config = load_editable_project(repo_root)?;
    if profile == EnvironmentProfile::Base {
        if let Some(environment) = config.environment.as_mut() {
            environment.profile = None;
            if environment.caches.is_empty()
                && environment.prepare.is_none()
                && environment.watch.is_empty()
            {
                config.environment = None;
            }
        }
    } else {
        let environment = config.environment.get_or_insert(EnvironmentConfig {
            profile: None,
            caches: Vec::new(),
            prepare: None,
            watch: Vec::new(),
        });
        environment.profile = Some(profile);
    }

    config.validate(repo_root)?;
    write_project_config(repo_root, &config)?;

    println!(
        "{} environment profile {}.",
        ProjectConfig::default_path(repo_root).display(),
        if profile == EnvironmentProfile::Base { "reset to base/default" } else { "updated" }
    );
    if profile != EnvironmentProfile::Base {
        println!("Selected profile: {profile}");
    }
    Ok(())
}

fn print_summary(
    resolved: &ResolvedProjectConfig,
    host_managed_domains: &[String],
    state_dir: &Path,
) -> Result<()> {
    let trusted = is_approved(state_dir, resolved);
    println!("Project config: {}", resolved.config_path.display());
    println!("Approval status: {}", if trusted { "trusted" } else { "untrusted" });
    println!("Approval fingerprint: {}", resolved.approval_fingerprint);
    if let Some(record) = load_approval_record(state_dir, resolved)? {
        println!("Approved at: {}", record.approved_at);
    }
    for line in resolved.summary_lines(host_managed_domains) {
        println!("{line}");
    }
    println!("Approval store: {}", approvals_dir(state_dir).display());
    Ok(())
}

fn load_host_context(config_override: Option<&Path>) -> (AboxConfig, Option<PolicyEngine>) {
    let config_path = config_override.map_or_else(
        || AboxConfig::default_path().unwrap_or_else(|_| PathBuf::from("~/.abox/config.toml")),
        PathBuf::from,
    );
    let config = AboxConfig::load(&config_path).unwrap_or_default();
    let policy_path = config.proxy.policy_dir.join("default.toml");
    let policy =
        if policy_path.exists() { PolicyEngine::from_file(&policy_path).ok() } else { None };
    (config, policy)
}

impl From<ProjectProfileArg> for EnvironmentProfile {
    fn from(value: ProjectProfileArg) -> Self {
        match value {
            ProjectProfileArg::Base => Self::Base,
            ProjectProfileArg::Node => Self::Node,
            ProjectProfileArg::Python => Self::Python,
            ProjectProfileArg::Rust => Self::Rust,
        }
    }
}
