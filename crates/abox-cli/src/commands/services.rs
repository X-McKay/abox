//! `abox services` — Manage ephemeral service sidecars.
//!
//! Provides commands to start, stop, and list ephemeral service containers
//! (PostgreSQL, Redis, Ollama) that run alongside sandbox VMs.

use abox_core::config::AboxConfig;
use abox_core::project::ProjectConfig;
use abox_core::services::{
    docker_available, find_service_def, pull_ollama_models, start_service, stop_sandbox_services,
    wait_for_service_ready, ServiceConfig, SERVICE_DEFS,
};
use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::Path;

#[derive(Debug, Args)]
pub struct ServicesArgs {
    #[command(subcommand)]
    pub action: ServicesAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ServicesAction {
    /// List all available service types.
    Available,

    /// Start a service sidecar for a sandbox.
    Start {
        /// Service name (postgres, redis, ollama, mysql).
        service: String,
        /// Sandbox ID to associate the service with.
        #[arg(long)]
        sandbox: String,
        /// Service version override.
        #[arg(long)]
        version: Option<String>,
        /// For Ollama: models to pre-pull (comma-separated).
        #[arg(long)]
        models: Option<String>,
        /// Skip waiting for the service to be ready.
        #[arg(long)]
        no_wait: bool,
    },

    /// Stop all service sidecars for a sandbox.
    Stop {
        /// Sandbox ID whose services to stop.
        sandbox: String,
    },

    /// Show the project's configured services.
    Show,
}

pub fn execute(args: &ServicesArgs, _config: &AboxConfig, repo_root: &Path) -> Result<()> {
    match &args.action {
        ServicesAction::Available => list_available_services(),
        ServicesAction::Start { service, sandbox, version, models, no_wait } => {
            start_service_cmd(service, sandbox, version.as_deref(), models.as_deref(), *no_wait)
        }
        ServicesAction::Stop { sandbox } => stop_services_cmd(sandbox),
        ServicesAction::Show => show_project_services(repo_root),
    }
}

fn list_available_services() -> Result<()> {
    println!("Available service sidecars:");
    println!();
    println!("{:<12} {:<10} {:<30} DESCRIPTION", "NAME", "DEFAULT", "IMAGE");
    println!("{}", "-".repeat(80));
    for def in SERVICE_DEFS {
        let image = def.image_template.replace("{version}", def.default_version);
        println!("{:<12} {:<10} {:<30} {}", def.name, def.default_version, image, def.description);
    }
    println!();
    println!("Configure in .abox/project.toml:");
    println!();
    println!("  [services]");
    println!("  postgres = {{ version = \"17\" }}");
    println!("  redis = {{ version = \"7\" }}");
    println!("  ollama = {{ models = [\"qwen2.5-coder:7b\"] }}");
    println!();
    println!("Connection URLs are injected as environment variables:");
    for def in SERVICE_DEFS {
        println!("  {} → {}", def.name, def.env_var_name);
    }

    Ok(())
}

fn start_service_cmd(
    service_name: &str,
    sandbox_id: &str,
    version: Option<&str>,
    models: Option<&str>,
    no_wait: bool,
) -> Result<()> {
    if !docker_available() {
        anyhow::bail!(
            "Docker is not available or not running.\n\
             Service sidecars require Docker. Install Docker and ensure it is running."
        );
    }

    let def = find_service_def(service_name).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown service '{service_name}'.\n\
             Run 'abox services available' to see supported services."
        )
    })?;

    let models_vec: Vec<String> = models
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let config = ServiceConfig {
        version: version.map(str::to_string),
        models: models_vec.clone(),
        env: std::collections::HashMap::default(),
        wait: !no_wait,
    };

    println!("Starting {service_name} sidecar for sandbox '{sandbox_id}'...");

    let running = start_service(service_name, &config, sandbox_id)?;

    println!("  Container: {}", &running.container_id[..12.min(running.container_id.len())]);
    println!("  Host port: {}", running.host_port);

    if config.wait {
        println!("  Waiting for {service_name} to be ready...");
        let container_name = format!("abox-{service_name}-{sandbox_id}");
        wait_for_service_ready(service_name, &container_name, 30)?;
        println!("  {service_name} is ready.");
    }

    // Pull Ollama models if specified
    if service_name == "ollama" && !models_vec.is_empty() {
        let container_name = format!("abox-{service_name}-{sandbox_id}");
        pull_ollama_models(&container_name, &models_vec)?;
    }

    println!();
    println!("Service started successfully.");
    println!("Connection URL: {}", running.connection_url);
    println!("Environment variable: {}={}", running.env_var, running.connection_url);
    println!();
    println!("Stop with: abox services stop --sandbox {sandbox_id}");

    let _ = def;
    Ok(())
}

fn stop_services_cmd(sandbox_id: &str) -> Result<()> {
    println!("Stopping service sidecars for sandbox '{sandbox_id}'...");
    stop_sandbox_services(sandbox_id)?;
    println!("Services stopped.");
    Ok(())
}

fn show_project_services(repo_root: &Path) -> Result<()> {
    let project = ProjectConfig::load(repo_root)?;
    match project {
        None => {
            println!("No .abox/project.toml found in this repository.");
            println!("Create one with: abox project init");
        }
        Some(config) if config.services.is_empty() => {
            println!("No services configured in .abox/project.toml.");
            println!();
            println!("Add services with:");
            println!("  [services]");
            println!("  postgres = {{ version = \"17\" }}");
        }
        Some(config) => {
            println!("Configured services in .abox/project.toml:");
            println!();
            for (name, svc_config) in &config.services {
                let def = find_service_def(name);
                let version = svc_config
                    .version
                    .as_deref()
                    .or_else(|| def.map(|d| d.default_version))
                    .unwrap_or("latest");
                println!("  {name} v{version}");
                if !svc_config.models.is_empty() {
                    println!("    models: {}", svc_config.models.join(", "));
                }
                if let Some(def) = def {
                    println!("    env var: {}", def.env_var_name);
                }
            }
        }
    }
    Ok(())
}
