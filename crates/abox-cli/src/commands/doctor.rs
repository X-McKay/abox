//! `abox doctor` — non-destructive environment health check.
//!
//! Prints a checklist of every prerequisite abox needs to run sandboxes.
//! Safe to run at any time; makes no changes to the system.

use abox_core::config::{
    default_claude_host_credential_file, default_codex_host_credential_file, AboxConfig,
};
use abox_core::project::{recommend_environment_profile, EnvironmentProfile, ProjectConfig};
use abox_core::runtime::images::ImageManifest;
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

/// Print a titled section of checks and collect them for the summary.
fn print_and_collect(title: &str, checks: Vec<Check>, all: &mut Vec<Check>) {
    print_section(title);
    for c in &checks {
        c.print();
    }
    all.extend(checks);
}

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

    let manifest = ImageManifest::embedded()?.with_overrides(config.images.overrides.clone());
    let mut all: Vec<Check> = Vec::new();

    print_and_collect("Host", vec![check_host_virtualization()], &mut all);
    print_and_collect(
        "Runtime (MicroSandbox)",
        vec![
            check_msb_binary(),
            check_libkrunfw(),
            check_guest_binaries(&config.state_dir),
            check_image_manifest(&manifest),
        ],
        &mut all,
    );

    // ── Configuration ────────────────────────────────────────────────────────
    print_and_collect(
        "Configuration",
        vec![
            check_config_file(config),
            check_policy_file(config),
            check_socket_path_length(config),
            check_merge_validation(config),
        ],
        &mut all,
    );

    // ── Managed Auth ─────────────────────────────────────────────────────────
    print_and_collect(
        "Managed Auth",
        vec![
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
        ],
        &mut all,
    );

    // ── CA Certificate ───────────────────────────────────────────────────────
    print_and_collect(
        "CA Certificate (HTTPS Credential Injection)",
        vec![check_ca_files(config), check_ca_trust(config)],
        &mut all,
    );

    // ── Agent-Specific Validation ────────────────────────────────────────────
    print_and_collect(
        "Agent Validation",
        vec![
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
        ],
        &mut all,
    );

    // ── Audit Log ────────────────────────────────────────────────────────────
    print_and_collect("Audit Log", vec![check_audit_log(config)], &mut all);

    // ── Environment ──────────────────────────────────────────────────────────
    let mut environment_checks = vec![
        check_profile_image_resolution(&manifest),
        check_repo_requested_profile_msb(repo_root, &manifest),
    ];
    if let Some(check) = check_declared_service_docker(repo_root) {
        environment_checks.push(check);
    }
    print_and_collect("Environment", environment_checks, &mut all);

    // ── Summary ──────────────────────────────────────────────────────────────
    let failures = all.iter().filter(|c| c.is_fail()).count();
    let warnings = all.iter().filter(|c| c.is_warn()).count();

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

// ── MicroSandbox runtime checks (ADR-008) ────────────────────────────────────

fn check_host_virtualization() -> Check {
    match crate::kvm::diagnose_host_virtualization() {
        crate::kvm::HostVirtStatus::Available { detail } => {
            Check::ok_with("Hardware virtualization", detail)
        }
        crate::kvm::HostVirtStatus::Unavailable { condition, remediation } => {
            Check::fail(condition, remediation)
        }
    }
}

fn check_msb_binary() -> Check {
    let label = "msb binary";
    match crate::msb::find_msb_binary() {
        Some(path) => Check::ok_with(label, path.display().to_string()),
        None => Check::fail(
            label,
            format!(
                "msb not found at {} or on PATH.\n\
                 Run 'abox init' to download the MicroSandbox runtime assets\n\
                 into {} (set MSB_HOME to relocate them).",
                crate::msb::msb_binary().display(),
                crate::msb::msb_home().display(),
            ),
        ),
    }
}

fn check_libkrunfw() -> Check {
    let label = "libkrunfw guest firmware";
    let lib_dir = crate::msb::msb_home().join("lib");
    let files = crate::msb::libkrunfw_files();
    if files.is_empty() {
        Check::fail(
            label,
            format!(
                "No libkrunfw.* found in {}.\n\
                 Run 'abox init' to download the MicroSandbox runtime assets.",
                lib_dir.display()
            ),
        )
    } else {
        let names: Vec<String> = files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        Check::ok_with(label, format!("{} ({})", lib_dir.display(), names.join(", ")))
    }
}

fn check_guest_binaries(state_dir: &Path) -> Check {
    let label = "Host-staged guest binaries (abox-shim, abox-bridge)";
    let dir = crate::msb::guest_binaries_dir(state_dir);
    if crate::msb::guest_binaries_present(state_dir) {
        Check::ok_with(label, dir.display().to_string())
    } else {
        Check::warn(
            label,
            format!(
                "Not found in {}.\n\
                 Official guest images bake fallback copies of abox-shim/abox-bridge,\n\
                 so sandboxes still work — host-staged copies keep the shim protocol\n\
                 in lockstep with this abox binary. Stage them with 'just build-guest-bins'.",
                dir.display()
            ),
        )
    }
}

fn check_image_manifest(manifest: &ImageManifest) -> Check {
    let label = "Guest image manifest (profile → OCI image)";
    let report = crate::msb::manifest_report(manifest);
    let listing = report.lines.join("\n");
    if !report.missing.is_empty() {
        Check::fail(
            label,
            format!(
                "{listing}\n\
                 No image mapping for: {}. Add an [images.overrides] entry in\n\
                 ~/.abox/config.toml or upgrade abox.",
                report.missing.join(", ")
            ),
        )
    } else if !report.unpinned.is_empty() {
        Check::warn(
            label,
            format!(
                "{listing}\n\
                 Unpinned (tag-addressed) profiles: {}. Digest pins are filled in\n\
                 by the image publish workflow; tag references are not content-addressed.",
                report.unpinned.join(", ")
            ),
        )
    } else {
        Check::ok_with(label, listing)
    }
}

/// Environment-section summary under the MicroSandbox backend: profiles are
/// backed by OCI images pulled on first use, not locally installed rootfs
/// files.
fn check_profile_image_resolution(manifest: &ImageManifest) -> Check {
    let label = "Guest profiles";
    let report = crate::msb::manifest_report(manifest);
    if report.missing.is_empty() {
        Check::ok_with(
            label,
            "all profiles resolve to OCI images (pulled on first use):\n".to_string()
                + &report.lines.join("\n"),
        )
    } else {
        Check::fail(
            label,
            format!(
                "profiles without an image mapping: {}\n{}",
                report.missing.join(", "),
                report.lines.join("\n")
            ),
        )
    }
}

fn check_repo_requested_profile_msb(repo_root: &Path, manifest: &ImageManifest) -> Check {
    let label = "Current repo environment profile";
    let config_path = ProjectConfig::default_path(repo_root);
    let recommendation = recommend_environment_profile(repo_root);
    let profile_advice = recommendation.advice();
    let loaded = match ProjectConfig::load(repo_root) {
        Ok(config) => config,
        Err(err) => {
            let advice = profile_advice
                .as_deref()
                .map(|advice| format!("\n\nProfile advice:\n{advice}"))
                .unwrap_or_default();
            return Check::warn(
                label,
                format!(
                    "Failed to load {}.\nRun `abox project validate` for details.\n{err:#}{advice}",
                    config_path.display()
                ),
            );
        }
    };

    let Some(project) = loaded else {
        return match (recommendation.profile, profile_advice) {
            (Some(recommended), Some(advice)) => Check::warn(
                label,
                format!(
                    "No repo config found; base profile would be used.\n{advice}\n\
                     Create a config with `abox project init --profile {recommended}` after review."
                ),
            ),
            (_, Some(advice)) => Check::warn(
                label,
                format!(
                    "No repo config found; base profile would be used.\n{advice}\n\
                     Create a config and choose a profile explicitly with `abox project init`."
                ),
            ),
            (_, None) => Check::ok_with(label, "no repo config found; base profile will be used"),
        };
    };

    let resolved = match project.resolve(repo_root) {
        Ok(resolved) => resolved,
        Err(err) => {
            let advice = profile_advice
                .as_deref()
                .map(|advice| format!("\n\nProfile advice:\n{advice}"))
                .unwrap_or_default();
            return Check::warn(
                label,
                format!(
                    "Failed to resolve {}.\nRun `abox project validate` for details.\n{err:#}{advice}",
                    config_path.display()
                ),
            );
        }
    };

    let profile = resolved.environment_profile;
    let image = match manifest.image_for_profile(profile) {
        Ok(image) => image,
        Err(err) => {
            return Check::fail(
                label,
                format!("Repo requests '{profile}' but no guest image resolves for it.\n{err:#}"),
            )
        }
    };

    // The only genuinely warn-worthy divergence is a musl Python profile paired
    // with dependencies that need manylinux wheels — that setup is likely
    // broken. Every other divergence from the *advisory* recommendation (a
    // deliberate `base`, a polyglot repo the user has already resolved, or an
    // unreadable metadata file that might hold scientific deps) is
    // informational only, so doctor does not nag about a valid explicit choice
    // or advise a downgrade it cannot justify.
    let scientific_musl_mismatch = profile == EnvironmentProfile::Python
        && recommendation.profile == Some(EnvironmentProfile::PythonGlibc)
        && !recommendation.scientific_python_dependencies.is_empty();

    match profile_advice {
        Some(advice) if scientific_musl_mismatch => Check::warn(
            label,
            format!(
                "{profile} → {} (pulled on first use)\n{advice}\n\
                 This profile uses musl, but manylinux wheels were detected.\n\
                 To switch after review: `abox project set-profile python-glibc`.",
                image.pull_reference()
            ),
        ),
        Some(advice) => Check::ok_with(
            label,
            format!("{profile} → {} (pulled on first use)\n{advice}", image.pull_reference()),
        ),
        None => Check::ok_with(
            label,
            format!("{profile} → {} (pulled on first use)", image.pull_reference()),
        ),
    }
}

/// Check Docker only for a project that has opted into Docker-backed sidecars.
/// Docker is not a prerequisite for ordinary microVM sandbox runs.
fn check_declared_service_docker(repo_root: &Path) -> Option<Check> {
    let project = ProjectConfig::load(repo_root).ok().flatten()?;
    if project.services.is_empty() {
        return None;
    }

    let mut services: Vec<String> = project.services.keys().cloned().collect();
    services.sort();
    Some(check_docker_for_services(&services, abox_core::services::docker_available()))
}

fn check_docker_for_services(services: &[String], docker_is_available: bool) -> Check {
    let label = "Docker service sidecars";
    let names = services.join(", ");
    if docker_is_available {
        Check::ok_with(label, format!("available for declared sidecars: {names}"))
    } else {
        Check::fail(
            label,
            format!(
                "This repo declares Docker-backed sidecars: {names}.\n\
                 Start Docker and verify it with `docker info`, or remove [services] from \
                 .abox/project.toml. Docker is only needed because this repo declared sidecars."
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

/// Compile the host `[merge.validation]` policy so an invalid glob is reported
/// here rather than only surfacing when a merge is attempted. The workspace
/// adapter defers this error to merge time to avoid bricking unrelated
/// commands, so doctor is the proactive diagnostic path.
fn check_merge_validation(config: &AboxConfig) -> Check {
    let label = "Merge validation ([merge.validation])";
    match abox_core::workspace::MergeValidationPolicy::compile(&config.merge.validation) {
        Ok(_) => Check::ok_with(label, "host merge validation rules compile"),
        Err(err) => Check::fail(
            label,
            format!(
                "Invalid [merge.validation] host config in ~/.abox/config.toml.\n\
                 `abox merge` will refuse to run until this is fixed:\n{err:#}"
            ),
        ),
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
                 suffixes like 'msb-<task-id>.sock_5000'.\n\
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
    use super::{
        check_declared_service_docker, check_docker_for_services, check_merge_validation,
        check_repo_requested_profile_msb, pem_cert_body, CheckStatus,
    };
    use abox_core::config::AboxConfig;
    use abox_core::runtime::images::ImageManifest;
    use tempfile::tempdir;

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

    #[test]
    fn profile_check_warns_when_scientific_python_uses_musl() {
        let temp = tempdir().unwrap();
        std::fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\ndependencies = [\"numpy>=2\"]\n",
        )
        .unwrap();
        let config_path = temp.path().join(".abox/project.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            config_path,
            "[network]\nmode = \"safe\"\n\n[environment]\nprofile = \"python\"\n",
        )
        .unwrap();

        let manifest = ImageManifest::embedded().unwrap();
        let check = check_repo_requested_profile_msb(temp.path(), &manifest);

        assert!(matches!(check.status, CheckStatus::Warn));
        assert!(check.detail.unwrap().contains("python-glibc"));
    }

    #[test]
    fn profile_check_does_not_warn_on_deliberate_base_config() {
        // The starter config `abox project init` writes has no [environment]
        // section (resolves to `base`). A detected ecosystem must not turn that
        // deliberate choice into a permanent warning.
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        let config_path = temp.path().join(".abox/project.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(config_path, "[network]\nmode = \"safe\"\n").unwrap();

        let manifest = ImageManifest::embedded().unwrap();
        let check = check_repo_requested_profile_msb(temp.path(), &manifest);

        assert!(matches!(check.status, CheckStatus::Ok), "deliberate base config should not warn");
    }

    #[test]
    fn merge_validation_check_fails_on_invalid_glob() {
        let mut config = AboxConfig::default();
        config.merge.validation.deny_patterns = vec!["/etc/**".to_string()];
        let check = check_merge_validation(&config);
        assert!(matches!(check.status, CheckStatus::Fail));

        let ok = check_merge_validation(&AboxConfig::default());
        assert!(matches!(ok.status, CheckStatus::Ok));
    }

    #[test]
    fn docker_check_is_only_created_for_declared_sidecars() {
        let temp = tempdir().unwrap();
        assert!(check_declared_service_docker(temp.path()).is_none());

        let services = vec!["postgres".to_string()];
        let check = check_docker_for_services(&services, false);
        assert!(matches!(check.status, CheckStatus::Fail));
        assert!(check.detail.unwrap().contains("postgres"));
    }
}
