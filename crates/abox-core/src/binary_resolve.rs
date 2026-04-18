//! Resolve VM binary paths (cloud-hypervisor, virtiofsd, ch-remote).
//!
//! The resolution order is:
//! 1. `state_dir/vm/<name>` — covers `install.sh` users whose binaries
//!    live at `~/.abox/vm/`.
//! 2. `$PATH` lookup — covers source builders who symlinked to
//!    `~/.local/bin/` via `bootstrap_vm.sh`.
//!
//! This replaces the previous bare `Command::new("binary-name")` calls
//! that relied solely on PATH, which broke for curl-pipe installs.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Resolve a VM binary by name using the standard search order.
///
/// Returns the absolute path to the binary if found, or an error with
/// actionable guidance if not.
pub fn resolve_vm_binary(name: &str, state_dir: &Path) -> Result<PathBuf> {
    // 1. state_dir/vm/<name> (covers install.sh users)
    let vm_path = state_dir.join("vm").join(name);
    if vm_path.exists() {
        return Ok(vm_path);
    }

    // 2. PATH fallback (covers source builders with symlinks)
    if let Ok(found) = which(name) {
        return Ok(found);
    }

    bail!(
        "{name} not found.\n\n\
         Checked:\n\
         \x20 {}\n\
         \x20 $PATH\n\n\
         Run 'abox init' or 'scripts/bootstrap_vm.sh' to install VM artifacts.",
        vm_path.display()
    )
}

/// Minimal `which`-style PATH lookup without pulling in the `which` crate.
fn which(name: &str) -> Result<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':') {
        let candidate = Path::new(dir).join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("{name} not found on PATH")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_finds_binary_in_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let vm_dir = tmp.path().join("vm");
        std::fs::create_dir_all(&vm_dir).unwrap();
        let bin = vm_dir.join("test-binary");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        let result = resolve_vm_binary("test-binary", tmp.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), bin);
    }

    #[test]
    fn resolve_returns_error_when_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_vm_binary("nonexistent-binary-xyz", tmp.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("nonexistent-binary-xyz not found"));
    }

    #[test]
    fn which_finds_common_binary() {
        // `sh` should be on PATH on any Linux system.
        let result = which("sh");
        assert!(result.is_ok());
    }
}
