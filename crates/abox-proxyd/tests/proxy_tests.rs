//! Integration tests for abox-proxyd components.
//!
//! Tests the audit log, CLI proxy (over Unix socket), and egress proxy
//! (HTTP CONNECT handling) in isolation and in combination.

use abox_core::policy::{CliPolicy, EgressRule, PolicyEngine, PolicyFile};

use tempfile::TempDir;

// We need to reference the proxyd modules. Since they are in a binary crate,
// we test the underlying components through their public interfaces.
// The audit log, CLI proxy, and egress proxy are internal modules of the
// abox-proxyd binary, so we test them indirectly through the core policy
// engine and through actual socket/HTTP interactions.

// ─── Audit Log Tests ────────────────────────────────────────────────────────
// The audit log is internal to abox-proxyd, but we can test its behavior
// by writing a standalone version that exercises the same JSON-lines format.

#[test]
fn test_audit_log_json_format() {
    // Verify that audit entries serialize to valid JSON lines
    use serde::Serialize;

    #[derive(Serialize)]
    struct AuditEntry {
        timestamp: String,
        sandbox_id: String,
        request_type: String,
        target: String,
        detail: String,
        decision: String,
        result_code: i32,
    }

    let entry = AuditEntry {
        timestamp: "2026-03-19T12:00:00Z".to_string(),
        sandbox_id: "task-1".to_string(),
        request_type: "cli".to_string(),
        target: "git".to_string(),
        detail: "status".to_string(),
        decision: "allowed".to_string(),
        result_code: 0,
    };

    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"sandbox_id\":\"task-1\""));
    assert!(json.contains("\"decision\":\"allowed\""));

    // Verify it can be deserialized back
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["target"], "git");
    assert_eq!(parsed["result_code"], 0);
}

#[test]
fn test_audit_log_file_write() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("audit.jsonl");

    // Write multiple entries
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap();

    use std::io::Write;
    for i in 0..5 {
        let json = serde_json::json!({
            "timestamp": format!("2026-03-19T12:00:0{}Z", i),
            "sandbox_id": format!("task-{}", i),
            "request_type": "cli",
            "target": "git",
            "detail": "status",
            "decision": "allowed",
            "result_code": 0,
        });
        writeln!(file, "{json}").unwrap();
    }
    drop(file);

    // Read and verify
    let content = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 5);

    for (i, line) in lines.iter().enumerate() {
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["sandbox_id"], format!("task-{i}"));
    }
}

// ─── CLI Proxy Protocol Tests ───────────────────────────────────────────────
// Test the JSON protocol that the CLI proxy and shim use to communicate.

#[test]
fn test_cli_proxy_request_serialization() {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug)]
    struct CliRequest {
        command: String,
        args: Vec<String>,
        cwd: String,
    }

    let request = CliRequest {
        command: "git".to_string(),
        args: vec!["status".to_string(), "--short".to_string()],
        cwd: "/workspace".to_string(),
    };

    let json = serde_json::to_string(&request).unwrap();
    let parsed: CliRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.command, "git");
    assert_eq!(parsed.args.len(), 2);
    assert_eq!(parsed.cwd, "/workspace");
}

#[test]
fn test_cli_proxy_response_serialization() {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug)]
    struct CliResponse {
        exit_code: i32,
        stdout: String,
        stderr: String,
    }

    // Success response
    let response = CliResponse {
        exit_code: 0,
        stdout: "On branch main\nnothing to commit\n".to_string(),
        stderr: String::new(),
    };
    let json = serde_json::to_string(&response).unwrap();
    let parsed: CliResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.exit_code, 0);
    assert!(parsed.stdout.contains("branch main"));

    // Denied response
    let denied = CliResponse {
        exit_code: 126,
        stdout: String::new(),
        stderr: "abox-proxyd: denied: command not allowed\n".to_string(),
    };
    let json = serde_json::to_string(&denied).unwrap();
    let parsed: CliResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.exit_code, 126);
    assert!(parsed.stderr.contains("denied"));
}

// ─── CLI Proxy End-to-End over Unix Socket ──────────────────────────────────

#[tokio::test]
async fn test_cli_proxy_unix_socket_roundtrip() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("test-proxy.sock");

    // Set up a minimal policy engine that allows "echo"
    let policy = PolicyFile {
        cli: vec![CliPolicy {
            command: "echo".to_string(),
            allow: vec![],
            deny: vec![],
            forward_ssh_agent: false,
        }],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
        bypass_tls: vec![],
    };
    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // Start a mock server that mimics the CLI proxy protocol
    let socket_path_clone = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let listener = UnixListener::bind(&socket_path_clone).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let command = request["command"].as_str().unwrap();
        let args: Vec<String> = request["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        // Evaluate policy
        let decision = engine.evaluate_cli(command, &args);

        let response = match decision {
            abox_core::policy::Decision::Allow => {
                // Actually execute the command
                let output =
                    tokio::process::Command::new(command).args(&args).output().await.unwrap();
                serde_json::json!({
                    "exit_code": output.status.code().unwrap_or(-1),
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                })
            }
            abox_core::policy::Decision::Deny(reason) => {
                serde_json::json!({
                    "exit_code": 126,
                    "stdout": "",
                    "stderr": format!("denied: {}", reason),
                })
            }
        };

        let response_json = serde_json::to_string(&response).unwrap();
        writer.write_all(response_json.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();
    });

    // Give the server a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Client: send a request
    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let request = serde_json::json!({
        "command": "echo",
        "args": ["hello", "world"],
        "cwd": "/workspace",
    });
    let request_json = serde_json::to_string(&request).unwrap();

    let (reader, mut writer) = stream.into_split();
    writer.write_all(request_json.as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.shutdown().await.unwrap();

    // Read response
    let mut reader = BufReader::new(reader);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).await.unwrap();

    let response: serde_json::Value = serde_json::from_str(response_line.trim()).unwrap();
    assert_eq!(response["exit_code"], 0);
    assert_eq!(response["stdout"].as_str().unwrap().trim(), "hello world");

    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_cli_proxy_denied_command() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("test-proxy-deny.sock");

    // Policy that only allows "git", denies everything else
    let policy = PolicyFile {
        cli: vec![CliPolicy {
            command: "git".to_string(),
            allow: vec![],
            deny: vec![],
            forward_ssh_agent: false,
        }],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
        bypass_tls: vec![],
    };
    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    let socket_path_clone = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let listener = UnixListener::bind(&socket_path_clone).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let command = request["command"].as_str().unwrap();
        let args: Vec<String> = request["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let decision = engine.evaluate_cli(command, &args);

        let response = match decision {
            abox_core::policy::Decision::Allow => {
                serde_json::json!({ "exit_code": 0, "stdout": "ok", "stderr": "" })
            }
            abox_core::policy::Decision::Deny(reason) => {
                serde_json::json!({
                    "exit_code": 126,
                    "stdout": "",
                    "stderr": format!("denied: {}", reason),
                })
            }
        };

        let response_json = serde_json::to_string(&response).unwrap();
        writer.write_all(response_json.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Client: send a "rm" command (should be denied)
    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let request = serde_json::json!({
        "command": "rm",
        "args": ["-rf", "/"],
        "cwd": "/workspace",
    });

    let (reader, mut writer) = stream.into_split();
    let request_json = serde_json::to_string(&request).unwrap();
    writer.write_all(request_json.as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.shutdown().await.unwrap();

    let mut reader = BufReader::new(reader);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).await.unwrap();

    let response: serde_json::Value = serde_json::from_str(response_line.trim()).unwrap();
    assert_eq!(response["exit_code"], 126);
    assert!(response["stderr"].as_str().unwrap().contains("denied"));

    server_handle.await.unwrap();
}

// ─── Egress Policy Tests ────────────────────────────────────────────────────

#[test]
fn test_egress_policy_wildcard_matching() {
    let policy = PolicyFile {
        cli: vec![],
        egress: vec![EgressRule {
            domain: "*.googleapis.com".to_string(),
            inject_header: "Authorization".to_string(),
            env_var: Some("GOOGLE_API_KEY".to_string()),
            credential_file: None,
            json_path: None,
            header_template: "Bearer {value}".to_string(),
            allow_methods: vec![],
            allow_path_prefixes: vec![],
        }],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
        bypass_tls: vec![],
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // Should match various googleapis subdomains
    assert!(engine.evaluate_egress("storage.googleapis.com").is_ok());
    assert!(engine.evaluate_egress("compute.googleapis.com").is_ok());
    assert!(engine.evaluate_egress("ml.googleapis.com").is_ok());

    // Should NOT match non-googleapis domains
    assert!(engine.evaluate_egress("googleapis.com.evil.com").is_err());
    assert!(engine.evaluate_egress("example.com").is_err());
}

#[test]
fn test_egress_policy_exact_domain_matching() {
    let policy = PolicyFile {
        cli: vec![],
        egress: vec![EgressRule {
            domain: "api.anthropic.com".to_string(),
            inject_header: "x-api-key".to_string(),
            env_var: Some("ANTHROPIC_API_KEY".to_string()),
            credential_file: None,
            json_path: None,
            header_template: "{value}".to_string(),
            allow_methods: vec![],
            allow_path_prefixes: vec![],
        }],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
        bypass_tls: vec![],
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // Exact match
    assert!(engine.evaluate_egress("api.anthropic.com").is_ok());

    // Subdomain should NOT match (not a wildcard rule)
    assert!(engine.evaluate_egress("sub.api.anthropic.com").is_err());

    // Different domain should NOT match
    assert!(engine.evaluate_egress("anthropic.com").is_err());
}

// ─── HTTP CONNECT Protocol Tests ────────────────────────────────────────────

#[tokio::test]
async fn test_egress_proxy_connect_handling() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // Start a minimal HTTP server that handles CONNECT
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);

        // Verify it's a CONNECT request
        assert!(request.starts_with("CONNECT"));

        // Send 200 OK to establish tunnel
        stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();

        // Echo back whatever comes through the tunnel
        let mut tunnel_buf = vec![0u8; 1024];
        let n = stream.read(&mut tunnel_buf).await.unwrap();
        stream.write_all(&tunnel_buf[..n]).await.unwrap();
    });

    // Client: send a CONNECT request
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
        .await
        .unwrap();

    // Read the 200 response
    let mut buf = vec![0u8; 1024];
    let n = client.read(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(response.contains("200"));

    // Send data through the tunnel
    client.write_all(b"hello through tunnel").await.unwrap();

    // Read echoed data
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello through tunnel");

    server_handle.await.unwrap();
}

// ─── Policy File Parsing Edge Cases ─────────────────────────────────────────

#[test]
fn test_policy_file_empty() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("empty.toml");
    std::fs::write(&path, "default_cli_action = \"deny\"\ndefault_egress_action = \"deny\"\n")
        .unwrap();

    let engine = PolicyEngine::from_file(&path).unwrap();

    // Everything should be denied
    let decision = engine.evaluate_cli("git", &["status".to_string()]);
    assert!(matches!(decision, abox_core::policy::Decision::Deny(_)));

    assert!(engine.evaluate_egress("example.com").is_err());
}

#[test]
fn test_policy_multiple_cli_commands() {
    let policy = PolicyFile {
        cli: vec![
            CliPolicy {
                command: "git".to_string(),
                allow: vec![r"^status".to_string(), r"^log".to_string()],
                deny: vec![],
                forward_ssh_agent: false,
            },
            CliPolicy {
                command: "gh".to_string(),
                allow: vec![r"^pr\s+".to_string(), r"^issue\s+".to_string()],
                deny: vec![r"delete".to_string()],
                forward_ssh_agent: false,
            },
            CliPolicy {
                command: "cargo".to_string(),
                allow: vec![],
                deny: vec![r"publish".to_string()],
                forward_ssh_agent: false,
            },
        ],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
        bypass_tls: vec![],
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // git status → allowed
    assert_eq!(
        engine.evaluate_cli("git", &["status".to_string()]),
        abox_core::policy::Decision::Allow
    );

    // gh pr list → allowed
    assert_eq!(
        engine.evaluate_cli("gh", &["pr".to_string(), "list".to_string()]),
        abox_core::policy::Decision::Allow
    );

    // gh pr delete → denied
    assert!(matches!(
        engine.evaluate_cli("gh", &["pr".to_string(), "delete".to_string(), "123".to_string()]),
        abox_core::policy::Decision::Deny(_)
    ));

    // cargo build → allowed (empty allow list = all allowed, no deny match)
    assert_eq!(
        engine.evaluate_cli("cargo", &["build".to_string()]),
        abox_core::policy::Decision::Allow
    );

    // cargo publish → denied
    assert!(matches!(
        engine.evaluate_cli("cargo", &["publish".to_string()]),
        abox_core::policy::Decision::Deny(_)
    ));

    // npm install → denied (not in any CLI policy, default is deny)
    assert!(matches!(
        engine.evaluate_cli("npm", &["install".to_string()]),
        abox_core::policy::Decision::Deny(_)
    ));
}
