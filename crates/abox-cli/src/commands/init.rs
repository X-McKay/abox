//! `abox init` — guided first-run setup wizard.
//!
//! Walks through every prerequisite in order, offering to fix each one:
//!   1. Check KVM access
//!   2. Bootstrap VM artifacts (runs bootstrap_vm.sh if needed)
//!   3. Write ~/.abox/config.toml (from the embedded example template)
//!   4. Install the default policy file
//!   5. Check PATH and print the export line if needed
//!   6. Print a "you're ready" summary
//!
//! All steps are idempotent — safe to re-run.

use anyhow::{Context, Result};
use crossterm::style::Stylize;
use crossterm::tty::IsTty;
use std::path::{Path, PathBuf};

// ── Color helpers ────────────────────────────────────────────────────────────

fn use_color() -> bool {
    std::env::var("NO_COLOR").is_err()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
        && std::io::stdout().is_tty()
}
fn col_green(s: &str) -> String {
    if use_color() { s.green().to_string() } else { s.to_string() }
}
fn col_yellow(s: &str) -> String {
    if use_color() { s.yellow().to_string() } else { s.to_string() }
}
fn col_bold(s: &str) -> String {
    if use_color() { s.bold().to_string() } else { s.to_string() }
}
fn col_dim(s: &str) -> String {
    if use_color() { s.dim().to_string() } else { s.to_string() }
}
fn col_cyan(s: &str) -> String {
    if use_color() { s.cyan().to_string() } else { s.to_string() }
}

#[derive(clap::Args)]
pub struct InitArgs {
    /// Non-interactive mode: automatically answer yes to all prompts.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn execute(args: &InitArgs) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    println!(
        "{}  {}",
        col_bold(&col_cyan("abox init")),
        col_dim(&format!("v{version} — first-run setup wizard"))
    );
    println!();

    // ── Step 1: KVM ──────────────────────────────────────────────────────────
    print_step(1, "Checking KVM access");
    check_kvm()?;

    // ── Step 2: VM artifacts ─────────────────────────────────────────────────
    print_step(2, "Checking VM artifacts");
    let vm_dir = default_state_dir().join("vm");
    ensure_vm_artifacts(&vm_dir, args.yes)?;

    // ── Step 3: virtiofsd sandbox capability ────────────────────────────────
    print_step(3, "Checking virtiofsd sandbox permissions");
    ensure_virtiofsd_caps(&vm_dir, !args.yes)?;

    // ── Step 4: Root CA ──────────────────────────────────────────────────────
    print_step(4, "Checking root CA");
    ensure_root_ca()?;

    // ── Step 5: Config file ──────────────────────────────────────────────────
    print_step(5, "Checking config file");
    let config_path = ensure_config_file(&vm_dir)?;

    // ── Step 6: Policy file ──────────────────────────────────────────────────
    print_step(6, "Checking policy file");
    ensure_policy_file()?;

    // ── Step 7: Credential detection ─────────────────────────────────────────
    print_step(7, "Detecting credentials");
    detect_credentials(&config_path, args.yes)?;

    // ── Step 8: PATH ─────────────────────────────────────────────────────────
    print_step(8, "Checking PATH");
    check_path();

    // ── Summary ──────────────────────────────────────────────────────────────
    println!();
    println!("  {}  Setup complete. You're ready to run your first sandbox:", col_green("✓"));
    println!();
    println!("  {}  cd /path/to/your/git/repo", col_dim("$"));
    println!("  {}  abox run --task hello -- echo \"hello from inside the sandbox\"", col_dim("$"));
    println!();
    println!("  {}  Run {} at any time to re-check your environment.",
        col_dim("tip"),
        col_bold("abox doctor")
    );

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

fn check_kvm() -> Result<()> {
    match crate::kvm::diagnose_kvm() {
        crate::kvm::KvmStatus::Available => {
            print_ok("/dev/kvm is accessible");
            Ok(())
        }
        crate::kvm::KvmStatus::Unavailable { condition, remediation } => {
            anyhow::bail!("{condition}\n\n{remediation}")
        }
    }
}

fn ensure_vm_artifacts(vm_dir: &Path, yes: bool) -> Result<()> {
    let required = ["cloud-hypervisor", "virtiofsd", "vmlinux", "rootfs.raw"];
    let missing: Vec<&str> =
        required.iter().copied().filter(|f| !vm_dir.join(f).exists()).collect();

    if missing.is_empty() {
        print_ok("VM artifacts already present");
        return Ok(());
    }

    println!("    Missing artifacts: {}", missing.join(", "));

    // Find the bootstrap script relative to the running binary or via a
    // well-known location. Fall back to asking the user to run it manually.
    let bootstrap = find_bootstrap_script();

    if let Some(script) = bootstrap {
        print_action(&format!("Running {} --yes --no-symlink", script.display()));
        let status = std::process::Command::new("bash")
            .arg(&script)
            .arg("--yes")
            .arg("--no-symlink")
            .env("BOOTSTRAP_YES", if yes { "1" } else { "0" })
            .status()
            .with_context(|| format!("Failed to run {}", script.display()))?;
        if !status.success() {
            anyhow::bail!(
                "bootstrap_vm.sh exited with status {status}.\nCheck the output above for details."
            );
        }
        print_ok("VM artifacts installed");
    } else {
        println!(
            "\n    Could not locate bootstrap_vm.sh automatically.\n\
             \n\
             Please run it manually from the abox source tree:\n\
             \n\
             \x20 ./scripts/bootstrap_vm.sh --yes\n\
             \n\
             Then re-run 'abox init'."
        );
        anyhow::bail!("VM artifacts missing — bootstrap required");
    }

    Ok(())
}

fn ensure_virtiofsd_caps(vm_dir: &Path, allow_sudo_prompt: bool) -> Result<()> {
    let virtiofsd = vm_dir.join("virtiofsd");
    match crate::virtiofsd::ensure_virtiofsd_caps(&virtiofsd, allow_sudo_prompt) {
        crate::virtiofsd::EnsureVirtiofsdCapsOutcome::AlreadyPresent => {
            print_ok(&format!(
                "virtiofsd sandbox capability already present: {}",
                virtiofsd.display()
            ));
            Ok(())
        }
        crate::virtiofsd::EnsureVirtiofsdCapsOutcome::Applied => {
            print_action(&format!("Applied cap_sys_admin+ep to {}", virtiofsd.display()));
            Ok(())
        }
        crate::virtiofsd::EnsureVirtiofsdCapsOutcome::NeedsManual { condition, remediation } => {
            anyhow::bail!("{condition}\n\n{remediation}")
        }
    }
}

/// Try to locate bootstrap_vm.sh. Checks:
///   1. Relative to the running binary (for source builds: binary is in
///      target/…/abox, script is at <repo-root>/scripts/bootstrap_vm.sh)
///   2. A sibling `scripts/` directory next to the binary
fn find_bootstrap_script() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;

    // Walk up from the binary looking for scripts/bootstrap_vm.sh
    let mut dir = exe.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("scripts/bootstrap_vm.sh");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
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

fn ensure_config_file(vm_dir: &Path) -> Result<PathBuf> {
    let state_dir = default_state_dir();
    let config_path = state_dir.join("config.toml");

    if config_path.exists() {
        print_ok(&format!("Config file already exists: {}", config_path.display()));
        return Ok(config_path);
    }

    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("Failed to create {}", state_dir.display()))?;

    // Write a config that wires image_path and kernel_path to the bootstrapped
    // artifact locations so users don't have to edit the file manually.
    let image_path = vm_dir.join("rootfs.raw");
    let kernel_path = vm_dir.join("vmlinux");
    let runtime_dir = state_dir.join("r");

    let content = format!(
        "# abox configuration — generated by 'abox init'\n\
         # Edit to customise; see templates/config.example.toml for all options.\n\
         \n\
         # runtime_dir is kept short to stay within Linux's 108-byte Unix socket\n\
         # path limit (abox appends per-sandbox suffixes to socket names).\n\
         runtime_dir = \"{runtime_dir}\"\n\
         \n\
         [vm_defaults]\n\
         memory_mib = 2048\n\
         vcpus = 2\n\
         image_path  = \"{image_path}\"\n\
         kernel_path = \"{kernel_path}\"\n\
         \n\
         [proxy]\n\
         egress_port = 18443\n",
        runtime_dir = runtime_dir.display(),
        image_path = image_path.display(),
        kernel_path = kernel_path.display(),
    );

    std::fs::write(&config_path, content)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    print_action(&format!("Created {}", config_path.display()));
    Ok(config_path)
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

/// Detect host credentials and offer to add them to config.
///
/// Claude credentials are already in the default config (`GuestConfig::default()`
/// adds `~/.claude/.credentials.json`). This wizard focuses on Codex, which is
/// not in the default, and prints a summary of all credential sources.
fn detect_credentials(config_path: &Path, yes: bool) -> Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

    // Check known credential sources.
    let claude_found = home.join(".claude/.credentials.json").exists();
    let codex_found = home.join(".codex/auth.json").exists();
    let github_token = std::env::var("GITHUB_TOKEN").is_ok();
    let google_key = std::env::var("GOOGLE_API_KEY").is_ok();

    // Offer to add Codex credentials if found and not already in config.
    if codex_found {
        let config_content = std::fs::read_to_string(config_path).unwrap_or_default();
        if config_content.contains(".codex/auth.json") {
            print_ok("Codex credentials already configured");
        } else {
            let add = if yes {
                true
            } else {
                print!("    Found Codex credentials at ~/.codex/auth.json. Add to sandbox config? [Y/n] ");
                use std::io::Write;
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                let trimmed = input.trim().to_lowercase();
                trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
            };

            if add {
                let entry = "\n\
                    [[guest.credential_files]]\n\
                    host = \"~/.codex/auth.json\"\n\
                    guest = \"~/.codex/auth.json\"\n\
                    mode = \"0600\"\n";
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(config_path)
                    .with_context(|| format!("Failed to append to {}", config_path.display()))?;
                use std::io::Write;
                file.write_all(entry.as_bytes())?;
                print_action("Added Codex credentials to config");
            }
        }
    }

    // Print status summary.
    println!();
    println!("    Credential status:");
    println!(
        "      Claude Code: {}",
        if claude_found {
            "~/.claude/.credentials.json (configured by default)"
        } else {
            "not found"
        }
    );
    println!(
        "      Codex:       {}",
        if codex_found { "~/.codex/auth.json (configured)" } else { "not found" }
    );
    println!(
        "      GITHUB_TOKEN: {}",
        if github_token { "set (policy handles injection)" } else { "not set" }
    );
    println!(
        "      GOOGLE_API_KEY: {}",
        if google_key { "set (policy handles injection)" } else { "not set" }
    );

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
