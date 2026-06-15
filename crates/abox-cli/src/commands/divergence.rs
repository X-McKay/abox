//! `abox divergence` — Show which files each sandbox has changed.

use abox_core::sandbox::SandboxOrchestrator;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Args)]
pub struct DivergenceArgs {
    /// Base branch to compare against. Default: "main".
    #[arg(long, default_value = "main")]
    pub base: String,
    /// Emit machine-readable JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// Stable JSON contract for one (file, sandbox) divergence entry.
#[derive(Debug, Serialize)]
pub struct DivergenceItem {
    pub file: String,
    pub sandbox: String,
    pub status: String,
}

pub fn execute<W: WorkspacePort, V: VmPort>(
    args: &DivergenceArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    let entries = orchestrator.divergence(&args.base)?;

    if args.json {
        let items: Vec<DivergenceItem> = entries
            .iter()
            .map(|e| DivergenceItem {
                file: e.file_path.clone(),
                sandbox: e.sandbox_id.clone(),
                status: e.status.to_string(),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No divergence from '{}'.", args.base);
        return Ok(());
    }

    // Group by file path to show which sandboxes touch the same files
    let mut by_file: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for entry in &entries {
        by_file
            .entry(entry.file_path.clone())
            .or_default()
            .push((entry.sandbox_id.clone(), entry.status.to_string()));
    }

    // Print the divergence matrix
    println!("{:<40} {:<16} {:<12}", "FILE", "SANDBOX", "STATUS");
    println!("{}", "-".repeat(70));

    for (file, sandboxes) in &by_file {
        let conflict_marker = if sandboxes.len() > 1 { " [!]" } else { "" };
        for (i, (sandbox, status)) in sandboxes.iter().enumerate() {
            let file_col = if i == 0 { format!("{file}{conflict_marker}") } else { String::new() };
            println!("{file_col:<40} {sandbox:<16} {status:<12}");
        }
    }

    // Warn about potential conflicts
    let conflicts: Vec<_> = by_file.iter().filter(|(_, v)| v.len() > 1).collect();

    if !conflicts.is_empty() {
        println!();
        println!("Warning: {} file(s) modified by multiple sandboxes [!]", conflicts.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergence_item_json_fields() {
        let item = DivergenceItem {
            file: "src/a.rs".into(),
            sandbox: "t".into(),
            status: "modified".into(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["file"], "src/a.rs");
        assert_eq!(json["sandbox"], "t");
        assert_eq!(json["status"], "modified");
    }
}
