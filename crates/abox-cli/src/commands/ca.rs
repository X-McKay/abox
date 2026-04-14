//! `abox ca` subcommand — manage the root CA for TLS-terminating proxy.

use abox_core::ca::RootCa;
use anyhow::{Context, Result};
use clap::Subcommand;

#[derive(Debug, Clone, Subcommand)]
pub enum CaCommand {
    /// Show the CA certificate info.
    Show,
    /// Regenerate the root CA (invalidates all leaf certs and requires rootfs rebuild).
    Rotate,
    /// Print the CA directory path.
    Path,
}

pub fn execute(cmd: &CaCommand) -> Result<()> {
    let ca_dir = RootCa::default_dir()?;

    match cmd {
        CaCommand::Show => {
            let cert_path = ca_dir.join("root.crt");
            let key_path = ca_dir.join("root.key");

            if !cert_path.exists() {
                println!("No CA certificate found at {}", cert_path.display());
                println!("Run `abox ca rotate` to generate one.");
                return Ok(());
            }

            let ca = RootCa::load(&ca_dir).context("Failed to load CA")?;
            let cert_lines: Vec<&str> = ca.cert_pem.lines().collect();
            let cert_bytes = ca.cert_pem.len();
            let key_exists = key_path.exists();

            println!("CA directory:  {}", ca_dir.display());
            println!("Cert file:     {}", cert_path.display());
            println!("Key file:      {} (exists: {key_exists})", key_path.display());
            println!("Cert PEM size: {cert_bytes} bytes ({} lines)", cert_lines.len());
            println!();
            println!("To inspect the certificate details, run:");
            println!("  openssl x509 -in {} -noout -text", cert_path.display());
            println!("  openssl x509 -in {} -noout -fingerprint -sha256", cert_path.display());
        }
        CaCommand::Rotate => {
            println!("Regenerating root CA in {}...", ca_dir.display());

            // Remove existing CA files
            let cert_path = ca_dir.join("root.crt");
            let key_path = ca_dir.join("root.key");
            if cert_path.exists() {
                std::fs::remove_file(&cert_path)?;
            }
            if key_path.exists() {
                std::fs::remove_file(&key_path)?;
            }

            let _ca = RootCa::generate_and_persist(&ca_dir).context("Failed to generate new CA")?;
            println!("New CA generated.");
            println!();
            println!("IMPORTANT: You must rebuild the guest rootfs for the new CA to take effect:");
            println!("  just bootstrap-vm");
            println!();
            println!("Any running sandboxes will need to be restarted.");
        }
        CaCommand::Path => {
            println!("{}", ca_dir.display());
        }
    }

    Ok(())
}
