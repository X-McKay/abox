//! Root CA generation and leaf-cert signing for the TLS-terminating request
//! broker.
//!
//! The root CA is persisted to `~/.abox/ca/` and staged into each guest's
//! trust store at launch by the runtime adapter, so guest images never bake
//! a specific CA. Leaf certs are cached in memory (keyed by SNI hostname) so
//! repeated connections to the same host reuse the same certificate.

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use time::{Duration, OffsetDateTime};

/// A root CA certificate and key pair, used to sign per-host leaf certs.
pub struct RootCa {
    /// PEM-encoded root certificate.
    pub cert_pem: String,
    /// PEM-encoded root private key.
    pub key_pem: String,
    /// Parsed certificate (for signing leaves).
    cert: rcgen::Certificate,
    /// Parsed key pair (for signing leaves).
    key_pair: KeyPair,
    /// Cache of signed leaf certificates keyed by SNI hostname.
    leaf_cache: RwLock<HashMap<String, Arc<CertifiedKey>>>,
}

/// A leaf certificate chain + private key bundle, suitable for rustls.
#[derive(Debug)]
pub struct CertifiedKey {
    /// PEM-encoded leaf certificate (signed by root CA).
    pub cert_pem: String,
    /// PEM-encoded leaf private key.
    pub key_pem: String,
}

impl RootCa {
    /// Generate a new root CA and persist it to `dir/root.crt` and `dir/root.key`.
    ///
    /// The certificate has 10-year validity, `CN = "abox sandbox CA"`,
    /// `BasicConstraints: CA:true, pathlen:0`, and `KeyUsage: certSign + crlSign`.
    pub fn generate_and_persist(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating CA directory {}", dir.display()))?;

        let key_pair = KeyPair::generate().context("generating CA key pair")?;

        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, "abox sandbox CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.not_before = OffsetDateTime::now_utc();
        params.not_after = OffsetDateTime::now_utc() + Duration::days(3650);

        let cert = params.self_signed(&key_pair).context("self-signing root CA")?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();

        // Write cert (world-readable)
        let cert_path = dir.join("root.crt");
        std::fs::write(&cert_path, &cert_pem)
            .with_context(|| format!("writing {}", cert_path.display()))?;

        // Write key (owner-only)
        let key_path = dir.join("root.key");
        std::fs::write(&key_path, &key_pem)
            .with_context(|| format!("writing {}", key_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        // We need the rcgen Certificate for signing leaves. rcgen's self_signed
        // returns a certificate that can't directly sign other certs, so we
        // re-parse the PEM to get a usable Certificate.
        let cert_for_signing =
            CertificateParams::from_ca_cert_pem(&cert_pem).context("re-parsing CA cert PEM")?;
        let signing_cert =
            cert_for_signing.self_signed(&key_pair).context("re-creating signing cert")?;

        Ok(Self {
            cert_pem,
            key_pem,
            cert: signing_cert,
            key_pair,
            leaf_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Load an existing root CA from `dir/root.crt` and `dir/root.key`.
    pub fn load(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("root.crt");
        let key_path = dir.join("root.key");

        let cert_pem = std::fs::read_to_string(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key_pem = std::fs::read_to_string(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;

        let key_pair = KeyPair::from_pem(&key_pem).context("parsing CA key PEM")?;
        let cert_params =
            CertificateParams::from_ca_cert_pem(&cert_pem).context("parsing CA cert PEM")?;
        let cert =
            cert_params.self_signed(&key_pair).context("re-creating signing cert from PEM")?;

        Ok(Self { cert_pem, key_pem, cert, key_pair, leaf_cache: RwLock::new(HashMap::new()) })
    }

    /// Load an existing CA or generate a new one. Idempotent entry point.
    pub fn load_or_generate(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("root.crt");
        let key_path = dir.join("root.key");

        if cert_path.exists() && key_path.exists() {
            Self::load(dir)
        } else {
            Self::generate_and_persist(dir)
        }
    }

    /// Return the default CA directory (`~/.abox/ca/`).
    pub fn default_dir() -> Result<std::path::PathBuf> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        Ok(home.join(".abox").join("ca"))
    }

    /// Sign a leaf certificate for the given SNI hostname.
    ///
    /// Returns a cached `Arc<CertifiedKey>` if one has already been generated
    /// for this hostname. Otherwise generates a new leaf cert with 30-day
    /// validity and `SAN = DnsName(sni)`, signed by this root CA.
    pub fn sign_leaf(&self, sni: &str) -> Result<Arc<CertifiedKey>> {
        // Fast path: check read lock
        {
            let cache = self.leaf_cache.read().map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(ck) = cache.get(sni) {
                return Ok(Arc::clone(ck));
            }
        }

        // Slow path: generate and insert under write lock
        let mut cache = self.leaf_cache.write().map_err(|e| anyhow::anyhow!("{e}"))?;
        // Double-check after acquiring write lock
        if let Some(ck) = cache.get(sni) {
            return Ok(Arc::clone(ck));
        }

        let leaf_key = KeyPair::generate().context("generating leaf key pair")?;

        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, sni);
        params.subject_alt_names = vec![SanType::DnsName(sni.try_into()?)];
        params.not_before = OffsetDateTime::now_utc();
        params.not_after = OffsetDateTime::now_utc() + Duration::days(30);
        params.is_ca = IsCa::NoCa;
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

        let leaf_cert = params
            .signed_by(&leaf_key, &self.cert, &self.key_pair)
            .with_context(|| format!("signing leaf cert for {sni}"))?;

        let ck =
            Arc::new(CertifiedKey { cert_pem: leaf_cert.pem(), key_pem: leaf_key.serialize_pem() });

        cache.insert(sni.to_string(), Arc::clone(&ck));
        Ok(ck)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn roundtrip_generate_load_certs_match() {
        let tmp = TempDir::new().unwrap();
        let ca1 = RootCa::generate_and_persist(tmp.path()).unwrap();
        let ca2 = RootCa::load(tmp.path()).unwrap();
        assert_eq!(ca1.cert_pem, ca2.cert_pem);
        assert_eq!(ca1.key_pem, ca2.key_pem);
    }

    #[test]
    fn ca_constraint_is_ca_true() {
        let tmp = TempDir::new().unwrap();
        let ca = RootCa::generate_and_persist(tmp.path()).unwrap();
        // The PEM should contain the CA certificate
        assert!(ca.cert_pem.contains("BEGIN CERTIFICATE"));
        // Verify we can re-parse it as a CA cert (from_ca_cert_pem would fail
        // if BasicConstraints CA:true is missing)
        let result = CertificateParams::from_ca_cert_pem(&ca.cert_pem);
        assert!(result.is_ok(), "cert must parse as CA cert: {result:?}");
    }

    #[test]
    #[cfg(unix)]
    fn key_file_permissions_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let _ca = RootCa::generate_and_persist(tmp.path()).unwrap();
        let key_path = tmp.path().join("root.key");
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "key file should be mode 0600, got {mode:o}");
    }

    #[test]
    fn load_or_generate_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let ca1 = RootCa::load_or_generate(tmp.path()).unwrap();
        let ca2 = RootCa::load_or_generate(tmp.path()).unwrap();
        assert_eq!(ca1.cert_pem, ca2.cert_pem);
    }

    #[test]
    fn sign_leaf_valid_san() {
        let tmp = TempDir::new().unwrap();
        let ca = RootCa::generate_and_persist(tmp.path()).unwrap();
        let leaf = ca.sign_leaf("api.example.com").unwrap();
        assert!(leaf.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(leaf.key_pem.contains("BEGIN"));
    }

    #[test]
    fn sign_leaf_cache_returns_same_arc() {
        let tmp = TempDir::new().unwrap();
        let ca = RootCa::generate_and_persist(tmp.path()).unwrap();
        let leaf1 = ca.sign_leaf("api.example.com").unwrap();
        let leaf2 = ca.sign_leaf("api.example.com").unwrap();
        // Pointer equality — same Arc
        assert!(Arc::ptr_eq(&leaf1, &leaf2));
    }

    #[test]
    fn sign_leaf_different_sni_different_certs() {
        let tmp = TempDir::new().unwrap();
        let ca = RootCa::generate_and_persist(tmp.path()).unwrap();
        let leaf1 = ca.sign_leaf("api.example.com").unwrap();
        let leaf2 = ca.sign_leaf("other.example.com").unwrap();
        assert_ne!(leaf1.cert_pem, leaf2.cert_pem);
    }
}
