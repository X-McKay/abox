//! Snapshot and template management.
//!
//! Manages Cloud Hypervisor VM snapshots for instant sandbox creation.
//! A "template" is a snapshot of a fully configured VM (with tools installed)
//! that can be restored in sub-second time.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// Metadata stored alongside a snapshot template.
///
/// Records the virtiofsd socket filenames so a restore can recreate them at
/// the same paths the snapshotted VM was configured with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMeta {
    /// Map of share tag to socket filename (just the filename, not full path).
    /// E.g. `{"workspace": "vfs-fix-auth.sock", "meta": "vfs-meta-fix-auth.sock", ...}`
    pub virtiofs_sockets: HashMap<String, String>,
}

impl TemplateMeta {
    /// Name of the metadata file inside a template directory.
    pub const FILENAME: &'static str = "meta.json";

    /// Write metadata to `<template_dir>/meta.json`.
    pub fn save(&self, template_dir: &Path) -> Result<()> {
        let path = template_dir.join(Self::FILENAME);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json).context("Failed to write template metadata")?;
        Ok(())
    }

    /// Read metadata from `<template_dir>/meta.json`.
    pub fn load(template_dir: &Path) -> Result<Self> {
        let path = template_dir.join(Self::FILENAME);
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read template metadata from {}", path.display()))?;
        let meta: Self = serde_json::from_str(&json)?;
        Ok(meta)
    }
}

/// Manages VM snapshots and templates.
pub struct SnapshotManager {
    /// Directory where templates are stored (e.g., `~/.abox/templates/`).
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
    /// `virtiofs_sockets` maps share tags (e.g. "workspace") to socket
    /// filenames so restores can recreate virtiofsd on the same paths.
    pub async fn create_snapshot(
        &self,
        api_socket: &Path,
        template_name: &str,
        virtiofs_sockets: HashMap<String, String>,
    ) -> Result<PathBuf> {
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
            bail!("Snapshot creation failed for template '{template_name}'");
        }

        // Save virtiofsd socket metadata so restores know which socket
        // filenames the snapshotted VM was configured with.
        let meta = TemplateMeta { virtiofs_sockets };
        meta.save(&snap_dir)?;

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
            bail!("Template '{template_name}' not found");
        }

        let api_socket = self.runtime_dir.join(format!("ch-api-{sandbox_id}.sock"));

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
            bail!("Failed to resume VM from template '{template_name}'");
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
            bail!("Template '{name}' not found");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_meta_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut sockets = HashMap::new();
        sockets.insert("workspace".to_string(), "vfs-fix-auth.sock".to_string());
        sockets.insert("meta".to_string(), "vfs-meta-fix-auth.sock".to_string());
        sockets.insert("status".to_string(), "vfs-status-fix-auth.sock".to_string());

        let meta = TemplateMeta { virtiofs_sockets: sockets };
        meta.save(dir.path()).unwrap();

        let loaded = TemplateMeta::load(dir.path()).unwrap();
        assert_eq!(loaded.virtiofs_sockets.get("workspace").unwrap(), "vfs-fix-auth.sock");
        assert_eq!(loaded.virtiofs_sockets.get("meta").unwrap(), "vfs-meta-fix-auth.sock");
        assert_eq!(loaded.virtiofs_sockets.get("status").unwrap(), "vfs-status-fix-auth.sock");
    }

    #[test]
    fn template_meta_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TemplateMeta::load(dir.path()).is_err());
    }

    #[test]
    fn snapshot_manager_list_empty() {
        let tdir = tempfile::tempdir().unwrap();
        let rdir = tempfile::tempdir().unwrap();
        let mgr =
            SnapshotManager::new(tdir.path().to_path_buf(), rdir.path().to_path_buf()).unwrap();
        let list = mgr.list_templates().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn snapshot_manager_delete_missing_errors() {
        let tdir = tempfile::tempdir().unwrap();
        let rdir = tempfile::tempdir().unwrap();
        let mgr =
            SnapshotManager::new(tdir.path().to_path_buf(), rdir.path().to_path_buf()).unwrap();
        assert!(mgr.delete_template("nonexistent").is_err());
    }
}
