//! MCP OAuth 2.0 discovery and token management.
//!
//! Implements RFC 8414 OAuth 2.0 Authorization Server Metadata discovery
//! from MCP server URLs, enabling `abox grant mcp <server-url>` to
//! automatically discover OAuth endpoints and perform the authorization flow.
//!
//! # Discovery Flow
//!
//! 1. Fetch `<server-url>/.well-known/oauth-authorization-server`
//! 2. Parse the metadata to find `authorization_endpoint` and `token_endpoint`
//! 3. Perform the authorization code flow with PKCE
//! 4. Store the resulting token for use by the egress proxy
//!
//! # Token Storage
//!
//! Tokens are stored as JSON files under `~/.abox/mcp-tokens/<name>.json`.
//! The egress proxy reads these files to inject the `Authorization` header.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// OAuth 2.0 Authorization Server Metadata (RFC 8414).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthServerMetadata {
    /// The authorization endpoint URL.
    pub authorization_endpoint: String,
    /// The token endpoint URL.
    pub token_endpoint: String,
    /// Optional Dynamic Client Registration endpoint.
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    /// Supported response types.
    #[serde(default)]
    pub response_types_supported: Vec<String>,
    /// Supported grant types.
    #[serde(default)]
    pub grant_types_supported: Vec<String>,
    /// Supported scopes.
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    /// PKCE code challenge methods supported.
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

/// A stored MCP OAuth token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToken {
    /// The provider/service name.
    pub name: String,
    /// The MCP server URL this token is for.
    pub server_url: String,
    /// The OAuth access token.
    pub access_token: String,
    /// The token type (usually "Bearer").
    #[serde(default = "default_token_type")]
    pub token_type: String,
    /// Optional refresh token.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Optional expiry timestamp (Unix seconds).
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// Granted scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// When this token was stored (ISO 8601).
    pub stored_at: String,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

impl McpToken {
    /// Check if this token has expired.
    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs().cast_signed());
            now >= exp
        } else {
            false
        }
    }

    /// Format the Authorization header value for this token.
    pub fn authorization_header(&self) -> String {
        format!("{} {}", self.token_type, self.access_token)
    }
}

/// Directory where MCP tokens are stored.
pub fn tokens_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("mcp-tokens")
}

/// Path for a specific MCP token.
pub fn token_path(state_dir: &Path, name: &str) -> PathBuf {
    tokens_dir(state_dir).join(format!("{name}.json"))
}

/// Save an MCP token to disk.
pub fn save_token(state_dir: &Path, token: &McpToken) -> Result<PathBuf> {
    let dir = tokens_dir(state_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("Creating {}", dir.display()))?;
    let path = token_path(state_dir, &token.name);
    let json = serde_json::to_string_pretty(token).context("Serializing token")?;
    std::fs::write(&path, json).with_context(|| format!("Writing token to {}", path.display()))?;
    Ok(path)
}

/// Load an MCP token from disk.
pub fn load_token(state_dir: &Path, name: &str) -> Result<Option<McpToken>> {
    let path = token_path(state_dir, name);
    if !path.exists() {
        return Ok(None);
    }
    let json =
        std::fs::read_to_string(&path).with_context(|| format!("Reading {}", path.display()))?;
    let token: McpToken =
        serde_json::from_str(&json).with_context(|| format!("Parsing {}", path.display()))?;
    Ok(Some(token))
}

/// List all stored MCP tokens.
pub fn list_tokens(state_dir: &Path) -> Result<Vec<McpToken>> {
    let dir = tokens_dir(state_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut tokens = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(json) = std::fs::read_to_string(&path) {
            if let Ok(token) = serde_json::from_str::<McpToken>(&json) {
                tokens.push(token);
            }
        }
    }
    tokens.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tokens)
}

/// Delete a stored MCP token.
pub fn delete_token(state_dir: &Path, name: &str) -> Result<bool> {
    let path = token_path(state_dir, name);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path).with_context(|| format!("Removing {}", path.display()))?;
    Ok(true)
}

/// Discover OAuth metadata from an MCP server URL.
///
/// Tries the standard RFC 8414 discovery endpoint:
/// `<server-url>/.well-known/oauth-authorization-server`
///
/// Returns `None` if the server does not support OAuth discovery.
pub async fn discover_oauth_metadata(server_url: &str) -> Result<Option<OAuthServerMetadata>> {
    let base = server_url.trim_end_matches('/');
    let discovery_url = format!("{base}/.well-known/oauth-authorization-server");

    let client = build_http_client()?;
    let response = client
        .get(&discovery_url)
        .header("Accept", "application/json")
        .send()
        .await
        .with_context(|| format!("Fetching OAuth discovery from {discovery_url}"))?;

    if response.status() == 404 {
        return Ok(None);
    }

    if !response.status().is_success() {
        return Ok(None);
    }

    let metadata: OAuthServerMetadata = response
        .json()
        .await
        .with_context(|| format!("Parsing OAuth metadata from {discovery_url}"))?;

    Ok(Some(metadata))
}

/// Build a reqwest HTTP client for OAuth flows.
fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("abox/0.1 (MCP OAuth client)")
        .build()
        .context("Building HTTP client")
}

/// Generate a PKCE code verifier and challenge.
///
/// Uses `/dev/urandom` on Unix for cryptographically secure randomness.
pub fn generate_pkce() -> (String, String) {
    use sha2::{Digest, Sha256};

    // Generate 32 cryptographically random bytes for the verifier.
    // We read from /dev/urandom directly to avoid adding a rand crate dependency.
    let verifier_bytes = read_random_bytes(32);
    let verifier = base64_url_encode(&verifier_bytes);

    // Challenge = BASE64URL(SHA256(verifier))
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64_url_encode(&hasher.finalize());

    (verifier, challenge)
}

/// Read `n` cryptographically random bytes from the OS.
fn read_random_bytes(n: usize) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut buf = vec![0u8; n];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            if f.read_exact(&mut buf).is_ok() {
                return buf;
            }
        }
        // Fallback: use time-based seed (less secure but functional)
        time_based_random(n)
    }
    #[cfg(not(unix))]
    {
        time_based_random(n)
    }
}

/// Fallback random byte generation using time-based entropy.
/// Only used when /dev/urandom is unavailable.
fn time_based_random(n: usize) -> Vec<u8> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(42, |d| d.subsec_nanos());
    // Simple LCG: multiply by a prime and add the index for variation
    (0..n)
        .map(|i| {
            let mixed = seed
                .wrapping_mul(1_664_525_u32)
                .wrapping_add(1_013_904_223_u32)
                .wrapping_add(i as u32 * 6_971_u32);
            (mixed >> 8) as u8
        })
        .collect()
}

/// Base64-URL encode bytes (no padding, RFC 4648 §5).
fn base64_url_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut result = String::with_capacity((bytes.len() * 4).div_ceil(3));
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i] as usize;
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] as usize } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] as usize } else { 0 };
        result.push(CHARS[b0 >> 2] as char);
        result.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if i + 1 < bytes.len() {
            result.push(CHARS[((b1 & 0xF) << 2) | (b2 >> 6)] as char);
        }
        if i + 2 < bytes.len() {
            result.push(CHARS[b2 & 0x3F] as char);
        }
        i += 3;
    }
    result
}

/// Perform the OAuth authorization code flow with PKCE.
///
/// This opens the browser for the user to authorize, then exchanges the
/// code for a token using a local redirect server.
pub async fn run_auth_flow(
    metadata: &OAuthServerMetadata,
    client_id: &str,
    scopes: &[String],
    server_url: &str,
) -> Result<(String, Option<String>, Option<i64>, Vec<String>)> {
    let (verifier, challenge) = generate_pkce();

    // Start a local redirect server
    let redirect_port = find_free_port().await?;
    let redirect_uri = format!("http://127.0.0.1:{redirect_port}/callback");

    // Build the authorization URL
    let scope_str = if scopes.is_empty() {
        String::new()
    } else {
        format!("&scope={}", urlencoded(scopes.join(" ").as_str()))
    };

    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256{}",
        metadata.authorization_endpoint,
        urlencoded(client_id),
        urlencoded(&redirect_uri),
        challenge,
        scope_str,
    );

    println!();
    println!("Opening browser for authorization...");
    println!("If the browser does not open, visit this URL manually:");
    println!();
    println!("  {auth_url}");
    println!();

    // Try to open the browser
    let _ = open_browser(&auth_url);

    // Wait for the callback
    println!("Waiting for authorization callback on port {redirect_port}...");
    let code = wait_for_callback(redirect_port).await?;

    println!("Authorization code received. Exchanging for token...");

    // Exchange code for token
    let client = build_http_client()?;
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client_id),
        ("code_verifier", verifier.as_str()),
    ];

    let scope_param;
    if !scopes.is_empty() {
        scope_param = scopes.join(" ");
        params.push(("scope", scope_param.as_str()));
    }

    let response = client
        .post(&metadata.token_endpoint)
        .form(&params)
        .send()
        .await
        .context("Token exchange request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed ({status}): {body}");
    }

    let token_response: serde_json::Value =
        response.json().await.context("Parsing token response")?;

    let access_token = token_response["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No access_token in response"))?
        .to_string();

    let refresh_token = token_response["refresh_token"].as_str().map(str::to_string);

    let expires_at = token_response["expires_in"].as_i64().map(|secs| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs().cast_signed());
        now + secs
    });

    let granted_scopes: Vec<String> = token_response["scope"]
        .as_str()
        .map_or_else(|| scopes.to_vec(), |s| s.split_whitespace().map(str::to_string).collect());

    let _ = server_url; // used for context in error messages
    Ok((access_token, refresh_token, expires_at, granted_scopes))
}

/// Find a free TCP port for the OAuth redirect server.
async fn find_free_port() -> Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Wait for the OAuth callback on the given port and return the authorization code.
async fn wait_for_callback(port: u16) -> Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let (mut stream, _) =
        tokio::time::timeout(std::time::Duration::from_mins(2), listener.accept())
            .await
            .context("Timed out waiting for OAuth callback (120s)")?
            .context("Failed to accept connection")?;

    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;

    // Parse the code from the request line: "GET /callback?code=xxx HTTP/1.1"
    let code = request_line
        .split_whitespace()
        .nth(1)
        .and_then(|path| {
            path.split('?').nth(1).and_then(|query| {
                query
                    .split('&')
                    .find(|p| p.starts_with("code="))
                    .map(|p| p.strip_prefix("code=").unwrap_or("").to_string())
            })
        })
        .ok_or_else(|| anyhow::anyhow!("No authorization code in callback URL: {request_line}"))?;

    // Send a success response
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body><h1>Authorization successful!</h1>\
        <p>You can close this window and return to the terminal.</p>\
        </body></html>";
    writer.write_all(response.as_bytes()).await?;

    Ok(code)
}

/// Open a URL in the default browser.
fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = url;
    }
    Ok(())
}

/// Percent-encode a string for use in a URL query parameter (RFC 3986).
fn urlencoded(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(c),
            ' ' => result.push('+'),
            c => {
                for byte in c.to_string().as_bytes() {
                    let _ = write!(result, "%{byte:02X}");
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_url_encode() {
        // Empty input
        assert_eq!(base64_url_encode(b""), "");
        // "Man" -> "TWFu" (standard base64 test vector)
        assert_eq!(base64_url_encode(b"Man"), "TWFu");
    }

    #[test]
    fn test_urlencoded() {
        assert_eq!(urlencoded("hello world"), "hello+world");
        assert_eq!(urlencoded("a=b&c=d"), "a%3Db%26c%3Dd");
        assert_eq!(urlencoded("simple"), "simple");
    }

    #[test]
    fn test_mcp_token_is_expired() {
        let expired_token = McpToken {
            name: "test".into(),
            server_url: "https://example.com".into(),
            access_token: "token".into(),
            token_type: "Bearer".into(),
            refresh_token: None,
            expires_at: Some(1), // Unix epoch + 1 second = definitely expired
            scopes: vec![],
            stored_at: "2024-01-01T00:00:00Z".into(),
        };
        assert!(expired_token.is_expired());

        let valid_token = McpToken {
            name: "test".into(),
            server_url: "https://example.com".into(),
            access_token: "token".into(),
            token_type: "Bearer".into(),
            refresh_token: None,
            expires_at: Some(i64::MAX), // Far future
            scopes: vec![],
            stored_at: "2024-01-01T00:00:00Z".into(),
        };
        assert!(!valid_token.is_expired());
    }

    #[test]
    fn test_token_save_load_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();

        let token = McpToken {
            name: "test-service".into(),
            server_url: "https://mcp.example.com".into(),
            access_token: "test-access-token".into(),
            token_type: "Bearer".into(),
            refresh_token: Some("test-refresh".into()),
            expires_at: None,
            scopes: vec!["read".into()],
            stored_at: "2024-01-01T00:00:00Z".into(),
        };

        let path = save_token(state_dir, &token).unwrap();
        assert!(path.exists());

        let loaded = load_token(state_dir, "test-service").unwrap().unwrap();
        assert_eq!(loaded.access_token, "test-access-token");
        assert_eq!(loaded.refresh_token, Some("test-refresh".into()));

        let deleted = delete_token(state_dir, "test-service").unwrap();
        assert!(deleted);
        assert!(!path.exists());

        let not_found = load_token(state_dir, "test-service").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_generate_pkce_produces_valid_base64url() {
        let (verifier, challenge) = generate_pkce();
        // Verifier should be non-empty and only contain base64url chars
        assert!(!verifier.is_empty());
        assert!(verifier.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'));
        // Challenge should also be valid base64url
        assert!(!challenge.is_empty());
        assert!(challenge.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'));
        // Verifier and challenge should differ
        assert_ne!(verifier, challenge);
    }
}
