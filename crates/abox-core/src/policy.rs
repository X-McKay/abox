//! Policy engine for credential proxy authorization.
//!
//! Evaluates whether a CLI command or HTTP request from a sandbox should be
//! allowed, denied, or have credentials injected. Policies are defined in
//! TOML files under the `policies/` directory.

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
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

/// An HTTP egress proxy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressRule {
    /// Domain pattern (e.g., "api.anthropic.com", "*.amazonaws.com").
    pub domain: String,

    /// Header name to inject (e.g., "x-api-key", "Authorization").
    pub inject_header: String,

    /// Environment variable on the host that contains the secret value.
    pub env_var: String,

    /// Optional header value template. `{value}` is replaced with the env var.
    /// Default: just the raw value.
    #[serde(default = "default_header_template")]
    pub header_template: String,
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
}

fn default_header_template() -> String {
    "{value}".to_string()
}

fn default_action() -> String {
    "deny".to_string()
}

/// The policy engine. Loaded once and used to evaluate every request.
pub struct PolicyEngine {
    cli_policies: Vec<CompiledCliPolicy>,
    egress_rules: Vec<EgressRule>,
    default_cli_action: String,
    default_egress_action: String,
}

struct CompiledCliPolicy {
    command: String,
    allow_patterns: Vec<Regex>,
    deny_patterns: Vec<Regex>,
    #[allow(dead_code)]
    forward_ssh_agent: bool,
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
                .map(|p| Regex::new(p).with_context(|| format!("Invalid allow regex: {}", p)))
                .collect::<Result<_>>()?;

            let deny_patterns: Vec<Regex> = cp
                .deny
                .iter()
                .map(|p| Regex::new(p).with_context(|| format!("Invalid deny regex: {}", p)))
                .collect::<Result<_>>()?;

            cli_policies.push(CompiledCliPolicy {
                command: cp.command.clone(),
                allow_patterns,
                deny_patterns,
                forward_ssh_agent: cp.forward_ssh_agent,
            });
        }

        Ok(Self {
            cli_policies,
            egress_rules: policy.egress,
            default_cli_action: policy.default_cli_action,
            default_egress_action: policy.default_egress_action,
        })
    }

    /// Evaluate a CLI command request.
    ///
    /// # Arguments
    /// * `command` - The binary name (e.g., "git").
    /// * `args` - The full argument list (e.g., ["push", "origin", "main"]).
    pub fn evaluate_cli(&self, command: &str, args: &[String]) -> Decision {
        let args_str = args.join(" ");

        // Find the matching policy
        let policy = self.cli_policies.iter().find(|p| p.command == command);

        let policy = match policy {
            Some(p) => p,
            None => {
                // No policy for this command — use default
                return if self.default_cli_action == "allow" {
                    Decision::Allow
                } else {
                    Decision::Deny(format!("No policy for command '{}'", command))
                };
            }
        };

        // Deny patterns are checked first (deny takes precedence)
        for pattern in &policy.deny_patterns {
            if pattern.is_match(&args_str) {
                return Decision::Deny(format!(
                    "Denied by pattern '{}' for command '{}'",
                    pattern, command
                ));
            }
        }

        // If there are allow patterns, at least one must match
        if !policy.allow_patterns.is_empty() {
            let allowed = policy.allow_patterns.iter().any(|p| p.is_match(&args_str));
            if !allowed {
                return Decision::Deny(format!(
                    "No allow pattern matched for '{}' with args: {}",
                    command, args_str
                ));
            }
        }

        Decision::Allow
    }

    /// Evaluate an HTTP egress request.
    ///
    /// # Arguments
    /// * `domain` - The target domain (e.g., "api.anthropic.com").
    ///
    /// Returns the matching egress rule if allowed, or a deny decision.
    pub fn evaluate_egress(&self, domain: &str) -> Result<Option<&EgressRule>, Decision> {
        for rule in &self.egress_rules {
            if domain_matches(&rule.domain, domain) {
                return Ok(Some(rule));
            }
        }

        if self.default_egress_action == "allow" {
            Ok(None)
        } else {
            Err(Decision::Deny(format!("No egress rule for domain '{}'", domain)))
        }
    }
}

/// Match a domain pattern against a domain.
/// Supports exact match and wildcard prefix (e.g., "*.amazonaws.com").
fn domain_matches(pattern: &str, domain: &str) -> bool {
    if pattern == domain {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return domain.ends_with(suffix) && domain.len() > suffix.len();
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
                env_var: "ANTHROPIC_API_KEY".to_string(),
                header_template: "{value}".to_string(),
            }],
            default_cli_action: "deny".to_string(),
            default_egress_action: "deny".to_string(),
        }
    }

    #[test]
    fn test_git_push_allowed() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "git",
            &["push", "origin", "main"].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
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
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        );
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn test_unknown_command_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine
            .evaluate_cli("rm", &["-rf", "/"].iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn test_aws_s3_allowed() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "aws",
            &["s3", "ls", "s3://my-bucket"].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_aws_iam_denied() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        let decision = engine.evaluate_cli(
            "aws",
            &["iam", "list-users"].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
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
    fn test_domain_wildcard() {
        assert!(domain_matches("*.amazonaws.com", "s3.amazonaws.com"));
        assert!(domain_matches("*.amazonaws.com", "sts.us-east-1.amazonaws.com"));
        assert!(!domain_matches("*.amazonaws.com", "amazonaws.com"));
        assert!(domain_matches("api.anthropic.com", "api.anthropic.com"));
        assert!(!domain_matches("api.anthropic.com", "evil.anthropic.com"));
    }
}
