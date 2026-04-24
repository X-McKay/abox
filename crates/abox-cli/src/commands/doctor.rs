//! `abox doctor` — non-destructive environment health check.
//!
//! Prints a checklist of every prerequisite abox needs to run sandboxes.
//! Safe to run at any time; makes no changes to the system.

use abox_core::config::AboxConfig;
use abox_core::util::{max_task_id_len_for_runtime_dir, TASK_ID_MAX_LEN};
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

// ── ANSI color helpers (crossterm is already a dep of abox-cli) ──────────────

use crossterm::style::Stylize;
use crossterm::tty::IsTty;

/// Returns true when stdout is a TTY and NO_COLOR / TERM=dumb are not set.
fn use_color() -> bool {
    std::env::var("NO_COLOR").is_err()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
        && std::io::stdout().is_tty()
}

fn col_green(s: &str) -> String {
    if use_color() {
        s.green().to_string()
    } else {
        s.to_string()
    }
}
fn col_yellow(s: &str) -> String {
    if use_color() {
        s.yellow().to_string()
    } else {
        s.to_string()
    }
}
fn col_red(s: &str) -> String {
    if use_color() {
        s.red().to_string()
    } else {
        s.to_string()
    }
}
fn col_bold(s: &str) -> String {
    if use_color() {
        s.bold().to_string()
    } else {
        s.to_string()
    }
}
fn col_dim(s: &str) -> String {
    if use_color() {
        s.dim().to_string()
    } else {
        s.to_string()
    }
}
fn col_cyan(s: &str) -> String {
    if use_color() {
        s.cyan().to_string()
    } else {
        s.to_string()
    }
}

fn print_section(title: &str) {
    println!("\n  {}", col_bold(&col_cyan(title)));
    println!("  {}", col_dim(&"─".repeat(title.len())));
}

// ── Check type ───────────────────────────────────────────────────────────────

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
        let (icon, label_str) = match self.status {
            CheckStatus::Ok => (col_green("✓"), self.label.clone()),
            CheckStatus::Warn => (col_yellow("!"), col_yellow(&self.label)),
            CheckStatus::Fail => (col_red("✗"), col_bold(&col_red(&self.label))),
        };
        println!("    {icon}  {label_str}");
        if let Some(ref d) = self.detail {
            for line in d.lines() {
                println!("       {}", col_dim(line));
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

// ── Main execute ─────────────────────────────────────────────────────────────

/// Run all doctor checks and print a summary. Returns `Ok(true)` if all
/// checks pass (or only warnings), `Ok(false)` if any check fails.
pub fn execute(config: &AboxConfig) -> Result<bool> {
    let version = env!("CARGO_PKG_VERSION");
    println!(
        "{}  {}",
        col_bold(&col_cyan("abox doctor")),
        col_dim(&format!("v{version} — environment health check"))
    );
    println!();

    let vm_dir = config.state_dir.join("vm");

    // ── Section 1: Host ──────────────────────────────────────────────────────
    print_section("Host");
    let kvm = check_kvm();
    kvm.print();

    // ── Section 2: VM Stack ──────────────────────────────────────────────────
    print_section("VM Stack");
    let vm_checks = [
        check_vm_artifact(&vm_dir, "cloud-hypervisor", "VMM binary"),
        check_vm_artifact(&vm_dir, "virtiofsd", "virtiofs daemon"),
        check_virtiofsd_caps(&vm_dir),
        check_virtiofsd_uid_map(&vm_dir),
        check_vm_artifact(&vm_dir, "vmlinux", "guest kernel"),
        check_vm_artifact(&vm_dir, "rootfs.raw", "guest root filesystem"),
        check_rootfs_freshness(&vm_dir),
    ];
    for c in &vm_checks {
        c.print();
    }

    // ── Section 3: Configuration ─────────────────────────────────────────────
    print_section("Configuration");
    let cfg_checks =
        [check_config_file(config), check_policy_file(config), check_socket_path_length(config)];
    for c in &cfg_checks {
        c.print();
    }

    // ── Section 4: Environment ───────────────────────────────────────────────
    print_section("Environment");
    let env_check = check_local_bin_on_path(&vm_dir);
    env_check.print();

    // ── Summary ──────────────────────────────────────────────────────────────
    let all_checks: Vec<&Check> = std::iter::once(&kvm)
        .chain(vm_checks.iter())
        .chain(cfg_checks.iter())
        .chain(std::iter::once(&env_check))
        .collect();

    let failures = all_checks.iter().filter(|c| c.is_fail()).count();
    let warnings = all_checks.iter().filter(|c| c.is_warn()).count();

    println!();
    if failures == 0 && warnings == 0 {
        println!(
            "  {}  All checks passed. Run {} to verify.",
            col_green("✓"),
            col_bold("abox run --task hello -- echo hi")
        );
    } else if failures == 0 {
        println!(
            "  {}  {} warning(s) — abox should work, but review the items above.",
            col_yellow("!"),
            warnings
        );
    } else {
        println!(
            "  {}  {} failure(s), {} warning(s) — run {} to fix setup issues.",
            col_red("✗"),
            failures,
            warnings,
            col_bold("abox init")
        );
    }
    println!();

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
    if vm_dir.join(name).exists() {
        Check::ok(label)
    } else {
        Check::fail(
            label,
            format!(
                "{name} not found in {}.\n\
                 Run 'abox init' or 'just bootstrap-vm' to download VM artifacts.",
                vm_dir.display()
            ),
        )
    }
}

fn check_virtiofsd_uid_map(vm_dir: &Path) -> Check {
    let label = "virtiofsd supports --uid-map";
    let bin = vm_dir.join("virtiofsd");
    if !bin.exists() {
        return Check::warn(label, "virtiofsd not yet installed — run 'abox init' first.");
    }
    let out = std::process::Command::new(&bin).arg("--help").output();
    let help_text = match out {
        Ok(o) => {
            String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)
        }
        Err(e) => return Check::fail(label, format!("Failed to run virtiofsd --help: {e}")),
    };
    if help_text.contains("--uid-map") {
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
    let (mut init_sh, mut shim_bin, mut build_rootfs_sh, mut rootfs_builder_dockerfile): (
        Option<PathBuf>,
        Option<PathBuf>,
        Option<PathBuf>,
        Option<PathBuf>,
    ) = (None, None, None, None);
    for _ in 0..6 {
        let Some(d) = dir else { break };
        let c1 = d.join("guest/init.sh");
        let c2 = d.join("target/x86_64-unknown-linux-musl/release/abox-shim");
        let c3 = d.join("scripts/build_rootfs.sh");
        let c4 = d.join("scripts/rootfs-builder.Dockerfile");
        if c1.exists() && init_sh.is_none() {
            init_sh = Some(c1);
        }
        if c2.exists() && shim_bin.is_none() {
            shim_bin = Some(c2);
        }
        if c3.exists() && build_rootfs_sh.is_none() {
            build_rootfs_sh = Some(c3);
        }
        if c4.exists() && rootfs_builder_dockerfile.is_none() {
            rootfs_builder_dockerfile = Some(c4);
        }
        if init_sh.is_some()
            && shim_bin.is_some()
            && build_rootfs_sh.is_some()
            && rootfs_builder_dockerfile.is_some()
        {
            break;
        }
        dir = d.parent();
    }
    let (Some(init_sh), Some(shim_bin), Some(build_rootfs_sh), Some(rootfs_builder_dockerfile)) =
        (init_sh, shim_bin, build_rootfs_sh, rootfs_builder_dockerfile)
    else {
        return Check::warn(
            label,
            "No source tree next to the binary — skipping freshness check.\n\
             (This is expected for released binaries.)",
        );
    };
    let init_hash = sha256_file(&init_sh);
    let shim_hash = sha256_file(&shim_bin);
    let build_rootfs_hash = sha256_file(&build_rootfs_sh);
    let rootfs_builder_dockerfile_hash = sha256_file(&rootfs_builder_dockerfile);
    let recorded = std::fs::read_to_string(&inputs).unwrap_or_default();
    let recorded_init =
        recorded.lines().find_map(|l| l.strip_prefix("init_sh=")).unwrap_or("<missing>");
    let recorded_shim =
        recorded.lines().find_map(|l| l.strip_prefix("shim=")).unwrap_or("<missing>");
    let recorded_build_rootfs =
        recorded.lines().find_map(|l| l.strip_prefix("build_rootfs_sh=")).unwrap_or("<missing>");
    let recorded_rootfs_builder_dockerfile = recorded
        .lines()
        .find_map(|l| l.strip_prefix("rootfs_builder_dockerfile="))
        .unwrap_or("<missing>");
    if init_hash == recorded_init
        && shim_hash == recorded_shim
        && build_rootfs_hash == recorded_build_rootfs
        && rootfs_builder_dockerfile_hash == recorded_rootfs_builder_dockerfile
    {
        Check::ok(label)
    } else {
        Check::fail(
            label,
            format!(
                "rootfs.raw is stale — guest/init.sh, the shim, or the rootfs builder\n\
                 has changed since the\n\
                 rootfs was built. Run:\n\
                 \n\
                 \x20 just rebuild-rootfs\n\
                 \n\
                 Mismatches:\n\
                 \x20 init_sh:  recorded={recorded_init}  live={init_hash}\n\
                 \x20 shim:     recorded={recorded_shim}  live={shim_hash}\n\
                 \x20 build:    recorded={recorded_build_rootfs}  live={build_rootfs_hash}\n\
                 \x20 docker:   recorded={recorded_rootfs_builder_dockerfile}  live={rootfs_builder_dockerfile_hash}"
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
    let supported_len = max_task_id_len_for_runtime_dir(&runtime);
    if supported_len == 0 {
        Check::fail(
            "Runtime dir socket path length",
            format!(
                "runtime_dir '{}' is too long ({} bytes base).\n\
                 Linux caps Unix socket paths at 108 bytes, and abox appends per-sandbox\n\
                 suffixes like 'vfs-status-<task-id>.sock' and 'vsock-<task-id>.sock_5000'.\n\
                 No task ID would fit with this runtime_dir.\n\
                 Set a shorter runtime_dir in ~/.abox/config.toml, e.g.:\n\
                 \n\
                 \x20 runtime_dir = \"/tmp/abox-$USER\"",
                runtime.display(),
                runtime.to_string_lossy().len(),
            ),
        )
    } else if supported_len < TASK_ID_MAX_LEN {
        Check::warn(
            "Runtime dir socket path length",
            format!(
                "runtime_dir '{}' supports task IDs up to {} characters.\n\
                 abox accepts task IDs up to {} characters in general, but longer ones\n\
                 would overflow the runtime socket path budget for this config.\n\
                 Consider a shorter runtime_dir if you want the full task-ID budget.",
                runtime.display(),
                supported_len,
                TASK_ID_MAX_LEN,
            ),
        )
    } else {
        Check::ok("Runtime dir socket path length")
    }
}
