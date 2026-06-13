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
    /// Last-modified time of the snapshot directory, used to prune oldest-first
    /// regardless of the (user-chosen) name.
    pub modified: Option<std::time::SystemTime>,
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
#[allow(clippy::struct_field_names)] // All three fields are semantically distinct directories.
pub struct SnapshotManager {
    /// Directory where templates are stored (e.g., `~/.abox/templates/`).
    template_dir: PathBuf,
    /// Directory for runtime sockets. Retained for API stability and future
    /// snapshot-runtime work; restore now goes through the VM manager.
    #[allow(dead_code)]
    runtime_dir: PathBuf,
    /// Base state directory (e.g., `~/.abox`). Used to resolve VM binary paths.
    state_dir: PathBuf,
}

impl SnapshotManager {
    pub fn new(template_dir: PathBuf, runtime_dir: PathBuf, state_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&template_dir)?;
        Ok(Self { template_dir, runtime_dir, state_dir })
    }

    /// Resolve a VM binary using the standard search order.
    fn resolve_binary(&self, name: &str) -> Result<PathBuf> {
        crate::binary_resolve::resolve_vm_binary(name, &self.state_dir)
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
        validate_template_name(template_name)?;
        let snap_dir = self.template_dir.join(template_name);

        if snap_dir.exists() {
            bail!("Template '{}' already exists at {}", template_name, snap_dir.display());
        }

        std::fs::create_dir_all(&snap_dir)?;

        // Use ch-remote to trigger the snapshot
        let status = Command::new(self.resolve_binary("ch-remote")?)
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

    // NOTE: VM restore is intentionally NOT implemented here. A correct restore
    // must recreate the snapshot's virtiofsd backends, keep the Cloud Hypervisor
    // child process alive, and register the VM in the manager's state so it is
    // visible to `abox list`/`attach`. All of that already exists in the VM
    // manager's `start()` path (`StartMode::Restore`), which `abox run
    // --template` uses. `abox snapshot restore` therefore goes through the
    // orchestrator rather than a second, divergent implementation here.

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
            let size_bytes = dir_size(&entry.path()).unwrap_or(0);
            let modified = entry.metadata().and_then(|m| m.modified()).ok();

            templates.push(TemplateInfo { name, path: entry.path(), size_bytes, modified });
        }

        templates.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(templates)
    }

    /// Delete a template.
    pub fn delete_template(&self, name: &str) -> Result<()> {
        validate_template_name(name)?;
        let path = self.template_dir.join(name);
        if !path.exists() {
            bail!("Template '{name}' not found");
        }
        std::fs::remove_dir_all(&path)?;
        tracing::info!(template = name, "Template deleted");
        Ok(())
    }
}

/// Validate a snapshot/template name before using it as a path component.
///
/// Names are joined onto `template_dir`; without this a name like
/// `../../etc/foo` would let create/delete escape the templates directory.
pub fn validate_template_name(name: &str) -> Result<()> {
    crate::util::validate_resource_name(name)
        .map_err(|e| anyhow::anyhow!("Invalid snapshot name: {e}"))
}

/// Recursively compute the size of a directory.
pub fn dir_size(path: &Path) -> Result<u64> {
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
        let mgr = SnapshotManager::new(
            tdir.path().to_path_buf(),
            rdir.path().to_path_buf(),
            tdir.path().to_path_buf(),
        )
        .unwrap();
        let list = mgr.list_templates().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn validate_template_name_rejects_traversal() {
        assert!(validate_template_name("../../etc/passwd").is_err());
        assert!(validate_template_name("a/b").is_err());
        assert!(validate_template_name("good-name_1").is_ok());
    }

    #[test]
    fn delete_template_rejects_traversal_name() {
        let tdir = tempfile::tempdir().unwrap();
        let rdir = tempfile::tempdir().unwrap();
        let mgr = SnapshotManager::new(
            tdir.path().to_path_buf(),
            rdir.path().to_path_buf(),
            tdir.path().to_path_buf(),
        )
        .unwrap();
        assert!(mgr.delete_template("../escape").is_err());
    }

    #[test]
    fn snapshot_manager_delete_missing_errors() {
        let tdir = tempfile::tempdir().unwrap();
        let rdir = tempfile::tempdir().unwrap();
        let mgr = SnapshotManager::new(
            tdir.path().to_path_buf(),
            rdir.path().to_path_buf(),
            tdir.path().to_path_buf(),
        )
        .unwrap();
        assert!(mgr.delete_template("nonexistent").is_err());
    }
}
