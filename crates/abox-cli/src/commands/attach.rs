//! `abox attach` — Attach to a sandbox's console.
//!
//! Connects to the VM's serial console socket via `socat`, giving the user
//! an interactive terminal inside the sandbox.

use super::validate_task_arg;
use abox_core::runtime::SandboxRuntimePort;
use abox_core::sandbox::SandboxOrchestrator;
use abox_core::workspace::WorkspacePort;
use anyhow::{Context, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct AttachArgs {
    /// The sandbox ID to attach to.
    pub task: String,
}

pub async fn execute<W: WorkspacePort, R: SandboxRuntimePort>(
    args: AttachArgs,
    orchestrator: &SandboxOrchestrator<W, R>,
) -> Result<()> {
    validate_task_arg(&args.task)?;

    // Verify the sandbox exists and is managed before attaching.
    orchestrator.runtime_info(&args.task).await.context("Failed to get sandbox info")?;

    let console_socket = orchestrator
        .console_output(&args.task)
        .context("This runtime does not expose a console to attach to")?;

    println!("Attaching to sandbox '{}' (Ctrl+] to detach)...", args.task);
    println!();

    // Use socat to connect to the console Unix socket interactively
    let status = tokio::process::Command::new("socat")
        .arg("-,raw,echo=0,escape=0x1d")
        .arg(format!("UNIX-CONNECT:{}", console_socket.display()))
        .status()
        .await
        .context("Failed to run socat. Is it installed?")?;

    if !status.success() {
        anyhow::bail!("socat exited with status: {status}");
    }

    Ok(())
}
