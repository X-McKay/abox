//! abox-proxyd: Host-side credential proxy daemon.
//!
//! This daemon runs on the host and provides two services:
//! 1. CLI Proxy: Listens on a Unix socket for command requests from abox-shim
//!    inside the VM, evaluates them against the policy engine, and executes
//!    allowed commands on the host.
//! 2. HTTP Egress Proxy: An HTTP CONNECT proxy that intercepts outbound HTTPS
//!    requests from the VM and injects credentials based on egress rules.
//!
//! Both services share the same policy engine and audit log.

mod audit;
mod cli_proxy;
mod egress_proxy;

use abox_core::config::AboxConfig;
use abox_core::policy::PolicyEngine;
use anyhow::{Context, Result};
use audit::AuditLog;
use clap::Parser;
use cli_proxy::CliProxyServer;
use egress_proxy::EgressProxyServer;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "abox-proxyd", about = "abox host-side credential proxy daemon", version)]
struct Cli {
    /// Path to the abox config file. Defaults to `~/.abox/config.toml`.
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,abox_proxyd=debug".into()),
        )
        .json()
        .init();

    let cli = Cli::parse();

    tracing::info!("abox-proxyd starting");

    // Load configuration
    let config_path = match cli.config {
        Some(p) => p,
        None => AboxConfig::default_path()?,
    };
    let config = AboxConfig::load(&config_path)?;
    config.ensure_dirs()?;

    // Load policy engine
    let policy_path = config.proxy.policy_dir.join("default.toml");
    let policy = if policy_path.exists() {
        PolicyEngine::from_file(&policy_path)
            .with_context(|| format!("Failed to load policy from {}", policy_path.display()))?
    } else {
        tracing::warn!(
            path = %policy_path.display(),
            "No policy file found, using deny-all defaults"
        );
        PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "deny".to_string(),
            default_egress_action: "deny".to_string(),
        })?
    };
    let policy = Arc::new(policy);

    // Initialize audit log
    let audit_path = config.logs_dir().join("audit.jsonl");
    let audit = Arc::new(AuditLog::new(&audit_path)?);
    tracing::info!(path = %audit_path.display(), "Audit log initialized");

    // Load the root CA for TLS-terminating MITM proxy
    let ca_dir = abox_core::ca::RootCa::default_dir()?;
    let root_ca = Arc::new(
        abox_core::ca::RootCa::load_or_generate(&ca_dir)
            .context("Failed to load or generate root CA")?,
    );
    tracing::info!(ca_dir = %ca_dir.display(), "Root CA loaded");

    // Start both proxy servers concurrently
    let cli_socket = config.runtime_dir().join("cli-proxy.sock");
    let cli_server = CliProxyServer::new(cli_socket, Arc::clone(&policy), Arc::clone(&audit));
    let egress_server = EgressProxyServer::new(
        config.proxy.egress_port,
        Arc::clone(&policy),
        Arc::clone(&audit),
        Arc::clone(&root_ca),
    );

    tokio::select! {
        result = cli_server.run() => {
            if let Err(e) = result {
                tracing::error!(error = %e, "CLI proxy server failed");
            }
        }
        result = egress_server.run() => {
            if let Err(e) = result {
                tracing::error!(error = %e, "Egress proxy server failed");
            }
        }
    }

    Ok(())
}
