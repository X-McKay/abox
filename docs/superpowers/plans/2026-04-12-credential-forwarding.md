# Credential Forwarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Forward host OAuth credentials into abox sandboxes via stub files + MITM proxy injection, so tools like Claude Code authenticate without real secrets entering the VM.

**Architecture:** A stub credential file passes the agent tool's local auth check inside the guest VM. The host-side MITM egress proxy reads the real credential from the host filesystem and replaces the stub's placeholder header before forwarding to the upstream API. A per-sandbox egress proxy instance (currently missing) is spawned alongside the existing proxy bridge to handle HTTPS traffic.

**Tech Stack:** Rust (abox-core, abox-proxyd, abox-cli), TOML config, serde_json for credential file parsing, existing TLS MITM infrastructure (rcgen, rustls, tokio-rustls).

**Spec:** `docs/superpowers/specs/2026-04-12-credential-forwarding-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/abox-core/src/config.rs` | Modify | Add `GuestConfig` and `CredentialFileEntry` types |
| `crates/abox-core/src/boot_meta.rs` | Modify | Add `StagedCredential`, emit credential placement in `runner_script()` |
| `crates/abox-core/src/policy.rs` | Modify | Add `credential_file` + `json_path` fields to `EgressRule` |
| `crates/abox-core/src/sandbox.rs` | Modify | Stage credential stubs/files, spawn per-sandbox egress proxy |
| `crates/abox-proxyd/src/egress_proxy.rs` | Modify | Resolve credential from JSON file (alongside env var) |
| `policies/default.toml` | Modify | Update `api.anthropic.com` rule to use `credential_file` |

---

## Task 1: Config types for guest credential files

**Files:**
- Modify: `crates/abox-core/src/config.rs`

- [ ] **Step 1: Write the failing test for `GuestConfig` deserialization**

Add to the existing `#[cfg(test)] mod tests` block in `config.rs`:

```rust
#[test]
fn test_parse_guest_credential_files() {
    let toml_str = r#"
        [guest]
        [[guest.credential_files]]
        host = "~/.claude/.credentials.json"
        guest = "/.claude/.credentials.json"
        mode = "0600"

        [guest.credential_files.stub]
        [guest.credential_files.stub.claudeAiOauth]
        accessToken = "abox-proxy-managed"
        refreshToken = "abox-proxy-managed"
        expiresAt = 9999999999999
        scopes = ["user:inference"]
        subscriptionType = "pro"
    "#;
    let config: AboxConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.guest.credential_files.len(), 1);
    let entry = &config.guest.credential_files[0];
    assert_eq!(entry.host, "~/.claude/.credentials.json");
    assert_eq!(entry.guest, "/.claude/.credentials.json");
    assert_eq!(entry.mode, "0600");
    assert!(entry.stub.is_some());
}

#[test]
fn test_parse_guest_credential_files_without_stub() {
    let toml_str = r#"
        [guest]
        [[guest.credential_files]]
        host = "~/.config/gh/hosts.yml"
        guest = "/root/.config/gh/hosts.yml"
    "#;
    let config: AboxConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.guest.credential_files.len(), 1);
    let entry = &config.guest.credential_files[0];
    assert!(entry.stub.is_none());
    assert_eq!(entry.mode, "0600"); // default
}

#[test]
fn test_parse_empty_guest_section() {
    let toml_str = r#"
        [vm_defaults]
        memory_mib = 2048
    "#;
    let config: AboxConfig = toml::from_str(toml_str).unwrap();
    assert!(config.guest.credential_files.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p abox-core test_parse_guest`
Expected: FAIL — `GuestConfig` type doesn't exist.

- [ ] **Step 3: Implement the types**

Add to `config.rs`, before the `impl Default for AboxConfig` block:

```rust
/// Guest VM configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuestConfig {
    /// Credential files to stage in the guest VM.
    #[serde(default)]
    pub credential_files: Vec<CredentialFileEntry>,
}

/// A credential file to place inside the guest VM at boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialFileEntry {
    /// Source path on the host. Supports `~` expansion.
    pub host: String,
    /// Absolute destination path inside the VM.
    pub guest: String,
    /// Unix permissions for the file. Default: "0600".
    #[serde(default = "default_credential_mode")]
    pub mode: String,
    /// If set, generate a stub JSON file instead of copying the host file.
    /// The value is serialized directly as the file content.
    pub stub: Option<toml::Value>,
}

fn default_credential_mode() -> String {
    "0600".to_string()
}
```

Add the `guest` field to `AboxConfig`:

```rust
pub struct AboxConfig {
    // ... existing fields ...

    /// Guest VM configuration (credential files, etc.).
    #[serde(default)]
    pub guest: GuestConfig,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p abox-core test_parse_guest`
Expected: All 3 tests PASS.

- [ ] **Step 5: Run full quality gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/abox-core/src/config.rs
git commit -m "feat(config): add GuestConfig with credential_files support"
```

---

## Task 2: Boot metadata credential placement

**Files:**
- Modify: `crates/abox-core/src/boot_meta.rs`

- [ ] **Step 1: Write the failing test for credential placement in runner script**

Add to the existing `#[cfg(test)] mod tests` block in `boot_meta.rs`:

```rust
#[test]
fn test_runner_script_with_credentials() {
    let meta = BootMeta {
        sandbox_id: "cred-test".into(),
        agent_command: vec!["claude".into(), "--print".into(), "hello".into()],
        env: vec![],
        credential_files: vec![
            StagedCredential {
                index: 0,
                guest_path: "/.claude/.credentials.json".into(),
                mode: "0600".into(),
            },
        ],
    };
    let script = meta.runner_script();
    assert!(script.contains("mkdir -p '/.claude'"));
    assert!(script.contains("cp '/abox-meta/credentials/0' '/.claude/.credentials.json'"));
    assert!(script.contains("chmod 0600 '/.claude/.credentials.json'"));
    // Credential placement must come before exec
    let cred_pos = script.find("cp '/abox-meta/credentials/0'").unwrap();
    let exec_pos = script.find("\nexec ").unwrap();
    assert!(cred_pos < exec_pos, "credentials must be placed before exec");
}

#[test]
fn test_runner_script_no_credentials() {
    let meta = BootMeta {
        sandbox_id: "no-cred".into(),
        agent_command: vec!["echo".into(), "hi".into()],
        env: vec![],
        credential_files: vec![],
    };
    let script = meta.runner_script();
    assert!(!script.contains("/abox-meta/credentials"));
}

#[test]
fn test_runner_script_credential_path_escaping() {
    let meta = BootMeta {
        sandbox_id: "escape-test".into(),
        agent_command: vec!["true".into()],
        env: vec![],
        credential_files: vec![
            StagedCredential {
                index: 0,
                guest_path: "/root/.config/it's a test/creds.json".into(),
                mode: "0600".into(),
            },
        ],
    };
    let script = meta.runner_script();
    // Single-quote escaping must work for guest paths
    assert!(script.contains(r"'/root/.config/it'\''s a test/creds.json'"));
}

#[test]
fn test_stage_with_credentials() {
    let tmp = TempDir::new().unwrap();
    let meta = BootMeta {
        sandbox_id: "stage-cred".into(),
        agent_command: vec!["true".into()],
        env: vec![],
        credential_files: vec![
            StagedCredential {
                index: 0,
                guest_path: "/.claude/.credentials.json".into(),
                mode: "0600".into(),
            },
        ],
    };
    meta.stage(tmp.path()).unwrap();
    // The runner script should contain credential placement
    let runner = std::fs::read_to_string(tmp.path().join("runner.sh")).unwrap();
    assert!(runner.contains("cp '/abox-meta/credentials/0'"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p abox-core test_runner_script_with_credentials test_runner_script_no_credentials test_runner_script_credential_path_escaping test_stage_with_credentials`
Expected: FAIL — `StagedCredential` doesn't exist, `credential_files` field missing from `BootMeta`.

- [ ] **Step 3: Implement StagedCredential and update BootMeta**

Add the `StagedCredential` struct and update `BootMeta` in `boot_meta.rs`:

```rust
/// A credential file staged in the boot metadata directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedCredential {
    /// Index in the credentials directory (maps to `credentials/<index>`).
    pub index: usize,
    /// Absolute destination path inside the guest VM.
    pub guest_path: String,
    /// Unix permissions (e.g., "0600").
    pub mode: String,
}
```

Add the field to `BootMeta`:

```rust
pub struct BootMeta {
    pub sandbox_id: String,
    pub agent_command: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Credential files staged in `<meta_dir>/credentials/`.
    #[serde(default)]
    pub credential_files: Vec<StagedCredential>,
}
```

Update `runner_script()` to emit credential placement commands between the env exports and the `exec`. Insert before the `s.push_str("exec");` line:

```rust
// Place credential files from boot metadata.
for cred in &self.credential_files {
    let parent = std::path::Path::new(&cred.guest_path)
        .parent()
        .unwrap_or(std::path::Path::new("/"))
        .display()
        .to_string();
    s.push_str(&format!("mkdir -p '{}'\n", sh_escape(&parent)));
    s.push_str(&format!(
        "cp '/abox-meta/credentials/{}' '{}'\n",
        cred.index,
        sh_escape(&cred.guest_path)
    ));
    s.push_str(&format!(
        "chmod {} '{}'\n",
        sh_escape(&cred.mode),
        sh_escape(&cred.guest_path)
    ));
}
```

- [ ] **Step 4: Fix existing tests**

The existing tests in `boot_meta.rs` construct `BootMeta` without `credential_files`. Add `credential_files: vec![]` to each existing test's `BootMeta` literal (`test_boot_meta_roundtrip`, `test_runner_script_basic`, `test_runner_script_quotes_metacharacters`, `test_stage_writes_files`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p abox-core boot_meta`
Expected: All tests (old + new) PASS.

- [ ] **Step 6: Run full quality gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/abox-core/src/boot_meta.rs
git commit -m "feat(boot_meta): emit credential file placement in runner script"
```

---

## Task 3: Policy engine — credential_file + json_path support

**Files:**
- Modify: `crates/abox-core/src/policy.rs`

- [ ] **Step 1: Write the failing test for credential_file policy parsing**

Add to the existing `#[cfg(test)] mod tests` block in `policy.rs`:

```rust
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
    assert_eq!(
        rule.credential_file.as_deref(),
        Some("~/.claude/.credentials.json")
    );
    assert_eq!(rule.json_path.as_deref(), Some("claudeAiOauth.accessToken"));
    assert_eq!(rule.header_template, "Bearer {value}");
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
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p abox-core test_egress_rule_with_credential_file test_egress_rule_with_env_var_still_works`
Expected: FAIL — `credential_file` and `json_path` not on `EgressRule`, `env_var` is `String` not `Option<String>`.

- [ ] **Step 3: Update EgressRule to support both credential sources**

Modify `EgressRule` in `policy.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressRule {
    /// Domain pattern (e.g., "api.anthropic.com", "*.amazonaws.com").
    pub domain: String,

    /// Header name to inject (e.g., "x-api-key", "Authorization").
    pub inject_header: String,

    /// Environment variable on the host that contains the secret value.
    /// Mutually exclusive with `credential_file`.
    #[serde(default)]
    pub env_var: Option<String>,

    /// Path to a JSON credential file on the host. Supports `~` expansion.
    /// Used with `json_path` to extract the credential value.
    #[serde(default)]
    pub credential_file: Option<String>,

    /// Dot-separated path to the credential value within the JSON file
    /// (e.g., "claudeAiOauth.accessToken").
    #[serde(default)]
    pub json_path: Option<String>,

    /// Optional header value template. `{value}` is replaced with the credential.
    /// Default: just the raw value.
    #[serde(default = "default_header_template")]
    pub header_template: String,
}
```

- [ ] **Step 4: Add a helper method to resolve the credential value**

Add to `EgressRule`:

```rust
impl EgressRule {
    /// Resolve the credential value from either an env var or a JSON file.
    ///
    /// Returns `None` if the credential source is not configured or the
    /// value cannot be read.
    pub fn resolve_credential(&self) -> Option<String> {
        if let Some(ref env_var) = self.env_var {
            return std::env::var(env_var).ok();
        }
        if let Some(ref cred_file) = self.credential_file {
            let path = expand_tilde(cred_file);
            let content = std::fs::read_to_string(&path).ok()?;
            let json: serde_json::Value = serde_json::from_str(&content).ok()?;
            let json_path = self.json_path.as_deref()?;
            return extract_json_path(&json, json_path);
        }
        None
    }
}

/// Expand `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}/{}", home.display(), rest);
        }
    }
    path.to_string()
}

/// Extract a value from a JSON object using a dot-separated path.
/// E.g., "claudeAiOauth.accessToken" extracts json["claudeAiOauth"]["accessToken"].
fn extract_json_path(json: &serde_json::Value, path: &str) -> Option<String> {
    let mut current = json;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    current.as_str().map(String::from)
}
```

Note: Add `dirs` to the existing imports at the top of `policy.rs` — it's already a dependency of `abox-core`.

- [ ] **Step 5: Add tests for resolve_credential and extract_json_path**

```rust
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
```

- [ ] **Step 6: Fix the existing test_policy() helper and tests that construct EgressRule**

The existing `test_policy()` function constructs an `EgressRule` with `env_var: String`. Update it to use `Option<String>`:

```rust
fn test_policy() -> PolicyFile {
    PolicyFile {
        // ... existing cli vec ...
        egress: vec![EgressRule {
            domain: "api.anthropic.com".to_string(),
            inject_header: "x-api-key".to_string(),
            env_var: Some("ANTHROPIC_API_KEY".to_string()),
            credential_file: None,
            json_path: None,
            header_template: "{value}".to_string(),
        }],
        // ... rest unchanged ...
    }
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p abox-core -- policy`
Expected: All policy tests PASS.

- [ ] **Step 8: Run full quality gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS (egress_proxy.rs will need updating in Task 4 to compile against the new `Option<String>` env_var).

Note: If the workspace build fails due to `egress_proxy.rs` referencing `rule.env_var` as a `String`, proceed to Task 4 and fix it there. Tasks 3+4 can be committed together if needed.

- [ ] **Step 9: Commit**

```bash
git add crates/abox-core/src/policy.rs
git commit -m "feat(policy): support credential_file + json_path in egress rules"
```

---

## Task 4: Egress proxy — resolve credential from JSON file

**Files:**
- Modify: `crates/abox-proxyd/src/egress_proxy.rs`

- [ ] **Step 1: Update handle_mitm_with_injection to use resolve_credential**

In `egress_proxy.rs`, replace the credential resolution block in `handle_mitm_with_injection` (the `if let Some(rule) = rule { match std::env::var(...)` block, approximately lines 347-384) with:

```rust
if let Some(rule) = rule {
    match rule.resolve_credential() {
        Some(value) => {
            let header_value = rule.header_template.replace("{value}", &value);
            let inject_line = format!("{}: {}", rule.inject_header, header_value);

            // Remove any existing header with the same name (case-insensitive)
            let header_lower = rule.inject_header.to_lowercase();
            lines.retain(|l| {
                if let Some(colon_pos) = l.find(':') {
                    l[..colon_pos].trim().to_lowercase() != header_lower
                } else {
                    true
                }
            });

            // Insert before the last empty line
            if let Some(pos) = lines.iter().rposition(|l| !l.is_empty()) {
                lines.insert(pos + 1, inject_line);
            } else {
                lines.push(inject_line);
            }

            tracing::debug!(
                header = %rule.inject_header,
                "Injected credential header"
            );
        }
        None => {
            tracing::warn!(
                domain = %rule.domain,
                "No credential value available (env var not set or credential file not found)"
            );
        }
    }
}
```

- [ ] **Step 2: Run the quality gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. The egress proxy now uses `resolve_credential()` which handles both env var and credential file sources.

- [ ] **Step 3: Commit**

```bash
git add crates/abox-proxyd/src/egress_proxy.rs
git commit -m "feat(egress): resolve credentials from JSON files via policy rules"
```

---

## Task 5: Sandbox orchestrator — stub generation + credential staging + per-sandbox egress proxy

**Files:**
- Modify: `crates/abox-core/src/sandbox.rs`

This is the largest task. It does three things:
1. Reads `config.guest.credential_files`, generates stubs or copies files into the meta dir
2. Passes `StagedCredential` entries to `BootMeta`
3. Spawns a per-sandbox `EgressProxyServer` (closes the P1 gap from `future-work.md`)

- [ ] **Step 1: Add RootCa parameter to run_sandbox**

The `run_sandbox` method needs access to the root CA for spawning the egress proxy. Update its signature:

```rust
pub async fn run_sandbox(
    &self,
    params: CreateSandboxParams,
    policy: std::sync::Arc<crate::policy::PolicyEngine>,
    root_ca: std::sync::Arc<crate::ca::RootCa>,
) -> Result<i32> {
```

- [ ] **Step 2: Update the call site in abox-cli**

In `crates/abox-cli/src/commands/run.rs`, update the `execute` function signature and call:

```rust
pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: RunArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
    policy: std::sync::Arc<abox_core::policy::PolicyEngine>,
    root_ca: std::sync::Arc<abox_core::ca::RootCa>,
) -> Result<()> {
```

Update the `orchestrator.run_sandbox(params, policy)` call to `orchestrator.run_sandbox(params, policy, root_ca)`.

In `crates/abox-cli/src/main.rs`, load the root CA and pass it:

```rust
// Load the root CA (after the policy engine load, before the match)
let ca_dir = abox_core::ca::RootCa::default_dir()?;
let root_ca = std::sync::Arc::new(
    abox_core::ca::RootCa::load_or_generate(&ca_dir)
        .context("Failed to load or generate root CA")?,
);
```

Update the `Commands::Run` match arm to pass `root_ca`:

```rust
Commands::Run(args) => {
    commands::run::execute(args, &orchestrator, std::sync::Arc::clone(&policy), std::sync::Arc::clone(&root_ca)).await
}
```

- [ ] **Step 3: Add credential staging logic to create_sandbox**

Add a helper function to `sandbox.rs` (outside the impl block):

```rust
use crate::boot_meta::StagedCredential;
use crate::config::CredentialFileEntry;

/// Stage credential files into the boot metadata directory.
///
/// For entries with a `stub`, generates a stub JSON file.
/// For entries without a `stub`, copies the host file.
/// Returns the list of staged credentials for `BootMeta`.
fn stage_credential_files(
    entries: &[CredentialFileEntry],
    meta_dir: &std::path::Path,
) -> Result<Vec<StagedCredential>> {
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let cred_dir = meta_dir.join("credentials");
    std::fs::create_dir_all(&cred_dir)?;

    let mut staged = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        let host_path = crate::policy::expand_tilde(&entry.host);

        if !std::path::Path::new(&host_path).exists() {
            tracing::debug!(
                host_path = %host_path,
                guest_path = %entry.guest,
                "Credential file not found on host, skipping"
            );
            continue;
        }

        let dest = cred_dir.join(index.to_string());

        if let Some(ref stub) = entry.stub {
            // Generate stub JSON from the TOML value
            let json_value = toml_to_json(stub);
            let content = serde_json::to_string_pretty(&json_value)
                .context("Serializing credential stub to JSON")?;
            std::fs::write(&dest, content)?;
            tracing::info!(
                guest_path = %entry.guest,
                "Staged credential stub"
            );
        } else {
            // Copy the host file as-is
            std::fs::copy(&host_path, &dest)
                .with_context(|| format!("Copying credential file {host_path}"))?;
            tracing::info!(
                host_path = %host_path,
                guest_path = %entry.guest,
                "Staged credential file"
            );
        }

        staged.push(StagedCredential {
            index,
            guest_path: entry.guest.clone(),
            mode: entry.mode.clone(),
        });
    }

    Ok(staged)
}

/// Convert a TOML value to a serde_json::Value.
fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(toml_to_json).collect())
        }
        toml::Value::Table(t) => {
            let map = t.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect();
            serde_json::Value::Object(map)
        }
    }
}
```

- [ ] **Step 4: Wire credential staging into create_sandbox**

In `create_sandbox`, after the `meta.stage(&meta_dir)` call in the `VmPort::start` implementation (this happens inside `cloud_hypervisor.rs`), we need to stage credentials BEFORE the VM starts. The correct place is to modify the `BootMeta` construction.

Actually, looking at the code flow: `create_sandbox` builds a `VmConfig` and passes it to `self.vm_manager.start(vm_config)`. The `VmConfig` already contains `env_vars` and `agent_command`. The `BootMeta` is constructed inside `cloud_hypervisor.rs::start()`. We need to pass the credential files through.

Add a `credential_files` field to `VmConfig` in `crates/abox-core/src/vm.rs`:

```rust
pub struct VmConfig {
    // ... existing fields ...
    /// Credential files to stage in the boot metadata.
    pub credential_files: Vec<crate::boot_meta::StagedCredential>,
}
```

In `sandbox.rs::create_sandbox`, stage the credentials before building `VmConfig`:

```rust
// Stage credential files (after worktree creation, before VM start)
let meta_dir = self.config.runtime_dir().join(format!("meta-{}", params.task_id));
let credential_files = stage_credential_files(
    &self.config.guest.credential_files,
    &meta_dir,
)?;
```

Add to the `VmConfig` construction:

```rust
let vm_config = VmConfig {
    // ... existing fields ...
    credential_files,
};
```

- [ ] **Step 5: Update cloud_hypervisor.rs to pass credentials to BootMeta**

In `crates/abox-core/src/adapters/cloud_hypervisor.rs`, update the `BootMeta` construction (around line 152):

```rust
let meta = crate::boot_meta::BootMeta {
    sandbox_id: config.id.clone(),
    agent_command: config.agent_command.clone(),
    env: config.env_vars.clone(),
    credential_files: config.credential_files.clone(),
};
```

- [ ] **Step 6: Spawn per-sandbox egress proxy in run_sandbox**

In `sandbox.rs::run_sandbox`, after the proxy bridge spawn and before the console streamer, add the per-sandbox egress proxy:

```rust
// Spawn per-sandbox egress proxy for HTTPS credential injection.
let egress_policy = std::sync::Arc::clone(&policy);
let egress_audit = audit_sink.clone();
let egress_root_ca = std::sync::Arc::clone(&root_ca);
let egress_sandbox_id = task_id.clone();
let egress_handle = tokio::spawn(async move {
    use abox_core::policy::PolicyEngine;

    let listener = match tokio::net::TcpListener::bind(
        format!("127.0.0.1:{}", status.egress_port)
    ).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                error = %e,
                port = status.egress_port,
                "Failed to bind per-sandbox egress proxy"
            );
            return;
        }
    };

    tracing::info!(
        port = status.egress_port,
        sandbox_id = %egress_sandbox_id,
        "Per-sandbox egress proxy listening"
    );

    // Run a minimal accept loop using the existing egress proxy logic.
    // We reuse the MITM infrastructure from EgressProxyServer.
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let policy = egress_policy.clone();
                let root_ca = egress_root_ca.clone();
                let bypass_tls = policy.bypass_tls_patterns().to_vec();
                let sandbox_id = egress_sandbox_id.clone();

                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let service = hyper::service::service_fn(move |req| {
                        let policy = policy.clone();
                        let root_ca = root_ca.clone();
                        let bypass_tls = bypass_tls.clone();
                        let sandbox_id = sandbox_id.clone();
                        async move {
                            abox_proxyd_egress::handle_request(
                                req, &policy, &root_ca, &bypass_tls, &sandbox_id, peer_addr,
                            ).await
                        }
                    });

                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(io, service)
                        .with_upgrades()
                        .await
                    {
                        tracing::debug!(error = %e, "Egress proxy connection error");
                    }
                });
            }
            Err(e) => {
                tracing::debug!(error = %e, "Egress accept error");
            }
        }
    }
});
```

**Important:** The egress proxy logic currently lives in `abox-proxyd` (a separate binary crate), not in `abox-core`. To avoid a circular dependency, we need to refactor `handle_request` and the MITM functions out of `abox-proxyd` into `abox-core`. This is a significant refactor.

**Simpler alternative:** Instead of refactoring the full egress proxy, make `handle_request` and its helpers `pub` in `abox-proxyd/src/egress_proxy.rs` and depend on `abox-proxyd` from `abox-core`. BUT this is backwards — the CLI depends on core, not the other way around.

**Correct approach:** Move the egress proxy core logic (`handle_request`, `handle_mitm`, `handle_mitm_with_injection`, `handle_passthrough`, `build_server_config`, `is_tls_bypassed`, the body helpers) into a new module `crates/abox-core/src/egress.rs`. Have `abox-proxyd/src/egress_proxy.rs` re-export from there. Then `sandbox.rs` can use `crate::egress::handle_request`.

- [ ] **Step 6a: Create `crates/abox-core/src/egress.rs`**

Move these functions from `abox-proxyd/src/egress_proxy.rs` to `crates/abox-core/src/egress.rs`:
- `handle_request` (make public)
- `handle_passthrough`
- `handle_mitm`
- `handle_mitm_with_injection`
- `build_server_config`
- `is_tls_bypassed`
- `empty_body`
- `full_body`

Add the necessary dependencies to `abox-core/Cargo.toml`:
```toml
hyper = { version = "1", features = ["full"] }
hyper-util = { version = "0.1", features = ["full"] }
http-body-util = "0.1"
bytes = "1"
rustls = "0.23"
tokio-rustls = "0.26"
webpki-roots = "0.26"
rustls-pemfile = "2"
```

Register the module in `crates/abox-core/src/lib.rs`:
```rust
pub mod egress;
```

The function signature for `handle_request` becomes:
```rust
pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    policy: &PolicyEngine,
    root_ca: &RootCa,
    bypass_tls: &[String],
    sandbox_id: &str,
    peer_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error>
```

(Drop the `AuditLog` parameter for now — the proxy bridge's `FileAuditSink` already covers audit. If needed, add audit later.)

- [ ] **Step 6b: Update abox-proxyd to use abox-core::egress**

Replace the contents of `abox-proxyd/src/egress_proxy.rs` with a thin wrapper that re-exports from `abox_core::egress` and adds the `EgressProxyServer` (which is the accept loop + audit logging specific to the standalone daemon).

- [ ] **Step 6c: Wire the egress proxy in run_sandbox**

Now `sandbox.rs` can use `crate::egress::handle_request`. Update the spawn block from Step 6 to use it:

```rust
let egress_handle = tokio::spawn(async move {
    let listener = match tokio::net::TcpListener::bind(
        format!("127.0.0.1:{}", status.egress_port)
    ).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, port = status.egress_port, "Failed to bind egress proxy");
            return;
        }
    };

    tracing::info!(port = status.egress_port, sandbox_id = %egress_sandbox_id, "Per-sandbox egress proxy listening");

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => { tracing::debug!(error = %e, "Egress accept error"); continue; }
        };
        let policy = egress_policy.clone();
        let root_ca = egress_root_ca.clone();
        let sandbox_id = egress_sandbox_id.clone();
        let bypass_tls = policy.bypass_tls_patterns().to_vec();

        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(stream);
            let service = hyper::service::service_fn(move |req| {
                let policy = policy.clone();
                let root_ca = root_ca.clone();
                let bypass_tls = bypass_tls.clone();
                let sandbox_id = sandbox_id.clone();
                async move {
                    crate::egress::handle_request(req, &policy, &root_ca, &bypass_tls, &sandbox_id, peer_addr).await
                }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .preserve_header_case(true)
                .title_case_headers(true)
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::debug!(error = %e, "Egress connection error");
            }
        });
    }
});
```

Add `egress_handle.abort();` to the cleanup section alongside `bridge_handle.abort();`.

- [ ] **Step 7: Run full quality gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(sandbox): stage credential stubs and spawn per-sandbox egress proxy

- Stage credential files (stubs or copies) into boot metadata
- Generate runner.sh commands to place them in the guest
- Spawn per-sandbox egress proxy for HTTPS credential injection
- Move egress proxy core logic from abox-proxyd into abox-core::egress
- Closes per-sandbox egress proxy spawning gap (future-work.md P1)"
```

---

## Task 6: Update policy and config files

**Files:**
- Modify: `policies/default.toml`
- Modify: `~/.abox/config.toml` (user config, not checked in)

- [ ] **Step 1: Update default.toml**

Replace the existing `api.anthropic.com` egress rule:

```toml
[[egress]]
domain = "api.anthropic.com"
inject_header = "Authorization"
credential_file = "~/.claude/.credentials.json"
json_path = "claudeAiOauth.accessToken"
header_template = "Bearer {value}"
```

Keep the other rules (OpenAI, Google, GitHub) unchanged — they still use `env_var`.

- [ ] **Step 2: Update the user's config.toml**

Add to `~/.abox/config.toml`:

```toml
[guest]

[[guest.credential_files]]
host = "~/.claude/.credentials.json"
guest = "/.claude/.credentials.json"
mode = "0600"

[guest.credential_files.stub]
[guest.credential_files.stub.claudeAiOauth]
accessToken = "abox-proxy-managed"
refreshToken = "abox-proxy-managed"
expiresAt = 9999999999999
scopes = ["user:inference"]
subscriptionType = "pro"
```

- [ ] **Step 3: Commit the policy change**

```bash
git add policies/default.toml
git commit -m "feat(policy): update anthropic egress rule to use credential_file injection"
```

---

## Task 7: Integration test

- [ ] **Step 1: Build the project**

Run: `cargo build --workspace --release`
Expected: PASS.

- [ ] **Step 2: Run the smoke test**

Run from the repo root:

```bash
abox/target/release/abox --repo abox run --task test-cred-stub --ephemeral \
    -- bash -c "cat /.claude/.credentials.json"
```

Expected: The stub JSON should be printed, containing `"accessToken": "abox-proxy-managed"` and the other stub fields. This confirms the stub file is staged and placed correctly.

- [ ] **Step 3: Run Claude Code through the sandbox**

```bash
abox/target/release/abox --repo abox run --task test-claude-auth --ephemeral \
    -- claude --print "Say hello in exactly one sentence"
```

Expected: Claude Code should authenticate via the stub + proxy injection and return a one-sentence response. No "Please run /login" error.

- [ ] **Step 4: Verify proxy injection in logs**

Check the console output for:
- `Per-sandbox egress proxy listening` log line
- `Injected credential header` debug log (may need `RUST_LOG=debug`)
- No `Credential env var not set` warnings

---

## Summary of changes

| Task | Files | Purpose |
|------|-------|---------|
| 1 | `config.rs` | `GuestConfig` + `CredentialFileEntry` types |
| 2 | `boot_meta.rs` | `StagedCredential` + credential placement in runner script |
| 3 | `policy.rs` | `credential_file` + `json_path` on `EgressRule` + `resolve_credential()` |
| 4 | `egress_proxy.rs` | Use `resolve_credential()` for header injection |
| 5 | `sandbox.rs`, `vm.rs`, `cloud_hypervisor.rs`, `egress.rs`, `lib.rs`, `main.rs`, `run.rs` | Stub generation, credential staging, per-sandbox egress proxy, egress module extraction |
| 6 | `default.toml`, `~/.abox/config.toml` | Policy + config for Claude Code credential forwarding |
| 7 | (manual) | Integration testing |
