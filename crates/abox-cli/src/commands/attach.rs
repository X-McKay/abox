//! `abox attach` — Attach to a sandbox's console.
//!
//! Connects to the VM's serial console socket via `socat`, giving the user
//! an interactive terminal inside the sandbox.

use abox_core::sandbox::SandboxOrchestrator;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::{Context, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct AttachArgs {
    /// The sandbox ID to attach to.
    pub task: String,
}

pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: AttachArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    let vm_info = orchestrator.vm_info(&args.task).await.context("Failed to get VM info")?;

    let console_socket = vm_info.console_socket;

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
        anyhow::bail!("socat exited with status: {}", status);
    }

    Ok(())
}
