//! HTTP egress proxy.
//!
//! An HTTP CONNECT proxy that intercepts outbound HTTPS requests from the VM.
//! When a request matches an egress policy rule, the proxy injects the
//! appropriate credential header (e.g., API key) before forwarding the request.
//!
//! The VM is configured with `HTTPS_PROXY=http://<host-ip>:<egress_port>` so
//! all HTTPS traffic from the agent flows through this proxy.
//!
//! For credential injection, the proxy uses a simple approach:
//! - For CONNECT requests, it checks the target domain against egress rules.
//! - If a rule matches, the proxy establishes the tunnel and injects headers
//!   by acting as a TLS-terminating proxy (using a generated CA certificate).
//! - If no rule matches and the default action is "deny", the connection is refused.
//!
//! Note: For the initial implementation, we use a simpler "passthrough with
//! environment variable injection" approach where the agent's SDK reads
//! credentials from environment variables that we set in the VM. The full
//! MITM proxy is a Phase 2 enhancement.

use crate::audit::AuditLog;
use abox_core::policy::PolicyEngine;
use anyhow::Result;
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

/// The egress proxy server.
pub struct EgressProxyServer {
    listen_addr: SocketAddr,
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLog>,
}

impl EgressProxyServer {
    pub fn new(port: u16, policy: Arc<PolicyEngine>, audit: Arc<AuditLog>) -> Self {
        Self { listen_addr: SocketAddr::from(([0, 0, 0, 0], port)), policy, audit }
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

            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let policy = policy.clone();
                let audit = audit.clone();

                let service = service_fn(move |req| {
                    let policy = policy.clone();
                    let audit = audit.clone();
                    async move { handle_request(req, &policy, &audit, peer_addr).await }
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

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    policy: &PolicyEngine,
    audit: &AuditLog,
    _peer_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    if req.method() == Method::CONNECT {
        // HTTPS CONNECT tunnel
        let host = req.uri().authority().map(|a| a.host().to_string());
        let port = req.uri().authority().and_then(|a| a.port_u16()).unwrap_or(443);

        let domain = match host {
            Some(h) => h,
            None => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(full_body("Missing host in CONNECT request"))
                    .unwrap());
            }
        };

        // Evaluate egress policy
        match policy.evaluate_egress(&domain) {
            Ok(rule_opt) => {
                // Allowed — log and establish tunnel
                if let Some(rule) = rule_opt {
                    tracing::debug!(
                        domain = %domain,
                        inject_header = %rule.inject_header,
                        "Egress allowed with credential injection"
                    );
                }
                audit.log_egress("unknown", &domain, "allowed", 200);

                // Establish the TCP tunnel
                let addr = format!("{}:{}", domain, port);
                tokio::spawn(async move {
                    match hyper::upgrade::on(req).await {
                        Ok(upgraded) => match TcpStream::connect(&addr).await {
                            Ok(mut target_stream) => {
                                let mut upgraded = TokioIo::new(upgraded);
                                let _ = tokio::io::copy_bidirectional(
                                    &mut upgraded,
                                    &mut target_stream,
                                )
                                .await;
                            }
                            Err(e) => {
                                tracing::error!(
                                    addr = %addr,
                                    error = %e,
                                    "Failed to connect to target"
                                );
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
                // Denied
                audit.log_egress("unknown", &domain, "denied", 403);
                tracing::warn!(domain = %domain, "Egress denied");
                Ok(Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(full_body("Blocked by egress policy"))
                    .unwrap())
            }
        }
    } else {
        // Plain HTTP request (not CONNECT) — typically not used by agents
        // but we handle it for completeness
        Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(full_body("Only CONNECT method is supported"))
            .unwrap())
    }
}

fn empty_body() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

fn full_body(msg: &str) -> BoxBody<Bytes, hyper::Error> {
    Full::new(Bytes::from(msg.to_string())).map_err(|never| match never {}).boxed()
}
