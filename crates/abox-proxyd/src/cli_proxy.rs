//! CLI proxy handler for `abox-proxyd`.
//!
//! Thin wrapper around the shared [`abox_core::command_broker::CommandBroker`]
//! library so the orchestrator (per-VM, fixed attribution) and the
//! standalone daemon (shared, request-based attribution) use the same code
//! path. This file's only job is to plug the daemon's [`AuditLog`] into
//! the bridge as an [`AuditSink`].

use crate::audit::AuditLog;
use abox_core::command_broker::{AuditSink, CommandBroker, SandboxAttribution};
use abox_core::policy::PolicyEngine;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

// `AuditLog` (= `abox_core::audit::AuditChainWriter`) implements `AuditSink` in
// `abox-core`, so it plugs into the bridge directly — no local impl needed.

/// The CLI proxy server. Owns the socket path and forwards requests
/// through a [`CommandBroker`] configured with `SandboxAttribution::FromRequest`.
pub struct CliProxyServer {
    bridge: CommandBroker,
}

impl CliProxyServer {
    pub fn new(socket_path: PathBuf, policy: Arc<PolicyEngine>, audit: Arc<AuditLog>) -> Self {
        let audit_sink: Arc<dyn AuditSink> = audit;
        Self {
            bridge: CommandBroker::new(
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
