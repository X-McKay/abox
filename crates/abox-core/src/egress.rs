//! HTTP egress proxy request handler for per-sandbox credential injection.
//!
//! Contains the core MITM/passthrough logic extracted from `abox-proxyd`.
//! Used by both the standalone proxy daemon and the per-sandbox egress proxy
//! spawned by [`crate::sandbox::SandboxOrchestrator::run_sandbox()`].

use crate::ca::RootCa;
use crate::policy::PolicyEngine;
use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{Method, Request, Response, StatusCode};
use rustls::pki_types::{CertificateDer, ServerName};
use std::io::BufReader;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Handle a single HTTP request from the egress proxy.
///
/// Evaluates the egress policy, then either denies the request, sets up a
/// passthrough tunnel, or performs MITM with credential injection.
///
/// The `audit_fn` callback is invoked with `(domain, decision, status_code)`
/// so callers can plug in their own audit mechanism.
#[allow(clippy::unused_async)]
pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    policy: &PolicyEngine,
    root_ca: Arc<RootCa>,
    bypass_tls: &[String],
    audit_fn: impl Fn(&str, &str, i32) + Send + 'static,
) -> std::result::Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    if req.method() == Method::CONNECT {
        let host = req.uri().authority().map(|a| a.host().to_string());
        let port =
            req.uri().authority().and_then(hyper::http::uri::Authority::port_u16).unwrap_or(443);

        let Some(domain) = host else {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(full_body("Missing host in CONNECT request"))
                .unwrap());
        };

        match policy.evaluate_egress(&domain) {
            Ok(rule_opt) => {
                if let Some(rule) = &rule_opt {
                    tracing::debug!(
                        domain = %domain,
                        inject_header = %rule.inject_header,
                        "Egress allowed with credential injection"
                    );
                }
                audit_fn(&domain, "allowed", 200);

                let should_bypass = is_tls_bypassed(&domain, bypass_tls);
                let rule_opt = rule_opt.cloned();
                let root_ca = Arc::clone(&root_ca);

                tokio::spawn(async move {
                    match hyper::upgrade::on(req).await {
                        Ok(upgraded) => {
                            if should_bypass {
                                handle_passthrough(upgraded, &domain, port).await;
                            } else if let Err(e) =
                                handle_mitm(upgraded, &domain, port, &root_ca, rule_opt.as_ref())
                                    .await
                            {
                                tracing::error!(
                                    domain = %domain,
                                    error = %e,
                                    "MITM proxy error"
                                );
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
                audit_fn(&domain, "denied", 403);
                tracing::warn!(domain = %domain, "Egress denied");
                Ok(Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(full_body("Blocked by egress policy"))
                    .unwrap())
            }
        }
    } else {
        Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(full_body("Only CONNECT method is supported"))
            .unwrap())
    }
}

/// Passthrough mode: plain TCP tunnel without TLS termination.
async fn handle_passthrough(upgraded: hyper::upgrade::Upgraded, domain: &str, port: u16) {
    let addr = format!("{domain}:{port}");
    match TcpStream::connect(&addr).await {
        Ok(mut target_stream) => {
            let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
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
    rule: Option<&crate::policy::EgressRule>,
) -> Result<()> {
    let leaf =
        root_ca.sign_leaf(domain).with_context(|| format!("signing leaf cert for {domain}"))?;

    let server_config = build_server_config(&leaf.cert_pem, &leaf.key_pem, &root_ca.cert_pem)?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let client_stream = hyper_util::rt::TokioIo::new(upgraded);
    let client_tls =
        acceptor.accept(client_stream).await.context("TLS accept from client failed")?;

    let upstream_addr = format!("{domain}:{port}");
    let upstream_tcp = TcpStream::connect(&upstream_addr)
        .await
        .with_context(|| format!("connecting to upstream {upstream_addr}"))?;

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let client_config =
        rustls::ClientConfig::builder().with_root_certificates(root_store).with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_config));
    let server_name: ServerName<'static> =
        domain.to_string().try_into().context("invalid server name")?;
    let upstream_tls = connector
        .connect(server_name, upstream_tcp)
        .await
        .context("TLS connect to upstream failed")?;

    if rule.is_some() {
        if let Err(e) = handle_mitm_with_injection(client_tls, upstream_tls, rule).await {
            tracing::debug!(error = %e, "MITM with injection ended");
        }
    } else {
        let mut client = client_tls;
        let mut upstream = upstream_tls;
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    }

    Ok(())
}

/// Handle MITM with header injection.
async fn handle_mitm_with_injection(
    mut client_tls: tokio_rustls::server::TlsStream<
        hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>,
    >,
    mut upstream_tls: tokio_rustls::client::TlsStream<TcpStream>,
    rule: Option<&crate::policy::EgressRule>,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut head_buf = Vec::with_capacity(8192);
    let mut temp = [0u8; 1];
    let mut found_end = false;

    while head_buf.len() < 65536 {
        let n = client_tls.read(&mut temp).await?;
        if n == 0 {
            break;
        }
        head_buf.push(temp[0]);

        if head_buf.len() >= 4 && head_buf[head_buf.len() - 4..] == [b'\r', b'\n', b'\r', b'\n'] {
            found_end = true;
            break;
        }
    }

    if !found_end {
        upstream_tls.write_all(&head_buf).await?;
        let _ = tokio::io::copy_bidirectional(&mut client_tls, &mut upstream_tls).await;
        return Ok(());
    }

    let head_str = String::from_utf8_lossy(&head_buf);
    let mut lines: Vec<String> = head_str.lines().map(String::from).collect();

    if let Some(rule) = rule {
        match rule.resolve_credential() {
            Some(value) => {
                let header_value = rule.header_template.replace("{value}", &value);
                let inject_line = format!("{}: {}", rule.inject_header, header_value);

                let header_lower = rule.inject_header.to_lowercase();
                lines.retain(|l| {
                    if let Some(colon_pos) = l.find(':') {
                        l[..colon_pos].trim().to_lowercase() != header_lower
                    } else {
                        true
                    }
                });

                if let Some(pos) = lines.iter().rposition(|l| !l.is_empty()) {
                    lines.insert(pos + 1, inject_line);
                } else {
                    lines.push(inject_line);
                }

                tracing::debug!(
                    header = %rule.inject_header,
                    "Injected credential header"
                );
            }
            None => {
                tracing::warn!(
                    domain = %rule.domain,
                    "No credential value available (env var not set or credential file not found)"
                );
            }
        }
    }

    let mut reconstructed = lines.join("\r\n");
    reconstructed.push_str("\r\n");
    upstream_tls.write_all(reconstructed.as_bytes()).await?;

    let _ = tokio::io::copy_bidirectional(&mut client_tls, &mut upstream_tls).await;
    Ok(())
}

/// Build a rustls `ServerConfig` from PEM-encoded leaf cert + key, chained with the CA cert.
pub fn build_server_config(
    leaf_cert_pem: &str,
    leaf_key_pem: &str,
    ca_cert_pem: &str,
) -> Result<rustls::ServerConfig> {
    let leaf_certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(leaf_cert_pem.as_bytes()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("parsing leaf cert PEM")?;

    let ca_certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(ca_cert_pem.as_bytes()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("parsing CA cert PEM")?;

    let key = rustls_pemfile::private_key(&mut BufReader::new(leaf_key_pem.as_bytes()))
        .context("parsing leaf key PEM")?
        .context("no private key found in PEM")?;

    let mut cert_chain = leaf_certs;
    cert_chain.extend(ca_certs);

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .context("building server TLS config")?;

    Ok(config)
}

/// Check if a domain should bypass TLS termination (passthrough).
pub fn is_tls_bypassed(domain: &str, bypass_list: &[String]) -> bool {
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

/// Create an empty HTTP response body.
pub fn empty_body() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

/// Create an HTTP response body from a string.
pub fn full_body(msg: &str) -> BoxBody<Bytes, hyper::Error> {
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
