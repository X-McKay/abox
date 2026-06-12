//! `abox grant` — Manage API credentials for transparent HTTP injection.
//!
//! Inspired by Moat's `moat grant` command, this allows users to store API
//! credentials that are injected at the network layer by the egress proxy.
//! The agent never sees the real token — it only sees a placeholder value
//! in its environment.
//!
//! # How it works
//!
//! 1. `abox grant add <name> --domain <domain> --header <header> --env <VAR>`
//!    registers a new credential injection rule in the policy file.
//! 2. When a sandbox makes an HTTPS request to `<domain>`, the egress proxy
//!    intercepts it, reads the credential from the host environment variable,
//!    and injects the `<header>` header before forwarding.
//! 3. The agent never sees the real token value.
//!
//! # Supported providers
//!
//! Built-in provider shortcuts are available for common services:
//! - `openai` — injects OPENAI_API_KEY as `Authorization: Bearer <key>`
//! - `anthropic` — injects ANTHROPIC_API_KEY as `x-api-key: <key>`
//! - `github` — injects GITHUB_TOKEN as `Authorization: Bearer <key>`
//!
//! # Example
//!
//! ```bash
//! # Use a built-in provider
//! abox grant add openai
//!
//! # Add a custom API key injection
//! abox grant add myservice \
//!   --domain api.myservice.com \
//!   --header "Authorization" \
//!   --env MY_SERVICE_API_KEY \
//!   --template "Bearer {value}"
//!
//! # List all configured grants
//! abox grant list
//!
//! # Remove a grant
//! abox grant remove myservice
//! ```

use abox_core::config::AboxConfig;
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use std::path::PathBuf;

/// Built-in provider definitions for common services.
struct BuiltinProvider {
    name: &'static str,
    domain: &'static str,
    header: &'static str,
    env_var: &'static str,
    template: &'static str,
    description: &'static str,
}

const BUILTIN_PROVIDERS: &[BuiltinProvider] = &[
    BuiltinProvider {
        name: "openai",
        domain: "api.openai.com",
        header: "Authorization",
        env_var: "OPENAI_API_KEY",
        template: "Bearer {value}",
        description: "OpenAI API (GPT-4, Codex, etc.)",
    },
    BuiltinProvider {
        name: "anthropic",
        domain: "api.anthropic.com",
        header: "x-api-key",
        env_var: "ANTHROPIC_API_KEY",
        template: "{value}",
        description: "Anthropic API (Claude)",
    },
    BuiltinProvider {
        name: "github",
        domain: "api.github.com",
        header: "Authorization",
        env_var: "GITHUB_TOKEN",
        template: "Bearer {value}",
        description: "GitHub API",
    },
    BuiltinProvider {
        name: "huggingface",
        domain: "huggingface.co",
        header: "Authorization",
        env_var: "HF_TOKEN",
        template: "Bearer {value}",
        description: "Hugging Face API",
    },
];

#[derive(Debug, Args)]
pub struct GrantArgs {
    #[command(subcommand)]
    pub action: GrantAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum GrantAction {
    /// Add a credential injection rule (or use a built-in provider shortcut).
    ///
    /// For built-in providers (openai, anthropic, github, huggingface),
    /// just run: abox grant add <provider>
    ///
    /// For custom services, provide --domain, --header, and --env.
    Add {
        /// Provider name or custom identifier.
        name: String,
        /// Target domain for credential injection (e.g., api.example.com).
        #[arg(long)]
        domain: Option<String>,
        /// HTTP header to inject (e.g., "Authorization", "x-api-key").
        #[arg(long)]
        header: Option<String>,
        /// Host environment variable containing the credential value.
        #[arg(long, name = "env")]
        env_var: Option<String>,
        /// Header value template. Use {value} as placeholder. Default: "{value}".
        #[arg(long, default_value = "{value}")]
        template: String,
        /// Path to policy file to update. Defaults to ~/.abox/policies/default.toml.
        #[arg(long)]
        policy: Option<PathBuf>,
    },

    /// List all configured credential injection rules.
    List {
        /// Path to policy file. Defaults to ~/.abox/policies/default.toml.
        #[arg(long)]
        policy: Option<PathBuf>,
    },

    /// Remove a credential injection rule by domain.
    Remove {
        /// Domain or provider name to remove.
        name: String,
        /// Path to policy file. Defaults to ~/.abox/policies/default.toml.
        #[arg(long)]
        policy: Option<PathBuf>,
    },

    /// List all available built-in provider shortcuts.
    Providers,

    /// Manage MCP OAuth tokens (discover, authorize, list, remove).
    #[command(subcommand)]
    Mcp(crate::commands::grant_mcp::GrantMcpAction),
}

pub async fn execute(args: &GrantArgs, config: &AboxConfig) -> Result<()> {
    match &args.action {
        GrantAction::Add { name, domain, header, env_var, template, policy } => {
            let policy_path = resolve_policy_path(policy.as_ref(), config);
            add_grant(name, domain.as_deref(), header.as_deref(), env_var.as_deref(), template, &policy_path)
        }
        GrantAction::List { policy } => {
            let policy_path = resolve_policy_path(policy.as_ref(), config);
            list_grants(&policy_path)
        }
        GrantAction::Remove { name, policy } => {
            let policy_path = resolve_policy_path(policy.as_ref(), config);
            remove_grant(name, &policy_path)
        }
        GrantAction::Providers => {
            list_providers();
            Ok(())
        }
        GrantAction::Mcp(action) => {
            let mcp_args = crate::commands::grant_mcp::GrantMcpArgs { action: action.clone() };
            crate::commands::grant_mcp::execute(&mcp_args, config).await
        }
    }
}

fn resolve_policy_path(override_path: Option<&PathBuf>, config: &AboxConfig) -> PathBuf {
    override_path.cloned().unwrap_or_else(|| config.proxy.policy_dir.join("default.toml"))
}

fn add_grant(
    name: &str,
    domain: Option<&str>,
    header: Option<&str>,
    env_var: Option<&str>,
    template: &str,
    policy_path: &PathBuf,
) -> Result<()> {
    // Check if it's a built-in provider
    if let Some(provider) = BUILTIN_PROVIDERS.iter().find(|p| p.name == name) {
        if domain.is_none() && header.is_none() && env_var.is_none() {
            return add_egress_rule(
                provider.domain,
                provider.header,
                provider.env_var,
                provider.template,
                policy_path,
                name,
            );
        }
    }

    // Custom grant: require all fields
    let domain = domain.ok_or_else(|| {
        anyhow::anyhow!(
            "Missing --domain for custom grant '{name}'.\n\
             For built-in providers (openai, anthropic, github, huggingface),\n\
             just run: abox grant add {name}\n\
             For custom services, provide --domain, --header, and --env."
        )
    })?;
    let header = header.ok_or_else(|| {
        anyhow::anyhow!("Missing --header for grant '{name}' (e.g., --header Authorization)")
    })?;
    let env_var = env_var.ok_or_else(|| {
        anyhow::anyhow!("Missing --env for grant '{name}' (e.g., --env MY_API_KEY)")
    })?;

    add_egress_rule(domain, header, env_var, template, policy_path, name)
}

fn add_egress_rule(
    domain: &str,
    header: &str,
    env_var: &str,
    template: &str,
    policy_path: &PathBuf,
    display_name: &str,
) -> Result<()> {
    // Read existing policy
    let content = if policy_path.exists() {
        std::fs::read_to_string(policy_path)
            .with_context(|| format!("Reading {}", policy_path.display()))?
    } else {
        String::new()
    };

    // Check if a rule for this domain already exists
    if content.contains(&format!("domain = \"{domain}\"")) {
        println!("A rule for domain '{domain}' already exists in {}", policy_path.display());
        println!("Remove it first with: abox grant remove {display_name}");
        return Ok(());
    }

    // Append the new egress rule
    let new_rule = format!(
        "\n[[egress]]\ndomain = \"{domain}\"\ninject_header = \"{header}\"\nenv_var = \"{env_var}\"\nheader_template = \"{template}\"\n"
    );

    let mut updated = content;
    updated.push_str(&new_rule);

    if let Some(parent) = policy_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Creating {}", parent.display()))?;
    }
    std::fs::write(policy_path, &updated)
        .with_context(|| format!("Writing {}", policy_path.display()))?;

    println!("Grant '{display_name}' added to {}", policy_path.display());
    println!();
    println!("Domain:   {domain}");
    println!("Header:   {header}: {template}");
    println!("Env var:  {env_var}");
    println!();
    println!("When a sandbox makes HTTPS requests to '{domain}', the proxy will");
    println!("inject the '{header}' header using the value of ${env_var} from the host.");
    println!("The agent never sees the real token.");
    println!();
    println!("Make sure ${env_var} is set on the host before running sandboxes.");

    Ok(())
}

fn list_grants(policy_path: &PathBuf) -> Result<()> {
    if !policy_path.exists() {
        println!("No policy file found at {}", policy_path.display());
        println!("Run 'abox init' to create the default policy, then 'abox grant add <provider>'.");
        return Ok(());
    }

    let content = std::fs::read_to_string(policy_path)
        .with_context(|| format!("Reading {}", policy_path.display()))?;

    // Parse the TOML to extract egress rules
    let policy: toml::Value = toml::from_str(&content)
        .with_context(|| format!("Parsing {}", policy_path.display()))?;

    let egress = policy.get("egress").and_then(|v| v.as_array());

    match egress {
        None => {
            println!("No credential injection rules configured.");
            println!();
            println!("Add one with: abox grant add <provider>");
            println!("See available providers with: abox grant providers");
        }
        Some(rules) if rules.is_empty() => {
            println!("No credential injection rules configured.");
            println!();
            println!("Add one with: abox grant add <provider>");
            println!("See available providers with: abox grant providers");
        }
        Some(rules) => {
            println!("Credential injection rules in {}:", policy_path.display());
            println!();
            println!("{:<30} {:<20} {:<20}", "DOMAIN", "HEADER", "SOURCE");
            println!("{}", "-".repeat(75));
            for rule in rules {
                let domain = rule.get("domain").and_then(|v| v.as_str()).unwrap_or("?");
                let header = rule.get("inject_header").and_then(|v| v.as_str()).unwrap_or("?");
                let source = if let Some(env) = rule.get("env_var").and_then(|v| v.as_str()) {
                    format!("env:{env}")
                } else if let Some(file) = rule.get("credential_file").and_then(|v| v.as_str()) {
                    format!("file:{file}")
                } else {
                    "?".to_string()
                };
                println!("{:<30} {:<20} {:<20}", domain, header, source);
            }
            println!();
            println!("{} rule(s) configured.", rules.len());
        }
    }

    Ok(())
}

fn remove_grant(name: &str, policy_path: &PathBuf) -> Result<()> {
    if !policy_path.exists() {
        anyhow::bail!("Policy file not found at {}", policy_path.display());
    }

    // Determine the domain to remove
    let domain = if let Some(provider) = BUILTIN_PROVIDERS.iter().find(|p| p.name == name) {
        provider.domain.to_string()
    } else {
        // Treat name as a domain directly
        name.to_string()
    };

    let content = std::fs::read_to_string(policy_path)
        .with_context(|| format!("Reading {}", policy_path.display()))?;

    let policy: toml::Value = toml::from_str(&content)
        .with_context(|| format!("Parsing {}", policy_path.display()))?;

    let egress = policy.get("egress").and_then(|v| v.as_array());
    let found = egress.map_or(false, |rules| {
        rules.iter().any(|r| r.get("domain").and_then(|v| v.as_str()) == Some(&domain))
    });

    if !found {
        anyhow::bail!(
            "No egress rule found for domain '{domain}' in {}",
            policy_path.display()
        );
    }

    // Remove the [[egress]] block for this domain by rewriting the TOML
    // We do a simple text-based removal of the [[egress]] block
    let updated = remove_egress_block(&content, &domain);
    std::fs::write(policy_path, &updated)
        .with_context(|| format!("Writing {}", policy_path.display()))?;

    println!("Grant for '{domain}' removed from {}", policy_path.display());
    Ok(())
}

/// Remove an [[egress]] block for a specific domain from TOML content.
/// This is a simple text-based approach that handles the common case.
fn remove_egress_block(content: &str, domain: &str) -> String {
    let mut result = Vec::new();
    let mut in_target_block = false;
    let mut skip_blank = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "[[egress]]" {
            // Start of a new egress block — peek ahead to check if it's the target
            in_target_block = false;
            // We'll decide when we see the domain line
            result.push(("[[egress]]", line));
            continue;
        }

        if trimmed.starts_with("domain = ") {
            let line_domain = trimmed
                .strip_prefix("domain = \"")
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or("");
            if line_domain == domain {
                // This is the target block — remove the [[egress]] line we just pushed
                if let Some(last) = result.last() {
                    if last.0 == "[[egress]]" {
                        result.pop();
                        in_target_block = true;
                        skip_blank = true;
                        continue;
                    }
                }
            }
        }

        if in_target_block {
            // Skip lines until we hit the next [[egress]] or end of file
            if trimmed.starts_with("[[") {
                in_target_block = false;
                skip_blank = false;
                result.push(("other", line));
            }
            // else skip this line
            continue;
        }

        if skip_blank && trimmed.is_empty() {
            skip_blank = false;
            continue;
        }
        skip_blank = false;

        result.push(("other", line));
    }

    result.iter().map(|(_, line)| *line).collect::<Vec<_>>().join("\n") + "\n"
}

fn list_providers() {
    println!("Built-in provider shortcuts:");
    println!();
    println!("{:<16} {:<30} {:<20} {}", "NAME", "DOMAIN", "ENV VAR", "DESCRIPTION");
    println!("{}", "-".repeat(90));
    for p in BUILTIN_PROVIDERS {
        println!("{:<16} {:<30} {:<20} {}", p.name, p.domain, p.env_var, p.description);
    }
    println!();
    println!("Usage: abox grant add <name>");
    println!("Example: abox grant add openai");
}
