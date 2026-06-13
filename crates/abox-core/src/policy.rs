//! Policy engine for credential proxy authorization.
//!
//! Evaluates whether a CLI command or HTTP request from a sandbox should be
//! allowed, denied, or have credentials injected. Policies are defined in
//! TOML files under the `policies/` directory.

use crate::project::{bundle_hosts, NetworkMode, NetworkScope};
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json;
use std::path::Path;

/// A policy decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Allow the request, optionally with credential injection.
    Allow,
    /// Deny the request with a reason.
    Deny(String),
}

/// A CLI proxy policy (e.g., for `git`, `aws`, `gh`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliPolicy {
    /// The command name this policy applies to (e.g., "git").
    pub command: String,

    /// Allowed subcommands/argument patterns (regexes).
    /// If empty, all subcommands are allowed.
    #[serde(default)]
    pub allow: Vec<String>,

    /// Denied argument patterns (regexes). Evaluated before allow.
    #[serde(default)]
    pub deny: Vec<String>,

    /// Whether to pass through the host's SSH agent for this command.
    #[serde(default)]
    pub forward_ssh_agent: bool,
}

/// A per-request rule for an egress domain.
///
/// Format: `"<allow|deny> <METHOD|*> <path-pattern>"`
///
/// Examples:
///   `"allow GET /repos/**"`
///   `"deny * /admin/**"`
///   `"allow POST /v1/messages"`
///
/// Rules are evaluated top-to-bottom; the first match wins.
/// `*` in the method position matches any HTTP method.
/// `*` in the path matches a single path segment.
/// `**` in the path matches zero or more path segments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EgressRequestRule {
    /// "allow" or "deny".
    pub action: String,
    /// HTTP method or "*" for any.
    pub method: String,
    /// Path pattern (supports `*` and `**` wildcards).
    pub path_pattern: String,
}

impl EgressRequestRule {
    /// Parse a rule string like `"allow GET /repos/**"`.
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.splitn(3, ' ').collect();
        if parts.len() != 3 {
            anyhow::bail!(
                "Invalid egress request rule {s:?}: expected \"<allow|deny> <METHOD|*> <path>\""
            );
        }
        let action = parts[0].to_lowercase();
        if action != "allow" && action != "deny" {
            anyhow::bail!(
                "Invalid egress request rule {s:?}: action must be 'allow' or 'deny', got {action:?}"
            );
        }
        Ok(Self { action, method: parts[1].to_uppercase(), path_pattern: parts[2].to_string() })
    }

    /// Check if this rule matches the given method and path.
    pub fn matches(&self, method: &str, path: &str) -> bool {
        let method_matches = self.method == "*" || self.method == method.to_uppercase();
        if !method_matches {
            return false;
        }
        path_matches(&self.path_pattern, path)
    }
}

/// An HTTP egress proxy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressRule {
    /// Domain pattern (e.g., "api.anthropic.com", "*.amazonaws.com").
    pub domain: String,

    /// Header name to inject (e.g., "x-api-key", "Authorization").
    pub inject_header: String,

    /// Environment variable on the host that contains the secret value.
    #[serde(default)]
    pub env_var: Option<String>,

    /// Path to a JSON file containing credentials (tilde-expanded).
    #[serde(default)]
    pub credential_file: Option<String>,

    /// JSON path (dot-separated) to the value within `credential_file`.
    #[serde(default)]
    pub json_path: Option<String>,

    /// Optional header value template. `{value}` is replaced with the credential value.
    /// Default: just the raw value.
    #[serde(default = "default_header_template")]
    pub header_template: String,

    /// Optional per-request rules that filter by HTTP method and path.
    ///
    /// Rules are evaluated top-to-bottom; the first match wins. If **no** rule
    /// matches, the request is allowed (the domain-level match already passed).
    /// To get allowlist semantics — permit a few paths, deny the rest — end the
    /// list with an explicit catch-all `"deny * /**"`.
    ///
    /// Enforcement only happens for domains whose TLS the proxy terminates. A
    /// domain listed in `bypass_tls` is tunneled opaquely and cannot have its
    /// method/path inspected; defining `request_rules` on such a domain is
    /// rejected at policy-load time rather than silently ignored.
    ///
    /// Each rule is a string in the format:
    ///   `"<allow|deny> <METHOD|*> <path-pattern>"`
    ///
    /// Example:
    /// ```toml
    /// [[egress]]
    /// domain = "api.github.com"
    /// inject_header = "Authorization"
    /// env_var = "GITHUB_TOKEN"
    /// header_template = "Bearer {value}"
    /// request_rules = [
    ///   "allow GET /repos/**",
    ///   "deny * /**",
    /// ]
    /// ```
    #[serde(default)]
    pub request_rules: Vec<String>,
}

impl EgressRule {
    /// Evaluate per-request rules against a method and path.
    ///
    /// Returns `Some(true)` if a rule allows the request,
    /// `Some(false)` if a rule denies it, or `None` if no rule matches.
    pub fn evaluate_request_rules(&self, method: &str, path: &str) -> Option<bool> {
        for rule_str in &self.request_rules {
            let rule = match EgressRequestRule::parse(rule_str) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(rule = rule_str, error = %e, "Skipping invalid request rule");
                    continue;
                }
            };
            if rule.matches(method, path) {
                return Some(rule.action == "allow");
            }
        }
        None
    }

    /// Resolve the credential value for this rule.
    ///
    /// - If `env_var` is Some, reads from the environment.
    /// - Else if `credential_file` + `json_path` are Some, reads the JSON file
    ///   and extracts the value at the given dot-separated path.
    /// - Returns `None` if neither is configured or the value cannot be read.
    pub fn resolve_credential(&self) -> Option<String> {
        if let Some(ref var) = self.env_var {
            return std::env::var(var).ok();
        }
        if let Some(ref file_path) = self.credential_file {
            let expanded = expand_tilde(file_path);
            let content = std::fs::read_to_string(&expanded).ok()?;
            let json: serde_json::Value = serde_json::from_str(&content).ok()?;
            if let Some(ref path) = self.json_path {
                return extract_json_path(&json, path);
            }
        }
        None
    }
}

/// Expand a leading `~` in a path to the current user's home directory.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}/{rest}", home.display());
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.display().to_string();
        }
    }
    path.to_string()
}

/// Extract a value from a `serde_json::Value` at a dot-separated path.
fn extract_json_path(json: &serde_json::Value, path: &str) -> Option<String> {
    let mut current = json;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    match current {
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Top-level policy configuration file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyFile {
    #[serde(default)]
    pub cli: Vec<CliPolicy>,

    #[serde(default)]
    pub egress: Vec<EgressRule>,

    /// Default action for CLI commands not matching any policy.
    /// "allow" or "deny". Default: "deny".
    #[serde(default = "default_action")]
    pub default_cli_action: String,

    /// Default action for HTTP requests not matching any egress rule.
    /// "allow" or "deny". Default: "deny".
    #[serde(default = "default_action")]
    pub default_egress_action: String,

    /// Domains that should bypass TLS termination (passthrough).
    /// Useful for cert-pinned clients that would reject MITM certs.
    /// Supports exact match and wildcard prefix (e.g., "*.pinned.io").
    #[serde(default)]
    pub bypass_tls: Vec<String>,
}

fn default_header_template() -> String {
    "{value}".to_string()
}

fn default_action() -> String {
    "deny".to_string()
}

/// The policy engine. Loaded once and used to evaluate every request.
#[derive(Clone)]
pub struct PolicyEngine {
    cli_policies: Vec<CompiledCliPolicy>,
    egress_rules: Vec<EgressRule>,
    default_cli_action: String,
    default_egress_action: String,
    bypass_tls: Vec<String>,
    network_scope: Option<CompiledNetworkScope>,
}

#[derive(Clone)]
struct CompiledCliPolicy {
    command: String,
    allow_patterns: Vec<Regex>,
    deny_patterns: Vec<Regex>,
    forward_ssh_agent: bool,
}

#[derive(Debug, Clone)]
struct CompiledNetworkScope {
    mode: NetworkMode,
    allowed_domains: Vec<String>,
}

/// How the proxy should handle an allowed outbound request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressTransport {
    /// Terminate TLS and inspect or inject request headers.
    Mitm,
    /// Leave TLS end-to-end and only tunnel bytes.
    Passthrough,
}

/// Result of evaluating one outbound CONNECT request.
#[derive(Debug, Clone, Copy)]
pub struct EgressEvaluation<'a> {
    /// The matched host-managed rule, if any.
    pub rule: Option<&'a EgressRule>,
    /// The transport to apply to the request.
    pub transport: EgressTransport,
}

impl PolicyEngine {
    /// Load and compile policies from a TOML file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Reading {}", path.display()))?;
        let policy: PolicyFile =
            toml::from_str(&content).with_context(|| format!("Parsing {}", path.display()))?;
        Self::from_policy_file(policy)
    }

    /// Load from a `PolicyFile` struct (useful for testing).
    pub fn from_policy_file(policy: PolicyFile) -> Result<Self> {
        let mut cli_policies = Vec::new();

        for cp in &policy.cli {
            let allow_patterns: Vec<Regex> = cp
                .allow
                .iter()
                .map(|p| Regex::new(p).with_context(|| format!("Invalid allow regex: {p}")))
                .collect::<Result<_>>()?;

            let deny_patterns: Vec<Regex> = cp
                .deny
                .iter()
                .map(|p| Regex::new(p).with_context(|| format!("Invalid deny regex: {p}")))
                .collect::<Result<_>>()?;

            cli_policies.push(CompiledCliPolicy {
                command: cp.command.clone(),
                allow_patterns,
                deny_patterns,
                forward_ssh_agent: cp.forward_ssh_agent,
            });
        }

        // Validate egress rules: a rule that names a `credential_file` must
        // also provide a `json_path`, otherwise resolve_credential() falls
        // through silently and the proxy passes the request without the auth
        // header — leading to opaque guest-side 401s with no policy error.
        // Catching this misconfiguration at policy-load is much louder.
        for (idx, rule) in policy.egress.iter().enumerate() {
            if rule.credential_file.is_some() && rule.json_path.is_none() {
                anyhow::bail!(
                    "Egress rule #{idx} ({domain}) sets `credential_file` but is missing `json_path`. \
                     The proxy needs both to extract a token from the file. \
                     Either set `json_path = \"path.to.token\"` or remove `credential_file`.",
                    domain = rule.domain,
                );
            }

            // Validate per-request rule syntax up front. A malformed rule string
            // (e.g. a typo'd action `dney`) would otherwise be silently skipped
            // at request time, leaving the request unexpectedly *allowed*.
            for raw in &rule.request_rules {
                EgressRequestRule::parse(raw).with_context(|| {
                    format!("Egress rule #{idx} ({}) has an invalid request_rule", rule.domain)
                })?;
            }

            // Per-request rules are only enforced for domains the proxy actually
            // terminates (MITM). A TLS-bypassed domain is tunneled as opaque TCP,
            // so the proxy never sees the method/path and could not enforce these
            // rules — fail loudly rather than give a false sense of restriction.
            if !rule.request_rules.is_empty() {
                let bypassed = policy
                    .bypass_tls
                    .iter()
                    .any(|pattern| domain_matches(pattern, &rule.domain));
                if bypassed {
                    anyhow::bail!(
                        "Egress rule #{idx} ({domain}) defines `request_rules`, but the domain is \
                         also in `bypass_tls`. Bypassed domains are tunneled without inspection, so \
                         per-request rules cannot be enforced. Remove the domain from `bypass_tls` \
                         (so the proxy can terminate TLS) or drop the `request_rules`.",
                        domain = rule.domain,
                    );
                }
            }
        }

        Ok(Self {
            cli_policies,
            egress_rules: policy.egress,
            default_cli_action: policy.default_cli_action,
            default_egress_action: policy.default_egress_action,
            bypass_tls: policy.bypass_tls,
            network_scope: None,
        })
    }

    /// Return a cloned policy engine with repo-level network scope applied.
    pub fn with_network_scope(&self, scope: NetworkScope) -> Result<Self> {
        Ok(Self {
            cli_policies: self.cli_policies.clone(),
            egress_rules: self.egress_rules.clone(),
            default_cli_action: self.default_cli_action.clone(),
            default_egress_action: self.default_egress_action.clone(),
            bypass_tls: self.bypass_tls.clone(),
            network_scope: Some(CompiledNetworkScope::from_scope(scope)?),
        })
    }

    /// Evaluate a CLI command request.
    ///
    /// # Arguments
    /// * `command` - The binary name (e.g., `git`).
    /// * `args` - The full argument list (e.g., `["push", "origin", "main"]`).
    ///
    /// For `git` specifically, this strips a known set of global options
    /// (`-c key=val`, `-C path`, `--git-dir`, `--work-tree`, `--no-pager`,
    /// etc.) from the front of `args` before matching against the
    /// allow/deny regex list. Any other leading dash-prefixed token is
    /// treated as an **unknown** global option and the request is denied.
    ///
    /// Without this stripping, a request like
    /// `git -c color.ui=always status` would not match an allow pattern
    /// anchored on `^status`, because the joined string would begin with
    /// `-c color.ui=always`. The fix lets ordinary workflows through while
    /// keeping the deny list tight: an attacker cannot inject extra tokens
    /// before the subcommand without explicitly being on the allow-list.
    pub fn evaluate_cli(&self, command: &str, args: &[String]) -> Decision {
        let stripped: Vec<String> = match strip_global_options(command, args) {
            Ok(s) => s,
            Err(reason) => return Decision::Deny(reason),
        };
        let args_str = stripped.join(" ");

        // Find the matching policy
        let policy = self.cli_policies.iter().find(|p| p.command == command);

        let Some(policy) = policy else {
            // No policy for this command — use default
            return if self.default_cli_action == "allow" {
                Decision::Allow
            } else {
                Decision::Deny(format!("No policy for command '{command}'"))
            };
        };

        // Deny patterns are checked first (deny takes precedence)
        for pattern in &policy.deny_patterns {
            if pattern.is_match(&args_str) {
                return Decision::Deny(format!(
                    "Denied by pattern '{pattern}' for command '{command}'"
                ));
            }
        }

        // If there are allow patterns, at least one must match
        if !policy.allow_patterns.is_empty() {
            let allowed = policy.allow_patterns.iter().any(|p| p.is_match(&args_str));
            if !allowed {
                return Decision::Deny(format!(
                    "No allow pattern matched for '{command}' with args: {args_str}"
                ));
            }
        }

        Decision::Allow
    }

    /// Look up whether a given command's policy requests SSH agent
    /// forwarding.
    ///
    /// Returns `false` for unknown commands. Used by the proxy bridge to
    /// decide whether to pass `SSH_AUTH_SOCK` through to the child process
    /// (and to deliberately unset it for commands that did not opt in).
    pub fn forward_ssh_agent(&self, command: &str) -> bool {
        self.cli_policies.iter().find(|p| p.command == command).is_some_and(|p| p.forward_ssh_agent)
    }

    /// Check if a domain should bypass TLS termination (passthrough).
    ///
    /// Returns `true` for domains in the `bypass_tls` list, which means
    /// the proxy should use plain TCP passthrough instead of MITM.
    pub fn is_tls_bypassed(&self, domain: &str) -> bool {
        for pattern in &self.bypass_tls {
            if domain_matches(pattern, domain) {
                return true;
            }
        }
        false
    }

    /// Return the list of TLS bypass patterns (for passing to egress proxy).
    pub fn bypass_tls_patterns(&self) -> &[String] {
        &self.bypass_tls
    }

    /// Return the host-managed egress domain patterns from the host policy.
    pub fn managed_egress_domains(&self) -> Vec<String> {
        let mut domains =
            self.egress_rules.iter().map(|rule| rule.domain.clone()).collect::<Vec<_>>();
        domains.sort();
        domains.dedup();
        domains
    }

    /// Evaluate an HTTP egress request.
    ///
    /// # Arguments
    /// * `domain` - The target domain (e.g., "api.anthropic.com").
    ///
    /// Returns the matching egress rule if allowed, or a deny decision.
    pub fn evaluate_egress(&self, domain: &str) -> Result<Option<&EgressRule>, Decision> {
        self.evaluate_egress_request(domain, 443).map(|evaluation| evaluation.rule)
    }

    // NOTE: Per-request rule enforcement lives in the egress proxy
    // (`egress::handle_mitm_with_injection`), which has the live HTTP request
    // and returns a 403 directly. The single source of truth for *evaluating* a
    // rule set is `EgressRule::evaluate_request_rules`; there is intentionally
    // no second copy of that logic here.

    /// Evaluate a CONNECT request against host-managed rules and any active
    /// repo-level network scope.
    pub fn evaluate_egress_request(
        &self,
        domain: &str,
        port: u16,
    ) -> Result<EgressEvaluation<'_>, Decision> {
        for rule in &self.egress_rules {
            if domain_matches(&rule.domain, domain) {
                let transport = if self.is_tls_bypassed(domain) {
                    EgressTransport::Passthrough
                } else {
                    EgressTransport::Mitm
                };
                return Ok(EgressEvaluation { rule: Some(rule), transport });
            }
        }

        if let Some(scope) = &self.network_scope {
            return scope.evaluate(domain, port);
        }

        if self.default_egress_action == "allow" {
            let transport = if self.is_tls_bypassed(domain) {
                EgressTransport::Passthrough
            } else {
                EgressTransport::Mitm
            };
            Ok(EgressEvaluation { rule: None, transport })
        } else {
            Err(Decision::Deny(format!("No egress rule for domain '{domain}'")))
        }
    }
}

impl CompiledNetworkScope {
    fn from_scope(scope: NetworkScope) -> Result<Self> {
        let mut allowed_domains = scope.domains;
        for bundle in &scope.bundles {
            let Some(hosts) = bundle_hosts(bundle) else {
                anyhow::bail!("unknown network bundle {bundle:?}");
            };
            allowed_domains.extend(hosts.iter().map(|host| (*host).to_string()));
        }
        allowed_domains.sort();
        allowed_domains.dedup();

        Ok(Self { mode: scope.mode, allowed_domains })
    }

    fn evaluate(&self, domain: &str, port: u16) -> Result<EgressEvaluation<'static>, Decision> {
        if port != 443 {
            return Err(Decision::Deny(format!(
                "Repo network modes only allow proxy-mediated HTTPS CONNECT traffic on port 443 (got {port})"
            )));
        }

        match self.mode {
            NetworkMode::Safe => Err(Decision::Deny(format!(
                "Network mode 'safe' does not allow unmanaged egress to '{domain}'"
            ))),
            NetworkMode::Scoped => {
                if self.allowed_domains.iter().any(|allowed| allowed == domain) {
                    Ok(EgressEvaluation { rule: None, transport: EgressTransport::Passthrough })
                } else {
                    Err(Decision::Deny(format!(
                        "Domain '{domain}' is not allowed by scoped network config"
                    )))
                }
            }
            NetworkMode::Open => {
                Ok(EgressEvaluation { rule: None, transport: EgressTransport::Passthrough })
            }
        }
    }
}

/// Strip known global options from the front of a command's args so the
/// subcommand (and its own args) can be matched against the allow/deny
/// regex list. Returns an error `reason` if an unknown option-like token
/// appears before the subcommand — this keeps the allow-list tight.
///
/// Currently knows about git's global options. Non-git commands pass
/// through unchanged.
fn strip_global_options(command: &str, args: &[String]) -> Result<Vec<String>, String> {
    if command != "git" {
        return Ok(args.to_vec());
    }

    // Git's documented global options that take a separate value (two tokens).
    const TWO_TOKEN: &[&str] = &["-c", "-C", "--git-dir", "--work-tree", "--namespace"];
    // Git's documented global options that are flags (one token, no value).
    const ONE_TOKEN_FLAGS: &[&str] = &[
        "--no-pager",
        "-p",
        "--paginate",
        "--no-optional-locks",
        "--bare",
        "--no-replace-objects",
    ];
    // Long options that use `--flag=value` (one token).
    const ONE_TOKEN_EQ_PREFIX: &[&str] =
        &["--git-dir=", "--work-tree=", "--namespace=", "--super-prefix=", "--config-env="];

    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();

        // Subcommand reached (first token not starting with '-').
        if !tok.starts_with('-') {
            return Ok(args[i..].to_vec());
        }

        if TWO_TOKEN.contains(&tok) {
            if i + 1 >= args.len() {
                return Err(format!("git global option '{tok}' requires a value"));
            }
            i += 2;
            continue;
        }

        if ONE_TOKEN_FLAGS.contains(&tok) {
            i += 1;
            continue;
        }

        if ONE_TOKEN_EQ_PREFIX.iter().any(|p| tok.starts_with(p)) {
            i += 1;
            continue;
        }

        // Unknown global option: deny rather than silently strip. This is a
        // deliberate deny-by-default — future git versions that add new
        // global flags will need an explicit update here, which is the
        // correct place to apply scrutiny.
        return Err(format!("Unknown git global option '{tok}' not in allow-list"));
    }

    // No subcommand at all — treat as deny.
    Err("git invocation has no subcommand".to_string())
}

/// Match a URL path pattern against a path.
///
/// Pattern syntax:
/// - `*` matches exactly one path segment (no slashes)
/// - `**` matches zero or more path segments (including slashes)
/// - `/**` matches any path (including the root `/`)
/// - Literal characters match exactly
///
/// Both the pattern and the path are **normalized** before matching: empty
/// segments (from `//`), `.` segments, and trailing slashes are collapsed, and
/// `..` segments are resolved. This closes common denylist-bypass tricks such
/// as `//admin`, `/admin/`, and `/repos/../admin`, so a `deny`/`allow` rule
/// can't be evaded by trivially re-spelling the path.
pub(crate) fn path_matches(pattern: &str, path: &str) -> bool {
    // Fast path: identical strings always match.
    if pattern == path {
        return true;
    }
    let pat = normalize_path_segments(pattern);
    let p = normalize_path_segments(path);
    path_segs_match(&pat, &p)
}

/// Split a path into normalized, non-empty segments.
///
/// Drops empty (`//`) and `.` segments, and pops on `..`. Wildcard tokens
/// (`*`, `**`) are preserved as ordinary segments.
fn normalize_path_segments(s: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn path_segs_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.first() {
        // Pattern exhausted: match iff the path is also exhausted.
        None => path.is_empty(),
        // `**` matches zero or more segments.
        Some(&"**") => {
            if pat.len() == 1 {
                return true; // trailing `**` matches the remainder, including none
            }
            for i in 0..=path.len() {
                if path_segs_match(&pat[1..], &path[i..]) {
                    return true;
                }
            }
            false
        }
        // `*` matches exactly one segment.
        Some(&"*") => !path.is_empty() && path_segs_match(&pat[1..], &path[1..]),
        // Literal segment must match exactly.
        Some(p) => !path.is_empty() && *p == path[0] && path_segs_match(&pat[1..], &path[1..]),
    }
}

/// Match a domain pattern against a domain.
/// Supports exact match and wildcard prefix (e.g., "*.amazonaws.com").
///
/// The wildcard `*` matches exactly one or more DNS labels separated by dots.
/// Crucially, the match requires a **dot boundary** so that `*.amazonaws.com`
/// matches `s3.amazonaws.com` but NOT `evilamazonaws.com`.
pub(crate) fn domain_matches(pattern: &str, domain: &str) -> bool {
    if pattern == domain {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // Require a dot boundary: the domain must end with ".{suffix}" (not
        // just any suffix-string). This prevents "*.amazonaws.com" from
        // matching "evilamazonaws.com".
        let dot_suffix = format!(".{suffix}");
        return domain.ends_with(dot_suffix.as_str()) && domain.len() > dot_suffix.len();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy() -> PolicyFile {
        PolicyFile {
            cli: vec![
                CliPolicy {
                    command: "git".to_string(),
                    allow: vec![
                        r"^(status|log|diff|show|branch)".to_string(),
                        r"^push\s+origin\s+\S+$".to_string(),
                        r"^pull\s+".to_string(),
                        r"^fetch\s+".to_string(),
                        r"^clone\s+".to_string(),
                        r"^add\s+".to_string(),
                        r"^commit\s+".to_string(),
                        r"^checkout\s+".to_string(),
                    ],
                    deny: vec![
                        r"--force".to_string(),
                        r"-f\b".to_string(),
                        r"push\s+--delete".to_string(),
                    ],
                    forward_ssh_agent: true,
                },
                CliPolicy {
                    command: "aws".to_string(),
                    allow: vec![r"^s3\s+(ls|cp|sync)".to_string()],
                    deny: vec![r"^iam\s+".to_string(), r"^ec2\s+".to_string()],
                    forward_ssh_agent: false,
                },
            ],
            egress: vec![EgressRule {
                domain: "api.anthropic.com".to_string(),
                inject_header: "x-api-key".to_string(),
                env_var: Some("ANTHROPIC_API_KEY".to_string()),
                credential_file: None,
                json_path: None,
                header_template: "{value}".to_string(),
                request_rules: vec![],
            }],
            default_cli_action: "deny".to_string(),
            default_egress_action: "deny".to_string(),
            bypass_tls: vec![],
        }
    }

    #[test]
    fn test_git_push_allowed() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["push", "origin", "main"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_git_force_push_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["push", "--force", "origin", "main"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn test_unknown_command_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "rm",
            &["-rf", "/"].iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
        );
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn test_aws_s3_allowed() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "aws",
            &["s3", "ls", "s3://my-bucket"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_aws_iam_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "aws",
            &["iam", "list-users"].iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
        );
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn test_egress_anthropic_allowed() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let result = engine.evaluate_egress("api.anthropic.com");
        assert!(result.is_ok());
        let rule = result.unwrap().unwrap();
        assert_eq!(rule.inject_header, "x-api-key");
    }

    #[test]
    fn test_egress_unknown_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let result = engine.evaluate_egress("evil.example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_network_scope_safe_denies_unmanaged_egress() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let scoped = engine
            .with_network_scope(NetworkScope {
                mode: NetworkMode::Safe,
                bundles: Vec::new(),
                domains: Vec::new(),
            })
            .unwrap();

        let result = scoped.evaluate_egress_request("docs.rs", 443);
        assert!(matches!(result, Err(Decision::Deny(_))));
    }

    #[test]
    fn test_network_scope_scoped_allows_bundle_host_as_passthrough() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let scoped = engine
            .with_network_scope(NetworkScope {
                mode: NetworkMode::Scoped,
                bundles: vec!["pypi-public".into()],
                domains: vec!["docs.rs".into()],
            })
            .unwrap();

        let result = scoped.evaluate_egress_request("files.pythonhosted.org", 443).unwrap();
        assert!(result.rule.is_none());
        assert_eq!(result.transport, EgressTransport::Passthrough);

        let result = scoped.evaluate_egress_request("docs.rs", 443).unwrap();
        assert!(result.rule.is_none());
        assert_eq!(result.transport, EgressTransport::Passthrough);
    }

    #[test]
    fn test_network_scope_open_allows_unmanaged_443_only() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let scoped = engine
            .with_network_scope(NetworkScope {
                mode: NetworkMode::Open,
                bundles: Vec::new(),
                domains: Vec::new(),
            })
            .unwrap();

        let result = scoped.evaluate_egress_request("example.com", 443).unwrap();
        assert!(result.rule.is_none());
        assert_eq!(result.transport, EgressTransport::Passthrough);

        let result = scoped.evaluate_egress_request("example.com", 8443);
        assert!(matches!(result, Err(Decision::Deny(_))));
    }

    #[test]
    fn test_network_scope_preserves_managed_rules() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let scoped = engine
            .with_network_scope(NetworkScope {
                mode: NetworkMode::Safe,
                bundles: Vec::new(),
                domains: Vec::new(),
            })
            .unwrap();

        let result = scoped.evaluate_egress_request("api.anthropic.com", 443).unwrap();
        assert!(result.rule.is_some());
        assert_eq!(result.transport, EgressTransport::Mitm);
    }

    #[test]
    fn test_domain_wildcard() {
        assert!(domain_matches("*.amazonaws.com", "s3.amazonaws.com"));
        assert!(domain_matches("*.amazonaws.com", "sts.us-east-1.amazonaws.com"));
        assert!(!domain_matches("*.amazonaws.com", "amazonaws.com"));
        assert!(domain_matches("api.anthropic.com", "api.anthropic.com"));
        assert!(!domain_matches("api.anthropic.com", "evil.anthropic.com"));
        // Regression: dot-boundary check — suffix-only match must NOT pass.
        // Without the fix, "*.amazonaws.com" matched "evilamazonaws.com" because
        // the domain ends with the string "amazonaws.com" even without a dot.
        assert!(!domain_matches("*.amazonaws.com", "evilamazonaws.com"));
        assert!(!domain_matches("*.anthropic.com", "evilanthropiccom"));
        // Deeply nested subdomains should still match.
        assert!(domain_matches("*.amazonaws.com", "a.b.c.amazonaws.com"));
    }

    // ─── Global-option bypass tests (S1) ────────────────────────────────────

    #[test]
    fn test_git_force_push_via_dash_c_is_denied() {
        // Bypass attempt: prepend `-c key=val` global options so the joined
        // args_str begins with "-c ..." rather than "push", defeating the
        // ^push\s+--force regex and slipping past the deny list.
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["-c", "core.hooks=./evil", "push", "--force", "origin", "main"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert!(
            matches!(decision, Decision::Deny(_)),
            "git -c ... push --force should be denied, got {decision:?}"
        );
    }

    #[test]
    fn test_git_force_push_via_dash_big_c_is_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["-C", "/tmp/evil", "push", "--force", "origin", "main"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert!(
            matches!(decision, Decision::Deny(_)),
            "git -C <path> push --force should be denied, got {decision:?}"
        );
    }

    #[test]
    fn test_git_status_via_dash_c_is_still_allowed() {
        // Tight on denies, still permissive for ordinary workflows.
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["-c", "color.ui=always", "status"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_git_status_via_dash_big_c_is_still_allowed() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["-C", "/some/path", "status"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_git_unknown_global_option_denied() {
        // Document the assumption: unknown global options are rejected
        // rather than silently stripped. Future git versions that add
        // new globals need an explicit allow-list update.
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["--exec-path=/evil", "status"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert!(
            matches!(decision, Decision::Deny(_)),
            "unknown global opts should be denied, got {decision:?}"
        );
    }

    #[test]
    fn test_git_no_subcommand_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["-c", "color.ui=always"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn test_forward_ssh_agent_lookup() {
        // git policy has forward_ssh_agent=true; aws has false; unknown
        // commands return false (default-safe).
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        assert!(engine.forward_ssh_agent("git"));
        assert!(!engine.forward_ssh_agent("aws"));
        assert!(!engine.forward_ssh_agent("nonexistent"));
    }

    #[test]
    fn test_non_git_command_unaffected_by_global_strip() {
        // Global-option stripping is git-only; aws + others should
        // pass straight to the regex match.
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "aws",
            &["s3", "ls", "s3://my-bucket"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        );
        assert_eq!(decision, Decision::Allow);
    }

    // ─── TLS bypass tests ──────────────────────────────────────────────────

    #[test]
    fn test_tls_bypass_exact_match() {
        let mut policy = test_policy();
        policy.bypass_tls = vec!["pinned.example.com".to_string()];
        let engine = PolicyEngine::from_policy_file(policy).unwrap();
        assert!(engine.is_tls_bypassed("pinned.example.com"));
        assert!(!engine.is_tls_bypassed("other.example.com"));
    }

    #[test]
    fn test_tls_bypass_wildcard() {
        let mut policy = test_policy();
        policy.bypass_tls = vec!["*.pinned.io".to_string()];
        let engine = PolicyEngine::from_policy_file(policy).unwrap();
        assert!(engine.is_tls_bypassed("api.pinned.io"));
        assert!(!engine.is_tls_bypassed("pinned.io"));
        assert!(!engine.is_tls_bypassed("evilpinned.io"));
    }

    #[test]
    fn test_tls_bypass_empty_list() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        assert!(!engine.is_tls_bypassed("anything.com"));
    }

    #[test]
    fn test_egress_rule_with_credential_file() {
        let toml_str = r#"
            default_cli_action = "deny"
            default_egress_action = "deny"

            [[egress]]
            domain = "api.anthropic.com"
            inject_header = "Authorization"
            credential_file = "~/.claude/.credentials.json"
            json_path = "claudeAiOauth.accessToken"
            header_template = "Bearer {value}"
        "#;
        let policy: PolicyFile = toml::from_str(toml_str).unwrap();
        let rule = &policy.egress[0];
        assert_eq!(rule.domain, "api.anthropic.com");
        assert_eq!(rule.inject_header, "Authorization");
        assert!(rule.env_var.is_none());
        assert_eq!(rule.credential_file.as_deref(), Some("~/.claude/.credentials.json"));
        assert_eq!(rule.json_path.as_deref(), Some("claudeAiOauth.accessToken"));
        assert_eq!(rule.header_template, "Bearer {value}");
    }

    #[test]
    fn test_path_matches_exact() {
        assert!(path_matches("/repos/owner/repo", "/repos/owner/repo"));
        assert!(!path_matches("/repos/owner/repo", "/repos/owner/other"));
    }

    #[test]
    fn test_path_matches_single_wildcard() {
        assert!(path_matches("/repos/*", "/repos/owner"));
        assert!(!path_matches("/repos/*", "/repos/owner/repo"));
    }

    #[test]
    fn test_path_matches_double_wildcard() {
        assert!(path_matches("/repos/**", "/repos/owner/repo"));
        assert!(path_matches("/repos/**", "/repos/owner/repo/branches"));
        assert!(path_matches("/repos/**", "/repos/owner"));
    }

    #[test]
    fn test_path_matches_double_wildcard_zero_segments() {
        // `/repos/**` must also match the prefix itself with no trailing
        // segments — `/repos` and `/repos/` (regression: `**` previously
        // failed to match zero segments).
        assert!(path_matches("/repos/**", "/repos"));
        assert!(path_matches("/repos/**", "/repos/"));
    }

    #[test]
    fn test_path_matches_normalizes_bypass_tricks() {
        // Denylist-bypass spellings must normalize to the same path so a
        // `deny`/`allow` rule cannot be evaded.
        assert!(path_matches("/admin", "//admin"));
        assert!(path_matches("/admin", "/admin/"));
        assert!(path_matches("/admin", "/admin/."));
        assert!(path_matches("/admin", "/foo/../admin"));
        // A normalized path that lands elsewhere must NOT match.
        assert!(!path_matches("/admin", "/foo/../public"));
    }

    #[test]
    fn test_path_matches_root_wildcard() {
        assert!(path_matches("/**", "/"));
        assert!(path_matches("/**", "/anything"));
        assert!(path_matches("/**", "/a/b/c"));
        assert!(path_matches("/**", ""));
    }

    #[test]
    fn test_load_rejects_invalid_request_rule_syntax() {
        let toml = r#"
            default_cli_action = "deny"
            default_egress_action = "deny"
            [[egress]]
            domain = "api.github.com"
            inject_header = "Authorization"
            env_var = "GITHUB_TOKEN"
            request_rules = ["dney GET /x"]
        "#;
        let pf: PolicyFile = toml::from_str(toml).unwrap();
        let err = PolicyEngine::from_policy_file(pf).err().expect("should reject invalid rule");
        assert!(format!("{err:#}").contains("invalid request_rule"));
    }

    #[test]
    fn test_load_rejects_request_rules_on_bypass_tls_domain() {
        let toml = r#"
            default_cli_action = "deny"
            default_egress_action = "deny"
            bypass_tls = ["api.github.com"]
            [[egress]]
            domain = "api.github.com"
            inject_header = "Authorization"
            env_var = "GITHUB_TOKEN"
            request_rules = ["allow GET /repos/**", "deny * /**"]
        "#;
        let pf: PolicyFile = toml::from_str(toml).unwrap();
        let err = PolicyEngine::from_policy_file(pf).err().expect("should reject bypass_tls rule");
        assert!(format!("{err:#}").contains("bypass_tls"));
    }

    #[test]
    fn test_egress_request_rule_parse() {
        let rule = EgressRequestRule::parse("allow GET /repos/**").unwrap();
        assert_eq!(rule.action, "allow");
        assert_eq!(rule.method, "GET");
        assert_eq!(rule.path_pattern, "/repos/**");

        let rule = EgressRequestRule::parse("deny * /**").unwrap();
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.method, "*");

        assert!(EgressRequestRule::parse("bad format").is_err());
        assert!(EgressRequestRule::parse("invalid GET /path").is_err());
    }

    #[test]
    fn test_egress_request_rule_matches() {
        let rule = EgressRequestRule::parse("allow GET /repos/**").unwrap();
        assert!(rule.matches("GET", "/repos/owner/repo"));
        assert!(!rule.matches("POST", "/repos/owner/repo"));
        assert!(!rule.matches("GET", "/users/me"));

        let wildcard_rule = EgressRequestRule::parse("deny * /**").unwrap();
        assert!(wildcard_rule.matches("GET", "/anything"));
        assert!(wildcard_rule.matches("DELETE", "/admin"));
    }

    #[test]
    fn test_evaluate_request_rules_allow_then_deny() {
        let rule = EgressRule {
            domain: "api.github.com".into(),
            inject_header: "Authorization".into(),
            env_var: None,
            credential_file: None,
            json_path: None,
            header_template: "{value}".into(),
            request_rules: vec!["allow GET /repos/**".into(), "deny * /**".into()],
        };
        assert_eq!(rule.evaluate_request_rules("GET", "/repos/owner/repo"), Some(true));
        assert_eq!(rule.evaluate_request_rules("POST", "/repos/owner/repo"), Some(false));
        assert_eq!(rule.evaluate_request_rules("DELETE", "/admin"), Some(false));
    }

    #[test]
    fn test_evaluate_request_rules_no_match_returns_none() {
        let rule = EgressRule {
            domain: "api.example.com".into(),
            inject_header: "Authorization".into(),
            env_var: None,
            credential_file: None,
            json_path: None,
            header_template: "{value}".into(),
            request_rules: vec!["allow GET /specific".into()],
        };
        // No rule matches POST /other
        assert_eq!(rule.evaluate_request_rules("POST", "/other"), None);
    }

    #[test]
    fn test_egress_rule_with_env_var_still_works() {
        let toml_str = r#"
            default_cli_action = "deny"
            default_egress_action = "deny"
            [[egress]]
            domain = "api.openai.com"
            inject_header = "Authorization"
            env_var = "OPENAI_API_KEY"
            header_template = "Bearer {value}"
        "#;
        let policy: PolicyFile = toml::from_str(toml_str).unwrap();
        let rule = &policy.egress[0];
        assert_eq!(rule.env_var.as_deref(), Some("OPENAI_API_KEY"));
        assert!(rule.credential_file.is_none());
        assert!(rule.request_rules.is_empty());
    }

    #[test]
    fn test_egress_rule_with_request_rules_parses() {
        let toml_str = r#"
            default_cli_action = "deny"
            default_egress_action = "deny"
            [[egress]]
            domain = "api.github.com"
            inject_header = "Authorization"
            env_var = "GITHUB_TOKEN"
            header_template = "Bearer {value}"
            request_rules = ["allow GET /repos/**", "deny * /**"]
        "#;
        let policy: PolicyFile = toml::from_str(toml_str).unwrap();
        let rule = &policy.egress[0];
        assert_eq!(rule.request_rules.len(), 2);
        assert_eq!(rule.evaluate_request_rules("GET", "/repos/owner/repo"), Some(true));
        assert_eq!(rule.evaluate_request_rules("POST", "/repos/owner/repo"), Some(false));
    }

    #[test]
    fn test_resolve_credential_from_json_file() {
        // Write a fake credential file and verify resolve_credential reads
        // the right field out of it.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"{"claudeAiOauth":{"accessToken":"real-token-xyz","refreshToken":"rt"}}"#,
        )
        .unwrap();
        let rule = EgressRule {
            domain: "api.anthropic.com".into(),
            inject_header: "Authorization".into(),
            env_var: None,
            credential_file: Some(tmp.path().display().to_string()),
            json_path: Some("claudeAiOauth.accessToken".into()),
            header_template: "Bearer {value}".into(),
            request_rules: vec![],
        };
        assert_eq!(rule.resolve_credential(), Some("real-token-xyz".to_string()));
    }

    #[test]
    #[allow(unsafe_code)]
    fn test_resolve_credential_env_var_takes_precedence() {
        // If both env_var and credential_file are set, env_var wins.
        // This documents current behavior and guards against accidental swap.
        let env_key = "ABOX_TEST_CRED_PRIORITY";
        // SAFETY: test-only; runs in a single-threaded #[test] context.
        unsafe {
            std::env::set_var(env_key, "env-value");
        }
        let rule = EgressRule {
            domain: "x".into(),
            inject_header: "Authorization".into(),
            env_var: Some(env_key.into()),
            credential_file: Some("/nonexistent".into()),
            json_path: Some("a.b".into()),
            header_template: "{value}".into(),
            request_rules: vec![],
        };
        assert_eq!(rule.resolve_credential(), Some("env-value".to_string()));
        unsafe {
            std::env::remove_var(env_key);
        }
    }

    #[test]
    fn test_resolve_credential_missing_file_returns_none() {
        let rule = EgressRule {
            domain: "x".into(),
            inject_header: "Authorization".into(),
            env_var: None,
            credential_file: Some("/definitely/does/not/exist.json".into()),
            json_path: Some("a".into()),
            header_template: "{value}".into(),
            request_rules: vec![],
        };
        assert_eq!(rule.resolve_credential(), None);
    }

    #[test]
    fn test_policy_load_rejects_credential_file_without_json_path() {
        let policy = PolicyFile {
            cli: vec![],
            egress: vec![EgressRule {
                domain: "api.example.com".into(),
                inject_header: "Authorization".into(),
                env_var: None,
                credential_file: Some("/some/file.json".into()),
                json_path: None,
                header_template: "Bearer {value}".into(),
                request_rules: vec![],
            }],
            default_cli_action: "deny".into(),
            default_egress_action: "deny".into(),
            bypass_tls: vec![],
        };
        let result = PolicyEngine::from_policy_file(policy);
        // PolicyEngine doesn't implement Debug, so we can't use expect_err.
        let Err(err) = result else {
            panic!("policy should be rejected when credential_file is set without json_path")
        };
        let msg = err.to_string();
        assert!(msg.contains("credential_file"), "error mentions the field: {msg}");
        assert!(msg.contains("json_path"), "error mentions the missing field: {msg}");
        assert!(msg.contains("api.example.com"), "error names the offending rule: {msg}");
    }

    #[test]
    fn test_extract_json_path_nested() {
        let json: serde_json::Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "test-token-123"
            }
        });
        assert_eq!(
            extract_json_path(&json, "claudeAiOauth.accessToken"),
            Some("test-token-123".to_string())
        );
    }

    #[test]
    fn test_extract_json_path_missing() {
        let json: serde_json::Value = serde_json::json!({"foo": "bar"});
        assert_eq!(extract_json_path(&json, "missing.path"), None);
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/.claude/creds.json");
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("/.claude/creds.json"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }
}
