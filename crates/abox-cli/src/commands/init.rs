//! `abox init` — guided first-run setup wizard.
//!
//! Walks through every prerequisite in order, offering to fix each one:
//!
//!   1. Check hardware virtualization (KVM / Hypervisor.framework)
//!   2. Install MicroSandbox runtime assets (msb + libkrunfw) into $MSB_HOME
//!   3. Generate the root CA
//!   4. Write ~/.abox/config.toml
//!   5. Install the default policy file
//!   6. Detect managed-agent credentials
//!   7. Check host-staged guest binaries + resolve requested profiles
//!   8. Check PATH
//!
//! All steps are idempotent — safe to re-run.

use abox_core::config::AboxConfig;
use abox_core::project::EnvironmentProfile;
use abox_core::runtime::images::ImageManifest;
use anyhow::{Context, Result};
use crossterm::style::Stylize;
use crossterm::tty::IsTty;
use std::path::{Path, PathBuf};

// ── Color helpers ────────────────────────────────────────────────────────────

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

#[derive(clap::Args)]
pub struct InitArgs {
    /// Non-interactive mode: automatically answer yes to all prompts.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Install one or more additional official guest profiles.
    ///
    /// `base` is always installed by default. Repeat this flag to add
    /// profiles such as `node`, `python`, or `rust`.
    #[arg(long = "profile", value_enum)]
    pub profiles: Vec<InitProfileArg>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum InitProfileArg {
    Base,
    Node,
    Python,
    PythonGlibc,
    Rust,
}

pub fn execute(args: &InitArgs) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");

    // init must work with a missing OR malformed config file, so a failed
    // load falls back to defaults.
    let config_path = default_state_dir().join("config.toml");
    let loaded = AboxConfig::load(&config_path).unwrap_or_else(|_| AboxConfig::default());

    println!(
        "{}  {}",
        col_bold(&col_cyan("abox init")),
        col_dim(&format!("v{version} — first-run setup wizard"))
    );
    println!();

    execute_microsandbox(args, &loaded)?;

    // ── Summary ──────────────────────────────────────────────────────────────
    println!();
    println!("  {}  Setup complete. You're ready to run your first sandbox:", col_green("✓"));
    println!();
    println!("  {}  cd /path/to/your/git/repo", col_dim("$"));
    println!("  {}  abox run --task hello -- echo \"hello from inside the sandbox\"", col_dim("$"));
    println!();
    println!(
        "  {}  Run {} at any time to re-check your environment.",
        col_dim("tip"),
        col_bold("abox doctor")
    );

    Ok(())
}

/// Setup flow for the default MicroSandbox (libkrun) runtime.
fn execute_microsandbox(args: &InitArgs, loaded: &AboxConfig) -> Result<()> {
    // ── Step 1: hardware virtualization ─────────────────────────────────────
    print_step(1, "Checking hardware virtualization");
    check_host_virtualization()?;

    // ── Step 2: MicroSandbox runtime assets ──────────────────────────────────
    print_step(2, "Checking MicroSandbox runtime assets");
    ensure_msb_assets()?;

    // ── Step 3: Root CA ──────────────────────────────────────────────────────
    print_step(3, "Checking root CA");
    ensure_root_ca()?;

    // ── Step 4: Config file ──────────────────────────────────────────────────
    print_step(4, "Checking config file");
    let config_path = ensure_config_file_msb()?;

    // ── Step 5: Policy file ──────────────────────────────────────────────────
    print_step(5, "Checking policy file");
    ensure_policy_file()?;

    // ── Step 6: Credential detection ─────────────────────────────────────────
    print_step(6, "Detecting credentials");
    detect_credentials(&config_path, args.yes)?;

    // ── Step 7: Guest binaries + profiles ────────────────────────────────────
    print_step(7, "Checking guest binaries and profiles");
    note_guest_binaries(&loaded.state_dir);
    verify_profiles_resolve(&args.profiles, loaded)?;

    // ── Step 8: PATH ─────────────────────────────────────────────────────────
    print_step(8, "Checking PATH");
    check_path();

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn print_step(n: u8, label: &str) {
    println!("\n  {}  {}", col_bold(&col_cyan(&format!("[{n}]"))), col_bold(label));
}

fn print_ok(msg: &str) {
    println!("      {}  {}", col_green("✓"), msg);
}

fn print_action(msg: &str) {
    println!("      {}  {}", col_yellow("→"), col_dim(msg));
}

fn default_state_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".abox")
}

fn check_host_virtualization() -> Result<()> {
    match crate::kvm::diagnose_host_virtualization() {
        crate::kvm::HostVirtStatus::Available { detail } => {
            print_ok(&detail);
            Ok(())
        }
        crate::kvm::HostVirtStatus::Unavailable { condition, remediation } => {
            anyhow::bail!("{condition}\n\n{remediation}")
        }
    }
}

/// Ensure msb + libkrunfw are installed under `$MSB_HOME` (default
/// `~/.microsandbox`), downloading them via the MicroSandbox SDK if needed.
fn ensure_msb_assets() -> Result<()> {
    let home = crate::msb::msb_home();
    if microsandbox::setup::is_installed() {
        print_ok(&format!("msb + libkrunfw already present in {}", home.display()));
        return Ok(());
    }

    print_action(&format!(
        "Downloading MicroSandbox runtime assets (msb + libkrunfw) into {} ...",
        home.display()
    ));
    crate::msb::block_on(microsandbox::setup::install())?
        .map_err(|e| anyhow::anyhow!("MicroSandbox runtime asset installation failed: {e}"))?;

    if !microsandbox::setup::is_installed() {
        anyhow::bail!(
            "MicroSandbox runtime assets are still missing from {} after installation.\n\
             Expected bin/msb and lib/libkrunfw.* — check the output above, then re-run\n\
             'abox init'.",
            home.display()
        );
    }
    print_ok(&format!("Installed msb + libkrunfw into {}", home.display()));
    Ok(())
}

/// Informational note about host-staged guest binaries. Never fails: the
/// official guest images bake fallback copies of abox-shim/abox-bridge.
fn note_guest_binaries(state_dir: &Path) {
    let dir = crate::msb::guest_binaries_dir(state_dir);
    if crate::msb::guest_binaries_present(state_dir) {
        print_ok(&format!("Host-staged guest binaries present: {}", dir.display()));
    } else {
        println!(
            "      {}  No host-staged guest binaries in {}.\n\
             \x20        Official guest images already include abox-shim and abox-bridge, so\n\
             \x20        nothing else is required. To stage host-built copies (keeps the shim\n\
             \x20        protocol in lockstep with this abox binary), run 'just build-guest-bins'.",
            col_dim("i"),
            dir.display()
        );
    }
}

/// Verify that every requested profile (plus the always-available `base`)
/// resolves in the image manifest, and print the OCI reference that will be
/// pulled on first use. Under MicroSandbox nothing is downloaded at init
/// time.
fn verify_profiles_resolve(requested: &[InitProfileArg], config: &AboxConfig) -> Result<()> {
    let manifest = ImageManifest::embedded()?.with_overrides(config.images.overrides.clone());

    let mut profiles: Vec<EnvironmentProfile> = vec![EnvironmentProfile::Base];
    for arg in requested {
        let profile: EnvironmentProfile = arg.as_str().parse()?;
        if !profiles.contains(&profile) {
            profiles.push(profile);
        }
    }

    for profile in profiles {
        let image = manifest.image_for_profile(profile).with_context(|| {
            format!("profile '{profile}' has no guest image in this abox build")
        })?;
        print_ok(&format!("profile {profile} → {} (pulled on first use)", image.pull_reference()));
    }
    Ok(())
}

/// Write a MicroSandbox-flavored `~/.abox/config.toml` (no kernel/rootfs
/// paths; documents the transitional `[runtime]` and `[images]` sections).
fn ensure_config_file_msb() -> Result<PathBuf> {
    let state_dir = default_state_dir();
    let config_path = state_dir.join("config.toml");

    if config_path.exists() {
        print_ok(&format!("Config file already exists: {}", config_path.display()));
        return Ok(config_path);
    }

    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("Failed to create {}", state_dir.display()))?;

    let runtime_dir = state_dir.join("r");
    let content = format!(
        "# abox configuration — generated by 'abox init'\n\
         # Edit to customise; see templates/config.example.toml for all options.\n\
         \n\
         # runtime_dir is kept short to stay within the 104-byte Unix socket\n\
         # path limit (abox appends per-sandbox suffixes to socket names).\n\
         runtime_dir = \"{runtime_dir}\"\n\
         \n\
         [sandbox_defaults]\n\
         memory_mib = 2048\n\
         vcpus = 2\n\
         \n\
         [proxy]\n\
         egress_port = 18443\n\
         \n\
         # ── Guest OCI images ─────────────────────────────────────────────────────\n\
         # Environment profiles resolve to pinned OCI images via the manifest\n\
         # embedded in this abox build. Development escape hatch (host-owned only):\n\
         #\n\
         # [images.overrides]\n\
         # node = \"localhost:5000/dev-guest:latest\"\n",
        runtime_dir = runtime_dir.display(),
    );

    std::fs::write(&config_path, content)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    print_action(&format!("Created {}", config_path.display()));
    Ok(config_path)
}

impl InitProfileArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Node => "node",
            Self::Python => "python",
            Self::PythonGlibc => "python-glibc",
            Self::Rust => "rust",
        }
    }
}

fn ensure_root_ca() -> Result<()> {
    let ca_dir = abox_core::ca::RootCa::default_dir()?;
    if ca_dir.join("root.crt").exists() && ca_dir.join("root.key").exists() {
        print_ok("Root CA already exists");
        return Ok(());
    }
    // Generate directly — no need for cargo/source tree.
    let _ca =
        abox_core::ca::RootCa::load_or_generate(&ca_dir).context("Failed to generate root CA")?;
    print_action(&format!("Generated root CA at {}", ca_dir.display()));
    Ok(())
}

fn ensure_policy_file() -> Result<()> {
    let policy_dir = default_state_dir().join("policies");
    let policy_path = policy_dir.join("default.toml");

    if policy_path.exists() {
        print_ok(&format!("Policy file already exists: {}", policy_path.display()));
        return Ok(());
    }

    std::fs::create_dir_all(&policy_dir)
        .with_context(|| format!("Failed to create {}", policy_dir.display()))?;

    // Locate the default policy from the source tree (same search as bootstrap).
    let source_policy = find_source_policy();

    if let Some(src) = source_policy {
        std::fs::copy(&src, &policy_path).with_context(|| {
            format!("Failed to copy {} to {}", src.display(), policy_path.display())
        })?;
        print_action(&format!("Installed default policy to {}", policy_path.display()));
    } else {
        // Embed a minimal but functional default policy as a fallback so
        // init works even when run from an installed binary with no source tree.
        let embedded = include_str!("../../../../policies/default.toml");
        std::fs::write(&policy_path, embedded)
            .with_context(|| format!("Failed to write {}", policy_path.display()))?;
        print_action(&format!("Installed embedded default policy to {}", policy_path.display()));
    }

    Ok(())
}

fn find_source_policy() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("policies/default.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

/// Detect managed provider credentials, enable providers when present, and
/// print a summary of what abox can support out of the box.
fn detect_credentials(config_path: &Path, yes: bool) -> Result<()> {
    let _ = yes;
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

    // Check known credential sources.
    let claude_found = home.join(".claude/.credentials.json").exists();
    let codex_found = home.join(".codex/auth.json").exists();

    let config_content = std::fs::read_to_string(config_path).unwrap_or_default();

    if claude_found {
        if provider_section_present(&config_content, "claude") {
            print_ok("Claude Code managed auth already configured");
        } else {
            append_managed_provider(config_path, "claude")?;
            print_action("Enabled Claude Code managed auth");
        }
    }

    if codex_found {
        if provider_section_present(&config_content, "codex") {
            print_ok("Codex managed auth already configured");
        } else {
            append_managed_provider(config_path, "codex")?;
            print_action("Enabled Codex managed auth");
        }
    }

    // Print status summary.
    println!();
    println!("    Managed provider status:");
    println!(
        "      Claude Code: {}",
        if claude_found {
            "~/.claude/.credentials.json (enabled)"
        } else {
            "not found — provider left disabled"
        }
    );
    println!(
        "      Codex:       {}",
        if codex_found {
            "~/.codex/auth.json (enabled)"
        } else {
            "not found — provider left disabled"
        }
    );

    if !claude_found && !codex_found {
        println!();
        println!("    {}  No Claude Code or Codex host credentials were found.", col_yellow("!"));
        println!(
            "      {}",
            col_dim(
                "abox can still launch arbitrary sandbox commands, but the default managed agent \
                 workflow requires at least one of those providers."
            )
        );
    }

    println!();
    println!(
        "    GitHub access is optional and stays on the host via managed 'git'/'gh' commands."
    );

    Ok(())
}

fn provider_section_present(config_content: &str, provider: &str) -> bool {
    config_content.contains(&format!("[auth.providers.{provider}]"))
}

fn append_managed_provider(config_path: &Path, provider: &str) -> Result<()> {
    let entry = format!("\n[auth.providers.{provider}]\nenabled = true\n");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(config_path)
        .with_context(|| format!("Failed to append to {}", config_path.display()))?;
    use std::io::Write;
    file.write_all(entry.as_bytes())?;
    Ok(())
}

fn check_path() {
    let abox_bin = default_state_dir().join("bin");

    let abox_on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|p| Path::new(p).join("abox").exists());

    if abox_on_path {
        print_ok("abox is on PATH");
    } else if abox_bin.join("abox").exists() {
        println!(
            "    ! abox is not on your PATH.\n\
             \n\
             \x20 Add this line to your shell profile (~/.bashrc or ~/.zshrc):\n\
             \n\
             \x20   export PATH=\"{}:$PATH\"\n\
             \n\
             \x20 Then reload your shell: source ~/.bashrc",
            abox_bin.display()
        );
    } else {
        print_ok("PATH check skipped (running from source build)");
    }
}
