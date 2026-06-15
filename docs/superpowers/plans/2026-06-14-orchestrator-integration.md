# Orchestrator Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make abox usable by third-party orchestrators — generic data-in (`--input-file`), a gated host-port bridge, machine-readable output (`--json` + `abox path`), quieter virtiofsd logs, and an honest `--prompt` help string.

**Architecture:** Four features are new *producers* of channels that already exist (the `/abox-meta` read-only share, the `/abox-meta/services` socat metadata file, the hash-chained audit log, and the CLI output layer). The fifth — the host-port bridge — is a deliberate, config-only, network-mode-gated, per-connection-audited hole in the egress boundary, reusing the existing vsock service-bridge plumbing with zero new guest code.

**Tech Stack:** Rust (clap, tokio, serde/serde_json, toml), Cloud Hypervisor + virtiofsd, vsock + socat.

**Spec:** `docs/superpowers/specs/2026-06-14-orchestrator-integration-design.md`

**Branch:** `docs/orchestrator-integration-spec` already exists with the spec. Either continue on it or create `feat/orchestrator-integration` off `main`. Never commit to `main` (AGENTS.md).

**Quality gate (run before every commit):**
```bash
RUSTUP_TOOLCHAIN=stable cargo fmt --all
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```
(Local env pins `RUSTUP_TOOLCHAIN=1.94.0`; CI uses stable, so always run clippy/test with `RUSTUP_TOOLCHAIN=stable` to avoid local-pass/CI-fail.)

---

## File Structure

**Modify:**
- `crates/abox-cli/src/commands/run.rs` — `--input-file` flag, parsing/validation, env injection, host-port gating, `--prompt` help text.
- `crates/abox-core/src/vm.rs` — `InputFile` struct; `input_files` field on `VmConfig`.
- `crates/abox-core/src/sandbox.rs` — `input_files` + `host_port_bridges` on `CreateSandboxParams`; thread into `VmConfig`; spawn host-port serve tasks in `run_sandbox`.
- `crates/abox-core/src/adapters/cloud_hypervisor.rs` — stage input files; pipe + classify virtiofsd stderr.
- `crates/abox-core/src/services.rs` — `HostPortBridge` config, `HostPortPlan`, `plan_host_port_bridges`, `serve_host_port_bridge`.
- `crates/abox-core/src/project.rs` — `host_ports` field on `ProjectConfig`.
- `crates/abox-core/src/audit.rs` — `log_host_port` inherent method.
- `crates/abox-core/src/proxy_bridge.rs` — `log_host_port` on `AuditSink` trait + impls.
- `crates/abox-cli/src/commands/list.rs` — `ListArgs` + `--json`.
- `crates/abox-cli/src/commands/divergence.rs` — `--json`.
- `crates/abox-cli/src/commands/grant.rs` — `--json` on `List`.
- `crates/abox-cli/src/main.rs` — `List(ListArgs)`, `Path` subcommand wiring.
- `crates/abox-cli/src/commands/mod.rs` — declare `path` module.
- `docs/explainer.md`, `docs/tutorial.md`, `docs/future-work.md` — host-port honesty + self-hosted-model routing.

**Create:**
- `crates/abox-cli/src/commands/path.rs` — `abox path <task>`.

---

## Task 0: Preflight — confirm deps

- [ ] **Step 1: Verify serde/serde_json are available to abox-cli**

Run:
```bash
grep -E '^serde(_json)?\b|^serde =|^serde_json =' crates/abox-cli/Cargo.toml
```
Expected: both `serde` (with `derive`) and `serde_json` appear. If either is missing, add to `crates/abox-cli/Cargo.toml` under `[dependencies]`:
```toml
serde = { workspace = true }
serde_json = { workspace = true }
```
(Use whatever form the other crates use — check `crates/abox-core/Cargo.toml` for the exact spec.)

- [ ] **Step 2: Build to confirm baseline is green**

Run: `RUSTUP_TOOLCHAIN=stable cargo build --workspace`
Expected: clean build.

---

## Task 1: #28 — Document the managed-agent constraint on `--prompt`/`--prompt-file`

**Files:**
- Modify: `crates/abox-cli/src/commands/run.rs:68-74`

- [ ] **Step 1: Update the help text**

Replace the existing doc comments at `run.rs:68-74`:
```rust
    /// Inline prompt content for known managed agents.
    #[arg(long, conflicts_with = "prompt_file")]
    pub prompt: Option<String>,

    /// Load prompt content from a file on the host.
    #[arg(long, conflicts_with = "prompt")]
    pub prompt_file: Option<PathBuf>,
```
with:
```rust
    /// Inline prompt content. Only known managed agents (claude, codex) can
    /// consume a prompt; for any other `--` command use `--input-file` instead.
    #[arg(long, conflicts_with = "prompt_file")]
    pub prompt: Option<String>,

    /// Load prompt content from a file on the host. Only known managed agents
    /// (claude, codex) can consume it; for any other `--` command use
    /// `--input-file` instead.
    #[arg(long, conflicts_with = "prompt")]
    pub prompt_file: Option<PathBuf>,
```

- [ ] **Step 2: Verify the help renders**

Run: `RUSTUP_TOOLCHAIN=stable cargo run -q -p abox-cli -- run --help`
Expected: the `--prompt` and `--prompt-file` entries mention "managed agents (claude, codex)" and "--input-file".

- [ ] **Step 3: Commit**

```bash
git add crates/abox-cli/src/commands/run.rs
git commit -m "docs(run): note managed-agent-only constraint on --prompt/--prompt-file (#28)"
```

---

## Task 2: #24 — `--input-file` parsing and validation (pure functions)

**Files:**
- Modify: `crates/abox-cli/src/commands/run.rs` (add flag, parse + validate fns, unit tests)

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `run.rs` (create the block if absent):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_file_derives_guest_name_from_basename() {
        let spec = parse_input_file_arg("/tmp/data/bundle.json").unwrap();
        assert_eq!(spec.host_path, std::path::PathBuf::from("/tmp/data/bundle.json"));
        assert_eq!(spec.guest_name, "bundle.json");
    }

    #[test]
    fn input_file_accepts_explicit_guest_name() {
        let spec = parse_input_file_arg("/tmp/x.json:task.json").unwrap();
        assert_eq!(spec.host_path, std::path::PathBuf::from("/tmp/x.json"));
        assert_eq!(spec.guest_name, "task.json");
    }

    #[test]
    fn input_file_rejects_traversal_guest_name() {
        assert!(parse_input_file_arg("/tmp/x.json:..").is_err());
        assert!(parse_input_file_arg("/tmp/x.json:a/b").is_err());
        assert!(parse_input_file_arg("/tmp/x.json:.").is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli input_file_ 2>&1 | head -20`
Expected: FAIL — `parse_input_file_arg` / `InputFileSpec` not found.

- [ ] **Step 3: Implement the parser + validator**

Add near `parse_env_var` in `run.rs` (after line 127):
```rust
/// A parsed `--input-file` argument: a host file plus the name it will take
/// inside `/abox-meta/inputs/` in the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InputFileSpec {
    host_path: PathBuf,
    guest_name: String,
}

/// Validate a guest-side input file name. Must be a single safe path component
/// so it cannot escape `/abox-meta/inputs/` or collide with reserved meta files.
fn validate_guest_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        anyhow::bail!("{name:?} is not a valid file name");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        anyhow::bail!("{name:?} may contain only ASCII letters, digits, '.', '_', '-'");
    }
    Ok(())
}

/// Parse `<hostpath>[:<guestname>]`. The guest name is taken after the last
/// `:` when that suffix is a plain file name (no `/`); otherwise the whole
/// string is the host path and the guest name is the host file's basename.
fn parse_input_file_arg(s: &str) -> Result<InputFileSpec> {
    let (host_str, guest_name) = match s.rsplit_once(':') {
        Some((host, name)) if !name.is_empty() && !name.contains('/') => {
            (host.to_string(), name.to_string())
        }
        _ => {
            let derived = Path::new(s)
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "--input-file {s:?}: cannot derive a guest file name; \
                         specify one as <hostpath>:<name>"
                    )
                })?;
            (s.to_string(), derived)
        }
    };
    validate_guest_name(&guest_name)
        .map_err(|e| anyhow::anyhow!("--input-file {s:?}: {e}"))?;
    Ok(InputFileSpec { host_path: PathBuf::from(host_str), guest_name })
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli input_file_`
Expected: 3 tests PASS.

- [ ] **Step 5: Add the CLI flag**

In `RunArgs` (after the `prompt_file` field, ~line 74) add:
```rust
    /// Stage an arbitrary host file into `/abox-meta/inputs/` (read-only) for
    /// any `--` command. Format: `<hostpath>[:<guestname>]`. Repeatable.
    /// The guest sees `ABOX_INPUT_DIR=/abox-meta/inputs`, plus
    /// `ABOX_INPUT_FILE` when exactly one is given.
    #[arg(long = "input-file")]
    pub input_files: Vec<String>,
```

- [ ] **Step 6: Commit**

```bash
git add crates/abox-cli/src/commands/run.rs
git commit -m "feat(run): add --input-file flag with parsing + validation (#24)"
```

---

## Task 3: #24 — Thread `InputFile` through the config structs

**Files:**
- Modify: `crates/abox-core/src/vm.rs` (add `InputFile`, field on `VmConfig`)
- Modify: `crates/abox-core/src/sandbox.rs` (field on `CreateSandboxParams`, set in `VmConfig` build)
- Modify: all `CreateSandboxParams { .. }` literals (compiler-guided)

- [ ] **Step 1: Add the `InputFile` type and `VmConfig` field**

In `vm.rs`, after the `CredentialToStage` struct (line 36), add:
```rust
/// A host file to stage read-only into `/abox-meta/inputs/<guest_name>`.
#[derive(Debug, Clone)]
pub struct InputFile {
    /// Absolute path to the file on the host.
    pub host_path: PathBuf,
    /// File name inside `/abox-meta/inputs/` (validated single component).
    pub guest_name: String,
}
```
In `VmConfig` (after the `services` field, ~line 83) add:
```rust
    /// Arbitrary host files staged read-only under `/abox-meta/inputs/`.
    /// Decoupled from the managed-agent prompt; usable by any `--` command.
    pub input_files: Vec<InputFile>,
```

- [ ] **Step 2: Add the `CreateSandboxParams` field**

In `sandbox.rs`, after the `service_bridges` field (line 58) add:
```rust
    /// Arbitrary host files to stage read-only under `/abox-meta/inputs/`.
    pub input_files: Vec<crate::vm::InputFile>,
```

- [ ] **Step 3: Set the field in the `VmConfig` build**

In `sandbox.rs`, in the `VmConfig { .. }` literal at line 265, after the `services: …collect(),` block (line 286) add:
```rust
            input_files: params.input_files.clone(),
```

- [ ] **Step 4: Build and let the compiler list every broken literal**

Run: `RUSTUP_TOOLCHAIN=stable cargo build --workspace --tests 2>&1 | grep -E 'missing field|CreateSandboxParams' | head`
Expected: errors at `run.rs:346`, `snapshot.rs:226`, `env.rs:354`, and 12 sites in `crates/abox-core/tests/integration_tests.rs`.

- [ ] **Step 5: Add the field to every `CreateSandboxParams` literal**

In each flagged literal, add this line before the closing `};`:
```rust
        input_files: Vec::new(),
```
For `crates/abox-cli/src/commands/run.rs:346`, add it right after `service_bridges,` (line 367). The 12 integration-test literals and the `snapshot.rs`/`env.rs` literals take the same `input_files: Vec::new(),` line.

- [ ] **Step 6: Build to verify all literals compile**

Run: `RUSTUP_TOOLCHAIN=stable cargo build --workspace --tests`
Expected: clean build.

- [ ] **Step 7: Commit**

```bash
git add crates/abox-core/src/vm.rs crates/abox-core/src/sandbox.rs \
        crates/abox-cli/src/commands/run.rs crates/abox-cli/src/commands/snapshot.rs \
        crates/abox-cli/src/commands/env.rs crates/abox-core/tests/integration_tests.rs
git commit -m "feat(core): thread InputFile through CreateSandboxParams and VmConfig (#24)"
```

---

## Task 4: #24 — Validate inputs, inject env vars (run.rs), and stage them (adapter)

**Files:**
- Modify: `crates/abox-cli/src/commands/run.rs` (resolve specs, existence + size checks, env vars, set param)
- Modify: `crates/abox-core/src/adapters/cloud_hypervisor.rs` (extract + test `stage_input_files`, call it)

- [ ] **Step 1: Write the failing staging test**

In `cloud_hypervisor.rs`, add to its `#[cfg(test)] mod tests` block:
```rust
    #[test]
    fn stage_input_files_copies_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("abox-inputs-{}", std::process::id()));
        let src_dir = tmp.join("src");
        let meta_dir = tmp.join("meta");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&meta_dir).unwrap();
        let host = src_dir.join("payload.json");
        std::fs::write(&host, b"{\"k\":1}").unwrap();

        let files = vec![crate::vm::InputFile {
            host_path: host.clone(),
            guest_name: "payload.json".to_string(),
        }];
        super::stage_input_files(&meta_dir, &files).unwrap();

        let dest = meta_dir.join("inputs").join("payload.json");
        assert_eq!(std::fs::read(&dest).unwrap(), b"{\"k\":1}");
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o444);
        let _ = std::fs::remove_dir_all(&tmp);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core stage_input_files 2>&1 | head -20`
Expected: FAIL — `stage_input_files` not found.

- [ ] **Step 3: Implement `stage_input_files` and call it**

In `cloud_hypervisor.rs`, add this free function near the top-level helpers (e.g. after `auxiliary_virtiofsd_args`, ~line 68):
```rust
/// Stage arbitrary host input files read-only under `<meta_dir>/inputs/`.
///
/// Each `guest_name` is a validated single path component (see the CLI), so
/// `join` cannot escape the inputs directory. Files are copied at mode `0444`.
fn stage_input_files(
    meta_dir: &std::path::Path,
    input_files: &[crate::vm::InputFile],
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    if input_files.is_empty() {
        return Ok(());
    }
    let inputs_dir = meta_dir.join("inputs");
    std::fs::create_dir_all(&inputs_dir)
        .with_context(|| format!("Creating inputs dir {}", inputs_dir.display()))?;
    for f in input_files {
        let dest = inputs_dir.join(&f.guest_name);
        std::fs::copy(&f.host_path, &dest).with_context(|| {
            format!("Staging input file {} -> {}", f.host_path.display(), dest.display())
        })?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o444))
            .with_context(|| format!("Setting input file read-only: {}", dest.display()))?;
    }
    Ok(())
}
```
Then call it in the staging sequence: immediately before the `// Stage the services file …` comment at line 303, add:
```rust
        stage_input_files(&meta_dir, &config.input_files)?;
```

- [ ] **Step 4: Run to verify it passes**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core stage_input_files`
Expected: PASS.

- [ ] **Step 5: Resolve, validate, and inject env vars in run.rs**

In `run.rs::execute`, after `env_vars` is built (after line 266) and before `ensure_managed_agent_ready` (line 268), add:
```rust
    // Resolve --input-file specs, validate they exist and are within the size
    // budget, and expose them to any command via /abox-meta/inputs.
    const MAX_INPUT_FILE_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_INPUT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
    let input_specs: Vec<InputFileSpec> =
        args.input_files.iter().map(|s| parse_input_file_arg(s)).collect::<Result<_>>()?;
    let mut input_total: u64 = 0;
    let mut input_files: Vec<abox_core::vm::InputFile> = Vec::with_capacity(input_specs.len());
    for spec in &input_specs {
        let meta = std::fs::metadata(&spec.host_path).with_context(|| {
            format!("--input-file: cannot read host file {}", spec.host_path.display())
        })?;
        if meta.len() > MAX_INPUT_FILE_BYTES {
            anyhow::bail!(
                "--input-file {}: {} bytes exceeds the {} byte per-file limit",
                spec.host_path.display(),
                meta.len(),
                MAX_INPUT_FILE_BYTES
            );
        }
        input_total += meta.len();
        if input_total > MAX_INPUT_TOTAL_BYTES {
            anyhow::bail!(
                "--input-file: total staged input exceeds the {} byte limit",
                MAX_INPUT_TOTAL_BYTES
            );
        }
        input_files.push(abox_core::vm::InputFile {
            host_path: spec.host_path.clone(),
            guest_name: spec.guest_name.clone(),
        });
    }
    if !input_files.is_empty() {
        env_vars.push(("ABOX_INPUT_DIR".to_string(), "/abox-meta/inputs".to_string()));
        if let [only] = input_files.as_slice() {
            env_vars.push((
                "ABOX_INPUT_FILE".to_string(),
                format!("/abox-meta/inputs/{}", only.guest_name),
            ));
        }
    }
```
Then in the `CreateSandboxParams { .. }` literal (line 346), replace the placeholder `input_files: Vec::new(),` added in Task 3 with:
```rust
        input_files,
```

- [ ] **Step 6: Build + clippy + test**

Run:
```bash
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/abox-cli/src/commands/run.rs crates/abox-core/src/adapters/cloud_hypervisor.rs
git commit -m "feat: stage --input-file payloads read-only with env discovery (#24)"
```

---

## Task 5: #25 — `HostPortBridge` config + `ProjectConfig.host_ports`

**Files:**
- Modify: `crates/abox-core/src/services.rs` (config struct)
- Modify: `crates/abox-core/src/project.rs` (field + serde test)

- [ ] **Step 1: Write the failing config round-trip test**

In `project.rs`'s `#[cfg(test)] mod tests`, add:
```rust
    #[test]
    fn parses_host_ports_section() {
        let toml = r#"
[[host_ports]]
guest = 4000
host = 4000
"#;
        let cfg: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.host_ports.len(), 1);
        assert_eq!(cfg.host_ports[0].guest, 4000);
        assert_eq!(cfg.host_ports[0].host, 4000);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core parses_host_ports 2>&1 | head -20`
Expected: FAIL — no `host_ports` field.

- [ ] **Step 3: Add the config struct**

In `services.rs`, after the `GuestServiceBridge` struct (line 125), add:
```rust
/// A repo-declared bridge from a guest loopback port to an existing host
/// loopback service. Unlike `[services]` sidecars, abox launches nothing —
/// it splices the guest port to a port the operator already runs on the host.
///
/// This is an explicit hole in the egress boundary: it is refused in `safe`
/// network mode and every connection through it is written to the audit log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostPortBridge {
    /// Port the agent connects to inside the guest (`127.0.0.1:<guest>`).
    pub guest: u16,
    /// Existing host loopback port to splice to (`127.0.0.1:<host>`).
    pub host: u16,
}
```

- [ ] **Step 4: Add the `ProjectConfig` field**

In `project.rs`, in `ProjectConfig` after the `services` field (line 245) add:
```rust
    /// Repo-declared host-port bridges. Each splices a guest loopback port to
    /// an existing host loopback service. Refused in `safe` network mode and
    /// audited per connection — an explicit egress-boundary exception.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_ports: Vec<crate::services::HostPortBridge>,
```

- [ ] **Step 5: Run to verify it passes**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core parses_host_ports`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/abox-core/src/services.rs crates/abox-core/src/project.rs
git commit -m "feat(config): add [[host_ports]] bridge declaration (#25)"
```

---

## Task 6: #25 — Plan, audit, and serve host-port bridges

**Files:**
- Modify: `crates/abox-core/src/audit.rs` (inherent `log_host_port`)
- Modify: `crates/abox-core/src/proxy_bridge.rs` (trait method + impls)
- Modify: `crates/abox-core/src/services.rs` (`HostPortPlan`, `plan_host_port_bridges`, `serve_host_port_bridge`)

- [ ] **Step 1: Write the failing plan test**

In `services.rs`'s `#[cfg(test)] mod tests`, add:
```rust
    #[test]
    fn host_port_plans_allocate_vsock_after_services() {
        let cfg = vec![
            HostPortBridge { guest: 4000, host: 4000 },
            HostPortBridge { guest: 8080, host: 9000 },
        ];
        // Two sidecar services already occupy SERVICE_VSOCK_BASE + 0..=1.
        let plans = plan_host_port_bridges(&cfg, 2);
        assert_eq!(plans[0].vsock_port, SERVICE_VSOCK_BASE + 2);
        assert_eq!(plans[1].vsock_port, SERVICE_VSOCK_BASE + 3);
        assert_eq!(plans[0].guest().name, "hostport-4000");
        assert_eq!(plans[1].guest().guest_port, 8080);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core host_port_plans 2>&1 | head -20`
Expected: FAIL — `plan_host_port_bridges` / `HostPortPlan` not found.

- [ ] **Step 3: Implement plan + serve in services.rs**

In `services.rs`, after `plan_service_bridge` (line 159) add:
```rust
/// A planned host-port bridge with its allocated vsock port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPortPlan {
    pub guest_port: u16,
    pub host_port: u16,
    pub vsock_port: u32,
}

impl HostPortPlan {
    /// Project to the guest-visible bridge line written into `/abox-meta/services`.
    pub fn guest(&self) -> GuestServiceBridge {
        GuestServiceBridge {
            name: format!("hostport-{}", self.host_port),
            guest_port: self.guest_port,
            vsock_port: self.vsock_port,
        }
    }
}

/// Plan host-port bridges, allocating vsock ports immediately after the
/// `service_count` sidecar bridges so the two ranges never collide.
pub fn plan_host_port_bridges(
    bridges: &[HostPortBridge],
    service_count: usize,
) -> Vec<HostPortPlan> {
    bridges
        .iter()
        .enumerate()
        .map(|(i, b)| HostPortPlan {
            guest_port: b.guest,
            host_port: b.host,
            vsock_port: SERVICE_VSOCK_BASE + (service_count + i) as u32,
        })
        .collect()
}

/// Like [`serve_service_bridge`], but for an operator-declared host port:
/// logs an audit entry at setup and on every accepted connection, since this
/// bypasses the egress proxy and is a deliberate boundary exception.
pub async fn serve_host_port_bridge(
    socket_path: PathBuf,
    guest_port: u16,
    host_port: u16,
    sandbox_id: String,
    audit: std::sync::Arc<dyn crate::proxy_bridge::AuditSink>,
) -> Result<()> {
    use tokio::net::{TcpStream, UnixListener};

    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Binding host-port bridge socket {}", socket_path.display()))?;
    audit.log_host_port(&sandbox_id, "host-port-bridge", guest_port, host_port);
    tracing::info!(socket = %socket_path.display(), guest_port, host_port, "Host-port bridge listening");

    loop {
        let (mut guest, _) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "Host-port bridge accept error");
                continue;
            }
        };
        audit.log_host_port(&sandbox_id, "host-port-connect", guest_port, host_port);
        tokio::spawn(async move {
            match TcpStream::connect(("127.0.0.1", host_port)).await {
                Ok(mut upstream) => {
                    if let Err(e) = tokio::io::copy_bidirectional(&mut guest, &mut upstream).await {
                        tracing::debug!(error = %e, host_port, "Host-port bridge copy ended");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, host_port, "Host-port bridge could not reach host service");
                }
            }
        });
    }
}
```

- [ ] **Step 4: Add the audit method (inherent)**

In `audit.rs`, after `log_egress` (line 391) add:
```rust
    /// Log a host-port bridge event (`host-port-bridge` at setup,
    /// `host-port-connect` per connection). Target encodes the port mapping.
    pub fn log_host_port(
        &self,
        sandbox_id: &str,
        event: &str,
        guest_port: u16,
        host_port: u16,
    ) {
        self.append(
            event,
            sandbox_id,
            &format!("guest:{guest_port}->host:{host_port}"),
            "",
            "allowed",
            0,
        );
    }
```

- [ ] **Step 5: Add the trait method + impls**

In `proxy_bridge.rs`, in the `AuditSink` trait after `log_egress` (line 60) add:
```rust
    /// Record a host-port bridge event. Default impl emits a tracing event so
    /// sinks that don't persist it still surface the boundary crossing.
    fn log_host_port(&self, sandbox_id: &str, event: &str, guest_port: u16, host_port: u16) {
        tracing::info!(
            sandbox_id = %sandbox_id,
            event = %event,
            guest_port,
            host_port,
            "host-port"
        );
    }
```
In `impl AuditSink for FileAuditSink` (after its `log_egress`, line 351) add:
```rust
    fn log_host_port(&self, sandbox_id: &str, event: &str, guest_port: u16, host_port: u16) {
        self.writer.log_host_port(sandbox_id, event, guest_port, host_port);
        tracing::info!(
            sandbox_id = %sandbox_id,
            event = %event,
            guest_port,
            host_port,
            "host-port"
        );
    }
```
In `impl AuditSink for crate::audit::AuditChainWriter` (after its `log_egress`, line 375) add:
```rust
    fn log_host_port(&self, sandbox_id: &str, event: &str, guest_port: u16, host_port: u16) {
        crate::audit::AuditChainWriter::log_host_port(self, sandbox_id, event, guest_port, host_port);
    }
```

- [ ] **Step 6: Run to verify it passes**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core host_port_plans`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/abox-core/src/services.rs crates/abox-core/src/audit.rs crates/abox-core/src/proxy_bridge.rs
git commit -m "feat(core): plan + serve + audit host-port bridges (#25)"
```

---

## Task 7: #25 — Gate in run.rs and spawn in run_sandbox

**Files:**
- Modify: `crates/abox-cli/src/commands/run.rs` (gating fn + test, plan, set param)
- Modify: `crates/abox-core/src/sandbox.rs` (`host_port_bridges` param, VmConfig wiring, spawn loop)

- [ ] **Step 1: Write the failing gating test**

In `run.rs`'s `#[cfg(test)] mod tests`, add:
```rust
    #[test]
    fn host_ports_refused_in_safe_mode() {
        use abox_core::services::HostPortBridge;
        use abox_core::project::NetworkMode;
        let hp = vec![HostPortBridge { guest: 4000, host: 4000 }];
        assert!(ensure_host_ports_allowed(&hp, NetworkMode::Safe).is_err());
        assert!(ensure_host_ports_allowed(&hp, NetworkMode::Scoped).is_ok());
        assert!(ensure_host_ports_allowed(&[], NetworkMode::Safe).is_ok());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli host_ports_refused 2>&1 | head -20`
Expected: FAIL — `ensure_host_ports_allowed` not found.

- [ ] **Step 3: Implement the gating function**

In `run.rs`, after `ensure_managed_agent_ready` (line 248) add:
```rust
/// Refuse `[[host_ports]]` unless the effective network mode permits an
/// unmediated path to a host service. `safe` means "only the host-managed
/// surface", which a host-port bridge is not.
fn ensure_host_ports_allowed(
    host_ports: &[abox_core::services::HostPortBridge],
    mode: NetworkMode,
) -> Result<()> {
    if !host_ports.is_empty() && mode == NetworkMode::Safe {
        anyhow::bail!(
            "[[host_ports]] requires network mode 'scoped' or 'open', but the \
             effective mode is 'safe'.\n\n\
             A host-port bridge gives the sandbox an unmediated path to a host \
             service, so it is refused in 'safe' mode. Set network.mode = \
             \"scoped\" in .abox/project.toml (or pass --network scoped) to enable it."
        );
    }
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli host_ports_refused`
Expected: PASS.

- [ ] **Step 5: Wire gating + planning into execute**

Ordering matters: `project_config` is **moved** at line 273 (`match project_config`), `network_scope` is computed at lines 293-297, and `service_bridges` is not assigned until line 339. Make three insertions:

(a) Immediately after line 272 (`let project_services = …;`) and **before** the `match project_config` at line 273, capture the host-port config while `project_config` is still borrowable:
```rust
    let project_host_ports =
        project_config.as_ref().map(|c| c.host_ports.clone()).unwrap_or_default();
```

(b) Immediately after `network_scope` is computed (after line 297), gate on the effective mode (this needs no `service_bridges`):
```rust
    let effective_mode = network_scope.as_ref().map_or(NetworkMode::Safe, |s| s.mode);
    ensure_host_ports_allowed(&project_host_ports, effective_mode)?;
```

(c) Immediately after `service_bridges` is assigned (after the `let service_bridges = start_project_services(…)?;` block ending ~line 339), allocate the vsock ports past the sidecars:
```rust
    let host_port_bridges =
        abox_core::services::plan_host_port_bridges(&project_host_ports, service_bridges.len());
```

Then in the `CreateSandboxParams { .. }` literal (line 346), after `service_bridges,` add:
```rust
        host_port_bridges,
```

- [ ] **Step 6: Add the `CreateSandboxParams` field + VmConfig wiring + spawn**

In `sandbox.rs`, in `CreateSandboxParams` after the `input_files` field (added Task 3) add:
```rust
    /// Repo-declared, gated host-port bridges (guest loopback → host loopback).
    pub host_port_bridges: Vec<crate::services::HostPortPlan>,
```
In the `VmConfig { .. }` build, replace the `services:` block (lines 282-286) with:
```rust
            services: params
                .service_bridges
                .iter()
                .map(crate::services::ServiceBridge::guest)
                .chain(params.host_port_bridges.iter().map(crate::services::HostPortPlan::guest))
                .collect(),
```
In `run_sandbox`, after `let service_bridges = params.service_bridges.clone();` (line 428) add:
```rust
        let host_port_bridges = params.host_port_bridges.clone();
```
After the service-bridge spawn loop (after line 577) add:
```rust
        // Spawn a host→guest bridge for each declared host-port. Reuses the
        // service-bridge vsock plumbing but logs every connection, since this
        // is a deliberate, audited exception to the egress boundary.
        let mut host_port_handles = Vec::new();
        for plan in &host_port_bridges {
            let socket = self
                .config
                .runtime_dir()
                .join(format!("vsock-{task_id}.sock_{}", plan.vsock_port));
            let guest_port = plan.guest_port;
            let host_port = plan.host_port;
            let sandbox_id = task_id.clone();
            let audit = std::sync::Arc::clone(&audit_sink);
            host_port_handles.push(tokio::spawn(async move {
                if let Err(e) = crate::services::serve_host_port_bridge(
                    socket, guest_port, host_port, sandbox_id, audit,
                )
                .await
                {
                    tracing::error!(host_port, error = %e, "host-port bridge crashed");
                }
            }));
        }
```
Then in the teardown section, alongside the existing `for handle in service_bridge_handles { handle.abort(); }` (line 640-642) add:
```rust
        for handle in host_port_handles {
            handle.abort();
        }
```

- [ ] **Step 7: Update the other `CreateSandboxParams` literals**

Run: `RUSTUP_TOOLCHAIN=stable cargo build --workspace --tests 2>&1 | grep 'missing field' | head`
Add `host_port_bridges: Vec::new(),` to every flagged literal (`snapshot.rs`, `env.rs`, the 12 integration-test sites). `run.rs` already sets it (Step 5).

- [ ] **Step 8: Build + clippy + test**

Run:
```bash
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/abox-cli/src/commands/run.rs crates/abox-core/src/sandbox.rs \
        crates/abox-cli/src/commands/snapshot.rs crates/abox-cli/src/commands/env.rs \
        crates/abox-core/tests/integration_tests.rs
git commit -m "feat: gate + spawn host-port bridges, refused in safe mode (#25)"
```

---

## Task 8: #27 — `abox list --json`

**Files:**
- Modify: `crates/abox-cli/src/commands/list.rs`
- Modify: `crates/abox-cli/src/main.rs`

- [ ] **Step 1: Write the failing serialization test**

In `list.rs`, add a `#[cfg(test)] mod tests`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use abox_core::sandbox::SandboxStatus;

    #[test]
    fn list_json_has_stable_fields() {
        let s = SandboxStatus {
            id: "t".into(),
            branch: "agent/t".into(),
            worktree_path: "/w/t".into(),
            vm_state: "running".into(),
            vm_pid: 42,
            commits_ahead: 3,
        };
        let item = ListItem::from(&s);
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["id"], "t");
        assert_eq!(json["state"], "running");
        assert_eq!(json["pid"], 42);
        assert_eq!(json["ahead"], 3);
        assert_eq!(json["worktree_path"], "/w/t");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli list_json_has 2>&1 | head -20`
Expected: FAIL — `ListItem` / `ListArgs` not found.

- [ ] **Step 3: Implement `ListArgs`, `ListItem`, and the `--json` branch**

Replace the whole body of `list.rs` with:
```rust
//! `abox list` — List all active sandboxes.

use abox_core::sandbox::{SandboxOrchestrator, SandboxStatus};
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Emit machine-readable JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// Stable JSON contract for one sandbox. Field names are a supported API.
#[derive(Debug, Serialize)]
pub struct ListItem {
    pub id: String,
    pub branch: String,
    pub state: String,
    pub pid: u32,
    pub ahead: usize,
    pub worktree_path: String,
}

impl From<&SandboxStatus> for ListItem {
    fn from(s: &SandboxStatus) -> Self {
        Self {
            id: s.id.clone(),
            branch: s.branch.clone(),
            state: s.vm_state.clone(),
            pid: s.vm_pid,
            ahead: s.commits_ahead,
            worktree_path: s.worktree_path.clone(),
        }
    }
}

pub async fn execute<W: WorkspacePort, V: VmPort>(
    args: &ListArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    let sandboxes = orchestrator.list_sandboxes().await?;

    if args.json {
        let items: Vec<ListItem> = sandboxes.iter().map(ListItem::from).collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if sandboxes.is_empty() {
        println!("No active sandboxes.");
        return Ok(());
    }

    println!("{:<16} {:<24} {:<10} {:<8} {:<8}", "ID", "BRANCH", "STATE", "PID", "AHEAD");
    println!("{}", "-".repeat(70));
    for s in &sandboxes {
        println!(
            "{:<16} {:<24} {:<10} {:<8} {:<8}",
            s.id, s.branch, s.vm_state, s.vm_pid, s.commits_ahead
        );
    }
    println!();
    println!("{} sandbox(es) active", sandboxes.len());
    Ok(())
}
```

- [ ] **Step 4: Update main.rs**

In `main.rs`, change the `List` variant (line 58-60):
```rust
    /// List all active sandboxes.
    #[command(alias = "ls")]
    List(commands::list::ListArgs),
```
And the dispatch (line 258):
```rust
        Commands::List(ref args) => commands::list::execute(args, &orchestrator).await,
```

- [ ] **Step 5: Run to verify it passes**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli list_json_has`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/abox-cli/src/commands/list.rs crates/abox-cli/src/main.rs
git commit -m "feat(list): add --json output (#27)"
```

---

## Task 9: #27 — `abox divergence --json`

**Files:**
- Modify: `crates/abox-cli/src/commands/divergence.rs`

- [ ] **Step 1: Write the failing test**

In `divergence.rs`, add:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergence_item_json_fields() {
        let item = DivergenceItem {
            file: "src/a.rs".into(),
            sandbox: "t".into(),
            status: "modified".into(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["file"], "src/a.rs");
        assert_eq!(json["sandbox"], "t");
        assert_eq!(json["status"], "modified");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli divergence_item_json 2>&1 | head -20`
Expected: FAIL — `DivergenceItem` not found.

- [ ] **Step 3: Implement the `--json` flag, item struct, and branch**

In `divergence.rs`, add the import and flag. Change the top of the file to include:
```rust
use serde::Serialize;
```
Add `json` to `DivergenceArgs`:
```rust
#[derive(Debug, Args)]
pub struct DivergenceArgs {
    /// Base branch to compare against. Default: "main".
    #[arg(long, default_value = "main")]
    pub base: String,
    /// Emit machine-readable JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}
```
Add the item struct after `DivergenceArgs`:
```rust
/// Stable JSON contract for one (file, sandbox) divergence entry.
#[derive(Debug, Serialize)]
pub struct DivergenceItem {
    pub file: String,
    pub sandbox: String,
    pub status: String,
}
```
In `execute`, right after `let entries = orchestrator.divergence(&args.base)?;` (line 21) add:
```rust
    if args.json {
        let items: Vec<DivergenceItem> = entries
            .iter()
            .map(|e| DivergenceItem {
                file: e.file_path.clone(),
                sandbox: e.sandbox_id.clone(),
                status: e.status.to_string(),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-cli divergence_item_json`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/abox-cli/src/commands/divergence.rs
git commit -m "feat(divergence): add --json output (#27)"
```

---

## Task 10: #27 — `abox grant list --json`

**Files:**
- Modify: `crates/abox-cli/src/commands/grant.rs`

- [ ] **Step 1: Add `--json` to the `List` action and thread it**

In `grant.rs`, change the `List` variant (line 128-133):
```rust
    /// List all configured credential injection rules.
    List {
        /// Path to policy file. Defaults to ~/.abox/policies/default.toml.
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
```
Update the match arm (line 165-168):
```rust
        GrantAction::List { policy, json } => {
            let policy_path = resolve_policy_path(policy.as_ref(), config);
            list_grants(&policy_path, *json)
        }
```

- [ ] **Step 2: Implement the JSON branch in `list_grants`**

Change `list_grants` signature (line 297) to `fn list_grants(policy_path: &PathBuf, json: bool) -> Result<()>`. After the rules are parsed — replace the `Some(rules) => { …table… }` arm (lines 326-356) so it first handles JSON:
```rust
        Some(rules) => {
            if json {
                let items: Vec<serde_json::Value> = rules
                    .iter()
                    .map(|rule| {
                        let domain = rule.get("domain").and_then(|v| v.as_str()).unwrap_or("?");
                        let header =
                            rule.get("inject_header").and_then(|v| v.as_str()).unwrap_or("?");
                        let source = if let Some(env) =
                            rule.get("env_var").and_then(|v| v.as_str())
                        {
                            format!("env:{env}")
                        } else if let Some(file) =
                            rule.get("credential_file").and_then(|v| v.as_str())
                        {
                            format!("file:{file}")
                        } else {
                            "?".to_string()
                        };
                        let request_rules = rule
                            .get("request_rules")
                            .and_then(|v| v.as_array())
                            .map_or(0, toml::value::Array::len);
                        serde_json::json!({
                            "domain": domain,
                            "header": header,
                            "source": source,
                            "request_rules": request_rules,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
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
```
**Security note:** the JSON intentionally carries only `source` (the env/file *name*), never the resolved credential value — mirroring the table.

- [ ] **Step 3: Build + verify no secret values leak**

Run: `RUSTUP_TOOLCHAIN=stable cargo build -p abox-cli`
Expected: clean. Confirm by inspection that the JSON object contains only `domain`, `header`, `source`, `request_rules`.

- [ ] **Step 4: Commit**

```bash
git add crates/abox-cli/src/commands/grant.rs
git commit -m "feat(grant): add --json to 'grant list', metadata only (#27)"
```

---

## Task 11: #27 — `abox path <task>`

**Files:**
- Create: `crates/abox-cli/src/commands/path.rs`
- Modify: `crates/abox-cli/src/commands/mod.rs`
- Modify: `crates/abox-cli/src/main.rs`

- [ ] **Step 1: Create the command**

Create `crates/abox-cli/src/commands/path.rs`:
```rust
//! `abox path` — Print the host worktree path for a sandbox.
//!
//! The worktree is the bind-mounted `/workspace`, so this is the supported way
//! to collect what an agent wrote without hardcoding `~/.abox/worktrees/<task>`.

use abox_core::sandbox::SandboxOrchestrator;
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct PathArgs {
    /// The task/sandbox identifier.
    pub task: String,
}

pub fn execute<W: WorkspacePort, V: VmPort>(
    args: &PathArgs,
    orchestrator: &SandboxOrchestrator<W, V>,
) -> Result<()> {
    match orchestrator.worktree_info(&args.task)? {
        Some(info) => {
            println!("{}", info.path.display());
            Ok(())
        }
        None => anyhow::bail!("No sandbox named '{}'.", args.task),
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/abox-cli/src/commands/mod.rs`, add (keeping alphabetical order if the file uses it):
```rust
pub mod path;
```

- [ ] **Step 3: Wire into main.rs**

In `main.rs`, add a variant after `Stop` (line 66):
```rust
    /// Print the host worktree path for a sandbox (for collecting results).
    Path(commands::path::PathArgs),
```
Add a dispatch arm near the other sync commands (next to `Divergence`, line 261):
```rust
        Commands::Path(ref args) => commands::path::execute(args, &orchestrator),
```

- [ ] **Step 4: Build + smoke the help**

Run:
```bash
RUSTUP_TOOLCHAIN=stable cargo build -p abox-cli
RUSTUP_TOOLCHAIN=stable cargo run -q -p abox-cli -- path --help
```
Expected: clean build; help shows `<TASK>`.

- [ ] **Step 5: Commit**

```bash
git add crates/abox-cli/src/commands/path.rs crates/abox-cli/src/commands/mod.rs crates/abox-cli/src/main.rs
git commit -m "feat: add 'abox path <task>' to expose the worktree contract (#27)"
```

---

## Task 12: #26 — Quiet benign virtiofsd credential noise

**Files:**
- Modify: `crates/abox-core/src/adapters/cloud_hypervisor.rs`

- [ ] **Step 1: Write the failing classifier test**

In `cloud_hypervisor.rs`'s `#[cfg(test)] mod tests`, add:
```rust
    #[test]
    fn classifies_benign_virtiofsd_credential_noise() {
        assert!(is_benign_virtiofsd_credential_noise(
            "[ERROR virtiofsd::passthrough::credentials] failed to change uid back to root: Invalid argument (os error 22)"
        ));
        assert!(is_benign_virtiofsd_credential_noise(
            "[ERROR virtiofsd::passthrough::credentials] failed to change gid back to root: Invalid argument (os error 22)"
        ));
        // A genuine error must NOT be classified as benign.
        assert!(!is_benign_virtiofsd_credential_noise(
            "[ERROR virtiofsd] failed to mount shared dir: Permission denied"
        ));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core classifies_benign 2>&1 | head -20`
Expected: FAIL — `is_benign_virtiofsd_credential_noise` not found.

- [ ] **Step 3: Implement the classifier + stderr forwarder**

In `cloud_hypervisor.rs`, near the virtiofsd arg helpers (after `auxiliary_virtiofsd_args`, ~line 68), add:
```rust
/// True only for the exact, known-benign virtiofsd messages emitted when its
/// passthrough credential code restores uid/gid to 0 inside our rootless user
/// namespace (no CAP_SETUID for uid 0 there). Matched tightly so a real
/// virtiofsd privilege error is never downgraded.
fn is_benign_virtiofsd_credential_noise(line: &str) -> bool {
    line.contains("failed to change uid back to root: Invalid argument")
        || line.contains("failed to change gid back to root: Invalid argument")
}

/// Drain a virtiofsd child's stderr, downgrading the benign credential noise to
/// debug and forwarding every other line at warn so real errors stay visible.
fn forward_virtiofsd_stderr(mut child: Child, label: &'static str) -> Child {
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if is_benign_virtiofsd_credential_noise(&line) {
                    tracing::debug!(target: "virtiofsd", instance = label, "{line}");
                } else {
                    tracing::warn!(target: "virtiofsd", instance = label, "{line}");
                }
            }
        });
    }
    child
}
```
Ensure `use std::process::Stdio;` is present near the top imports (add it if missing).

- [ ] **Step 4: Pipe stderr at each spawn site**

For each of the four virtiofsd spawns (workspace ~376, meta ~388, status ~399, cache ~409), add `.stderr(Stdio::piped())` before `.spawn()` and wrap the resulting child. Apply this pattern:

Workspace (lines 372-378) becomes:
```rust
        let mut cmd = Command::new(self.resolve_binary("virtiofsd")?);
        for a in &virtiofsd_args {
            cmd.arg(a);
        }
        let virtiofsd_child = cmd.stderr(Stdio::piped()).kill_on_drop(true).spawn().context(
            "Failed to start workspace virtiofsd. Run scripts/bootstrap_vm.sh to install it.",
        )?;
        let virtiofsd_child = forward_virtiofsd_stderr(virtiofsd_child, "workspace");
```
Meta (lines 388-389):
```rust
        let meta_virtiofsd_child = meta_cmd
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start meta virtiofsd")?;
        let meta_virtiofsd_child = forward_virtiofsd_stderr(meta_virtiofsd_child, "meta");
```
Status (lines 399-400):
```rust
        let status_virtiofsd_child = status_cmd
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start status virtiofsd")?;
        let status_virtiofsd_child = forward_virtiofsd_stderr(status_virtiofsd_child, "status");
```
Cache (line 409):
```rust
            Some(forward_virtiofsd_stderr(
                cache_cmd
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .context("Failed to start cache virtiofsd")?,
                "cache",
            ))
```

- [ ] **Step 5: Run to verify the classifier passes + everything builds**

Run:
```bash
RUSTUP_TOOLCHAIN=stable cargo test -p abox-core classifies_benign
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
```
Expected: test PASS, clippy clean.

- [ ] **Step 6: Confirm-benign-first on a real run (manual, needs KVM)**

Run a sandbox (`RUSTUP_TOOLCHAIN=stable just tier-vm` or `scripts/local/e2e_test.sh`) and confirm:
1. The run still exits cleanly (rc=0), proving the credential restore failure is benign.
2. The four `failed to change uid/gid back to root` lines no longer appear at ERROR/WARN in the console.
If the run does NOT exit cleanly, STOP — the message is not benign and must be investigated, not muted.

- [ ] **Step 7: Commit**

```bash
git add crates/abox-core/src/adapters/cloud_hypervisor.rs
git commit -m "fix(virtiofsd): downgrade benign rootless credential noise, keep real errors (#26)"
```

---

## Task 13: Documentation — honest egress boundary + self-hosted-model routing

**Files:**
- Modify: `docs/explainer.md` (sections 5 and 8)
- Modify: `docs/tutorial.md` and/or `docs/future-work.md`

- [ ] **Step 1: Amend the "no SSRF vector" claims**

In `docs/explainer.md` section 5 (vsock) and section 8 (egress), add a carve-out paragraph after the existing "no SSRF vector / no unmediated outbound" statements:
```markdown
> **Exception — declared host-port bridges.** In `scoped`/`open` mode a repo may
> declare `[[host_ports]]` in `.abox/project.toml` to splice a specific guest
> loopback port to an existing host loopback service. This is the one
> operator-authorized, version-controlled exception to "no unmediated outbound":
> it is refused in `safe` mode and every connection is recorded in the audit log
> (`host-port-bridge` at setup, `host-port-connect` per connection). Prefer the
> egress proxy + a `scoped` egress rule whenever the service is reachable over
> the network; the bridge exists for loopback-only host services.
```

- [ ] **Step 2: Document the self-hosted-model routing**

In `docs/tutorial.md` (or `docs/future-work.md`, wherever model setup is discussed), add a short subsection:
```markdown
### Reaching a self-hosted model

Prefer the mediated path: if your model endpoint (e.g. vLLM on Kubernetes) is
reachable over the network, add it as a `scoped` egress rule so requests stay
behind the abox proxy (policy-checked, audited, credential injection available).

Only when the model gateway is bound to host loopback (e.g. a LiteLLM gateway on
`localhost:4000`) and cannot be exposed otherwise, declare a host-port bridge:

\`\`\`toml
# .abox/project.toml — requires network.mode = "scoped" (or "open")
[[host_ports]]
guest = 4000
host  = 4000
\`\`\`

The agent then reaches the gateway at `127.0.0.1:4000` inside the guest. Pair
this with `--input-file` to hand a custom runner its task payload.
```
(The `\`\`\`` fences above are escaped for this plan; write real triple-backtick fences.)

- [ ] **Step 3: Verify docs render and links are intact**

Run: `RUSTUP_TOOLCHAIN=stable cargo build --workspace` (sanity) and re-read both edited docs.
Expected: prose is accurate and consistent with the implemented gating/audit behavior.

- [ ] **Step 4: Commit**

```bash
git add docs/explainer.md docs/tutorial.md docs/future-work.md
git commit -m "docs: carve out host-port bridge in egress claims; self-hosted model routing (#25)"
```

---

## Final verification

- [ ] **Full quality gate**

```bash
RUSTUP_TOOLCHAIN=stable cargo fmt --all -- --check
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```
Expected: all clean.

- [ ] **VM-backed end-to-end (needs KVM)** — see AGENTS.md pre-PR checklist; touching VM/guest/proxy code requires `just e2e-vm` and the `vm-attested` PR label. Validate at minimum:
  - `abox run --task t --input-file /tmp/payload.json -- /bin/sh -lc 'cat "$ABOX_INPUT_FILE"'` prints the payload.
  - With `network.mode = "scoped"` and `[[host_ports]] guest=4000 host=4000`, a guest `curl 127.0.0.1:4000` reaches a host `python -m http.server 4000`, and `abox audit show` lists `host-port-connect`.
  - The same `[[host_ports]]` under `safe` mode is refused with the gating error.
  - `abox list --json`, `abox divergence --json`, `abox grant list --json` emit parseable JSON; `abox path t` prints the worktree dir.
  - A clean run shows no virtiofsd credential ERROR lines.

- [ ] **Update CHANGELOG-relevant docs** per AGENTS.md pre-PR checklist (README/explainer/config example) and open a PR off the feature branch.
