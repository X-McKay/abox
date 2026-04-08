//! CLI proxy handler for `abox-proxyd`.
//!
//! Thin wrapper around the shared [`abox_core::proxy_bridge::ProxyBridge`]
//! library so the orchestrator (per-VM, fixed attribution) and the
//! standalone daemon (shared, request-based attribution) use the same code
//! path. This file's only job is to plug the daemon's [`AuditLog`] into
//! the bridge as an [`AuditSink`].

use crate::audit::AuditLog;
use abox_core::policy::PolicyEngine;
use abox_core::proxy_bridge::{AuditSink, ProxyBridge, SandboxAttribution};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

impl AuditSink for AuditLog {
    fn log_cli(
        &self,
        sandbox_id: &str,
        command: &str,
        args: &[String],
        decision: &str,
        exit_code: i32,
    ) {
        AuditLog::log_cli(self, sandbox_id, command, args, decision, exit_code);
    }
}

/// The CLI proxy server. Owns the socket path and forwards requests
/// through a [`ProxyBridge`] configured with `SandboxAttribution::FromRequest`.
pub struct CliProxyServer {
    bridge: ProxyBridge,
}

impl CliProxyServer {
    pub fn new(socket_path: PathBuf, policy: Arc<PolicyEngine>, audit: Arc<AuditLog>) -> Self {
        let audit_sink: Arc<dyn AuditSink> = audit;
        Self {
            bridge: ProxyBridge::new(
                socket_path,
                policy,
                audit_sink,
                SandboxAttribution::FromRequest,
            ),
        }
    }

    pub async fn run(self) -> Result<()> {
        self.bridge.run().await
    }
}
