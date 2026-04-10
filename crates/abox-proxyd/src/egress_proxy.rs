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

use crate::audit::AuditLog;
use abox_core::ca::RootCa;
use abox_core::policy::PolicyEngine;
use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, ServerName};
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

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
    pub fn new(port: u16, policy: Arc<PolicyEngine>, audit: Arc<AuditLog>, root_ca: Arc<RootCa>) -> Self {
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
            let (stream, peer_addr) = listener.accept().await?;
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

                let service = service_fn(move |req| {
                    let policy = policy.clone();
                    let audit = audit.clone();
                    let root_ca = Arc::clone(&root_ca);
                    let bypass_tls = bypass_tls.clone();
                    let sandbox_id = sandbox_id.clone();
                    async move {
                        handle_request(req, &policy, &audit, root_ca, &bypass_tls, &sandbox_id, peer_addr).await
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

#[allow(clippy::unused_async)] // hyper's service requires async fn signature
async fn handle_request(
    req: Request<hyper::body::Incoming>,
    policy: &PolicyEngine,
    audit: &AuditLog,
    root_ca: Arc<RootCa>,
    bypass_tls: &[String],
    sandbox_id: &str,
    _peer_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    if req.method() == Method::CONNECT {
        // HTTPS CONNECT tunnel
        let host = req.uri().authority().map(|a| a.host().to_string());
        let port =
            req.uri().authority().and_then(hyper::http::uri::Authority::port_u16).unwrap_or(443);

        let Some(domain) = host else {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(full_body("Missing host in CONNECT request"))
                .unwrap());
        };

        // Evaluate egress policy
        match policy.evaluate_egress(&domain) {
            Ok(rule_opt) => {
                if let Some(rule) = &rule_opt {
                    tracing::debug!(
                        domain = %domain,
                        inject_header = %rule.inject_header,
                        "Egress allowed with credential injection"
                    );
                }
                audit.log_egress(sandbox_id, &domain, "allowed", 200);

                let should_bypass = is_tls_bypassed(&domain, bypass_tls);
                let rule_opt = rule_opt.cloned();
                let domain = domain.clone();
                let root_ca = Arc::clone(&root_ca);

                // Establish the tunnel
                tokio::spawn(async move {
                    match hyper::upgrade::on(req).await {
                        Ok(upgraded) => {
                            if should_bypass {
                                // Passthrough mode — no MITM
                                handle_passthrough(upgraded, &domain, port).await;
                            } else {
                                // MITM mode — terminate TLS, inspect/modify, re-encrypt
                                if let Err(e) = handle_mitm(upgraded, &domain, port, &root_ca, rule_opt.as_ref()).await {
                                    tracing::error!(
                                        domain = %domain,
                                        error = %e,
                                        "MITM proxy error"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Upgrade failed");
                        }
                    }
                });

                Ok(Response::new(empty_body()))
            }
            Err(_decision) => {
                // Denied
                audit.log_egress(sandbox_id, &domain, "denied", 403);
                tracing::warn!(domain = %domain, "Egress denied");
                Ok(Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(full_body("Blocked by egress policy"))
                    .unwrap())
            }
        }
    } else {
        // Plain HTTP request (not CONNECT) — typically not used by agents
        Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(full_body("Only CONNECT method is supported"))
            .unwrap())
    }
}

/// Passthrough mode: plain TCP tunnel without TLS termination.
async fn handle_passthrough(
    upgraded: hyper::upgrade::Upgraded,
    domain: &str,
    port: u16,
) {
    let addr = format!("{domain}:{port}");
    match TcpStream::connect(&addr).await {
        Ok(mut target_stream) => {
            let mut upgraded = TokioIo::new(upgraded);
            let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut target_stream).await;
        }
        Err(e) => {
            tracing::error!(addr = %addr, error = %e, "Failed to connect to target (passthrough)");
        }
    }
}

/// MITM mode: terminate TLS from client, read plaintext HTTP, inject headers,
/// open new TLS connection to upstream, forward request and relay response.
async fn handle_mitm(
    upgraded: hyper::upgrade::Upgraded,
    domain: &str,
    port: u16,
    root_ca: &RootCa,
    rule: Option<&abox_core::policy::EgressRule>,
) -> Result<()> {
    // Step 1: Generate leaf cert for this domain
    let leaf = root_ca
        .sign_leaf(domain)
        .with_context(|| format!("signing leaf cert for {domain}"))?;

    // Step 2: Build server TLS config with leaf cert
    let server_config = build_server_config(&leaf.cert_pem, &leaf.key_pem, &root_ca.cert_pem)?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    // Step 3: Accept TLS from the client (VM)
    let client_stream = TokioIo::new(upgraded);
    let client_tls = acceptor
        .accept(client_stream)
        .await
        .context("TLS accept from client failed")?;

    // Step 4: Connect to real upstream over TLS
    let upstream_addr = format!("{domain}:{port}");
    let upstream_tcp = TcpStream::connect(&upstream_addr)
        .await
        .with_context(|| format!("connecting to upstream {upstream_addr}"))?;

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_config));
    let server_name: ServerName<'static> = domain.to_string().try_into().context("invalid server name")?;
    let upstream_tls = connector
        .connect(server_name, upstream_tcp)
        .await
        .context("TLS connect to upstream failed")?;

    // Step 5: Bidirectional relay.
    //
    // For the initial implementation we do a simple byte-level relay between
    // the decrypted client stream and the upstream TLS stream. This means
    // header injection happens only when we have an egress rule AND we parse
    // the first HTTP request. For simplicity in v1, we do a raw bidirectional
    // copy (no per-request header injection yet — that's Task 5).
    //
    // Task 5 will add request-level parsing with header injection before
    // forwarding. For now, the MITM plumbing is in place and working.
    if rule.is_some() {
        // When we have a rule, we need to parse the HTTP request to inject
        // headers. We'll read the first request, modify it, send it upstream,
        // then relay the rest bidirectionally.
        if let Err(e) = handle_mitm_with_injection(client_tls, upstream_tls, rule).await {
            tracing::debug!(error = %e, "MITM with injection ended");
        }
    } else {
        // No injection needed — just relay bytes
        let mut client = client_tls;
        let mut upstream = upstream_tls;
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    }

    Ok(())
}

/// Handle MITM with header injection: read the raw HTTP request from the
/// client, inject credential headers, forward to upstream, then relay the
/// response and any subsequent data bidirectionally.
///
/// This uses a simple line-based HTTP/1.1 parser rather than a full HTTP
/// stack, because we only need to inject headers into the first request
/// before switching to bidirectional copy for the response + body.
async fn handle_mitm_with_injection(
    mut client_tls: tokio_rustls::server::TlsStream<TokioIo<hyper::upgrade::Upgraded>>,
    mut upstream_tls: tokio_rustls::client::TlsStream<TcpStream>,
    rule: Option<&abox_core::policy::EgressRule>,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Read the HTTP request head from the client.
    // We read until we see \r\n\r\n (end of headers).
    let mut head_buf = Vec::with_capacity(8192);
    let mut temp = [0u8; 1];
    let mut found_end = false;

    while head_buf.len() < 65536 {
        let n = client_tls.read(&mut temp).await?;
        if n == 0 {
            break;
        }
        head_buf.push(temp[0]);

        // Check for \r\n\r\n
        if head_buf.len() >= 4
            && head_buf[head_buf.len() - 4..] == [b'\r', b'\n', b'\r', b'\n']
        {
            found_end = true;
            break;
        }
    }

    if !found_end {
        // Couldn't parse headers — just forward what we have and relay
        upstream_tls.write_all(&head_buf).await?;
        let _ = tokio::io::copy_bidirectional(&mut client_tls, &mut upstream_tls).await;
        return Ok(());
    }

    // Parse the header block and inject credentials
    let head_str = String::from_utf8_lossy(&head_buf);
    let mut lines: Vec<String> = head_str.lines().map(String::from).collect();

    if let Some(rule) = rule {
        match std::env::var(&rule.env_var) {
            Ok(value) => {
                let header_value = rule.header_template.replace("{value}", &value);
                // Insert the header before the empty line (last element after split)
                // Find insertion point: just before the trailing empty line
                let inject_line = format!("{}: {}", rule.inject_header, header_value);

                // Remove any existing header with the same name (case-insensitive)
                let header_lower = rule.inject_header.to_lowercase();
                lines.retain(|l| {
                    if let Some(colon_pos) = l.find(':') {
                        l[..colon_pos].trim().to_lowercase() != header_lower
                    } else {
                        true
                    }
                });

                // Insert before the last empty line
                if let Some(pos) = lines.iter().rposition(|l| !l.is_empty()) {
                    lines.insert(pos + 1, inject_line);
                } else {
                    lines.push(inject_line);
                }

                tracing::debug!(
                    header = %rule.inject_header,
                    env_var = %rule.env_var,
                    "Injected credential header"
                );
            }
            Err(_) => {
                tracing::warn!(
                    env_var = %rule.env_var,
                    "Credential env var not set, skipping injection"
                );
            }
        }
    }

    // Reconstruct the head and send to upstream
    let mut reconstructed = lines.join("\r\n");
    reconstructed.push_str("\r\n"); // final \r\n after headers
    upstream_tls.write_all(reconstructed.as_bytes()).await?;

    // Now relay the rest bidirectionally (body + response)
    let _ = tokio::io::copy_bidirectional(&mut client_tls, &mut upstream_tls).await;
    Ok(())
}

/// Build a rustls `ServerConfig` from PEM-encoded leaf cert + key, chained with the CA cert.
fn build_server_config(
    leaf_cert_pem: &str,
    leaf_key_pem: &str,
    ca_cert_pem: &str,
) -> Result<rustls::ServerConfig> {
    // Parse leaf cert
    let leaf_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(
        &mut BufReader::new(leaf_cert_pem.as_bytes()),
    )
    .collect::<std::result::Result<Vec<_>, _>>()
    .context("parsing leaf cert PEM")?;

    // Parse CA cert (for the chain)
    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(
        &mut BufReader::new(ca_cert_pem.as_bytes()),
    )
    .collect::<std::result::Result<Vec<_>, _>>()
    .context("parsing CA cert PEM")?;

    // Parse leaf private key
    let key = rustls_pemfile::private_key(&mut BufReader::new(leaf_key_pem.as_bytes()))
        .context("parsing leaf key PEM")?
        .context("no private key found in PEM")?;

    // Chain: [leaf, ca]
    let mut cert_chain = leaf_certs;
    cert_chain.extend(ca_certs);

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .context("building server TLS config")?;

    Ok(config)
}

/// Check if a domain should bypass TLS termination (passthrough).
fn is_tls_bypassed(domain: &str, bypass_list: &[String]) -> bool {
    for pattern in bypass_list {
        if pattern == domain {
            return true;
        }
        if let Some(suffix) = pattern.strip_prefix("*.") {
            if domain.ends_with(suffix) && domain.len() > suffix.len() {
                return true;
            }
        }
    }
    false
}

fn empty_body() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

fn full_body(msg: &str) -> BoxBody<Bytes, hyper::Error> {
    Full::new(Bytes::from(msg.to_string())).map_err(|never| match never {}).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

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
