//! Shared protocol types for communication between the guest shim and host proxy.
//!
//! These types define the JSON-over-Unix-socket protocol used by `abox-shim`
//! (running inside the VM) to request command execution from `abox-proxyd`
//! (running on the host). Both sides serialize/deserialize these types, so they
//! live in `abox-core` to avoid duplication.

use serde::{Deserialize, Serialize};

/// Request sent from the guest shim to the host proxy daemon.
///
/// The shim determines the `command` from `argv[0]` (when invoked via symlink)
/// or from the first positional argument (when invoked directly as `abox-shim`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRequest {
    /// The command name (e.g., "git", "gh", "aws").
    pub command: String,
    /// The full argument list (`argv[1..]`).
    pub args: Vec<String>,
    /// Current working directory inside the VM.
    pub cwd: String,
}

/// Response sent from the host proxy daemon back to the guest shim.
///
/// Contains the exit code and captured output from the proxied command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyResponse {
    /// The exit code of the proxied command (126 = denied by policy).
    pub exit_code: i32,
    /// Captured standard output.
    #[serde(default)]
    pub stdout: String,
    /// Captured standard error.
    #[serde(default)]
    pub stderr: String,
}

impl ProxyResponse {
    /// Create a successful response.
    pub fn success(stdout: String, stderr: String) -> Self {
        Self { exit_code: 0, stdout, stderr }
    }

    /// Create a denial response (exit code 126, matching POSIX "command not executable").
    pub fn denied(reason: &str) -> Self {
        Self {
            exit_code: 126,
            stdout: String::new(),
            stderr: format!("abox-proxy: denied: {reason}"),
        }
    }

    /// Create a response from a command that ran but may have failed.
    pub fn from_exit(exit_code: i32, stdout: String, stderr: String) -> Self {
        Self { exit_code, stdout, stderr }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_roundtrip() {
        let req = ProxyRequest {
            command: "git".into(),
            args: vec!["status".into(), "--short".into()],
            cwd: "/workspace".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: ProxyRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.command, "git");
        assert_eq!(decoded.args, vec!["status", "--short"]);
    }

    #[test]
    fn test_response_denied() {
        let resp = ProxyResponse::denied("command not in allowlist");
        assert_eq!(resp.exit_code, 126);
        assert!(resp.stderr.contains("denied"));
        assert!(resp.stdout.is_empty());
    }

    #[test]
    fn test_response_success() {
        let resp = ProxyResponse::success("hello\n".into(), String::new());
        assert_eq!(resp.exit_code, 0);
        assert_eq!(resp.stdout, "hello\n");
    }

    #[test]
    fn test_response_deserialize_missing_fields() {
        // stdout and stderr have #[serde(default)], so they can be omitted
        let json = r#"{"exit_code": 0}"#;
        let resp: ProxyResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.exit_code, 0);
        assert!(resp.stdout.is_empty());
        assert!(resp.stderr.is_empty());
    }
}
