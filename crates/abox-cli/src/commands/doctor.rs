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
    checks.push(check_virtiofsd_caps(&vm_dir));
    checks.push(check_virtiofsd_uid_map(&vm_dir));
    checks.push(check_vm_artifact(&vm_dir, "vmlinux", "guest kernel"));
    checks.push(check_vm_artifact(&vm_dir, "rootfs.raw", "guest root filesystem"));
    checks.push(check_rootfs_freshness(&vm_dir));

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
    match crate::kvm::diagnose_kvm() {
        crate::kvm::KvmStatus::Available => Check::ok("/dev/kvm accessible"),
        crate::kvm::KvmStatus::Unavailable { condition, remediation } => {
            Check::fail(condition, remediation)
        }
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

fn check_virtiofsd_uid_map(vm_dir: &Path) -> Check {
    let label = "virtiofsd supports --uid-map";
    let bin = vm_dir.join("virtiofsd");
    if !bin.exists() {
        return Check::warn(label, "virtiofsd not yet installed — run 'abox init' first.");
    }
    let output = match std::process::Command::new(&bin).arg("--help").output() {
        Ok(o) => o,
        Err(e) => {
            return Check::fail(label, format!("Failed to run virtiofsd --help: {e}"));
        }
    };
    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    if combined.contains("--uid-map") {
        Check::ok(label)
    } else {
        Check::fail(
            label,
            format!(
                "The shipped virtiofsd at {} does not advertise --uid-map.\n\
                 abox uses --uid-map to remap workspace file ownership into the\n\
                 guest agent user (see ADR-004). Requires virtiofsd >= 1.10.\n\
                 Re-run 'just bootstrap-vm' to refresh the binary.",
                bin.display()
            ),
        )
    }
}

fn check_virtiofsd_caps(vm_dir: &Path) -> Check {
    let label = "virtiofsd has cap_sys_admin+ep";
    let bin = vm_dir.join("virtiofsd");
    if !bin.exists() {
        return Check::warn(label, "virtiofsd not yet installed — run 'abox init' first.");
    }

    match crate::virtiofsd::diagnose_virtiofsd_caps(&bin) {
        crate::virtiofsd::VirtiofsdCapsStatus::Ready => {
            Check::ok_with(label, bin.display().to_string())
        }
        crate::virtiofsd::VirtiofsdCapsStatus::Missing { condition, remediation } => {
            Check::fail(label, format!("{condition}\n{remediation}"))
        }
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

fn check_rootfs_freshness(vm_dir: &Path) -> Check {
    let label = "Rootfs freshness";
    let inputs = vm_dir.join("rootfs.raw.inputs");
    if !inputs.exists() {
        return Check::warn(
            label,
            "rootfs.raw.inputs sidecar not found — cannot verify freshness.\n\
             If you're running from source, re-run 'just rebuild-rootfs' to populate it.",
        );
    }
    let Ok(exe) = std::env::current_exe() else {
        return Check::warn(label, "Could not locate running binary; skipping check.");
    };
    let mut dir = exe.parent();
    let (mut init_sh, mut shim_bin): (Option<PathBuf>, Option<PathBuf>) = (None, None);
    for _ in 0..6 {
        let Some(d) = dir else { break };
        let c1 = d.join("guest/init.sh");
        let c2 = d.join("target/x86_64-unknown-linux-musl/release/abox-shim");
        if c1.exists() && init_sh.is_none() {
            init_sh = Some(c1);
        }
        if c2.exists() && shim_bin.is_none() {
            shim_bin = Some(c2);
        }
        if init_sh.is_some() && shim_bin.is_some() {
            break;
        }
        dir = d.parent();
    }
    let (Some(init_sh), Some(shim_bin)) = (init_sh, shim_bin) else {
        return Check::warn(
            label,
            "No source tree next to the binary — skipping freshness check.\n\
             (This is expected for released binaries.)",
        );
    };
    let init_hash = sha256_file(&init_sh);
    let shim_hash = sha256_file(&shim_bin);
    let recorded = std::fs::read_to_string(&inputs).unwrap_or_default();
    let recorded_init =
        recorded.lines().find_map(|l| l.strip_prefix("init_sh=")).unwrap_or("<missing>");
    let recorded_shim =
        recorded.lines().find_map(|l| l.strip_prefix("shim=")).unwrap_or("<missing>");
    if init_hash == recorded_init && shim_hash == recorded_shim {
        Check::ok(label)
    } else {
        Check::fail(
            label,
            format!(
                "rootfs.raw is stale — guest/init.sh or the shim has changed since the\n\
                 rootfs was built. Run:\n\
                 \n\
                 \x20 just rebuild-rootfs\n\
                 \n\
                 Mismatches:\n\
                 \x20 init_sh:  recorded={recorded_init}  live={init_hash}\n\
                 \x20 shim:     recorded={recorded_shim}  live={shim_hash}"
            ),
        )
    }
}

fn sha256_file(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut h = Sha256::new();
            h.update(&bytes);
            format!("{:x}", h.finalize())
        }
        Err(_) => "<read-error>".to_string(),
    }
}
