//! `abox doctor` — non-destructive environment health check.
//!
//! Prints a checklist of every prerequisite abox needs to run sandboxes.
//! Safe to run at any time; makes no changes to the system.

use abox_core::config::{
    default_claude_host_credential_file, default_codex_host_credential_file, AboxConfig,
};
use abox_core::project::{image_path_for_profile, EnvironmentProfile, ProjectConfig};
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
        && std::env::var("TERM").map_or(true, |t| t != "dumb")
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
pub fn execute(config: &AboxConfig, repo_root: &Path) -> Result<bool> {
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

    // ── Section 4: Managed Auth ──────────────────────────────────────────────
    print_section("Managed Auth");
    let auth_checks = [
        check_managed_provider(
            "Claude Code",
            "auth.providers.claude",
            config.auth.claude_enabled(),
            &default_claude_host_credential_file(),
        ),
        check_managed_provider(
            "Codex",
            "auth.providers.codex",
            config.auth.codex_enabled(),
            &default_codex_host_credential_file(),
        ),
    ];
    for c in &auth_checks {
        c.print();
    }

    // ── Section 5: CA Certificate ────────────────────────────────────────────
    print_section("CA Certificate (HTTPS Credential Injection)");
    let ca_checks = [check_ca_files(config), check_ca_trust(config)];
    for c in &ca_checks {
        c.print();
    }

    // ── Section 6: Agent-Specific Validation ─────────────────────────────────
    print_section("Agent Validation");
    let agent_checks = [
        check_agent_credential_injection(
            "Claude Code",
            config.auth.claude_enabled(),
            &abox_core::config::default_claude_host_credential_file(),
        ),
        check_agent_credential_injection(
            "Codex",
            config.auth.codex_enabled(),
            &abox_core::config::default_codex_host_credential_file(),
        ),
    ];
    for c in &agent_checks {
        c.print();
    }

    // ── Section 7: Audit Log ─────────────────────────────────────────────────
    print_section("Audit Log");
    let audit_check = check_audit_log(config);
    audit_check.print();

    // ── Section 8: Environment ───────────────────────────────────────────────
    print_section("Environment");
    let env_check = check_local_bin_on_path(&vm_dir);
    env_check.print();
    let installed_profiles = check_installed_guest_profiles(config);
    installed_profiles.print();
    let repo_profile = check_repo_requested_profile(config, repo_root);
    repo_profile.print();

    // ── Summary ──────────────────────────────────────────────────────────────
    let all_checks: Vec<&Check> = std::iter::once(&kvm)
        .chain(vm_checks.iter())
        .chain(cfg_checks.iter())
        .chain(auth_checks.iter())
        .chain(ca_checks.iter())
        .chain(agent_checks.iter())
        .chain(std::iter::once(&audit_check))
        .chain(std::iter::once(&env_check))
        .chain(std::iter::once(&installed_profiles))
        .chain(std::iter::once(&repo_profile))
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

fn installed_profiles(config: &AboxConfig) -> Vec<EnvironmentProfile> {
    let mut profiles = Vec::new();
    for profile in [
        EnvironmentProfile::Base,
        EnvironmentProfile::Node,
        EnvironmentProfile::Python,
        EnvironmentProfile::PythonGlibc,
        EnvironmentProfile::Rust,
    ] {
        if image_path_for_profile(config, profile).exists() {
            profiles.push(profile);
        }
    }
    profiles
}

fn check_installed_guest_profiles(config: &AboxConfig) -> Check {
    let installed = installed_profiles(config);
    let label = "Installed guest profiles";
    if installed.is_empty() {
        return Check::fail(
            label,
            "No guest profile images are installed.\nRun 'abox init' to bootstrap the base image."
                .to_string(),
        );
    }

    let installed_names = installed.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
    let missing_names = [
        EnvironmentProfile::Base,
        EnvironmentProfile::Node,
        EnvironmentProfile::Python,
        EnvironmentProfile::PythonGlibc,
        EnvironmentProfile::Rust,
    ]
    .into_iter()
    .filter(|profile| !installed.contains(profile))
    .map(|profile| profile.to_string())
    .collect::<Vec<_>>();

    let detail = if missing_names.is_empty() {
        format!("installed: {installed_names}")
    } else {
        format!(
            "installed: {installed_names}\noptional profiles not yet installed: {}",
            missing_names.join(", ")
        )
    };
    Check::ok_with(label, detail)
}

fn check_repo_requested_profile(config: &AboxConfig, repo_root: &Path) -> Check {
    let label = "Current repo environment profile";
    let config_path = ProjectConfig::default_path(repo_root);
    let loaded = match ProjectConfig::load(repo_root) {
        Ok(config) => config,
        Err(err) => {
            return Check::warn(
                label,
                format!(
                    "Failed to load {}.\nRun `abox project validate` for details.\n{err:#}",
                    config_path.display()
                ),
            )
        }
    };

    let Some(project) = loaded else {
        return Check::ok_with(label, "no repo config found; base profile will be used");
    };

    let resolved = match project.resolve(repo_root) {
        Ok(resolved) => resolved,
        Err(err) => {
            return Check::warn(
                label,
                format!(
                    "Failed to resolve {}.\nRun `abox project validate` for details.\n{err:#}",
                    config_path.display()
                ),
            )
        }
    };

    let image_path = image_path_for_profile(config, resolved.environment_profile);
    if image_path.exists() {
        Check::ok_with(
            label,
            format!("{} ({})", resolved.environment_profile, image_path.display()),
        )
    } else {
        Check::fail(
            label,
            format!(
                "Repo requests '{}' but the image is missing at {}.\n\
                 Install it with `abox init --profile {}`.",
                resolved.environment_profile,
                image_path.display(),
                resolved.environment_profile
            ),
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

fn check_managed_provider(
    provider_name: &str,
    config_key: &str,
    enabled: bool,
    host_credential_file: &str,
) -> Check {
    let label = format!("Managed auth: {provider_name}");
    let expanded = abox_core::policy::expand_tilde(host_credential_file);
    let exists = Path::new(&expanded).exists();

    match (enabled, exists) {
        (true, true) => Check::ok_with(label, format!("enabled ({expanded})")),
        (true, false) => Check::warn(
            label,
            format!(
                "Enabled in config, but host credentials were not found at {expanded}.\n\
                 Log in to {provider_name} on the host, or disable {config_key} in ~/.abox/config.toml."
            ),
        ),
        (false, true) => Check::ok_with(
            label,
            format!("available on host at {expanded}, currently disabled in config"),
        ),
        (false, false) => Check::warn(
            label,
            "Not detected on the host and not enabled in config.\n\
             abox can still launch arbitrary sandbox commands, but the default managed agent \
             workflow needs at least one of Claude Code or Codex."
                .to_string(),
        ),
    }
}

/// Check that the root CA key and certificate files exist.
fn check_ca_files(_config: &AboxConfig) -> Check {
    let label = "Root CA files";
    let ca_dir = match abox_core::ca::RootCa::default_dir() {
        Ok(d) => d,
        Err(e) => return Check::fail(label, format!("Cannot determine CA directory: {e}")),
    };
    let cert = ca_dir.join("root.crt");
    let key = ca_dir.join("root.key");

    match (cert.exists(), key.exists()) {
        (true, true) => Check::ok_with(label, format!("cert + key in {}", ca_dir.display())),
        (false, _) => Check::warn(
            label,
            format!(
                "Root CA not yet generated (expected at {})\n\
                 The CA is created automatically on the first 'abox run'.\n\
                 Run 'abox ca init' to generate it now.",
                ca_dir.display()
            ),
        ),
        (true, false) => Check::fail(
            label,
            format!(
                "CA certificate found but private key is missing at {}\n\
                 Delete {} and run 'abox ca init' to regenerate.",
                key.display(),
                ca_dir.display()
            ),
        ),
    }
}

/// Check that the root CA certificate is trusted by the host OS.
/// This is a best-effort check using the system CA bundle.
fn check_ca_trust(config: &AboxConfig) -> Check {
    let _ = config;
    let label = "Root CA trusted by host OS";
    let Ok(ca_dir) = abox_core::ca::RootCa::default_dir() else {
        return Check::warn(label, "Cannot determine CA directory.");
    };
    let cert_path = ca_dir.join("root.crt");
    if !cert_path.exists() {
        return Check::warn(
            label,
            "Root CA not yet generated — trust check skipped.\n\
             Run 'abox ca init' then add the CA to your system trust store.",
        );
    }

    // Compare the abox CA *content* against common trust-store locations,
    // rather than merely checking that some file exists at those paths. This
    // avoids a false "trusted" when an unrelated cert sits at the path, and
    // detects a stale copy left behind after the CA was rotated.
    let Some(abox_body) = std::fs::read_to_string(&cert_path).ok().and_then(|s| pem_cert_body(&s))
    else {
        return Check::warn(label, "Could not read the abox CA certificate for comparison.");
    };

    let trust_locations = [
        "/etc/ssl/certs/abox-ca.pem",
        "/usr/local/share/ca-certificates/abox.crt",
        "/etc/pki/ca-trust/source/anchors/abox.crt",
    ];

    let mut stale_at: Option<&str> = None;
    for path in trust_locations {
        let Some(body) = std::fs::read_to_string(path).ok().and_then(|s| pem_cert_body(&s)) else {
            continue;
        };
        if body == abox_body {
            return Check::ok_with(label, format!("abox CA installed and current at {path}"));
        }
        stale_at = Some(path);
    }

    if let Some(path) = stale_at {
        return Check::fail(
            label,
            format!(
                "A different certificate is installed at {path} — it does not match the current\n\
                 abox CA at {}. The CA was likely rotated. Re-copy the current CA and refresh the\n\
                 trust store (Linux: update-ca-certificates; macOS: re-add to the keychain).",
                cert_path.display(),
            ),
        );
    }

    // On macOS, trust lives in the keychain, which these file paths can't see.
    let macos_note = if cfg!(target_os = "macos") {
        "\n\nNote: on macOS the CA is trusted via the keychain, which this file-based check\n\
         cannot inspect. If you already ran the `security add-trusted-cert` command below,\n\
         this warning is expected and can be ignored."
    } else {
        ""
    };

    Check::warn(
        label,
        format!(
            "Root CA at {} is not yet trusted by the host OS.\n\
             Without this, HTTPS credential injection will fail for tools\n\
             that use the system CA bundle.\n\
             \n\
             To trust the CA:\n\
             \x20 macOS:  sudo security add-trusted-cert -d -r trustRoot \\\n\
             \x20         -k /Library/Keychains/System.keychain {}\n\
             \x20 Linux:  sudo cp {} /usr/local/share/ca-certificates/abox.crt\n\
             \x20         && sudo update-ca-certificates{macos_note}",
            cert_path.display(),
            cert_path.display(),
            cert_path.display(),
        ),
    )
}

/// Extract the base64 body of the first PEM CERTIFICATE block, stripping the
/// header/footer and all whitespace, so two encodings of the same certificate
/// compare equal regardless of line wrapping or trailing newlines.
fn pem_cert_body(pem: &str) -> Option<String> {
    let start = pem.find("-----BEGIN CERTIFICATE-----")?;
    let after = &pem[start + "-----BEGIN CERTIFICATE-----".len()..];
    let end = after.find("-----END CERTIFICATE-----")?;
    let body: String = after[..end].chars().filter(|c| !c.is_whitespace()).collect();
    if body.is_empty() {
        None
    } else {
        Some(body)
    }
}

/// Check that a managed agent's credential injection chain is complete:
/// enabled in config AND host credential file exists.
fn check_agent_credential_injection(agent: &str, enabled: bool, host_cred_file: &str) -> Check {
    let label = format!("{agent} credential injection");
    let expanded = abox_core::policy::expand_tilde(host_cred_file);
    let cred_exists = std::path::Path::new(&expanded).exists();

    match (enabled, cred_exists) {
        (true, true) => Check::ok_with(
            label,
            format!(
                "enabled — host credential at {expanded} will be injected at the network layer"
            ),
        ),
        (true, false) => Check::fail(
            label,
            format!(
                "Enabled in config but host credential not found at {expanded}.\n\
                 Log in to {agent} on the host first, or disable the provider in\n\
                 ~/.abox/config.toml."
            ),
        ),
        (false, true) => Check::warn(
            label,
            format!(
                "Host credential found at {expanded} but provider is not enabled in config.\n\
                 To enable: add [auth.providers.{}] / enabled = true to ~/.abox/config.toml,\n\
                 or run 'abox init' to auto-detect and enable it.",
                agent.to_lowercase().replace(' ', "_")
            ),
        ),
        (false, false) => {
            Check::ok_with(label, format!("not configured — {agent} not detected on host"))
        }
    }
}

/// Check the audit log status: whether it exists and whether its chain is intact.
fn check_audit_log(config: &AboxConfig) -> Check {
    let label = "Audit log integrity";
    let logs_dir = config.logs_dir();
    let log_path = abox_core::audit::default_log_path(&logs_dir);

    if !log_path.exists() {
        return Check::ok_with(
            label,
            format!("no audit log yet (will be created at {})", log_path.display()),
        );
    }

    let content = match std::fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(e) => return Check::fail(label, format!("Cannot read audit log: {e}")),
    };

    let entry_count = content.lines().filter(|l| !l.trim().is_empty()).count();

    // Check if entries have hash fields (new format) or are old format.
    let first_entry = content.lines().find(|l| !l.trim().is_empty());
    let has_hash_chain = first_entry
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .is_some_and(|v| v.get("hash").is_some());

    if !has_hash_chain {
        return Check::warn(
            label,
            format!(
                "Audit log at {} has {entry_count} entries in legacy format (no hash chain).\n\
                 New entries will use the hash-chained format automatically.\n\
                 Run 'abox audit verify' after the next sandbox run to confirm.",
                log_path.display()
            ),
        );
    }

    // The keyed chain requires the host-only key written by abox-proxyd.
    let key_path = logs_dir.join(abox_core::audit::KEY_FILENAME);
    if !key_path.exists() {
        return Check::warn(
            label,
            format!(
                "Audit log at {} is hash-chained but the host key {} is missing, so the chain \
                 cannot be authenticated. Was the key deleted?",
                log_path.display(),
                key_path.display()
            ),
        );
    }
    let key = match abox_core::audit::load_or_create_key(&logs_dir) {
        Ok(k) => k,
        Err(e) => return Check::fail(label, format!("Cannot load audit key: {e}")),
    };
    let tip = abox_core::audit::load_tip(&logs_dir);
    let report = abox_core::audit::verify_chain(&content, &key, tip.as_ref());

    if report.is_ok() {
        Check::ok_with(
            label,
            format!("{entry_count} entries, keyed hash chain intact ({})", log_path.display()),
        )
    } else {
        Check::fail(
            label,
            format!(
                "Hash chain integrity failure in {} ({} error(s)):\n{}",
                log_path.display(),
                report.errors.len(),
                report.errors.join("\n")
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

#[cfg(test)]
mod tests {
    use super::pem_cert_body;

    #[test]
    fn pem_cert_body_normalizes_whitespace() {
        let a = "-----BEGIN CERTIFICATE-----\nAAAB\nCCDD\n-----END CERTIFICATE-----\n";
        let b = "noise\n-----BEGIN CERTIFICATE-----\r\nAAABCCDD\r\n-----END CERTIFICATE-----";
        assert_eq!(pem_cert_body(a), Some("AAABCCDD".to_string()));
        assert_eq!(pem_cert_body(a), pem_cert_body(b));
    }

    #[test]
    fn pem_cert_body_rejects_non_pem() {
        assert_eq!(pem_cert_body("not a certificate"), None);
        assert_eq!(pem_cert_body("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----"), None);
    }
}
