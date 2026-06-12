//! HTTP egress proxy request handler for per-sandbox credential injection.
//!
//! Contains the core MITM/passthrough logic extracted from `abox-proxyd`.
//! Used by both the standalone proxy daemon and the per-sandbox egress proxy
//! spawned by [`crate::sandbox::SandboxOrchestrator::run_sandbox()`].

use crate::ca::RootCa;
use crate::policy::{domain_matches, EgressTransport, PolicyEngine};
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
    _bypass_tls: &[String],
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

        match policy.evaluate_egress_request(&domain, port) {
            Ok(evaluation) => {
                if let Some(rule) = &evaluation.rule {
                    tracing::debug!(
                        domain = %domain,
                        inject_header = %rule.inject_header,
                        "Egress allowed with credential injection"
                    );
                }
                audit_fn(&domain, "allowed", 200);

                let rule_opt = evaluation.rule.cloned();
                let root_ca = Arc::clone(&root_ca);
                let transport = evaluation.transport;

                tokio::spawn(async move {
                    match hyper::upgrade::on(req).await {
                        Ok(upgraded) => match transport {
                            EgressTransport::Passthrough => {
                                handle_passthrough(upgraded, &domain, port).await;
                            }
                            EgressTransport::Mitm => {
                                if let Err(e) = handle_mitm(
                                    upgraded,
                                    &domain,
                                    port,
                                    &root_ca,
                                    rule_opt.as_ref(),
                                )
                                .await
                                {
                                    tracing::error!(
                                        domain = %domain,
                                        error = %e,
                                        "MITM proxy error"
                                    );
                                }
                            }
                        },
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

/// Handle MITM with header injection, using hyper for proper HTTP/1.1
/// framing on both client and upstream sides.
///
/// # Why hyper, not hand-rolled bytes
///
/// The previous implementation read request bytes from the client, searched
/// for `\r\n\r\n`, did string surgery to inject the Authorization header,
/// then handed off to `copy_bidirectional` for the rest. That approach had
/// three latent bugs we traced during the credential-forwarding investigation:
///
/// 1. **Keep-alive** — only the first request on each TLS tunnel got header
///    injection; subsequent requests pipelined over the same connection flowed
///    through `copy_bidirectional` unmodified.
/// 2. **Header ordering** — inserting Authorization at the end of the header
///    block rewrote the client's original ordering. Some endpoints are
///    order-sensitive.
/// 3. **Body framing** — request bodies are delivered via `Content-Length` or
///    `Transfer-Encoding: chunked`; hand-rolled forwarding can leak prefix
///    body bytes or corrupt framing when the head and body are read in the
///    same TLS record.
///
/// Hyper's HTTP/1.1 codec handles all three correctly: each request is
/// parsed into a `Request<Incoming>`, we mutate exactly one header, and the
/// codec writes it back to upstream with correct framing. Response streams
/// (SSE, chunked) are forwarded through hyper's body types, preserving
/// byte-perfect delivery.
async fn handle_mitm_with_injection(
    client_tls: tokio_rustls::server::TlsStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>,
    upstream_tls: tokio_rustls::client::TlsStream<TcpStream>,
    rule: Option<&crate::policy::EgressRule>,
) -> Result<()> {
    use hyper::header::{HeaderName, HeaderValue};

    // Set up hyper client connection against the upstream TLS stream. The
    // connection driver runs on a background task; the SendRequest handle
    // below sends requests over that connection and gets back responses.
    // Keep-alive works naturally: multiple client requests flow over the
    // same tunnel, each passes through the service_fn, each gets injection.
    let upstream_io = hyper_util::rt::TokioIo::new(upstream_tls);
    let (sender, upstream_conn) = hyper::client::conn::http1::handshake(upstream_io)
        .await
        .context("upstream http1 handshake failed")?;
    let upstream_driver = tokio::spawn(async move {
        if let Err(e) = upstream_conn.await {
            tracing::debug!(error = %e, "upstream connection driver ended");
        }
    });

    // Share the single SendRequest between all HTTP/1 requests that may flow
    // over the same TLS tunnel (keep-alive). A Mutex serializes access; for
    // HTTP/1.1 on one connection, requests are inherently sequential anyway.
    let sender = std::sync::Arc::new(tokio::sync::Mutex::new(sender));

    // Take owned copies of the rule and its parsed header name. Both are
    // captured by the service_fn closure below and cloned into each
    // per-request future. The credential itself is resolved *per request*
    // (not cached at tunnel setup) so that OAuth tokens rotated mid-tunnel
    // (common for file-backed credentials via `credential_file` + periodic
    // refresh on the host) take effect immediately on the next request.
    let rule_owned: Option<crate::policy::EgressRule> = rule.cloned();
    let inject_header_name: Option<HeaderName> =
        rule_owned.as_ref().and_then(|r| HeaderName::from_bytes(r.inject_header.as_bytes()).ok());

    let service = hyper::service::service_fn(move |mut req: Request<hyper::body::Incoming>| {
        let sender = std::sync::Arc::clone(&sender);
        let rule_owned = rule_owned.clone();
        let inject_header_name = inject_header_name.clone();
        async move {
            // Per-request rule evaluation: if the matched domain rule has
            // request_rules, evaluate them against the HTTP method and path.
            // A deny decision returns 403 immediately without forwarding.
            if let Some(ref rule) = rule_owned {
                if !rule.request_rules.is_empty() {
                    let method = req.method().as_str();
                    let path = req.uri().path();
                    if let Some(false) = rule.evaluate_request_rules(method, path) {
                        tracing::warn!(
                            domain = %rule.domain,
                            method = %method,
                            path = %path,
                            "Request denied by per-request egress rule"
                        );
                        let msg = format!(
                            "Request {method} {path} denied by egress policy for domain '{}'",
                            rule.domain
                        );
                        return Ok(Response::builder()
                            .status(StatusCode::FORBIDDEN)
                            .body(full_body(&msg))
                            .unwrap());
                    }
                    // Some(true) or None: allowed or no matching rule
                }
            }

            // Credential injection: if a rule matches and a credential
            // resolves, insert-or-replace the target header. This matches
            // the original proxy contract: the proxy's job is to *provide*
            // the credential on behalf of the guest, whether or not the
            // guest shipped a placeholder header. `HeaderMap::insert`
            // replaces any existing value with the same name.
            if let (Some(name), Some(r)) = (inject_header_name, rule_owned.as_ref()) {
                match r.resolve_credential() {
                    Some(value) => {
                        let new_value = r.header_template.replace("{value}", &value);
                        match HeaderValue::from_str(&new_value) {
                            Ok(hv) => {
                                let had_prior = req.headers().contains_key(&name);
                                req.headers_mut().insert(name.clone(), hv);
                                tracing::debug!(
                                    header = %name,
                                    had_prior,
                                    "Injected credential header"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    header = %name,
                                    error = %e,
                                    "Injected header value is not a valid HeaderValue; forwarding unmodified"
                                );
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            domain = %r.domain,
                            "Rule matched but credential could not be resolved (env var not set, file missing, or json path absent); forwarding unmodified"
                        );
                    }
                }
            }

            let mut s = sender.lock().await;
            let resp = s.send_request(req).await?;
            let (parts, body) = resp.into_parts();
            let boxed: BoxBody<Bytes, hyper::Error> = body.boxed();
            Ok::<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error>(Response::from_parts(
                parts, boxed,
            ))
        }
    });

    // Serve HTTP/1 requests from the client over the TLS tunnel we've already
    // accepted. `preserve_header_case` and `title_case_headers` keep the
    // original on-the-wire casing (important for some servers that hash
    // header names case-sensitively for TLS-independent fingerprinting).
    let client_io = hyper_util::rt::TokioIo::new(client_tls);
    let serve_result = hyper::server::conn::http1::Builder::new()
        .preserve_header_case(true)
        .title_case_headers(true)
        .serve_connection(client_io, service)
        .with_upgrades()
        .await;

    if let Err(e) = serve_result {
        tracing::debug!(error = %e, "MITM server connection ended");
    }

    // Upstream driver is owned by the spawn; abort to release resources when
    // the client side is done. If the driver already exited (normal case on
    // clean close), abort is a no-op.
    upstream_driver.abort();
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
    bypass_list.iter().any(|pattern| domain_matches(pattern, domain))
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
        assert!(!is_tls_bypassed("evilpinned.io", &bypass));
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
