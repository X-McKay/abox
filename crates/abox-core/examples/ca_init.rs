//! Generate the abox root CA if it doesn't already exist.
//!
//! Used by `bootstrap_vm.sh` to ensure a CA cert is available before
//! building the guest rootfs.

fn main() -> anyhow::Result<()> {
    let ca_dir = abox_core::ca::RootCa::default_dir()?;
    let ca = abox_core::ca::RootCa::load_or_generate(&ca_dir)?;
    println!("CA cert: {}/root.crt", ca_dir.display());
    println!("CA key:  {}/root.key", ca_dir.display());
    println!("Cert PEM length: {} bytes", ca.cert_pem.len());
    Ok(())
}
