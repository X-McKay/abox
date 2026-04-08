//! `abox template` — Manage VM snapshot templates.

use abox_core::config::AboxConfig;
use abox_core::snapshot::SnapshotManager;
use abox_core::util::format_size;
use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct TemplateArgs {
    #[command(subcommand)]
    pub action: TemplateAction,
}

#[derive(Debug, Subcommand)]
pub enum TemplateAction {
    /// List available templates.
    List,

    /// Create a template from a paused sandbox.
    Create {
        /// Name for the new template.
        #[arg(long)]
        name: String,
        /// Sandbox ID to snapshot (must be paused first).
        #[arg(long)]
        from: String,
    },

    /// Delete a template.
    Delete {
        /// Template name to delete.
        name: String,
    },
}

pub fn execute(args: TemplateArgs, config: &AboxConfig) -> Result<()> {
    let snap_mgr = SnapshotManager::new(config.templates_dir(), config.runtime_dir())?;

    match args.action {
        TemplateAction::List => {
            let templates = snap_mgr.list_templates()?;
            if templates.is_empty() {
                println!("No templates available.");
                println!();
                println!("Create one with: abox template create --name <name> --from <sandbox>");
            } else {
                println!("{:<20} {:<12} PATH", "NAME", "SIZE");
                println!("{}", "-".repeat(60));
                for t in &templates {
                    println!(
                        "{:<20} {:<12} {}",
                        t.name,
                        format_size(t.size_bytes),
                        t.path.display()
                    );
                }
            }
        }
        TemplateAction::Create { name, from: _ } => {
            // In a full implementation, this looks up the sandbox's API socket
            // and calls snap_mgr.create_snapshot(). For now, prints instructions.
            println!("To create a template:");
            println!("  1. Pause the sandbox:  abox pause <sandbox>");
            println!(
                "  2. Create template:    abox template create --name {name} --from <sandbox>"
            );
            println!();
            println!("Template creation requires the sandbox to be paused.");
        }
        TemplateAction::Delete { name } => {
            snap_mgr.delete_template(&name)?;
            println!("Template '{name}' deleted.");
        }
    }

    Ok(())
}
