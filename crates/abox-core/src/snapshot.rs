//! Snapshot and template management.
//!
//! Manages Cloud Hypervisor VM snapshots for instant sandbox creation.
//! A "template" is a snapshot of a fully configured VM (with tools installed)
//! that can be restored in sub-second time.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Information about a stored template.
#[derive(Debug, Clone)]
pub struct TemplateInfo {
    /// Template name.
    pub name: String,
    /// Path to the snapshot directory.
    pub path: PathBuf,
    /// Size on disk in bytes.
    pub size_bytes: u64,
}

/// Manages VM snapshots and templates.
pub struct SnapshotManager {
    /// Directory where templates are stored (e.g., `~/.agentbox/templates/`).
    template_dir: PathBuf,
    /// Directory for runtime sockets.
    runtime_dir: PathBuf,
}

impl SnapshotManager {
    pub fn new(template_dir: PathBuf, runtime_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&template_dir)?;
        Ok(Self { template_dir, runtime_dir })
    }

    /// Create a snapshot of a paused VM and store it as a template.
    ///
    /// The VM must be paused before calling this (via `VmPort::pause`).
    pub async fn create_snapshot(&self, api_socket: &Path, template_name: &str) -> Result<PathBuf> {
        let snap_dir = self.template_dir.join(template_name);

        if snap_dir.exists() {
            bail!("Template '{}' already exists at {}", template_name, snap_dir.display());
        }

        std::fs::create_dir_all(&snap_dir)?;

        // Use ch-remote to trigger the snapshot
        let status = Command::new("ch-remote")
            .arg("--api-socket")
            .arg(api_socket.display().to_string())
            .arg("snapshot")
            .arg(format!("file://{}", snap_dir.display()))
            .status()
            .await
            .context("Failed to run ch-remote snapshot")?;

        if !status.success() {
            // Clean up on failure
            let _ = std::fs::remove_dir_all(&snap_dir);
            bail!("Snapshot creation failed for template '{}'", template_name);
        }

        tracing::info!(
            template = template_name,
            path = %snap_dir.display(),
            "Snapshot created"
        );

        Ok(snap_dir)
    }

    /// Restore a VM from a snapshot template.
    ///
    /// Returns the API socket path of the new VM.
    pub async fn restore_from_snapshot(
        &self,
        template_name: &str,
        sandbox_id: &str,
    ) -> Result<PathBuf> {
        let snap_dir = self.template_dir.join(template_name);
        if !snap_dir.exists() {
            bail!("Template '{}' not found", template_name);
        }

        let api_socket = self.runtime_dir.join(format!("ch-api-{}.sock", sandbox_id));

        // Clean up stale socket
        let _ = std::fs::remove_file(&api_socket);

        // Start Cloud Hypervisor in restore mode
        let _child = Command::new("cloud-hypervisor")
            .arg("--api-socket")
            .arg(api_socket.display().to_string())
            .arg("--restore")
            .arg(format!("source_url=file://{}", snap_dir.display()))
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start cloud-hypervisor in restore mode")?;

        // Wait for the API socket
        let start = std::time::Instant::now();
        loop {
            if api_socket.exists() {
                break;
            }
            if start.elapsed().as_millis() > 5000 {
                bail!("Timed out waiting for restored VM API socket");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Resume the VM (it was paused when the snapshot was taken)
        let status = Command::new("ch-remote")
            .arg("--api-socket")
            .arg(api_socket.display().to_string())
            .arg("resume")
            .status()
            .await
            .context("Failed to resume restored VM")?;

        if !status.success() {
            bail!("Failed to resume VM from template '{}'", template_name);
        }

        tracing::info!(template = template_name, sandbox_id, "VM restored from snapshot");

        Ok(api_socket)
    }

    /// List all available templates.
    pub fn list_templates(&self) -> Result<Vec<TemplateInfo>> {
        let mut templates = Vec::new();

        if !self.template_dir.exists() {
            return Ok(templates);
        }

        for entry in std::fs::read_dir(&self.template_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let name = entry.file_name().to_str().unwrap_or_default().to_string();

            // Calculate total size of the snapshot directory
            let size_bytes = dir_size(&entry.path()).unwrap_or(0);

            templates.push(TemplateInfo { name, path: entry.path(), size_bytes });
        }

        templates.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(templates)
    }

    /// Delete a template.
    pub fn delete_template(&self, name: &str) -> Result<()> {
        let path = self.template_dir.join(name);
        if !path.exists() {
            bail!("Template '{}' not found", name);
        }
        std::fs::remove_dir_all(&path)?;
        tracing::info!(template = name, "Template deleted");
        Ok(())
    }
}

/// Recursively compute the size of a directory.
fn dir_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}
