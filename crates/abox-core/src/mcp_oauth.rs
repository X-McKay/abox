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
    /// OAuth token endpoint, retained so the token can be refreshed later.
    #[serde(default)]
    pub token_endpoint: Option<String>,
    /// OAuth client ID used for this grant, retained for refresh.
    #[serde(default)]
    pub client_id: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

/// The result of a successful token exchange or refresh.
#[derive(Debug, Clone)]
pub struct TokenResponse {
    /// The OAuth access token.
    pub access_token: String,
    /// Optional refresh token, if the server issued one.
    pub refresh_token: Option<String>,
    /// Absolute expiry as Unix seconds, derived from `expires_in`.
    pub expires_at: Option<i64>,
    /// Scopes actually granted by the server.
    pub scopes: Vec<String>,
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

/// Validate a token name before using it as a filename.
///
/// Tokens are stored at `<state_dir>/mcp-tokens/<name>.json`; an unsanitized
/// name (`../../foo`, `a/b`) would let the file escape the tokens directory.
fn validate_token_name(name: &str) -> Result<()> {
    crate::util::validate_resource_name(name)
        .map_err(|e| anyhow::anyhow!("Invalid MCP token name: {e}"))
}

/// Path for a specific MCP token. Validates `name` against path traversal.
pub fn token_path(state_dir: &Path, name: &str) -> Result<PathBuf> {
    validate_token_name(name)?;
    Ok(tokens_dir(state_dir).join(format!("{name}.json")))
}

/// Save an MCP token to disk with owner-only permissions (0600 file, 0700 dir).
pub fn save_token(state_dir: &Path, token: &McpToken) -> Result<PathBuf> {
    let dir = tokens_dir(state_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("Creating {}", dir.display()))?;
    restrict_dir_permissions(&dir);
    let path = token_path(state_dir, &token.name)?;
    let json = serde_json::to_string_pretty(token).context("Serializing token")?;
    write_secret_file(&path, json.as_bytes())
        .with_context(|| format!("Writing token to {}", path.display()))?;
    Ok(path)
}

/// Write a file containing secret material with mode 0600 (owner read/write).
///
/// Creates the file with restrictive permissions from the start (via
/// `OpenOptions::mode`) so the secret is never briefly world-readable.
fn write_secret_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // Tighten permissions even if the file already existed with looser mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    f.write_all(contents)?;
    f.flush()
}

/// Best-effort restriction of a directory to mode 0700 (owner-only).
fn restrict_dir_permissions(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// Load an MCP token from disk.
pub fn load_token(state_dir: &Path, name: &str) -> Result<Option<McpToken>> {
    let path = token_path(state_dir, name)?;
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
    let path = token_path(state_dir, name)?;
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

    // 404 means "no OAuth discovery here" — a normal, expected answer.
    if response.status() == 404 {
        return Ok(None);
    }

    // Any other non-success status is an *error*, not "no OAuth": surface it so
    // the user can distinguish a misconfigured/erroring server (500, 401, …)
    // from one that genuinely does not advertise OAuth.
    if !response.status().is_success() {
        anyhow::bail!(
            "OAuth discovery at {discovery_url} returned HTTP {} (expected 200 or 404)",
            response.status()
        );
    }

    let metadata: OAuthServerMetadata = response
        .json()
        .await
        .with_context(|| format!("Parsing OAuth metadata from {discovery_url}"))?;

    // Security: the authorization and token endpoints carry the auth code and
    // PKCE verifier. A malicious or misconfigured discovery document must not
    // be able to redirect those to a plaintext (or attacker-controlled http://)
    // endpoint. Require HTTPS for both. Loopback http:// is allowed only for
    // local testing against 127.0.0.1/localhost.
    require_secure_endpoint("authorization_endpoint", &metadata.authorization_endpoint)?;
    require_secure_endpoint("token_endpoint", &metadata.token_endpoint)?;

    Ok(Some(metadata))
}

/// Require that a discovered OAuth endpoint uses HTTPS (or loopback http for
/// local testing). Rejects anything else to keep secrets off the wire.
fn require_secure_endpoint(field: &str, url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    let is_loopback_http = url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost")
        || url.starts_with("http://[::1]");
    if is_loopback_http {
        return Ok(());
    }
    anyhow::bail!(
        "Discovered {field} {url:?} is not HTTPS. Refusing to send the authorization code / PKCE \
         verifier over an insecure channel."
    )
}

/// Build a reqwest HTTP client for OAuth flows.
fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("abox/0.1 (MCP OAuth client)")
        .build()
        .context("Building HTTP client")
}

/// Generate a PKCE code verifier and challenge (RFC 7636).
///
/// Uses the OS CSPRNG via [`crate::util::secure_random_bytes`]. Returns an
/// error if the OS entropy source is unavailable — we never fall back to a
/// weak/guessable verifier, since that would silently defeat PKCE's purpose.
pub fn generate_pkce() -> Result<(String, String)> {
    use sha2::{Digest, Sha256};

    // 32 cryptographically random bytes for the verifier.
    let mut verifier_bytes = [0u8; 32];
    crate::util::secure_random_bytes(&mut verifier_bytes)
        .map_err(|e| anyhow::anyhow!("Cannot generate PKCE verifier: {e}"))?;
    let verifier = base64_url_encode(&verifier_bytes);

    // Challenge = BASE64URL(SHA256(verifier))
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64_url_encode(&hasher.finalize());

    Ok((verifier, challenge))
}

/// Generate a random OAuth `state` value for CSRF protection.
///
/// Returns an error if the OS CSPRNG is unavailable.
fn generate_state() -> Result<String> {
    let mut bytes = [0u8; 16];
    crate::util::secure_random_bytes(&mut bytes)
        .map_err(|e| anyhow::anyhow!("Cannot generate OAuth state: {e}"))?;
    Ok(base64_url_encode(&bytes))
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
) -> Result<TokenResponse> {
    let (verifier, challenge) = generate_pkce()?;
    let state = generate_state()?;

    // Bind the loopback redirect listener up front and keep it bound for the
    // whole flow. Binding once (rather than bind→drop→rebind) closes a TOCTOU
    // window where another process could steal the port in between.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("Binding local OAuth redirect listener")?;
    let redirect_port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{redirect_port}/callback");

    // Build the authorization URL, including the CSRF `state`.
    let scope_str = if scopes.is_empty() {
        String::new()
    } else {
        format!("&scope={}", urlencoded(scopes.join(" ").as_str()))
    };

    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256{}",
        metadata.authorization_endpoint,
        urlencoded(client_id),
        urlencoded(&redirect_uri),
        urlencoded(&state),
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

    // Wait for the callback, validating the returned `state`.
    println!("Waiting for authorization callback on port {redirect_port}...");
    let code = wait_for_callback(listener, &state).await?;

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

    let _ = server_url; // reserved for richer error context
    exchange_token(&client, &metadata.token_endpoint, &params, scopes).await
}

/// Exchange OAuth form parameters at the token endpoint and parse the response.
///
/// Shared by the initial authorization-code exchange and refresh-token flow.
async fn exchange_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    params: &[(&str, &str)],
    requested_scopes: &[String],
) -> Result<TokenResponse> {
    let response =
        client.post(token_endpoint).form(params).send().await.context("Token request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Token request failed ({status}): {body}");
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

    let scopes: Vec<String> = token_response["scope"].as_str().map_or_else(
        || requested_scopes.to_vec(),
        |s| s.split_whitespace().map(str::to_string).collect(),
    );

    Ok(TokenResponse { access_token, refresh_token, expires_at, scopes })
}

/// Refresh an access token using a stored refresh token (RFC 6749 §6).
///
/// Returns a fresh [`TokenResponse`]. If the server omits a new refresh token,
/// the caller should retain the previous one.
pub async fn refresh_access_token(
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
    scopes: &[String],
) -> Result<TokenResponse> {
    require_secure_endpoint("token_endpoint", token_endpoint)?;
    let client = build_http_client()?;
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    let scope_param;
    if !scopes.is_empty() {
        scope_param = scopes.join(" ");
        params.push(("scope", scope_param.as_str()));
    }
    exchange_token(&client, token_endpoint, &params, scopes).await
}

/// Wait for the OAuth callback on an already-bound listener.
///
/// Validates the `state` parameter against `expected_state` (CSRF protection),
/// surfaces an `error=` response from the IdP, and percent-decodes the code.
async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (mut stream, _) =
        tokio::time::timeout(std::time::Duration::from_mins(2), listener.accept())
            .await
            .context("Timed out waiting for OAuth callback (120s)")?
            .context("Failed to accept connection")?;

    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;

    // Extract the raw query string from "GET /callback?<query> HTTP/1.1".
    let query = request_line
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split('?').nth(1))
        .unwrap_or("")
        .to_string();
    let params = parse_query(&query);

    // Always answer the browser so the user gets feedback, regardless of
    // whether the callback was valid.
    let (status_line, body): (&str, &str) = if params.contains_key("code") {
        ("200 OK", "<h1>Authorization successful!</h1><p>You can close this window.</p>")
    } else {
        ("400 Bad Request", "<h1>Authorization failed.</h1><p>Return to the terminal.</p>")
    };
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
         <html><body>{body}</body></html>"
    );
    let _ = writer.write_all(response.as_bytes()).await;

    // Surface an explicit error response from the authorization server.
    if let Some(err) = params.get("error") {
        let desc = params.get("error_description").map_or("", String::as_str);
        anyhow::bail!("Authorization server returned error '{err}': {desc}");
    }

    // CSRF protection: the returned state must match what we sent.
    match params.get("state") {
        Some(got) if got == expected_state => {}
        Some(_) => anyhow::bail!("OAuth state mismatch — possible CSRF; aborting"),
        None => anyhow::bail!("OAuth callback missing required 'state' parameter; aborting"),
    }

    let code = params
        .get("code")
        .filter(|c| !c.is_empty())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No authorization code in callback"))?;

    Ok(code)
}

/// Parse a URL query string into a map, percent-decoding keys and values.
///
/// Treats `+` as a space (form-encoding) and decodes `%XX` escapes.
fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

/// Percent-decode a URL component (`%XX` escapes, `+` → space).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
        // Spaces are encoded as %20 (unambiguous in both path and query).
        assert_eq!(urlencoded("hello world"), "hello%20world");
        assert_eq!(urlencoded("a=b&c=d"), "a%3Db%26c%3Dd");
        assert_eq!(urlencoded("simple"), "simple");
    }

    #[test]
    fn test_percent_decode_roundtrip() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%2Fpath%2Fx"), "/path/x");
        // Malformed escape is passed through literally rather than panicking.
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn test_parse_query_extracts_params() {
        let q = parse_query("code=abc%20123&state=xyz&error=");
        assert_eq!(q.get("code").map(String::as_str), Some("abc 123"));
        assert_eq!(q.get("state").map(String::as_str), Some("xyz"));
        assert_eq!(q.get("error").map(String::as_str), Some(""));
    }

    #[test]
    fn test_generate_state_is_random_and_urlsafe() {
        let a = generate_state().unwrap();
        let b = generate_state().unwrap();
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn test_require_secure_endpoint() {
        assert!(require_secure_endpoint("token_endpoint", "https://idp.example.com/token").is_ok());
        assert!(require_secure_endpoint("token_endpoint", "http://127.0.0.1:9000/token").is_ok());
        assert!(require_secure_endpoint("token_endpoint", "http://evil.example.com/token").is_err());
    }

    #[test]
    fn test_token_path_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(token_path(tmp.path(), "../escape").is_err());
        assert!(token_path(tmp.path(), "a/b").is_err());
        assert!(token_path(tmp.path(), "valid-name").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_saved_token_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let token = McpToken {
            name: "perms".into(),
            server_url: "https://mcp.example.com".into(),
            access_token: "secret".into(),
            token_type: "Bearer".into(),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
            stored_at: "2024-01-01T00:00:00Z".into(),
            token_endpoint: None,
            client_id: None,
        };
        let path = save_token(tmp.path(), &token).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "token file must be owner-only");
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
            token_endpoint: None,
            client_id: None,
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
            token_endpoint: None,
            client_id: None,
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
            token_endpoint: None,
            client_id: None,
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
        let (verifier, challenge) = generate_pkce().unwrap();
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
