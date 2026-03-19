//! Audit logging for the proxy daemon.
//!
//! Every proxied request (CLI and HTTP) is logged to a structured JSON file
//! for post-hoc review. The audit log captures the sandbox ID, timestamp,
//! command/URL, policy decision, and exit code.

use chrono::Utc;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A single audit log entry.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Sandbox ID that made the request.
    pub sandbox_id: String,
    /// Type of request: "cli" or "egress".
    pub request_type: String,
    /// The command or URL.
    pub target: String,
    /// Full arguments (for CLI) or headers (for egress).
    pub detail: String,
    /// Policy decision: "allowed" or "denied".
    pub decision: String,
    /// Exit code (for CLI) or HTTP status (for egress). 0 if not yet completed.
    pub result_code: i32,
}

/// Thread-safe audit logger that writes JSON lines to a file.
pub struct AuditLog {
    writer: Mutex<std::io::BufWriter<std::fs::File>>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl AuditLog {
    /// Create a new audit log, appending to the given file.
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;

        Ok(Self { writer: Mutex::new(std::io::BufWriter::new(file)), path: path.to_path_buf() })
    }

    /// Write an audit entry.
    pub fn log(&self, entry: &AuditEntry) -> anyhow::Result<()> {
        let json = serde_json::to_string(entry)?;
        let mut writer = self.writer.lock().map_err(|e| anyhow::anyhow!("{}", e))?;
        writeln!(writer, "{}", json)?;
        writer.flush()?;
        Ok(())
    }

    /// Convenience: log a CLI proxy request.
    pub fn log_cli(
        &self,
        sandbox_id: &str,
        command: &str,
        args: &[String],
        decision: &str,
        exit_code: i32,
    ) {
        let entry = AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            sandbox_id: sandbox_id.to_string(),
            request_type: "cli".to_string(),
            target: command.to_string(),
            detail: args.join(" "),
            decision: decision.to_string(),
            result_code: exit_code,
        };
        if let Err(e) = self.log(&entry) {
            tracing::error!(error = %e, "Failed to write audit log");
        }
    }

    /// Convenience: log an HTTP egress request.
    pub fn log_egress(&self, sandbox_id: &str, domain: &str, decision: &str, status_code: i32) {
        let entry = AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            sandbox_id: sandbox_id.to_string(),
            request_type: "egress".to_string(),
            target: domain.to_string(),
            detail: String::new(),
            decision: decision.to_string(),
            result_code: status_code,
        };
        if let Err(e) = self.log(&entry) {
            tracing::error!(error = %e, "Failed to write audit log");
        }
    }

    /// Return the path to the audit log file.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
