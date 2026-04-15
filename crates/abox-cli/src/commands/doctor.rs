//! `abox doctor` — non-destructive environment health check.
//!
//! Prints a checklist of every prerequisite abox needs to run sandboxes.
//! Safe to run at any time; makes no changes to the system.

use abox_core::config::AboxConfig;
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

/// Result of a single doctor check.
struct Check {
    label: String,
    status: CheckStatus,
    detail: Option<String>,
}

enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl Check {
    fn ok(label: impl Into<String>) -> Self {
        Self { label: label.into(), status: CheckStatus::Ok, detail: None }
    }

    fn ok_with(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { label: label.into(), status: CheckStatus::Ok, detail: Some(detail.into()) }
    }

    fn warn(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { label: label.into(), status: CheckStatus::Warn, detail: Some(detail.into()) }
    }

    fn fail(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { label: label.into(), status: CheckStatus::Fail, detail: Some(detail.into()) }
    }

    fn print(&self) {
        let icon = match self.status {
            CheckStatus::Ok => "✓",
            CheckStatus::Warn => "!",
            CheckStatus::Fail => "✗",
        };
        println!("  [{icon}] {}", self.label);
        if let Some(ref d) = self.detail {
            for line in d.lines() {
                println!("      {line}");
            }
        }
    }

    fn is_fail(&self) -> bool {
        matches!(self.status, CheckStatus::Fail)
    }

    fn is_warn(&self) -> bool {
        matches!(self.status, CheckStatus::Warn)
    }
}

/// Run all doctor checks and print a summary. Returns `Ok(true)` if all
/// checks pass (or only warnings), `Ok(false)` if any check fails.
pub fn execute(config: &AboxConfig) -> Result<bool> {
    println!("abox doctor — environment health check\n");

    let mut checks: Vec<Check> = Vec::new();

    // ── 1. KVM access ────────────────────────────────────────────────────────
    checks.push(check_kvm());

    // ── 2. VM artifacts ──────────────────────────────────────────────────────
    let vm_dir = config.state_dir.join("vm");
    checks.push(check_vm_artifact(&vm_dir, "cloud-hypervisor", "VMM binary"));
    checks.push(check_vm_artifact(&vm_dir, "virtiofsd", "virtiofs daemon"));
    checks.push(check_vm_artifact(&vm_dir, "vmlinux", "guest kernel"));
    checks.push(check_vm_artifact(&vm_dir, "rootfs.raw", "guest root filesystem"));

    // ── 3. Config file ───────────────────────────────────────────────────────
    checks.push(check_config_file(config));

    // ── 4. Policy file ───────────────────────────────────────────────────────
    checks.push(check_policy_file(config));

    // ── 5. Runtime dir socket-path length ────────────────────────────────────
    checks.push(check_socket_path_length(config));

    // ── 6. PATH: ~/.local/bin ────────────────────────────────────────────────
    checks.push(check_local_bin_on_path(&vm_dir));

    // ── Print all checks ─────────────────────────────────────────────────────
    for check in &checks {
        check.print();
    }

    let failures = checks.iter().filter(|c| c.is_fail()).count();
    let warnings = checks.iter().filter(|c| c.is_warn()).count();

    println!();
    if failures == 0 && warnings == 0 {
        println!(
            "All checks passed. Run 'abox run --task hello -- /bin/sh -c \"echo hi\"' to verify."
        );
    } else if failures == 0 {
        println!("{warnings} warning(s). abox should work but review the items above.");
    } else {
        println!(
            "{failures} check(s) failed, {warnings} warning(s). Run 'abox init' to fix setup issues."
        );
    }

    Ok(failures == 0)
}

fn check_kvm() -> Check {
    let kvm = Path::new("/dev/kvm");
    if !kvm.exists() {
        return Check::fail(
            "KVM device /dev/kvm",
            "Not found. abox requires a Linux host with KVM support.\n\
             Check that your kernel has KVM enabled and that you're not inside\n\
             a VM that doesn't expose nested virtualisation.",
        );
    }
    // Check read/write access
    match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
        Ok(_) => Check::ok("/dev/kvm accessible"),
        Err(_) => Check::fail(
            "/dev/kvm accessible",
            "Permission denied. Add yourself to the kvm group:\n\
             \n\
             \x20 sudo usermod -aG kvm $USER\n\
             \n\
             Then log out and back in for the change to take effect.",
        ),
    }
}

fn check_vm_artifact(vm_dir: &Path, name: &str, description: &str) -> Check {
    let label = format!("VM artifact: {description} ({name})");
    let path = vm_dir.join(name);
    if path.exists() {
        Check::ok_with(label, path.display().to_string())
    } else {
        Check::fail(
            label,
            format!(
                "Not found at {}\n\
                 Run 'abox init' or 'just bootstrap-vm' to download VM assets.",
                path.display()
            ),
        )
    }
}

fn check_config_file(config: &AboxConfig) -> Check {
    let config_path =
        AboxConfig::default_path().unwrap_or_else(|_| PathBuf::from("~/.abox/config.toml"));
    if config_path.exists() {
        Check::ok_with("Config file (~/.abox/config.toml)", config_path.display().to_string())
    } else {
        Check::warn(
            "Config file (~/.abox/config.toml)",
            format!(
                "Not found — using built-in defaults (state_dir={}, runtime_dir={}).\n\
                 Run 'abox init' to create a config file.",
                config.state_dir.display(),
                config.runtime_dir().display(),
            ),
        )
    }
}

fn check_policy_file(config: &AboxConfig) -> Check {
    let policy_path = config.proxy.policy_dir.join("default.toml");
    if policy_path.exists() {
        Check::ok_with("Policy file (default.toml)", policy_path.display().to_string())
    } else {
        Check::fail(
            "Policy file (default.toml)",
            format!(
                "Not found at {}\n\
                 abox will refuse to run sandboxes without a policy file.\n\
                 Run 'abox init' to install the default policy.",
                policy_path.display()
            ),
        )
    }
}

fn check_socket_path_length(config: &AboxConfig) -> Check {
    let runtime = config.runtime_dir();
    // Longest suffix abox appends: "vfs-status-<task-id>.sock"
    // Assume a generous 20-char task ID → 45 chars total suffix.
    let worst_case = runtime.to_string_lossy().len() + 1 + 45;
    if worst_case >= 108 {
        Check::fail(
            "Runtime dir socket path length",
            format!(
                "runtime_dir '{}' is too long ({} bytes base).\n\
                 Linux caps Unix socket paths at 108 bytes; abox appends per-sandbox\n\
                 suffixes like 'vfs-status-<task-id>.sock'.\n\
                 Set a shorter runtime_dir in ~/.abox/config.toml, e.g.:\n\
                 \n\
                 \x20 runtime_dir = \"/tmp/abox-$USER\"",
                runtime.display(),
                runtime.to_string_lossy().len(),
            ),
        )
    } else if worst_case >= 90 {
        Check::warn(
            "Runtime dir socket path length",
            format!(
                "runtime_dir '{}' is close to the 108-byte Unix socket limit\n\
                 (worst-case path ~{worst_case} bytes). Consider shortening it if you\n\
                 use long task IDs.",
                runtime.display(),
            ),
        )
    } else {
        Check::ok_with(
            "Runtime dir socket path length",
            format!("{} (worst-case ~{worst_case} bytes, limit 108)", runtime.display()),
        )
    }
}

fn check_local_bin_on_path(vm_dir: &Path) -> Check {
    let local_bin: PathBuf =
        dirs::home_dir().map_or_else(|| PathBuf::from("~/.local/bin"), |h| h.join(".local/bin"));

    // Check whether cloud-hypervisor is reachable on PATH
    let ch_on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|p| Path::new(p).join("cloud-hypervisor").exists());

    let ch_in_vm_dir = vm_dir.join("cloud-hypervisor").exists();

    if ch_on_path {
        Check::ok("cloud-hypervisor reachable on PATH")
    } else if ch_in_vm_dir {
        Check::warn(
            "cloud-hypervisor reachable on PATH",
            format!(
                "cloud-hypervisor exists in {} but is not on PATH.\n\
                 Add {} to your shell profile:\n\
                 \n\
                 \x20 export PATH=\"{}:$PATH\"",
                vm_dir.display(),
                local_bin.display(),
                local_bin.display(),
            ),
        )
    } else {
        Check::warn(
            "cloud-hypervisor reachable on PATH",
            "VM artifacts not yet installed — run 'abox init' first.",
        )
    }
}
