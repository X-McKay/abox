//! Hash-chained audit logging for the proxy daemon.
//!
//! Every proxied request (CLI and HTTP) is logged to a structured JSON file
//! for post-hoc review. The audit log captures the sandbox ID, timestamp,
//! command/URL, policy decision, and result code.
//!
//! The entry format, canonicalization, keyed hashing, and verification all
//! live in [`abox_core::audit`] so this writer, `abox audit verify`, and
//! `abox doctor` share one implementation and cannot drift. See that module
//! for the precise tamper-evidence guarantees.

use abox_core::audit::{self, compute_hash, AuditEntry, AuditEntryCore, ChainTip, ZERO_HASH};
use chrono::Utc;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Thread-safe audit logger that writes hash-chained JSON lines to a file.
pub struct AuditLog {
    inner: Mutex<AuditLogInner>,
    #[allow(dead_code)]
    path: PathBuf,
}

struct AuditLogInner {
    writer: std::io::BufWriter<std::fs::File>,
    /// Directory holding the log, key, and tip files.
    logs_dir: PathBuf,
    /// Host-only HMAC key for the chain.
    key: Vec<u8>,
    /// Sequence counter for the next entry.
    next_seq: u64,
    /// Hash of the last written entry (or ZERO_HASH if empty).
    last_hash: String,
}

impl AuditLog {
    /// Create a new audit log, appending to the given file.
    ///
    /// Loads (or creates) the host-only HMAC key, acquires an exclusive
    /// advisory lock on the file so only one writer chains the log at a time,
    /// and reads existing entries to recover the current chain tip.
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        let logs_dir = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        std::fs::create_dir_all(&logs_dir)?;

        let key = audit::load_or_create_key(&logs_dir)?;

        // Read existing entries to find the current chain tip.
        let (next_seq, last_hash) =
            if path.exists() { Self::read_chain_tip(path)? } else { (0, ZERO_HASH.to_string()) };

        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;

        // Single-writer guard: a non-blocking exclusive advisory lock. Two
        // proxyd processes sharing one audit file would otherwise interleave
        // writes and fork the chain, producing spurious "tampered" verdicts on
        // an honest system.
        acquire_exclusive_lock(&file).map_err(|e| {
            anyhow::anyhow!(
                "Another process is already writing the audit log at {} ({e}). \
                 Only one abox-proxyd may own an audit log.",
                path.display()
            )
        })?;

        Ok(Self {
            inner: Mutex::new(AuditLogInner {
                writer: std::io::BufWriter::new(file),
                logs_dir,
                key,
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
                last_hash.clone_from(&entry.hash);
                found_any = true;
            }
        }

        let next_seq = if found_any { last_seq + 1 } else { 0 };
        Ok((next_seq, last_hash))
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

        let hash = match compute_hash(&inner.key, &inner.last_hash, &core) {
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
        // Durability: push the record to stable storage before we treat it as
        // committed. Without fsync a crash could lose entries that flush()
        // already acknowledged.
        if let Err(e) = inner.writer.get_ref().sync_data() {
            tracing::warn!(error = %e, "Failed to fsync audit log");
        }

        inner.last_hash.clone_from(&hash);
        inner.next_seq += 1;

        // Persist the chain tip so truncation of the log tail is detectable.
        let tip = ChainTip { seq: entry.seq, hash };
        let logs_dir = inner.logs_dir.clone();
        if let Err(e) = audit::save_tip(&logs_dir, &tip) {
            tracing::warn!(error = %e, "Failed to persist audit chain tip");
        }
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
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Acquire a non-blocking exclusive advisory lock (`flock`) on the file.
///
/// Uses `rustix`'s safe wrapper (no `unsafe`); the lock is released
/// automatically when the file is closed (i.e. when `AuditLog` is dropped).
#[cfg(unix)]
fn acquire_exclusive_lock(file: &std::fs::File) -> std::io::Result<()> {
    use rustix::fs::{flock, FlockOperation};
    flock(file, FlockOperation::NonBlockingLockExclusive).map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn acquire_exclusive_lock(_file: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use abox_core::audit::{load_or_create_key, load_tip, verify_chain};
    use tempfile::tempdir;

    fn log_path(dir: &Path) -> PathBuf {
        dir.join("audit.jsonl")
    }

    #[test]
    fn test_audit_log_creates_chained_entries() {
        let dir = tempdir().unwrap();
        let path = log_path(dir.path());
        let log = AuditLog::new(&path).unwrap();

        log.log_cli("sandbox1", "git", &["status".to_string()], "allowed", 0);
        log.log_egress("sandbox1", "api.anthropic.com", "allowed", 200);
        log.log_cli("sandbox1", "curl", &["https://example.com".to_string()], "denied", -1);
        drop(log);

        let key = load_or_create_key(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let report = verify_chain(&content, &key, load_tip(dir.path()).as_ref());
        assert!(report.is_ok(), "Errors: {:?}", report.errors);
        assert_eq!(report.total_entries, 3);
    }

    #[test]
    fn test_audit_log_detects_tampering() {
        let dir = tempdir().unwrap();
        let path = log_path(dir.path());
        let log = AuditLog::new(&path).unwrap();
        log.log_cli("sandbox1", "git", &["status".to_string()], "allowed", 0);
        log.log_egress("sandbox1", "api.openai.com", "allowed", 200);
        drop(log);

        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replacen("\"allowed\"", "\"denied\"", 1);
        std::fs::write(&path, tampered).unwrap();

        let key = load_or_create_key(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let report = verify_chain(&content, &key, None);
        assert!(!report.is_ok(), "Should have detected tampering");
    }

    #[test]
    fn test_audit_log_first_entry_uses_zero_hash() {
        let dir = tempdir().unwrap();
        let path = log_path(dir.path());
        let log = AuditLog::new(&path).unwrap();
        log.log_cli("s", "echo", &[], "allowed", 0);
        drop(log);

        let content = std::fs::read_to_string(&path).unwrap();
        let entry: AuditEntry = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry.seq, 0);
        assert_eq!(entry.prev_hash, ZERO_HASH);
    }

    #[test]
    fn test_audit_log_resumes_chain_after_reopen() {
        let dir = tempdir().unwrap();
        let path = log_path(dir.path());

        {
            let log = AuditLog::new(&path).unwrap();
            log.log_cli("s", "git", &["status".to_string()], "allowed", 0);
        }
        {
            let log = AuditLog::new(&path).unwrap();
            log.log_egress("s", "api.github.com", "allowed", 200);
        }

        let key = load_or_create_key(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let report = verify_chain(&content, &key, load_tip(dir.path()).as_ref());
        assert!(report.is_ok(), "Errors: {:?}", report.errors);
        assert_eq!(report.total_entries, 2);

        let entries: Vec<AuditEntry> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[1].prev_hash, entries[0].hash);
    }

    #[test]
    fn test_truncation_detected_via_tip() {
        let dir = tempdir().unwrap();
        let path = log_path(dir.path());
        let log = AuditLog::new(&path).unwrap();
        for i in 0..4 {
            log.log_cli("s", "git", &[format!("op{i}")], "allowed", 0);
        }
        drop(log);

        // Truncate the log to the first entry, leaving the tip recording seq=3.
        let content = std::fs::read_to_string(&path).unwrap();
        let first_line = content.lines().next().unwrap();
        std::fs::write(&path, format!("{first_line}\n")).unwrap();

        let key = load_or_create_key(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let report = verify_chain(&content, &key, load_tip(dir.path()).as_ref());
        assert!(!report.is_ok(), "truncation should be detected");
        assert!(report.errors.iter().any(|e| e.contains("truncated")));
    }
}
