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
            add_grant(
                name,
                domain.as_deref(),
                header.as_deref(),
                env_var.as_deref(),
                template,
                &policy_path,
            )
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
    // Read and parse the existing policy as an editable TOML document so we
    // never build TOML by string interpolation (which would corrupt the file
    // or allow injection via a crafted --header/--template) and so we don't
    // mangle the user's comments and formatting.
    let content = if policy_path.exists() {
        std::fs::read_to_string(policy_path)
            .with_context(|| format!("Reading {}", policy_path.display()))?
    } else {
        String::new()
    };
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("Parsing {}", policy_path.display()))?;

    // Ensure `egress` is an array-of-tables.
    let egress = doc.entry("egress").or_insert_with(|| {
        toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new())
    });
    let egress = egress.as_array_of_tables_mut().ok_or_else(|| {
        anyhow::anyhow!("`egress` in {} is not an array of tables", policy_path.display())
    })?;

    // Reject a duplicate domain by structurally inspecting the parsed rules.
    let exists = egress.iter().any(|t| t.get("domain").and_then(|v| v.as_str()) == Some(domain));
    if exists {
        println!("A rule for domain '{domain}' already exists in {}", policy_path.display());
        println!("Remove it first with: abox grant remove {display_name}");
        return Ok(());
    }

    // Build the new rule as a structured table; toml_edit escapes the values.
    let mut table = toml_edit::Table::new();
    table["domain"] = toml_edit::value(domain);
    table["inject_header"] = toml_edit::value(header);
    table["env_var"] = toml_edit::value(env_var);
    table["header_template"] = toml_edit::value(template);
    egress.push(table);

    if let Some(parent) = policy_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Creating {}", parent.display()))?;
    }
    std::fs::write(policy_path, doc.to_string())
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
    let policy: toml::Value =
        toml::from_str(&content).with_context(|| format!("Parsing {}", policy_path.display()))?;

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
            println!("{:<30} {:<20} {:<20} REQUEST RULES", "DOMAIN", "HEADER", "SOURCE");
            println!("{}", "-".repeat(90));
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
                // Surface per-request path restrictions so a path-scoped grant
                // is never silently hidden from the operator.
                let req_rules = rule
                    .get("request_rules")
                    .and_then(|v| v.as_array())
                    .map_or(0, toml::value::Array::len);
                let rules_col = if req_rules == 0 {
                    "(none)".to_string()
                } else {
                    format!("{req_rules} rule(s)")
                };
                println!("{domain:<30} {header:<20} {source:<20} {rules_col}");
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
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("Parsing {}", policy_path.display()))?;

    let Some(egress) = doc.get_mut("egress").and_then(toml_edit::Item::as_array_of_tables_mut)
    else {
        anyhow::bail!("No egress rules configured in {}", policy_path.display());
    };

    // Structurally remove the matching rule(s). retain() preserves the
    // surrounding document, comments, and other rules.
    let before = egress.len();
    egress.retain(|t| t.get("domain").and_then(|v| v.as_str()) != Some(domain.as_str()));
    let removed = before - egress.len();

    if removed == 0 {
        anyhow::bail!("No egress rule found for domain '{domain}' in {}", policy_path.display());
    }

    std::fs::write(policy_path, doc.to_string())
        .with_context(|| format!("Writing {}", policy_path.display()))?;

    println!("Grant for '{domain}' removed from {}", policy_path.display());
    Ok(())
}

fn list_providers() {
    println!("Built-in provider shortcuts:");
    println!();
    println!("{:<16} {:<30} {:<20} DESCRIPTION", "NAME", "DOMAIN", "ENV VAR");
    println!("{}", "-".repeat(90));
    for p in BUILTIN_PROVIDERS {
        println!("{:<16} {:<30} {:<20} {}", p.name, p.domain, p.env_var, p.description);
    }
    println!();
    println!("Usage: abox grant add <name>");
    println!("Example: abox grant add openai");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_remove_round_trips_via_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let policy_path = tmp.path().join("default.toml");
        std::fs::write(&policy_path, "# my policy\ndefault_egress_action = \"deny\"\n").unwrap();

        add_egress_rule(
            "api.example.com",
            "Authorization",
            "EXAMPLE_TOKEN",
            "Bearer {value}",
            &policy_path,
            "example",
        )
        .unwrap();

        let content = std::fs::read_to_string(&policy_path).unwrap();
        // The user's leading comment must be preserved.
        assert!(content.contains("# my policy"));
        // The rule parses back as valid structured TOML.
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        let egress = parsed.get("egress").and_then(|v| v.as_array()).unwrap();
        assert_eq!(egress.len(), 1);
        assert_eq!(egress[0].get("domain").and_then(|v| v.as_str()), Some("api.example.com"));

        remove_grant("api.example.com", &policy_path).unwrap();
        let content = std::fs::read_to_string(&policy_path).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        let egress = parsed.get("egress").and_then(|v| v.as_array());
        assert!(egress.is_none_or(Vec::is_empty));
        assert!(content.contains("# my policy"));
    }

    #[test]
    fn add_escapes_injection_in_values() {
        let tmp = tempfile::tempdir().unwrap();
        let policy_path = tmp.path().join("default.toml");

        // A template containing quotes and newlines must not corrupt the file
        // or inject extra TOML keys — toml_edit escapes it.
        add_egress_rule(
            "api.example.com",
            "Authorization",
            "TOK",
            "Bearer \"{value}\"\ninjected = true",
            &policy_path,
            "example",
        )
        .unwrap();

        let content = std::fs::read_to_string(&policy_path).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        // No top-level `injected` key should have been smuggled in.
        assert!(parsed.get("injected").is_none());
        let egress = parsed.get("egress").and_then(|v| v.as_array()).unwrap();
        assert_eq!(
            egress[0].get("header_template").and_then(|v| v.as_str()),
            Some("Bearer \"{value}\"\ninjected = true")
        );
    }

    #[test]
    fn remove_missing_domain_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let policy_path = tmp.path().join("default.toml");
        std::fs::write(&policy_path, "default_egress_action = \"deny\"\n").unwrap();
        assert!(remove_grant("nope.example.com", &policy_path).is_err());
    }
}
