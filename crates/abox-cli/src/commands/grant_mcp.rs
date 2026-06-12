//! `abox grant mcp` — MCP OAuth discovery and token management.
//!
//! Implements the `abox grant mcp` subcommand for discovering OAuth endpoints
//! from MCP server URLs and performing the authorization flow.

use abox_core::config::AboxConfig;
use abox_core::mcp_oauth::{
    delete_token, discover_oauth_metadata, list_tokens, load_token, run_auth_flow, save_token,
    McpToken,
};
use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct GrantMcpArgs {
    #[command(subcommand)]
    pub action: GrantMcpAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum GrantMcpAction {
    /// Discover OAuth endpoints from an MCP server and authorize.
    ///
    /// Performs RFC 8414 OAuth metadata discovery from the server URL,
    /// then runs the authorization code flow with PKCE.
    Auth {
        /// MCP server URL (e.g., https://mcp.example.com).
        server_url: String,
        /// Name to store the token under (defaults to the server hostname).
        #[arg(long)]
        name: Option<String>,
        /// OAuth client ID (required if server does not support Dynamic Client Registration).
        #[arg(long)]
        client_id: Option<String>,
        /// OAuth scopes to request (space-separated).
        #[arg(long)]
        scopes: Option<String>,
    },

    /// List all stored MCP OAuth tokens.
    List,

    /// Show details of a stored MCP token.
    Show {
        /// Token name.
        name: String,
    },

    /// Remove a stored MCP token.
    Remove {
        /// Token name.
        name: String,
    },
}

pub async fn execute(args: &GrantMcpArgs, config: &AboxConfig) -> Result<()> {
    match &args.action {
        GrantMcpAction::Auth { server_url, name, client_id, scopes } => {
            auth(server_url, name.as_deref(), client_id.as_deref(), scopes.as_deref(), config).await
        }
        GrantMcpAction::List => list_mcp_tokens(config),
        GrantMcpAction::Show { name } => show_token(name, config),
        GrantMcpAction::Remove { name } => remove_mcp_token(name, config),
    }
}

async fn auth(
    server_url: &str,
    name: Option<&str>,
    client_id: Option<&str>,
    scopes: Option<&str>,
    config: &AboxConfig,
) -> Result<()> {
    // Derive name from server URL if not provided
    let token_name = name.map_or_else(
        || {
            server_url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or(server_url)
                .replace('.', "-")
        },
        str::to_string,
    );

    println!("Discovering OAuth endpoints for {server_url}...");

    let metadata = discover_oauth_metadata(server_url).await?;

    let metadata = match metadata {
        Some(m) => {
            println!("  Authorization endpoint: {}", m.authorization_endpoint);
            println!("  Token endpoint:         {}", m.token_endpoint);
            if let Some(ref reg) = m.registration_endpoint {
                println!("  Registration endpoint:  {reg}");
            }
            m
        }
        None => {
            anyhow::bail!(
                "Server at {server_url} does not support OAuth 2.0 discovery.\n\
                 The server must expose /.well-known/oauth-authorization-server\n\
                 as per RFC 8414."
            );
        }
    };

    // Use provided client_id or a default
    let effective_client_id = client_id.unwrap_or("abox-cli");

    let scopes_vec: Vec<String> = scopes
        .unwrap_or("")
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    println!();
    println!("Starting OAuth authorization flow...");
    println!("Client ID: {effective_client_id}");
    if !scopes_vec.is_empty() {
        println!("Scopes: {}", scopes_vec.join(" "));
    }

    let (access_token, refresh_token, expires_at, granted_scopes) =
        run_auth_flow(&metadata, effective_client_id, &scopes_vec, server_url).await?;

    let stored_at = chrono::Utc::now().to_rfc3339();
    let token = McpToken {
        name: token_name.clone(),
        server_url: server_url.to_string(),
        access_token,
        token_type: "Bearer".to_string(),
        refresh_token,
        expires_at,
        scopes: granted_scopes,
        stored_at,
    };

    let path = save_token(&config.state_dir, &token)?;

    println!();
    println!("Token stored as '{token_name}' at {}", path.display());
    if let Some(exp) = expires_at {
        let dt = chrono::DateTime::from_timestamp(exp, 0).map_or_else(
            || "unknown".to_string(),
            |dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        );
        println!("Expires: {dt}");
    }
    println!();
    println!("To use this token for credential injection, add to your policy:");
    println!();
    println!("  [[egress]]");
    println!("  domain = \"{}\"", extract_domain(server_url));
    println!("  inject_header = \"Authorization\"");
    println!("  credential_file = \"~/.abox/mcp-tokens/{token_name}.json\"");
    println!("  json_path = \"access_token\"");
    println!("  header_template = \"Bearer {{value}}\"");
    println!();
    println!("Or run: abox grant add {token_name} --domain {} --header Authorization --env ABOX_MCP_{}_TOKEN",
        extract_domain(server_url),
        token_name.to_uppercase().replace('-', "_")
    );

    Ok(())
}

fn list_mcp_tokens(config: &AboxConfig) -> Result<()> {
    let tokens = list_tokens(&config.state_dir)?;

    if tokens.is_empty() {
        println!("No MCP OAuth tokens stored.");
        println!();
        println!("Authorize with: abox grant mcp auth <server-url>");
        return Ok(());
    }

    println!("{:<24} {:<40} {:<10} SCOPES", "NAME", "SERVER", "STATUS");
    println!("{}", "-".repeat(90));
    for token in &tokens {
        let status = if token.is_expired() { "expired" } else { "valid" };
        let scopes =
            if token.scopes.is_empty() { "(none)".to_string() } else { token.scopes.join(", ") };
        let server = if token.server_url.len() > 38 {
            format!("{}…", &token.server_url[..37])
        } else {
            token.server_url.clone()
        };
        println!("{:<24} {:<40} {:<10} {}", token.name, server, status, scopes);
    }
    println!();
    println!("{} token(s) stored.", tokens.len());

    Ok(())
}

fn show_token(name: &str, config: &AboxConfig) -> Result<()> {
    let token = load_token(&config.state_dir, name)?
        .ok_or_else(|| anyhow::anyhow!("No token found for '{name}'"))?;

    println!("Token: {name}");
    println!("Server: {}", token.server_url);
    println!("Type: {}", token.token_type);
    println!("Status: {}", if token.is_expired() { "EXPIRED" } else { "valid" });
    if let Some(exp) = token.expires_at {
        let dt = chrono::DateTime::from_timestamp(exp, 0).map_or_else(
            || "unknown".to_string(),
            |dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        );
        println!("Expires: {dt}");
    }
    println!("Stored: {}", token.stored_at);
    if !token.scopes.is_empty() {
        println!("Scopes: {}", token.scopes.join(", "));
    }
    println!("Has refresh token: {}", token.refresh_token.is_some());
    println!();
    println!("Access token: {}…", &token.access_token[..token.access_token.len().min(20)]);

    Ok(())
}

fn remove_mcp_token(name: &str, config: &AboxConfig) -> Result<()> {
    let deleted = delete_token(&config.state_dir, name)?;
    if deleted {
        println!("Token '{name}' removed.");
    } else {
        anyhow::bail!("No token found for '{name}'");
    }
    Ok(())
}

fn extract_domain(url: &str) -> &str {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
}
