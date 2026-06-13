//! Shared audit-log format, hashing, and verification.
//!
//! This module is the single source of truth for the hash-chained audit log
//! written by `abox-proxyd` and verified by `abox audit verify` / `abox doctor`.
//! Keeping the entry types, canonicalization, hashing, and verification here
//! prevents the three consumers from drifting apart (a drift would silently
//! break verification of historical logs).
//!
//! # Tamper-evidence — what it does and does not guarantee
//!
//! Each entry carries `prev_hash` and `hash`, forming a chain:
//!   `hash = HMAC_SHA256(key, DOMAIN || prev_hash || "||" || canonical_core)`
//!
//! The chain is **keyed** with a host-only secret (`audit.key`, mode 0600) that
//! the sandboxed guest cannot read. This means:
//!
//! - A compromised **guest/agent** cannot forge or rewrite the log: it has no
//!   filesystem access to the host log *or* the key.
//! - Accidental edits, insertions, deletions, and re-orderings are detected.
//! - **Truncation** of the tail is detected by comparing against a separately
//!   persisted chain tip (`audit.tip`).
//!
//! It does **not** defend against an attacker who already holds the host key
//! (e.g. root on the host): such an attacker can recompute a valid chain. For
//! guarantees against a fully-compromised host, periodically export the chain
//! tip (`seq` + `hash`) to an append-only or external sink. `verify` reports the
//! current tip so this can be automated.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The zero hash used as the predecessor of the first entry.
pub const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Domain-separation tag mixed into every hash. The version suffix means a
/// future change to the canonical format can bump the tag without colliding
/// with chains produced by older code.
const HASH_DOMAIN: &str = "abox-audit-v1";

/// Filename of the audit log within the logs directory.
pub const LOG_FILENAME: &str = "audit.jsonl";
/// Filename of the host-only HMAC key.
pub const KEY_FILENAME: &str = "audit.key";
/// Filename of the persisted chain tip (for truncation detection).
pub const TIP_FILENAME: &str = "audit.tip";

/// The canonical, hashed core of an audit entry (chain fields excluded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntryCore {
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
    /// Hex hash of the previous entry (or [`ZERO_HASH`] for seq 0).
    pub prev_hash: String,
    /// Hex `HMAC_SHA256(key, DOMAIN || prev_hash || "||" || canonical_core)`.
    pub hash: String,
}

impl AuditEntry {
    /// Extract the canonical core fields used for hashing.
    pub fn core(&self) -> AuditEntryCore {
        AuditEntryCore {
            seq: self.seq,
            timestamp: self.timestamp.clone(),
            sandbox_id: self.sandbox_id.clone(),
            request_type: self.request_type.clone(),
            target: self.target.clone(),
            detail: self.detail.clone(),
            decision: self.decision.clone(),
            result_code: self.result_code,
        }
    }
}

/// Compute the chain hash for an entry.
///
/// `key` is the host-only HMAC key. The hash is keyed so the chain cannot be
/// recomputed by anyone without the key.
pub fn compute_hash(key: &[u8], prev_hash: &str, core: &AuditEntryCore) -> Result<String> {
    let canonical = serde_json::to_string(core).context("Serializing audit core")?;
    let mut msg = Vec::with_capacity(HASH_DOMAIN.len() + prev_hash.len() + 2 + canonical.len());
    msg.extend_from_slice(HASH_DOMAIN.as_bytes());
    msg.extend_from_slice(prev_hash.as_bytes());
    msg.extend_from_slice(b"||");
    msg.extend_from_slice(canonical.as_bytes());
    Ok(hex(&hmac_sha256(key, &msg)))
}

/// HMAC-SHA256 (RFC 2104), implemented over `sha2` to avoid an extra crate.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        k[..32].copy_from_slice(&digest);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The persisted chain tip, used to detect truncation of the log tail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainTip {
    pub seq: u64,
    pub hash: String,
}

/// Result of verifying an audit log.
#[derive(Debug)]
pub struct VerifyReport {
    /// Number of entries successfully walked.
    pub total_entries: usize,
    /// Human-readable errors; empty means the chain is intact.
    pub errors: Vec<String>,
    /// The last seq seen (if any), for tip export.
    pub tip_seq: Option<u64>,
    /// The last hash seen (if any).
    pub tip_hash: Option<String>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Verify the integrity of audit log `content` using `key`.
///
/// Walks the chain from seq 0. On the **first** structural failure (parse
/// error, sequence gap, broken link, or hash mismatch) it records a precise
/// error and stops — once the chain is broken everything after is unverifiable,
/// so cascading errors would only obscure the true tamper point.
///
/// If `recorded_tip` is provided, a log whose final entry is *behind* the
/// recorded tip is flagged as truncated.
pub fn verify_chain(content: &str, key: &[u8], recorded_tip: Option<&ChainTip>) -> VerifyReport {
    let mut errors: Vec<String> = Vec::new();
    let mut prev_hash = ZERO_HASH.to_string();
    let mut expected_seq = 0u64;
    let mut total = 0usize;
    let mut last_hash: Option<String> = None;
    let mut last_seq: Option<u64> = None;

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let entry: AuditEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("Line {}: JSON parse error: {e}", line_num + 1));
                break;
            }
        };

        if entry.seq != expected_seq {
            errors.push(format!(
                "Line {} (seq={}): sequence gap — expected seq={expected_seq}",
                line_num + 1,
                entry.seq
            ));
            break;
        }

        if entry.prev_hash != prev_hash {
            errors.push(format!(
                "Line {} (seq={}): prev_hash mismatch — expected {}…, got {}…",
                line_num + 1,
                entry.seq,
                short(&prev_hash),
                short(&entry.prev_hash),
            ));
            break;
        }

        let expected_hash = match compute_hash(key, &entry.prev_hash, &entry.core()) {
            Ok(h) => h,
            Err(e) => {
                errors.push(format!("Line {} (seq={}): hash error: {e}", line_num + 1, entry.seq));
                break;
            }
        };
        if entry.hash != expected_hash {
            errors.push(format!(
                "Line {} (seq={}): hash mismatch — entry claims {}…, computed {}…",
                line_num + 1,
                entry.seq,
                short(&entry.hash),
                short(&expected_hash),
            ));
            break;
        }

        prev_hash.clone_from(&entry.hash);
        last_hash = Some(entry.hash.clone());
        last_seq = Some(entry.seq);
        expected_seq = entry.seq + 1;
        total += 1;
    }

    // Truncation detection: the log must reach at least the recorded tip.
    if errors.is_empty() {
        if let Some(tip) = recorded_tip {
            match last_seq {
                Some(seq) if seq >= tip.seq => {}
                _ => errors.push(format!(
                    "log truncated — recorded tip is seq={} but log ends at seq={}",
                    tip.seq,
                    last_seq.map_or_else(|| "none".to_string(), |s| s.to_string()),
                )),
            }
        }
    }

    VerifyReport { total_entries: total, errors, tip_seq: last_seq, tip_hash: last_hash }
}

fn short(hash: &str) -> &str {
    &hash[..8.min(hash.len())]
}

/// Load the host-only HMAC key, creating it (mode 0600) if absent.
///
/// The key lives next to the log so a single logs directory is self-contained.
pub fn load_or_create_key(logs_dir: &Path) -> Result<Vec<u8>> {
    std::fs::create_dir_all(logs_dir)
        .with_context(|| format!("Creating logs dir {}", logs_dir.display()))?;
    let path = logs_dir.join(KEY_FILENAME);
    if path.exists() {
        let key = std::fs::read(&path).with_context(|| format!("Reading {}", path.display()))?;
        if key.len() >= 32 {
            return Ok(key);
        }
        anyhow::bail!("Audit key at {} is too short ({} bytes)", path.display(), key.len());
    }
    let mut key = vec![0u8; 32];
    crate::util::secure_random_bytes(&mut key)
        .map_err(|e| anyhow::anyhow!("Generating audit key: {e}"))?;
    write_owner_only(&path, &key).with_context(|| format!("Writing {}", path.display()))?;
    Ok(key)
}

/// Load the recorded chain tip, if any.
pub fn load_tip(logs_dir: &Path) -> Option<ChainTip> {
    let path = logs_dir.join(TIP_FILENAME);
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Persist the chain tip (best-effort, mode 0600).
pub fn save_tip(logs_dir: &Path, tip: &ChainTip) -> Result<()> {
    let path = logs_dir.join(TIP_FILENAME);
    let json = serde_json::to_string(tip)?;
    write_owner_only(&path, json.as_bytes())?;
    Ok(())
}

/// Write a file with mode 0600 (owner read/write only) on Unix.
fn write_owner_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents)?;
    f.flush()
}

/// Resolve the default audit log path within a logs directory.
pub fn default_log_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join(LOG_FILENAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_core(seq: u64) -> AuditEntryCore {
        AuditEntryCore {
            seq,
            timestamp: "2024-01-01T00:00:00Z".into(),
            sandbox_id: "s".into(),
            request_type: "cli".into(),
            target: "git".into(),
            detail: "status".into(),
            decision: "allowed".into(),
            result_code: 0,
        }
    }

    fn build_chain(key: &[u8], n: u64) -> String {
        let mut prev = ZERO_HASH.to_string();
        let mut out = String::new();
        for seq in 0..n {
            let core = sample_core(seq);
            let hash = compute_hash(key, &prev, &core).unwrap();
            let entry = AuditEntry {
                seq: core.seq,
                timestamp: core.timestamp,
                sandbox_id: core.sandbox_id,
                request_type: core.request_type,
                target: core.target,
                detail: core.detail,
                decision: core.decision,
                result_code: core.result_code,
                prev_hash: prev.clone(),
                hash: hash.clone(),
            };
            out.push_str(&serde_json::to_string(&entry).unwrap());
            out.push('\n');
            prev = hash;
        }
        out
    }

    #[test]
    fn hmac_sha256_matches_known_vector() {
        // RFC 4231 test case 1: key = 0x0b*20, data = "Hi There".
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(hex(&mac), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    }

    #[test]
    fn valid_chain_verifies() {
        let key = b"test-key-0123456789012345678901234";
        let log = build_chain(key, 3);
        let report = verify_chain(&log, key, None);
        assert!(report.is_ok(), "{:?}", report.errors);
        assert_eq!(report.total_entries, 3);
        assert_eq!(report.tip_seq, Some(2));
    }

    #[test]
    fn tampered_entry_detected() {
        let key = b"test-key-0123456789012345678901234";
        let log = build_chain(key, 3).replacen("\"allowed\"", "\"denied\"", 1);
        let report = verify_chain(&log, key, None);
        assert!(!report.is_ok());
    }

    #[test]
    fn wrong_key_fails_to_verify() {
        let log = build_chain(b"key-a-padded-out-to-some-length-xx", 2);
        let report = verify_chain(&log, b"key-b-padded-out-to-some-length-xx", None);
        assert!(!report.is_ok(), "a different key must not verify");
    }

    #[test]
    fn truncation_detected_against_tip() {
        let key = b"test-key-0123456789012345678901234";
        let log = build_chain(key, 5);
        // Drop the last two lines.
        let mut truncated = String::new();
        for l in log.lines().take(3) {
            truncated.push_str(l);
            truncated.push('\n');
        }
        let tip = ChainTip { seq: 4, hash: "whatever".into() };
        let report = verify_chain(&truncated, key, Some(&tip));
        assert!(!report.is_ok());
        assert!(report.errors.iter().any(|e| e.contains("truncated")));
    }

    #[test]
    fn parse_error_stops_without_cascade() {
        let key = b"test-key-0123456789012345678901234";
        let mut log = build_chain(key, 3);
        log.push_str("{not valid json\n");
        log.push_str("{also bad\n");
        let report = verify_chain(&log, key, None);
        // Exactly one error reported, not one per bad line.
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn key_is_created_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let key = load_or_create_key(tmp.path()).unwrap();
        assert_eq!(key.len(), 32);
        // Loading again returns the same key.
        let key2 = load_or_create_key(tmp.path()).unwrap();
        assert_eq!(key, key2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode =
                std::fs::metadata(tmp.path().join(KEY_FILENAME)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
