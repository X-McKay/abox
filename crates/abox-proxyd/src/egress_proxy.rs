//! HTTP egress proxy with TLS-terminating MITM for credential injection.
//!
//! An HTTP CONNECT proxy that intercepts outbound HTTPS requests from the VM.
//! When a request matches an egress policy rule, the proxy terminates TLS using
//! a dynamically-generated leaf certificate (signed by the abox root CA),
//! inspects/modifies the plaintext HTTP request (injecting credential headers),
//! then re-encrypts and forwards to the real upstream over a fresh TLS connection.
//!
//! Domains listed in the bypass list are handled with plain TCP passthrough
//! (no MITM), preserving end-to-end TLS for cert-pinned clients.
//!
//! The core request handling logic lives in [`abox_core::egress`]; this module
//! wraps it with the daemon's audit logging and server lifecycle.

use crate::audit::AuditLog;
use abox_core::ca::RootCa;
use abox_core::policy::PolicyEngine;
use anyhow::Result;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request;
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// The egress proxy server.
pub struct EgressProxyServer {
    listen_addr: SocketAddr,
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLog>,
    root_ca: Arc<RootCa>,
    bypass_tls: Vec<String>,
    /// Optional sandbox ID for audit attribution.
    sandbox_id: Option<String>,
}

impl EgressProxyServer {
    pub fn new(
        port: u16,
        policy: Arc<PolicyEngine>,
        audit: Arc<AuditLog>,
        root_ca: Arc<RootCa>,
    ) -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], port)),
            policy,
            audit,
            root_ca,
            bypass_tls: Vec::new(),
            sandbox_id: None,
        }
    }

    /// Set the list of domains that should bypass TLS termination (passthrough).
    pub fn with_bypass_tls(mut self, domains: Vec<String>) -> Self {
        self.bypass_tls = domains;
        self
    }

    /// Set the sandbox ID for audit attribution.
    #[allow(dead_code)] // used when spawning per-sandbox proxy instances
    pub fn with_sandbox_id(mut self, id: String) -> Self {
        self.sandbox_id = Some(id);
        self
    }

    /// Start the egress proxy. Runs forever.
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        tracing::info!(
            addr = %self.listen_addr,
            "Egress proxy listening"
        );

        loop {
            let (stream, _peer_addr) = listener.accept().await?;
            let policy = Arc::clone(&self.policy);
            let audit = Arc::clone(&self.audit);
            let root_ca = Arc::clone(&self.root_ca);
            let bypass_tls = self.bypass_tls.clone();
            let sandbox_id = self.sandbox_id.clone().unwrap_or_else(|| "unknown".to_string());

            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let policy = policy.clone();
                let audit = audit.clone();
                let root_ca = root_ca.clone();
                let bypass_tls = bypass_tls.clone();
                let sandbox_id = sandbox_id.clone();

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let policy = policy.clone();
                    let audit = audit.clone();
                    let root_ca = Arc::clone(&root_ca);
                    let bypass_tls = bypass_tls.clone();
                    let sandbox_id = sandbox_id.clone();
                    async move {
                        abox_core::request_broker::handle_request(
                            req,
                            &policy,
                            root_ca,
                            &bypass_tls,
                            move |domain: &str, decision: &str, status_code: i32| {
                                audit.log_egress(&sandbox_id, domain, decision, status_code);
                            },
                        )
                        .await
                    }
                });

                if let Err(e) = http1::Builder::new()
                    .preserve_header_case(true)
                    .title_case_headers(true)
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await
                {
                    tracing::debug!(error = %e, "Egress proxy connection error");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use abox_core::ca::RootCa;
    use abox_core::request_broker::{build_server_config, is_tls_bypassed};

    #[test]
    fn test_is_tls_bypassed_exact() {
        let bypass = vec!["pinned.example.com".to_string()];
        assert!(is_tls_bypassed("pinned.example.com", &bypass));
        assert!(!is_tls_bypassed("other.example.com", &bypass));
    }

    #[test]
    fn test_is_tls_bypassed_wildcard() {
        let bypass = vec!["*.pinned.io".to_string()];
        assert!(is_tls_bypassed("api.pinned.io", &bypass));
        assert!(!is_tls_bypassed("pinned.io", &bypass));
    }

    #[test]
    fn test_is_tls_bypassed_empty_list() {
        let bypass: Vec<String> = vec![];
        assert!(!is_tls_bypassed("anything.com", &bypass));
    }

    #[test]
    fn test_build_server_config_from_generated_ca() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ca = RootCa::generate_and_persist(tmp.path()).unwrap();
        let leaf = ca.sign_leaf("test.example.com").unwrap();
        let config = build_server_config(&leaf.cert_pem, &leaf.key_pem, &ca.cert_pem);
        assert!(config.is_ok(), "should build valid server config: {config:?}");
    }
}
