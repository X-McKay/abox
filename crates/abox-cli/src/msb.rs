//! Shared MicroSandbox runtime helpers used by `abox init` and `abox doctor`.
//!
//! Centralizes host-side knowledge about where the MicroSandbox runtime
//! assets live (`$MSB_HOME`, default `~/.microsandbox`), where abox stages
//! host-built guest binaries (`<state_dir>/guest/<arch>/`), and how the
//! embedded profile → OCI image manifest resolves (ADR-008).

use abox_core::project::EnvironmentProfile;
use abox_core::runtime::images::ImageManifest;
use std::path::{Path, PathBuf};

/// Every official guest environment profile, in display order.
pub const ALL_PROFILES: [EnvironmentProfile; 5] = [
    EnvironmentProfile::Base,
    EnvironmentProfile::Node,
    EnvironmentProfile::Python,
    EnvironmentProfile::PythonGlibc,
    EnvironmentProfile::Rust,
];

/// Resolve the MicroSandbox home directory: `$MSB_HOME` verbatim when set,
/// otherwise `~/.microsandbox`. Matches the SDK's own resolution.
pub fn msb_home() -> PathBuf {
    if let Some(home) = std::env::var_os("MSB_HOME") {
        return PathBuf::from(home);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".microsandbox")
}

/// The canonical install path of the `msb` binary under the MSB home.
pub fn msb_binary() -> PathBuf {
    msb_home().join("bin").join("msb")
}

/// Locate the `msb` binary: `$MSB_HOME/bin/msb` first, then `$PATH`.
pub fn find_msb_binary() -> Option<PathBuf> {
    let installed = msb_binary();
    if installed.is_file() {
        return Some(installed);
    }
    which_on_path("msb")
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file())
    })
}

/// libkrunfw library files (`libkrunfw.*`) under `<msb home>/lib`.
pub fn libkrunfw_files() -> Vec<PathBuf> {
    libkrunfw_files_in(&msb_home().join("lib"))
}

/// libkrunfw library files (`libkrunfw.*`) in a specific directory.
pub fn libkrunfw_files_in(lib_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(lib_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("libkrunfw"))
        })
        .collect();
    files.sort();
    files
}

/// Where abox stages host-built guest binaries for the MicroSandbox runtime.
/// The guest architecture always matches the host architecture under both
/// KVM and Hypervisor.framework.
pub fn guest_binaries_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("guest").join(std::env::consts::ARCH)
}

/// Whether both host-staged guest binaries (abox-shim, abox-bridge) are
/// present under `<state_dir>/guest/<arch>/`.
pub fn guest_binaries_present(state_dir: &Path) -> bool {
    let dir = guest_binaries_dir(state_dir);
    dir.join("abox-shim").is_file() && dir.join("abox-bridge").is_file()
}

/// Run an async future to completion from a synchronous CLI code path.
///
/// `abox init` is synchronous but runs inside the `#[tokio::main]`
/// multi-thread runtime, where `block_in_place` is the correct bridge.
/// Outside any runtime (unit tests, future call sites) a throwaway runtime
/// is built instead.
pub fn block_on<F: std::future::Future>(future: F) -> anyhow::Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            Ok(tokio::task::block_in_place(|| handle.block_on(future)))
        }
        Ok(_) => anyhow::bail!(
            "cannot run blocking MicroSandbox setup inside a current-thread tokio runtime"
        ),
        Err(_) => {
            Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(future))
        }
    }
}

/// How the embedded manifest (plus host overrides) resolves each official
/// guest profile. Produced for `abox doctor` reporting.
pub struct ManifestReport {
    /// One human-readable line per profile: `<profile> → <pull-ref> [...]`.
    pub lines: Vec<String>,
    /// Profiles with no image mapping (fail-closed at launch time).
    pub missing: Vec<String>,
    /// Profiles that resolve to a tag instead of a pinned content digest.
    pub unpinned: Vec<String>,
}

/// Resolve every official profile against a manifest and summarize the
/// pull references, digest pinning, and any missing mappings.
pub fn manifest_report(manifest: &ImageManifest) -> ManifestReport {
    let mut report =
        ManifestReport { lines: Vec::new(), missing: Vec::new(), unpinned: Vec::new() };
    for profile in ALL_PROFILES {
        match manifest.image_for_profile(profile) {
            Ok(image) => {
                let pin =
                    if image.is_pinned() { "digest-pinned" } else { "tag, not digest-pinned" };
                report.lines.push(format!("{profile} → {} ({pin})", image.pull_reference()));
                if !image.is_pinned() {
                    report.unpinned.push(profile.to_string());
                }
            }
            Err(err) => {
                report.lines.push(format!("{profile} → unresolved: {err}"));
                report.missing.push(profile.to_string());
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_base_only() -> ImageManifest {
        ImageManifest::parse(
            r#"
            version = 1
            release = "test"
            [profiles.base]
            reference = "example.com/guest"
            tag = "1.0"
            digest = "sha256:abc"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn manifest_report_flags_missing_and_pinned_profiles() {
        let report = manifest_report(&manifest_with_base_only());
        assert_eq!(report.lines.len(), ALL_PROFILES.len());
        assert!(report.lines[0].contains("base → example.com/guest@sha256:abc (digest-pinned)"));
        assert!(!report.missing.contains(&"base".to_string()));
        assert!(report.missing.contains(&"node".to_string()));
        assert!(report.missing.contains(&"rust".to_string()));
        assert!(report.unpinned.is_empty());
    }

    #[test]
    fn manifest_report_flags_unpinned_profiles() {
        let manifest = ImageManifest::parse(
            r#"
            version = 1
            release = "test"
            [profiles.base]
            reference = "example.com/guest"
            tag = "1.0"
            "#,
        )
        .unwrap();
        let report = manifest_report(&manifest);
        assert!(report.lines[0].contains("example.com/guest:1.0 (tag, not digest-pinned)"));
        assert_eq!(report.unpinned, vec!["base".to_string()]);
    }

    #[test]
    fn embedded_manifest_resolves_every_profile() {
        let manifest = ImageManifest::embedded().unwrap();
        let report = manifest_report(&manifest);
        assert!(report.missing.is_empty(), "missing profiles: {:?}", report.missing);
    }

    #[test]
    fn libkrunfw_scan_matches_prefix_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("libkrunfw.5.dylib"), b"x").unwrap();
        std::fs::write(tmp.path().join("libkrunfw.so.4"), b"x").unwrap();
        std::fs::write(tmp.path().join("libother.so"), b"x").unwrap();
        let files = libkrunfw_files_in(tmp.path());
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|p| p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("libkrunfw")));
    }

    #[test]
    fn libkrunfw_scan_of_missing_dir_is_empty() {
        assert!(libkrunfw_files_in(Path::new("/nonexistent/abox-test-dir")).is_empty());
    }

    #[test]
    fn guest_binaries_detection_requires_both_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        assert!(!guest_binaries_present(state_dir));
        let dir = guest_binaries_dir(state_dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("abox-shim"), b"x").unwrap();
        assert!(!guest_binaries_present(state_dir));
        std::fs::write(dir.join("abox-bridge"), b"x").unwrap();
        assert!(guest_binaries_present(state_dir));
    }
}
