//! `abox audit` — Verify the integrity of the audit log.
//!
//! The audit log at `~/.abox/logs/audit.jsonl` uses a hash-chained format
//! where each entry includes the SHA-256 of the previous entry. This command
//! verifies the chain, detecting any tampering, deletion, or insertion of
//! entries.

use abox_core::config::AboxConfig;
use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

// Re-export the verify logic from abox-proxyd's audit module via a thin
// wrapper so the CLI does not depend on abox-proxyd as a library crate.
// Instead, we duplicate the minimal verification logic here.

/// The zero hash used as the predecessor of the first entry.
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Args)]
pub struct AuditArgs {
    #[command(subcommand)]
    pub action: AuditAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AuditAction {
    /// Verify the integrity of the audit log hash chain.
    Verify {
        /// Path to the audit log file. Defaults to ~/.abox/logs/audit.jsonl.
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Print recent audit log entries.
    Show {
        /// Path to the audit log file. Defaults to ~/.abox/logs/audit.jsonl.
        #[arg(long)]
        log: Option<PathBuf>,
        /// Number of most recent entries to show (default: 20).
        #[arg(long, short = 'n', default_value = "20")]
        count: usize,
        /// Filter by sandbox ID.
        #[arg(long)]
        sandbox: Option<String>,
        /// Filter by request type: "cli" or "egress".
        #[arg(long)]
        request_type: Option<String>,
    },
}

/// Minimal audit entry for verification (mirrors abox-proxyd's AuditEntry).
#[derive(Debug, serde::Deserialize)]
struct AuditEntry {
    pub seq: u64,
    pub timestamp: String,
    pub sandbox_id: String,
    pub request_type: String,
    pub target: String,
    pub detail: String,
    pub decision: String,
    pub result_code: i32,
    pub prev_hash: String,
    pub hash: String,
}

/// Core fields used for hash computation (must match abox-proxyd's AuditEntryCore).
#[derive(Debug, serde::Serialize)]
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

fn compute_hash(prev_hash: &str, core: &AuditEntryCore) -> Result<String> {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_string(core)?;
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"||");
    hasher.update(canonical.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn resolve_log_path(log_override: Option<PathBuf>, config: &AboxConfig) -> PathBuf {
    log_override.unwrap_or_else(|| config.logs_dir().join("audit.jsonl"))
}

pub fn execute(args: &AuditArgs, config: &AboxConfig) -> Result<()> {
    match &args.action {
        AuditAction::Verify { log } => verify(&resolve_log_path(log.clone(), config)),
        AuditAction::Show { log, count, sandbox, request_type } => show(
            &resolve_log_path(log.clone(), config),
            *count,
            sandbox.as_deref(),
            request_type.as_deref(),
        ),
    }
}

fn verify(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "Audit log not found at {}\n\n\
             The audit log is created when abox-proxyd first runs.\n\
             Start a sandbox to generate audit entries.",
            path.display()
        );
    }

    println!("Verifying audit log: {}", path.display());
    println!("{}", "=".repeat(60));

    let content = std::fs::read_to_string(path)?;
    let mut errors: Vec<String> = Vec::new();
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

        if entry.seq != expected_seq {
            errors.push(format!(
                "seq={}: sequence gap — expected {expected_seq}, got {}",
                entry.seq, entry.seq
            ));
        }

        if entry.prev_hash != prev_hash {
            let prev_short = &prev_hash[..8.min(prev_hash.len())];
            let got_short = &entry.prev_hash[..8.min(entry.prev_hash.len())];
            errors.push(format!(
                "seq={}: prev_hash mismatch — expected {prev_short}…, got {got_short}…",
                entry.seq
            ));
        }

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

        let expected_hash = compute_hash(&entry.prev_hash, &core)?;
        if entry.hash != expected_hash {
            let claimed = &entry.hash[..8.min(entry.hash.len())];
            let computed = &expected_hash[..8.min(expected_hash.len())];
            errors.push(format!(
                "seq={}: hash mismatch — entry claims {claimed}…, computed {computed}…",
                entry.seq
            ));
        }

        prev_hash.clone_from(&entry.hash);
        expected_seq = entry.seq + 1;
        total += 1;
    }

    println!("Chain Integrity");
    if errors.is_empty() {
        println!("  [ok] Hash chain: {total} entries, no gaps, all hashes valid");
        println!();
        println!("{}", "=".repeat(60));
        println!("VERDICT: [ok] INTACT — No tampering detected");
    } else {
        for err in &errors {
            println!("  [FAIL] {err}");
        }
        println!();
        println!("{}", "=".repeat(60));
        println!("VERDICT: [FAIL] TAMPERED — {} error(s) detected", errors.len());
        std::process::exit(1);
    }

    Ok(())
}

fn show(
    path: &std::path::Path,
    count: usize,
    sandbox_filter: Option<&str>,
    type_filter: Option<&str>,
) -> Result<()> {
    if !path.exists() {
        println!("No audit log found at {}", path.display());
        println!("Start a sandbox to generate audit entries.");
        return Ok(());
    }

    let content = std::fs::read_to_string(path)?;
    let entries: Vec<AuditEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|e: &AuditEntry| {
            sandbox_filter.is_none_or(|s| e.sandbox_id.contains(s))
                && type_filter.is_none_or(|t| e.request_type == t)
        })
        .collect();

    let display: Vec<_> = entries.iter().rev().take(count).collect();

    if display.is_empty() {
        println!("No audit entries found.");
        return Ok(());
    }

    println!(
        "{:<6} {:<26} {:<14} {:<10} {:<8} TARGET",
        "SEQ", "TIMESTAMP", "SANDBOX", "TYPE", "DECISION"
    );
    println!("{}", "-".repeat(90));

    for entry in display.iter().rev() {
        let ts = entry.timestamp.get(..19).unwrap_or(&entry.timestamp);
        let sandbox = if entry.sandbox_id.len() > 12 {
            format!("{}…", &entry.sandbox_id[..11])
        } else {
            entry.sandbox_id.clone()
        };
        let target = if entry.target.len() > 40 {
            format!("{}…", &entry.target[..39])
        } else {
            entry.target.clone()
        };
        println!(
            "{:<6} {:<26} {:<14} {:<10} {:<8} {}",
            entry.seq, ts, sandbox, entry.request_type, entry.decision, target
        );
        if !entry.detail.is_empty() {
            let detail = if entry.detail.len() > 60 {
                format!("{}…", &entry.detail[..59])
            } else {
                entry.detail.clone()
            };
            println!("       args: {detail}");
        }
    }

    println!();
    println!("Showing {} of {} entries. Use --count to see more.", display.len(), entries.len());

    Ok(())
}
