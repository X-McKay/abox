//! `abox audit` — Verify and inspect the audit log.
//!
//! The audit log at `~/.abox/logs/audit.jsonl` uses a hash-chained format
//! where each entry includes an HMAC of the previous entry (keyed with a
//! host-only secret). This command verifies the chain — detecting tampering,
//! deletion, insertion, or truncation — and prints recent entries.
//!
//! All format, hashing, and verification logic lives in [`abox_core::audit`],
//! shared with `abox-proxyd` (the writer) and `abox doctor`.

use abox_core::audit::{self, AuditEntry};
use abox_core::config::AboxConfig;
use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};

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

fn resolve_log_path(log_override: Option<PathBuf>, config: &AboxConfig) -> PathBuf {
    log_override.unwrap_or_else(|| audit::default_log_path(&config.logs_dir()))
}

pub fn execute(args: &AuditArgs, config: &AboxConfig) -> Result<()> {
    match &args.action {
        AuditAction::Verify { log } => verify(&resolve_log_path(log.clone(), config), config),
        AuditAction::Show { log, count, sandbox, request_type } => show(
            &resolve_log_path(log.clone(), config),
            *count,
            sandbox.as_deref(),
            request_type.as_deref(),
        ),
    }
}

/// Resolve the logs directory that holds the key and tip for a given log path.
fn logs_dir_for(path: &Path, config: &AboxConfig) -> PathBuf {
    path.parent().map_or_else(|| config.logs_dir(), Path::to_path_buf)
}

fn verify(path: &Path, config: &AboxConfig) -> Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "Audit log not found at {}\n\n\
             The audit log is created when abox-proxyd first runs.\n\
             Start a sandbox to generate audit entries.",
            path.display()
        );
    }

    let logs_dir = logs_dir_for(path, config);
    let key_path = logs_dir.join(audit::KEY_FILENAME);
    if !key_path.exists() {
        anyhow::bail!(
            "Audit key not found at {}\n\n\
             The keyed hash chain cannot be verified without the host-only key \
             that abox-proxyd created. If this key was lost, the chain's \
             authenticity cannot be established.",
            key_path.display()
        );
    }
    let key = audit::load_or_create_key(&logs_dir)?;
    let recorded_tip = audit::load_tip(&logs_dir);

    println!("Verifying audit log: {}", path.display());
    println!("{}", "=".repeat(60));

    let content = std::fs::read_to_string(path)?;
    let report = audit::verify_chain(&content, &key, recorded_tip.as_ref());

    println!("Chain Integrity");
    if report.is_ok() {
        println!(
            "  [ok] Hash chain: {} entries, no gaps, all HMACs valid",
            report.total_entries
        );
        if let (Some(seq), Some(hash)) = (report.tip_seq, report.tip_hash.as_ref()) {
            println!("  tip: seq={seq} hash={}…", &hash[..8.min(hash.len())]);
        }
        println!();
        println!("{}", "=".repeat(60));
        println!("VERDICT: [ok] INTACT — No tampering detected");
        Ok(())
    } else {
        for err in &report.errors {
            println!("  [FAIL] {err}");
        }
        println!();
        println!("{}", "=".repeat(60));
        println!("VERDICT: [FAIL] TAMPERED — {} error(s) detected", report.errors.len());
        // Use a clean error rather than process::exit so callers can compose.
        std::process::exit(1);
    }
}

fn show(
    path: &Path,
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
        let sandbox = truncate_chars(&entry.sandbox_id, 12);
        let target = truncate_chars(&entry.target, 40);
        println!(
            "{:<6} {:<26} {:<14} {:<10} {:<8} {}",
            entry.seq, ts, sandbox, entry.request_type, entry.decision, target
        );
        if !entry.detail.is_empty() {
            let detail = truncate_chars(&entry.detail, 60);
            println!("       args: {detail}");
        }
    }

    println!();
    println!("Showing {} of {} entries. Use --count to see more.", display.len(), entries.len());

    Ok(())
}

/// Truncate a string to at most `max` characters (not bytes), appending `…`.
///
/// `target`/`detail` are agent-influenced (URLs, command args) and may contain
/// multi-byte UTF-8; slicing by byte index here could panic, so take chars.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{prefix}…")
}

#[cfg(test)]
mod tests {
    use super::truncate_chars;

    #[test]
    fn truncate_chars_no_panic_on_multibyte() {
        let s = "λλλλλλλλλλλλλλλλλλλλ"; // 20 two-byte chars
        let out = truncate_chars(s, 5);
        assert!(out.chars().count() <= 5);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_chars_passthrough() {
        assert_eq!(truncate_chars("short", 40), "short");
    }
}
