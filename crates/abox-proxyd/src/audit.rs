//! Hash-chained audit logging for the proxy daemon.
//!
//! Every proxied request (CLI and HTTP) is logged to a structured JSON file
//! for post-hoc review. The audit log captures the sandbox ID, timestamp,
//! command/URL, policy decision, and exit code.
//!
//! # Tamper-Evidence
//!
//! Each entry carries a `hash` field that is the SHA-256 of:
//!   `prev_hash || entry_json_without_hash_field`
//!
//! The first entry uses a zero hash as the predecessor. This creates a
//! linked chain: any modification, insertion, or deletion of an entry
//! changes all subsequent hashes, making tampering detectable by
//! `abox audit verify`.
//!
//! # Format
//!
//! Each line in `audit.jsonl` is a JSON object with the fields:
//!   - `seq`: monotonically increasing sequence number (u64)
//!   - `timestamp`: ISO 8601 UTC timestamp
//!   - `sandbox_id`: the sandbox that made the request
//!   - `request_type`: `"cli"` or `"egress"`
//!   - `target`: the command or URL
//!   - `detail`: full args (CLI) or empty string (egress)
//!   - `decision`: `"allowed"` or `"denied"`
//!   - `result_code`: exit code (CLI) or HTTP status (egress); 0 if not yet completed
//!   - `prev_hash`: hex SHA-256 of the previous entry (or 64 zeros for the first)
//!   - `hash`: hex SHA-256 of `prev_hash || canonical_entry_json`

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The zero hash used as the predecessor of the first entry.
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A single audit log entry (without the chaining hash fields).
/// Used as the canonical input to the hash computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuditEntryCore {
    pub seq: u64,
    pub timestamp: String,
    pub sandbox_id: String,
    pub request_type: String,
    pub target: String,
    pub detail: String,
    pub decision: String,
    pub result_code: i32,
}

/// A complete audit log entry including hash-chain fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub timestamp: String,
    pub sandbox_id: String,
    pub request_type: String,
    pub target: String,
    pub detail: String,
    pub decision: String,
    pub result_code: i32,
    /// Hex SHA-256 of the previous entry's canonical JSON (or ZERO_HASH for seq=0).
    pub prev_hash: String,
    /// Hex SHA-256 of `prev_hash || canonical_entry_json`.
    pub hash: String,
}

/// Result of verifying an audit log file.
#[derive(Debug)]
pub struct VerifyResult {
    pub total_entries: usize,
    pub ok: bool,
    pub errors: Vec<String>,
}

impl VerifyResult {
    pub fn is_ok(&self) -> bool {
        self.ok
    }
}

/// Thread-safe audit logger that writes hash-chained JSON lines to a file.
pub struct AuditLog {
    inner: Mutex<AuditLogInner>,
    path: PathBuf,
}

struct AuditLogInner {
    writer: std::io::BufWriter<std::fs::File>,
    /// Sequence counter for the next entry.
    next_seq: u64,
    /// Hash of the last written entry (or ZERO_HASH if empty).
    last_hash: String,
}

impl AuditLog {
    /// Create a new audit log, appending to the given file.
    /// Reads existing entries to establish the current chain state.
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Read existing entries to find the current chain tip.
        let (next_seq, last_hash) = if path.exists() {
            Self::read_chain_tip(path)?
        } else {
            (0, ZERO_HASH.to_string())
        };

        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            inner: Mutex::new(AuditLogInner {
                writer: std::io::BufWriter::new(file),
                next_seq,
                last_hash,
            }),
            path: path.to_path_buf(),
        })
    }

    /// Read the last sequence number and hash from an existing log file.
    fn read_chain_tip(path: &Path) -> anyhow::Result<(u64, String)> {
        let content = std::fs::read_to_string(path)?;
        let mut last_seq = 0u64;
        let mut last_hash = ZERO_HASH.to_string();
        let mut found_any = false;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<AuditEntry>(line) {
                last_seq = entry.seq;
                last_hash = entry.hash.clone();
                found_any = true;
            }
        }

        let next_seq = if found_any { last_seq + 1 } else { 0 };
        Ok((next_seq, last_hash))
    }

    /// Compute the hash for a new entry given the previous hash and core fields.
    fn compute_hash(prev_hash: &str, core: &AuditEntryCore) -> anyhow::Result<String> {
        let canonical = serde_json::to_string(core)?;
        let mut hasher = Sha256::new();
        hasher.update(prev_hash.as_bytes());
        hasher.update(b"||");
        hasher.update(canonical.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Write an audit entry with hash chaining.
    fn log_entry(
        &self,
        request_type: &str,
        sandbox_id: &str,
        target: &str,
        detail: &str,
        decision: &str,
        result_code: i32,
    ) {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(error = %e, "Audit log mutex poisoned");
                return;
            }
        };

        let core = AuditEntryCore {
            seq: inner.next_seq,
            timestamp: Utc::now().to_rfc3339(),
            sandbox_id: sandbox_id.to_string(),
            request_type: request_type.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
            decision: decision.to_string(),
            result_code,
        };

        let hash = match Self::compute_hash(&inner.last_hash, &core) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "Failed to compute audit entry hash");
                return;
            }
        };

        let entry = AuditEntry {
            seq: core.seq,
            timestamp: core.timestamp,
            sandbox_id: core.sandbox_id,
            request_type: core.request_type,
            target: core.target,
            detail: core.detail,
            decision: core.decision,
            result_code: core.result_code,
            prev_hash: inner.last_hash.clone(),
            hash: hash.clone(),
        };

        let json = match serde_json::to_string(&entry) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(error = %e, "Failed to serialize audit entry");
                return;
            }
        };

        if let Err(e) = writeln!(inner.writer, "{json}") {
            tracing::error!(error = %e, "Failed to write audit entry");
            return;
        }
        if let Err(e) = inner.writer.flush() {
            tracing::error!(error = %e, "Failed to flush audit log");
            return;
        }

        inner.last_hash = hash;
        inner.next_seq += 1;
    }

    /// Log a CLI proxy request.
    pub fn log_cli(
        &self,
        sandbox_id: &str,
        command: &str,
        args: &[String],
        decision: &str,
        exit_code: i32,
    ) {
        self.log_entry("cli", sandbox_id, command, &args.join(" "), decision, exit_code);
    }

    /// Log an HTTP egress request.
    pub fn log_egress(&self, sandbox_id: &str, domain: &str, decision: &str, status_code: i32) {
        self.log_entry("egress", sandbox_id, domain, "", decision, status_code);
    }

    /// Return the path to the audit log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verify the integrity of an audit log file.
    ///
    /// Checks:
    /// 1. Each entry's `prev_hash` matches the previous entry's `hash`.
    /// 2. Each entry's `hash` matches the computed hash of `prev_hash || core_json`.
    /// 3. Sequence numbers are contiguous starting from 0.
    pub fn verify(path: &Path) -> anyhow::Result<VerifyResult> {
        let content = std::fs::read_to_string(path)?;
        let mut errors = Vec::new();
        let mut prev_hash = ZERO_HASH.to_string();
        let mut expected_seq = 0u64;
        let mut total = 0usize;

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let entry: AuditEntry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(e) => {
                    errors.push(format!("Line {}: JSON parse error: {e}", line_num + 1));
                    continue;
                }
            };

            // Check sequence continuity
            if entry.seq != expected_seq {
                errors.push(format!(
                    "Line {}: sequence gap — expected seq={expected_seq}, got seq={}",
                    line_num + 1,
                    entry.seq
                ));
            }

            // Check prev_hash linkage
            if entry.prev_hash != prev_hash {
                let prev_short = &prev_hash[..8.min(prev_hash.len())];
                let got_short = &entry.prev_hash[..8.min(entry.prev_hash.len())];
                errors.push(format!(
                    "Line {} (seq={}): prev_hash mismatch — expected {prev_short}…, got {got_short}…",
                    line_num + 1,
                    entry.seq,
                ));
            }

            // Recompute hash from core fields
            let core = AuditEntryCore {
                seq: entry.seq,
                timestamp: entry.timestamp.clone(),
                sandbox_id: entry.sandbox_id.clone(),
                request_type: entry.request_type.clone(),
                target: entry.target.clone(),
                detail: entry.detail.clone(),
                decision: entry.decision.clone(),
                result_code: entry.result_code,
            };
            let expected_hash = match Self::compute_hash(&entry.prev_hash, &core) {
                Ok(h) => h,
                Err(e) => {
                    errors.push(format!(
                        "Line {} (seq={}): hash computation error: {e}",
                        line_num + 1,
                        entry.seq
                    ));
                    continue;
                }
            };

            if entry.hash != expected_hash {
                let claimed_short = &entry.hash[..8.min(entry.hash.len())];
                let computed_short = &expected_hash[..8.min(expected_hash.len())];
                errors.push(format!(
                    "Line {} (seq={}): hash mismatch — entry claims {claimed_short}…, computed {computed_short}…",
                    line_num + 1,
                    entry.seq,
                ));
            }

            prev_hash = entry.hash.clone();
            expected_seq = entry.seq + 1;
            total += 1;
        }

        let ok = errors.is_empty();
        Ok(VerifyResult { total_entries: total, ok, errors })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_audit_log_creates_chained_entries() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::new(tmp.path()).unwrap();

        log.log_cli("sandbox1", "git", &["status".to_string()], "allowed", 0);
        log.log_egress("sandbox1", "api.anthropic.com", "allowed", 200);
        log.log_cli("sandbox1", "curl", &["https://example.com".to_string()], "denied", -1);

        let result = AuditLog::verify(tmp.path()).unwrap();
        assert!(result.is_ok(), "Errors: {:?}", result.errors);
        assert_eq!(result.total_entries, 3);
    }

    #[test]
    fn test_audit_log_detects_tampering() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::new(tmp.path()).unwrap();
        log.log_cli("sandbox1", "git", &["status".to_string()], "allowed", 0);
        log.log_egress("sandbox1", "api.openai.com", "allowed", 200);
        drop(log);

        // Tamper with the file: modify the first entry's decision
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let tampered = content.replacen("\"allowed\"", "\"denied\"", 1);
        std::fs::write(tmp.path(), tampered).unwrap();

        let result = AuditLog::verify(tmp.path()).unwrap();
        assert!(!result.is_ok(), "Should have detected tampering");
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_audit_log_first_entry_uses_zero_hash() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::new(tmp.path()).unwrap();
        log.log_cli("s", "echo", &[], "allowed", 0);
        drop(log);

        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let entry: AuditEntry = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry.seq, 0);
        assert_eq!(entry.prev_hash, ZERO_HASH);
    }

    #[test]
    fn test_audit_log_verify_empty_file() {
        let tmp = NamedTempFile::new().unwrap();
        let result = AuditLog::verify(tmp.path()).unwrap();
        assert!(result.is_ok());
        assert_eq!(result.total_entries, 0);
    }

    #[test]
    fn test_audit_log_resumes_chain_after_reopen() {
        let tmp = NamedTempFile::new().unwrap();

        // First session
        {
            let log = AuditLog::new(tmp.path()).unwrap();
            log.log_cli("s", "git", &["status".to_string()], "allowed", 0);
        }

        // Second session — should continue the chain
        {
            let log = AuditLog::new(tmp.path()).unwrap();
            log.log_egress("s", "api.github.com", "allowed", 200);
        }

        let result = AuditLog::verify(tmp.path()).unwrap();
        assert!(result.is_ok(), "Errors: {:?}", result.errors);
        assert_eq!(result.total_entries, 2);

        // Check seq numbers are 0 and 1
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let entries: Vec<AuditEntry> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[1].prev_hash, entries[0].hash);
    }
}
