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
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct InitArgs {
    /// Non-interactive mode: automatically answer yes to all prompts.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn execute(args: &InitArgs) -> Result<()> {
    println!("abox init — first-run setup\n");

    // ── Step 1: KVM ──────────────────────────────────────────────────────────
    print_step(1, "Checking KVM access");
    check_kvm()?;

    // ── Step 2: VM artifacts ─────────────────────────────────────────────────
    print_step(2, "Checking VM artifacts");
    let vm_dir = default_state_dir().join("vm");
    ensure_vm_artifacts(&vm_dir, args.yes)?;

    // ── Step 3: Config file ──────────────────────────────────────────────────
    print_step(3, "Checking config file");
    ensure_config_file(&vm_dir)?;

    // ── Step 4: Policy file ──────────────────────────────────────────────────
    print_step(4, "Checking policy file");
    ensure_policy_file()?;

    // ── Step 5: PATH ─────────────────────────────────────────────────────────
    print_step(5, "Checking PATH");
    check_path(&vm_dir);

    // ── Summary ──────────────────────────────────────────────────────────────
    println!();
    println!("Setup complete. You're ready to run your first sandbox:");
    println!();
    println!("  cd /path/to/your/git/repo");
    println!("  abox run --task hello -- /bin/sh -c \"echo hello from inside the sandbox\"");
    println!();
    println!("See 'abox doctor' at any time to re-check your environment.");

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn print_step(n: u8, label: &str) {
    println!("[{n}] {label}...");
}

fn print_ok(msg: &str) {
    println!("    ✓ {msg}");
}

fn print_action(msg: &str) {
    println!("    → {msg}");
}

fn default_state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".abox")
}

fn check_kvm() -> Result<()> {
    let kvm = Path::new("/dev/kvm");
    if !kvm.exists() {
        anyhow::bail!(
            "/dev/kvm not found.\n\n\
             abox requires a Linux host with KVM support.\n\
             Ensure your kernel has KVM enabled and that you're running on\n\
             bare metal or a VM that exposes nested virtualisation."
        );
    }
    match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
        Ok(_) => {
            print_ok("/dev/kvm is accessible");
            Ok(())
        }
        Err(_) => {
            anyhow::bail!(
                "Permission denied on /dev/kvm.\n\n\
                 Add yourself to the kvm group and log out/in:\n\n\
                 \x20 sudo usermod -aG kvm $USER"
            )
        }
    }
}

fn ensure_vm_artifacts(vm_dir: &Path, yes: bool) -> Result<()> {
    let required = ["cloud-hypervisor", "virtiofsd", "vmlinux", "rootfs.raw"];
    let missing: Vec<&str> = required.iter().copied().filter(|f| !vm_dir.join(f).exists()).collect();

    if missing.is_empty() {
        print_ok("VM artifacts already present");
        return Ok(());
    }

    println!("    Missing artifacts: {}", missing.join(", "));

    // Find the bootstrap script relative to the running binary or via a
    // well-known location. Fall back to asking the user to run it manually.
    let bootstrap = find_bootstrap_script();

    match bootstrap {
        Some(script) => {
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
                    "bootstrap_vm.sh exited with status {}.\n\
                     Check the output above for details.",
                    status
                );
            }
            print_ok("VM artifacts installed");
        }
        None => {
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
    }

    Ok(())
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

fn ensure_config_file(vm_dir: &Path) -> Result<()> {
    let state_dir = default_state_dir();
    let config_path = state_dir.join("config.toml");

    if config_path.exists() {
        print_ok(&format!("Config file already exists: {}", config_path.display()));
        return Ok(());
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

    match source_policy {
        Some(src) => {
            std::fs::copy(&src, &policy_path).with_context(|| {
                format!("Failed to copy {} to {}", src.display(), policy_path.display())
            })?;
            print_action(&format!("Installed default policy to {}", policy_path.display()));
        }
        None => {
            // Embed a minimal but functional default policy as a fallback so
            // init works even when run from an installed binary with no source tree.
            let embedded = include_str!("../../../../policies/default.toml");
            std::fs::write(&policy_path, embedded)
                .with_context(|| format!("Failed to write {}", policy_path.display()))?;
            print_action(&format!("Installed embedded default policy to {}", policy_path.display()));
        }
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

fn check_path(vm_dir: &Path) {
    let local_bin: PathBuf = dirs::home_dir()
        .map(|h: PathBuf| h.join(".local/bin"))
        .unwrap_or_else(|| PathBuf::from("~/.local/bin"));

    let ch_on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|p| Path::new(p).join("cloud-hypervisor").exists());

    if ch_on_path {
        print_ok("cloud-hypervisor is on PATH");
    } else if vm_dir.join("cloud-hypervisor").exists() {
        println!(
            "    ! ~/.local/bin is not on your PATH.\n\
             \n\
             \x20 Add this line to your shell profile (~/.bashrc or ~/.zshrc):\n\
             \n\
             \x20   export PATH=\"{}:$PATH\"\n\
             \n\
             \x20 Then reload your shell: source ~/.bashrc",
            local_bin.display()
        );
    } else {
        print_ok("PATH check skipped (VM artifacts not yet installed)");
    }
}
