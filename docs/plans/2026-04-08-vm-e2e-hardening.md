# VM End-to-End Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drain the P0 and P1 items from [`docs/backlog/2026-04-08-vm-e2e-mvp-followups.md`](../backlog/2026-04-08-vm-e2e-mvp-followups.md) — plus a handful of small P2s — so the VM-end-to-end path is correct (propagates exit codes, fixes policy bypass), robust (graceful console drain, CI gate), and ergonomic (PATH symlinks, `--detach`). Ship a clean branch with every behavior change covered by tests and a green `just check` + `./scripts/e2e_test.sh`.

**Architecture:** Each backlog item is its own task. Behavior changes follow TDD (failing test → implementation → passing test → commit). The big structural addition is a **third virtiofs share** (`aboxstatus`, read-write) that the guest init writes the agent exit code to before poweroff; the orchestrator reads it back before returning. Everything else is a focused, small-surface fix. F3 (HTTPS credential injection) is the one L-sized backlog item — it is explicitly **not implemented** here; instead, Task 16 writes a dedicated spec at `docs/plans/2026-04-08-credential-injection.md` so a future session can pick it up.

**Tech Stack:** Rust (abox-core, abox-cli, abox-shim, abox-proxyd), tokio, serde, virtiofsd, cloud-hypervisor, bash (bootstrap + e2e), GitHub Actions (CI).

---

## File Structure

**New files:**
- `.github/workflows/ci.yml` — GitHub Actions CI (Task 12)
- `docs/plans/2026-04-08-credential-injection.md` — deferred F3 spec (Task 16)

**Modified files (by task):**
- Task 1 (F2 exit code): `crates/abox-core/src/adapters/cloud_hypervisor.rs`, `crates/abox-core/src/sandbox.rs`, `guest/init.sh`, `crates/abox-core/tests/integration_tests.rs`, `crates/abox-core/src/vm.rs` (doc comment), `scripts/e2e_test.sh` (phase 6 assertion)
- Task 2 (console drain / D5): `crates/abox-core/src/console.rs`, `crates/abox-core/src/sandbox.rs`
- Task 3 (policy bypass / S1): `crates/abox-core/src/policy.rs`
- Task 4 (bootstrap PATH symlinks / D1): `scripts/bootstrap_vm.sh`, `docs/vm-setup.md`
- Task 5 (`abox run --detach` / F1): `crates/abox-cli/src/commands/run.rs`, `crates/abox-core/src/sandbox.rs`, `crates/abox-cli/src/commands/stop.rs`, `crates/abox-core/src/config.rs`
- Task 6 (S2/S3/S4 cleanup sweep): `crates/abox-core/src/adapters/cli_proxy.rs` or `crates/abox-proxyd/src/cli_proxy.rs`, `crates/abox-shim/src/main.rs`
- Task 7 (F4 template create stub fix): `crates/abox-cli/src/commands/template.rs`
- Task 8 (F5 dashboard refresh): `crates/abox-cli/src/tui/dashboard.rs`
- Task 9 (D2 tunable timing constants): `crates/abox-core/src/config.rs`, `crates/abox-core/src/sandbox.rs`, `crates/abox-core/src/console.rs`
- Task 10 (D3 musl target opt-in): `scripts/bootstrap_vm.sh`
- Task 11 (D4 console output assertion): `scripts/e2e_test.sh`
- Task 12 (D6 CI workflow): `.github/workflows/ci.yml`, `.gitignore`
- Task 13 (H3 e2e trap robustness): `scripts/e2e_test.sh`
- Task 14 (docs sweep): `README.md`, `docs/vm-setup.md`, `CONTRIBUTING.md`, `docs/plans/2026-04-07-vm-end-to-end-mvp.md` (retrospective), `docs/decisions/002-aboxstatus-share.md` (new ADR)
- Task 15 (backlog file updates): `docs/backlog/2026-04-08-vm-e2e-mvp-followups.md`
- Task 16 (F3 deferred spec): `docs/plans/2026-04-08-credential-injection.md` (new)
- Task 17 (tutorial): `docs/tutorial.md` (new)
- Task 18 (ELI5 explainer): `docs/explainer.md` (new)
- Task 19 (final verification): runs `just check` + `./scripts/e2e_test.sh`, pushes branch

**Explicitly deferred (not in this plan):**
- F3 (HTTPS credential injection) — Task 16 writes the spec only
- S2 (egress audit attribution) — blocked on F3
- H1 (squash-merge commit cleanup) — controller preference, not code

**Single-responsibility decomposition:**
- The new `aboxstatus` virtiofs share is a third read-write share introduced cleanly alongside the existing `workspace` + `aboxmeta` shares. `cloud_hypervisor.rs` grows a small `status_dir` field and a third `virtiofsd_child`; no other file owns that lifecycle.
- Policy bypass (S1) is fixed in `PolicyEngine::evaluate_cli` by parsing out `-c key=val` / `-C path` global options before the subcommand match. The fix is confined to one method.
- `--detach` (F1) reuses `run_sandbox`; the CLI spawns the future as a tokio task when the flag is set, writes the PID file, and returns. No orchestrator changes beyond exposing a PID file helper.
- Dashboard refresh (F5) uses an existing ratatui pattern: a `tokio::time::interval` polls the orchestrator and queues a redraw.

---

## Task 1: F2 — Propagate the guest agent exit code

**Files:**
- Modify: `crates/abox-core/src/adapters/cloud_hypervisor.rs` (add `status_dir` field, third virtiofsd, pass `aboxstatus` to CH)
- Modify: `crates/abox-core/src/sandbox.rs` (`run_sandbox` reads `/<status_dir>/exit-code` before returning)
- Modify: `guest/init.sh` (mount `/abox-status`, write `$?` there before poweroff)
- Modify: `crates/abox-core/src/vm.rs` (add doc comment for status_dir if a new config field is needed; none required if adapter owns it)
- Modify: `scripts/e2e_test.sh` (phase 6: assert `abox run` surfaces a non-zero exit when the guest command fails)
- Test: `crates/abox-core/tests/integration_tests.rs` (unit test for `read_exit_code` helper)

- [ ] **Step 1: Write a failing unit test for `read_exit_code(dir)`**

Add to `crates/abox-core/tests/integration_tests.rs`:

```rust
#[test]
fn test_read_exit_code_present() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("exit-code"), "42\n").unwrap();
    let code = abox_core::adapters::cloud_hypervisor::read_exit_code(tmp.path())
        .expect("read_exit_code succeeds");
    assert_eq!(code, 42);
}

#[test]
fn test_read_exit_code_missing_file_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    // No exit-code file written.
    let result = abox_core::adapters::cloud_hypervisor::read_exit_code(tmp.path());
    // Missing file is a legitimate "VM died without writing" — return None.
    assert!(result.is_none() || result == Some(-1));
}

#[test]
fn test_read_exit_code_malformed_returns_negative() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("exit-code"), "not-a-number").unwrap();
    let code = abox_core::adapters::cloud_hypervisor::read_exit_code(tmp.path());
    // Malformed content: caller treats as failure.
    assert_eq!(code, None);
}
```

- [ ] **Step 2: Run the test; it fails because `read_exit_code` doesn't exist**

```bash
cargo test -p abox-core --test integration_tests read_exit_code 2>&1 | tail -15
```
Expected: compilation error, `cannot find function read_exit_code`.

- [ ] **Step 3: Implement `read_exit_code` in `cloud_hypervisor.rs`**

Add at the top of the file (after the struct definitions):

```rust
/// Read the guest agent's exit code from a staged status directory.
///
/// The guest init script writes the agent's exit status to
/// `<status_dir>/exit-code` as a single-line integer before poweroff.
/// Returns `None` if the file is missing (the VM crashed or was killed
/// before writing) or if the file contents don't parse as an i32.
pub fn read_exit_code(status_dir: &std::path::Path) -> Option<i32> {
    let contents = std::fs::read_to_string(status_dir.join("exit-code")).ok()?;
    contents.trim().parse::<i32>().ok()
}
```

- [ ] **Step 4: Run the test again; expect pass**

```bash
cargo test -p abox-core --test integration_tests read_exit_code 2>&1 | tail -10
```
Expected: 3 passed.

- [ ] **Step 5: Add the `aboxstatus` virtiofs share to `CloudHypervisorAdapter::start`**

In `crates/abox-core/src/adapters/cloud_hypervisor.rs`, modify `start()`:

After the `meta_dir` definition, add:

```rust
        let status_dir = self.runtime_dir.join(format!("status-{}", config.id));
        let status_socket = self.runtime_dir.join(format!("virtiofs-status-{}.sock", config.id));
        // Ensure status_dir exists (empty at boot — the guest writes into it).
        std::fs::create_dir_all(&status_dir).with_context(|| {
            format!("Failed to create status dir {}", status_dir.display())
        })?;
        // Pre-create an empty exit-code file so virtiofsd has something to serve
        // and the guest can truncate it without permission errors.
        std::fs::write(status_dir.join("exit-code"), "").ok();
```

Add `status_socket` to the cleanup list (alongside `virtiofs_socket` etc.).

After the meta virtiofsd spawn block, add a third:

```rust
        let status_virtiofsd_child = Command::new("virtiofsd")
            .arg(format!("--socket-path={}", status_socket.display()))
            .arg(format!("--shared-dir={}", status_dir.display()))
            .arg("--cache=never")
            .arg("--sandbox=none")
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start status virtiofsd")?;

        Self::wait_for_socket(&status_socket, 5000)
            .await
            .context("status virtiofsd socket did not appear within 5 seconds")?;
```

In the `--fs` arguments passed to `cloud-hypervisor`, append a third positional value:

```rust
            .arg(format!(
                "tag=aboxstatus,socket={},num_queues=1,queue_size=256",
                status_socket.display()
            ))
```

Add the new field to `RunningVm`:

```rust
struct RunningVm {
    ch_child: Child,
    virtiofsd_child: Child,
    meta_virtiofsd_child: Child,
    status_virtiofsd_child: Child,
    meta_dir: PathBuf,
    status_dir: PathBuf,
    api_socket: PathBuf,
    console_socket: PathBuf,
    #[allow(dead_code)]
    config: VmConfig,
}
```

Populate it in the insert:

```rust
        let running = RunningVm {
            ch_child,
            virtiofsd_child,
            meta_virtiofsd_child,
            status_virtiofsd_child,
            meta_dir: meta_dir.clone(),
            status_dir: status_dir.clone(),
            api_socket: api_socket.clone(),
            console_socket: console_socket.clone(),
            config,
        };
```

Update `stop()` to kill the third virtiofsd:

```rust
            let _ = vm.status_virtiofsd_child.kill().await;
```

Update `cleanup_vm_files()` to clean the new socket + status dir:

```rust
    fn cleanup_vm_files(&self, id: &str, vm: &RunningVm) {
        for suffix in ["virtiofs", "virtiofs-meta", "virtiofs-status", "ch-api", "vsock"] {
            let sock = self.runtime_dir.join(format!("{suffix}-{id}.sock"));
            let _ = std::fs::remove_file(sock);
        }
        let _ = std::fs::remove_file(&vm.console_socket);
        let _ = std::fs::remove_file(self.runtime_dir.join(format!("vsock-{id}.sock_5000")));
        let _ = std::fs::remove_dir_all(&vm.meta_dir);
        let _ = std::fs::remove_dir_all(&vm.status_dir);
    }
```

- [ ] **Step 6: Expose the status_dir so the orchestrator can read it**

Add a public accessor in `cloud_hypervisor.rs` (the orchestrator needs to know where to look after the VM exits):

```rust
impl CloudHypervisorAdapter {
    /// Runtime status directory for a given sandbox id. Exists as long
    /// as the VM has been started; cleaned up in `stop()`/`cleanup_vm_files`.
    pub fn status_dir(&self, id: &str) -> PathBuf {
        self.runtime_dir.join(format!("status-{}", id))
    }
}
```

**Important:** the orchestrator must read the exit-code **before** `cleanup_vm_files` removes `status_dir`. In the current code, `cleanup_vm_files` runs inside `info()` after `ch_child` exits (see `cloud_hypervisor.rs:276`). This means `run_sandbox`'s polling loop would read the file *after* it has been deleted.

Fix: defer the `status_dir` removal. Change `cleanup_vm_files` to accept a flag:

```rust
    fn cleanup_vm_files(&self, id: &str, vm: &RunningVm, remove_status_dir: bool) {
        for suffix in ["virtiofs", "virtiofs-meta", "virtiofs-status", "ch-api", "vsock"] {
            let sock = self.runtime_dir.join(format!("{suffix}-{id}.sock"));
            let _ = std::fs::remove_file(sock);
        }
        let _ = std::fs::remove_file(&vm.console_socket);
        let _ = std::fs::remove_file(self.runtime_dir.join(format!("vsock-{id}.sock_5000")));
        let _ = std::fs::remove_dir_all(&vm.meta_dir);
        if remove_status_dir {
            let _ = std::fs::remove_dir_all(&vm.status_dir);
        }
    }
```

- Pass `false` from `info()` (VM exit detection) — leave status_dir for the orchestrator to read
- Pass `true` from `stop()` (explicit teardown) — safe to remove everything

- [ ] **Step 7: Update `guest/init.sh` to mount and write the exit code**

Replace the current runner-invocation block with:

```sh
# Status share (writable) for reporting the agent's exit code back to the host.
mkdir -p /abox-status
mount -t virtiofs aboxstatus /abox-status 2>/dev/null || \
    echo "WARNING: failed to mount aboxstatus virtiofs"

if [ -f /abox-meta/runner.sh ]; then
    echo "==> running /abox-meta/runner.sh"
    sh /abox-meta/runner.sh
    RC=$?
else
    echo "==> no /abox-meta/runner.sh found"
    RC=127
fi

# Report exit code back to host through the writable status share.
echo "$RC" > /abox-status/exit-code 2>/dev/null || \
    echo "WARNING: could not write /abox-status/exit-code"
sync
```

- [ ] **Step 8: Update `run_sandbox` in `sandbox.rs` to read the exit code**

The orchestrator currently only has a trait reference (`vm_manager: V` where `V: VmPort`), so it cannot call `CloudHypervisorAdapter::status_dir` directly. Extend the `VmPort` trait with a default method:

In `crates/abox-core/src/vm.rs`:

```rust
pub trait VmPort: Send + Sync {
    // ... existing methods ...

    /// Return the path to the status directory for a given sandbox id.
    /// Default: none (in-memory adapters don't have one).
    fn status_dir(&self, _id: &str) -> Option<std::path::PathBuf> {
        None
    }
}
```

Override it in `CloudHypervisorAdapter`:

```rust
impl VmPort for CloudHypervisorAdapter {
    // ... existing methods ...

    fn status_dir(&self, id: &str) -> Option<std::path::PathBuf> {
        Some(self.runtime_dir.join(format!("status-{}", id)))
    }
}
```

In `sandbox.rs`, `run_sandbox`, replace the final `Ok(0)` with:

```rust
        bridge_handle.abort();
        console_handle.abort();

        // Read the exit code the guest wrote into /abox-status/exit-code.
        // If the status dir isn't available (in-memory adapter) or the file
        // is missing/malformed, fall back to 0.
        let exit_code = self
            .vm_manager
            .status_dir(&task_id)
            .and_then(|d| crate::adapters::cloud_hypervisor::read_exit_code(&d))
            .unwrap_or(0);

        // Tear down the status dir now that we've read it.
        if let Some(sd) = self.vm_manager.status_dir(&task_id) {
            let _ = std::fs::remove_dir_all(&sd);
        }

        Ok(exit_code)
    }
```

- [ ] **Step 9: Update existing `VmConfig` construction in `integration_tests.rs` (none needed — no new field on VmConfig)**

Verify by running:

```bash
cargo build -p abox-core 2>&1 | tail -15
```
Expected: clean build.

- [ ] **Step 10: Run all core tests**

```bash
cargo test -p abox-core 2>&1 | tail -15
```
Expected: all pass, including the three new `read_exit_code` tests.

- [ ] **Step 11: Add a phase-6 assertion that a failing guest command surfaces a non-zero exit**

In `scripts/e2e_test.sh`, **inside** the phase-6 `else` branch (after the successful `git status` test), add:

```bash
    step "Non-zero agent exit propagates to abox run"
    how 'abox run --task vm-e2e-fail -- /bin/sh -c "exit 7"'
    expect "abox run exits with 7"
    if timeout 90 $ABOX run --task vm-e2e-fail --base main -- /bin/sh -c "exit 7" >"$SCRATCH/fail-run.log" 2>&1; then
        fail "exit code propagation" "abox run returned 0 but guest exited 7"
    else
        rc=$?
        if [[ "$rc" == "7" ]]; then
            pass "exit code propagation (rc=7)"
        else
            fail "exit code propagation" "expected rc=7, got rc=$rc"
            tail -20 "$SCRATCH/fail-run.log" | sed "s/^/    /"
        fi
    fi
    $ABOX stop vm-e2e-fail --clean 2>/dev/null || true
```

- [ ] **Step 12: Run the e2e script end-to-end (bootstrap already done)**

```bash
./scripts/e2e_test.sh 2>&1 | tail -30
```
Expected: `✓ e2e PASSED` with the new assertion included.

- [ ] **Step 13: Commit**

```bash
git add crates/abox-core/src/adapters/cloud_hypervisor.rs \
        crates/abox-core/src/sandbox.rs \
        crates/abox-core/src/vm.rs \
        crates/abox-core/tests/integration_tests.rs \
        guest/init.sh \
        scripts/e2e_test.sh
git commit -m "feat(vm): propagate guest agent exit code via aboxstatus share

Adds a third read-write virtiofs share ('aboxstatus', mounted at
/abox-status in the guest) that guest/init.sh writes the agent's
exit code into before poweroff. The orchestrator reads it from the
host-side status dir after the VM exits and returns it from
run_sandbox instead of hardcoded Ok(0).

Phase 6 of the e2e test now exercises the failure path by running
'sh -c exit 7' inside the guest and asserting abox run exits 7.

Closes F2 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 2: D5 — Console tail exit signal (graceful drain)

**Files:**
- Modify: `crates/abox-core/src/console.rs`
- Modify: `crates/abox-core/src/sandbox.rs`
- Test: inline `#[tokio::test]` in `console.rs`

- [ ] **Step 1: Write a failing test for `tail_to_stdout_until`**

Add at the bottom of `crates/abox-core/src/console.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn test_tail_drains_remaining_bytes_after_shutdown_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("console.log");
        std::fs::write(&log, b"initial ").unwrap();

        let notify = std::sync::Arc::new(Notify::new());
        let notify_clone = notify.clone();
        let log_path = log.clone();
        let handle = tokio::spawn(async move {
            // We can't easily capture stdout from a test, so we use the
            // generic reader variant and collect bytes into a Vec.
            let mut out = Vec::<u8>::new();
            tail_to_writer_until(&log_path, &mut out, notify_clone)
                .await
                .unwrap();
            out
        });

        // Give the pump a moment to read "initial ", then append more, then signal.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(b"final").unwrap();
        drop(f);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        notify.notify_one();

        let out = handle.await.unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("initial"), "missing initial; got: {s:?}");
        assert!(s.contains("final"), "drain lost final bytes; got: {s:?}");
    }
}
```

- [ ] **Step 2: Run the test; it fails**

```bash
cargo test -p abox-core --lib console 2>&1 | tail -15
```
Expected: compilation error — `tail_to_writer_until` doesn't exist.

- [ ] **Step 3: Refactor console.rs to take a shutdown signal**

Replace the body of `console.rs` with:

```rust
//! Stream a Cloud Hypervisor console log file to the orchestrator's stdout.
//!
//! `cloud-hypervisor --console file=<path>` writes the guest's serial
//! console output to a plain file. This module tails that file and writes
//! new bytes to the orchestrator's stdout so the user sees live guest
//! output.
//!
//! The pump exits gracefully when a `Notify` is signalled: it performs one
//! final read-to-EOF before returning so the last ~50 ms of output is not
//! dropped.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Notify;

/// Tail `path`, streaming new bytes to `stdout` until `shutdown` is notified.
/// Performs a final drain before returning so no bytes are lost.
pub async fn tail_to_stdout_until(path: &Path, shutdown: Arc<Notify>) -> Result<()> {
    let mut stdout = tokio::io::stdout();
    tail_to_writer_until(path, &mut stdout, shutdown).await
}

/// Generic variant used by tests. Tails `path` into any `AsyncWrite` sink.
pub async fn tail_to_writer_until<W>(
    path: &Path,
    sink: &mut W,
    shutdown: Arc<Notify>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    // Wait up to ~5 s for the file to appear.
    for _ in 0..200 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    if !path.exists() {
        anyhow::bail!("console log never appeared: {}", path.display());
    }

    let mut file = File::open(path)
        .await
        .with_context(|| format!("opening console log: {}", path.display()))?;
    file.seek(std::io::SeekFrom::Start(0)).await?;

    let mut buf = [0u8; 8192];
    loop {
        tokio::select! {
            biased;
            _ = shutdown.notified() => {
                // Final drain: read until EOF.
                drain_to_eof(&mut file, sink, &mut buf).await?;
                return Ok(());
            }
            read = file.read(&mut buf) => {
                let n = read?;
                if n == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                sink.write_all(&buf[..n]).await?;
                sink.flush().await?;
            }
        }
    }
}

async fn drain_to_eof<R, W>(file: &mut R, sink: &mut W, buf: &mut [u8]) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let n = file.read(buf).await?;
        if n == 0 {
            sink.flush().await?;
            return Ok(());
        }
        sink.write_all(&buf[..n]).await?;
    }
}

/// Legacy wrapper kept for callers that don't use a shutdown signal.
///
/// This calls `tail_to_stdout_until` with a Notify that is never triggered,
/// so it behaves exactly like the previous infinite-loop implementation.
/// New call sites should prefer `tail_to_stdout_until` so the pump can
/// drain cleanly on shutdown.
pub async fn tail_to_stdout(path: &Path) -> Result<()> {
    tail_to_stdout_until(path, Arc::new(Notify::new())).await
}
```

- [ ] **Step 4: Run the new test**

```bash
cargo test -p abox-core --lib console 2>&1 | tail -15
```
Expected: 1 passed.

- [ ] **Step 5: Wire the shutdown signal into `run_sandbox`**

In `crates/abox-core/src/sandbox.rs`, `run_sandbox`, replace the console handle creation with:

```rust
        let console_log = self.config.runtime_dir().join(format!("console-{task_id}.log"));
        let console_shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
        let console_shutdown_for_task = console_shutdown.clone();
        let console_handle = tokio::spawn(async move {
            if let Err(e) = Box::pin(
                crate::console::tail_to_stdout_until(&console_log, console_shutdown_for_task),
            )
            .await
            {
                tracing::debug!(error = %e, "console stream ended");
            }
        });
```

And replace the post-loop shutdown with:

```rust
        bridge_handle.abort();
        // Signal the console tailer to drain and exit gracefully, then await it
        // with a short timeout. Fall back to abort if it doesn't finish.
        console_shutdown.notify_one();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), console_handle).await;
```

(The fallback is implicit — after the `.await` timeout, the handle is dropped and the task is cancelled.)

- [ ] **Step 6: Run all tests**

```bash
cargo test --workspace 2>&1 | tail -15
```
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/abox-core/src/console.rs crates/abox-core/src/sandbox.rs
git commit -m "feat(console): graceful drain via shutdown signal

The console tailer used to run in an infinite loop, killed only by
abort(). On slow systems this could drop the last ~50 ms of guest
output. Switch to tokio::select! with a Notify-based shutdown; when
the orchestrator signals, the pump reads to EOF and returns.

Closes D5 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 3: S1 — Policy regex bypass via global options

**Files:**
- Modify: `crates/abox-core/src/policy.rs` (evaluate_cli parsing)
- Test: inline `#[cfg(test)]` additions in `policy.rs`

- [ ] **Step 1: Write failing tests that demonstrate the bypass**

Add these tests to the existing `#[cfg(test)] mod tests` in `policy.rs`:

```rust
    #[test]
    fn test_git_force_push_via_dash_c_is_denied() {
        // Bypass attempt: prepend `-c` global options so the joined
        // args_str begins with "-c ..." rather than "push", defeating
        // the ^push\s+--force check and slipping past the deny regex.
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
        // We want the parser to be tight on denies but still let
        // ordinary workflows through.
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
    fn test_git_unknown_global_option_denied() {
        // Document the assumption: unknown global options are rejected
        // rather than silently stripped. This keeps the allow-list tight.
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
```

- [ ] **Step 2: Run; the first two should fail (bypass succeeds), the others may or may not fail**

```bash
cargo test -p abox-core --lib policy 2>&1 | tail -20
```
Expected: `test_git_force_push_via_dash_c_is_denied` and `test_git_force_push_via_dash_big_c_is_denied` both FAIL. The `_still_allowed` and `_unknown_global` tests may fail compilation-free but assert-wise.

- [ ] **Step 3: Implement global-option parsing in `evaluate_cli`**

Replace the current `evaluate_cli` body in `policy.rs` with:

```rust
    /// Evaluate a CLI command request.
    ///
    /// For `git` specifically, this strips a known set of global options
    /// (`-c key=val`, `-C path`, `--git-dir`, `--work-tree`, `--no-pager`,
    /// `-p`, `--paginate`, `--no-optional-locks`) from the front of `args`
    /// before matching against the allow/deny regex list. Any other leading
    /// dash-prefixed token is treated as an **unknown** global option and
    /// the request is denied.
    ///
    /// This prevents allow-list bypasses like
    /// `git -c core.hooks=./evil push --force`, which would otherwise slip
    /// through because the joined arg string didn't start with `push`.
    pub fn evaluate_cli(&self, command: &str, args: &[String]) -> Decision {
        // Strip known global options before matching.
        let stripped = match strip_global_options(command, args) {
            Ok(s) => s,
            Err(reason) => return Decision::Deny(reason),
        };
        let args_str = stripped.join(" ");

        let policy = self.cli_policies.iter().find(|p| p.command == command);

        let Some(policy) = policy else {
            return if self.default_cli_action == "allow" {
                Decision::Allow
            } else {
                Decision::Deny(format!("No policy for command '{command}'"))
            };
        };

        for pattern in &policy.deny_patterns {
            if pattern.is_match(&args_str) {
                return Decision::Deny(format!(
                    "Denied by pattern '{pattern}' for command '{command}'"
                ));
            }
        }

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
```

Add a free function below the impl block:

```rust
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

    // Git's documented global options that take an argument (two tokens).
    const TWO_TOKEN: &[&str] = &["-c", "-C", "--git-dir", "--work-tree", "--namespace"];
    // Git's documented global options that are flags (one token).
    const ONE_TOKEN_FLAGS: &[&str] =
        &["--no-pager", "-p", "--paginate", "--no-optional-locks", "--bare", "--no-replace-objects"];
    // Long options that use `--flag=value` (one token).
    const ONE_TOKEN_EQ_PREFIX: &[&str] = &[
        "--git-dir=",
        "--work-tree=",
        "--namespace=",
        "--super-prefix=",
        "--config-env=",
    ];

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

        // Unknown global option: reject rather than silently strip. This
        // is a deliberate deny-by-default — future git versions that add
        // new global flags will need an explicit update here, which is a
        // correct place to apply scrutiny.
        return Err(format!("Unknown git global option '{tok}' not in allow-list"));
    }

    // No subcommand at all — treat as deny.
    Err("git invocation has no subcommand".to_string())
}
```

- [ ] **Step 4: Run the policy tests**

```bash
cargo test -p abox-core --lib policy 2>&1 | tail -20
```
Expected: all policy tests pass (including the 4 new bypass tests).

- [ ] **Step 5: Run all tests to ensure no regressions**

```bash
cargo test --workspace 2>&1 | tail -15
```
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/abox-core/src/policy.rs
git commit -m "fix(policy): reject git global-option bypass of deny list

PolicyEngine::evaluate_cli used to join argv with spaces and match
the joined string against regexes like ^push\s+--force. An attacker
could slip past by prepending a global option (git -c foo=bar push
--force), which makes the joined string start with '-c foo=bar'
instead of 'push'.

Fix: parse out known git global options (-c, -C, --git-dir,
--work-tree, --no-pager, etc.) before the regex match, and reject
any unknown option-like token before the subcommand. This keeps the
allow-list tight while letting normal workflows through.

Closes S1 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 4: D1 — Bootstrap PATH symlinks

**Files:**
- Modify: `scripts/bootstrap_vm.sh`
- Modify: `docs/vm-setup.md` (remove the manual PATH export step; leave the troubleshooting section)

- [ ] **Step 1: Add a `--no-symlink` opt-out flag and the symlink phase to `bootstrap_vm.sh`**

After the artifact-version block and before `mkdir -p "$ABOX_VM_DIR"`, add:

```bash
# ─── Argument parsing ────────────────────────────────────────────────────
DO_SYMLINK=1
for arg in "$@"; do
    case "$arg" in
        --no-symlink) DO_SYMLINK=0 ;;
        --help|-h)
            cat <<EOF
Usage: $(basename "$0") [--no-symlink]

  --no-symlink   Do not create symlinks in ~/.local/bin. You will need
                 to add $ABOX_VM_DIR to your PATH manually.
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done
```

After the final `ls -lh "$ABOX_VM_DIR"` line, add:

```bash
# ─── Install convenience symlinks into ~/.local/bin ──────────────────────
if [[ "$DO_SYMLINK" == "1" ]]; then
    LOCAL_BIN="$HOME/.local/bin"
    mkdir -p "$LOCAL_BIN"
    for bin in cloud-hypervisor ch-remote virtiofsd; do
        ln -sf "$ABOX_VM_DIR/$bin" "$LOCAL_BIN/$bin"
        echo "  symlinked $LOCAL_BIN/$bin -> $ABOX_VM_DIR/$bin"
    done

    # Warn if ~/.local/bin isn't on PATH.
    case ":$PATH:" in
        *":$LOCAL_BIN:"*) : ;;  # already on PATH
        *)
            echo
            echo "WARNING: $LOCAL_BIN is not on your PATH."
            echo "Add this to your shell profile (e.g., ~/.bashrc):"
            echo '  export PATH="$HOME/.local/bin:$PATH"'
            ;;
    esac
fi
```

- [ ] **Step 2: Run the bootstrap to make sure it still succeeds (cached, fast)**

```bash
./scripts/bootstrap_vm.sh 2>&1 | tail -20
ls -l ~/.local/bin/cloud-hypervisor ~/.local/bin/virtiofsd ~/.local/bin/ch-remote
```
Expected: bootstrap prints "symlinked ..." lines, `ls` shows three symlinks pointing into `~/.abox/vm/`.

- [ ] **Step 3: Verify `--no-symlink` flag honored (test it without re-downloading)**

```bash
./scripts/bootstrap_vm.sh --no-symlink 2>&1 | grep -c symlinked
```
Expected: `0` (the symlinked lines are not printed).

- [ ] **Step 4: Update `docs/vm-setup.md` to reflect the new default**

Replace the paragraph after the rootfs.raw explanation:

```markdown
After bootstrap finishes, the binaries are symlinked into
`~/.local/bin/` automatically, so running `abox run` just works if
`~/.local/bin` is on your `PATH`. If it isn't, the script will print
a warning telling you what to add to your shell profile.

If you prefer to manage `PATH` yourself (e.g., for a shared install),
pass `--no-symlink` and add `$HOME/.abox/vm` to `PATH` manually:

```bash
./scripts/bootstrap_vm.sh --no-symlink
export PATH="$HOME/.abox/vm:$PATH"
```
```

- [ ] **Step 5: Commit**

```bash
git add scripts/bootstrap_vm.sh docs/vm-setup.md
git commit -m "feat(bootstrap): symlink VM binaries into ~/.local/bin by default

After bootstrap_vm.sh finishes, cloud-hypervisor, ch-remote, and
virtiofsd are symlinked into ~/.local/bin so 'abox run' works
without an extra PATH export. Pass --no-symlink to opt out. Warns
if ~/.local/bin isn't already on PATH.

Closes D1 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 5: F1 — `abox run --detach`

**Files:**
- Modify: `crates/abox-cli/src/commands/run.rs`
- Modify: `crates/abox-cli/src/commands/stop.rs` (read the detached PID file if present)
- Modify: `crates/abox-core/src/config.rs` (detach state dir helper, if not already present)

- [ ] **Step 1: Add `--detach` flag to `RunArgs`**

In `crates/abox-cli/src/commands/run.rs`, add:

```rust
    /// Detach after launching the sandbox instead of blocking on the agent.
    /// Console output is redirected to `<runtime>/console-<task>.log` and
    /// the PID of the supervisor task is written to `<runtime>/run-<task>.pid`.
    #[arg(long)]
    pub detach: bool,
```

- [ ] **Step 2: Implement detach in `execute`**

Replace the end of `execute()` with:

```rust
    if args.detach {
        // Fork-style detach: re-exec the current binary with the --detach
        // flag *removed* and stdout/stderr redirected to the console log,
        // then return immediately. The child will block on run_sandbox.
        let console_log = orchestrator.runtime_dir().join(format!("console-{}.log", args.task));
        let pid_file = orchestrator.runtime_dir().join(format!("run-{}.pid", args.task));
        std::fs::create_dir_all(orchestrator.runtime_dir())?;

        // Rebuild argv without --detach.
        let exe = std::env::current_exe()?;
        let mut child_args: Vec<String> = std::env::args().skip(1).collect();
        child_args.retain(|a| a != "--detach");

        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&console_log)?;
        let log_err = log.try_clone()?;

        use std::os::unix::process::CommandExt;
        let child = std::process::Command::new(&exe)
            .args(&child_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log))
            .stderr(std::process::Stdio::from(log_err))
            .process_group(0)
            .spawn()?;

        let child_pid = child.id();
        std::fs::write(&pid_file, child_pid.to_string())?;
        println!("Sandbox '{}' detached (pid {child_pid}).", args.task);
        println!("  logs:  {}", console_log.display());
        println!("  stop:  abox stop {}", args.task);
        // Intentionally do NOT await the child — we want the CLI to return.
        std::mem::forget(child);
        return Ok(());
    }

    println!("Sandbox '{}' starting...", args.task);
    let exit_code = orchestrator.run_sandbox(params, policy).await?;
    // ...rest unchanged
```

**Note:** This requires `SandboxOrchestrator::runtime_dir()` to exist. Add it if missing:

In `crates/abox-core/src/sandbox.rs`, add to the `impl` block:

```rust
    /// Runtime directory (where sockets and detached PID files live).
    pub fn runtime_dir(&self) -> std::path::PathBuf {
        self.config.runtime_dir()
    }
```

- [ ] **Step 3: Teach `abox stop` to kill the detached supervisor if its PID file exists**

In `crates/abox-cli/src/commands/stop.rs`, before calling `orchestrator.stop_sandbox`:

```rust
    // If this sandbox was launched with `abox run --detach`, kill the
    // supervisor process first so it can clean up the VM itself. If the
    // pid file is missing or the process is already gone, fall through
    // to the orchestrator's own stop path.
    let pid_file = orchestrator.runtime_dir().join(format!("run-{}.pid", args.task));
    if pid_file.exists() {
        if let Ok(s) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = s.trim().parse::<i32>() {
                // SIGTERM; the supervisor's run_sandbox loop reacts to ch_child exit.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
                // Best-effort wait for the PID file to disappear.
                for _ in 0..20 {
                    if !pid_file.exists() { break; }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        let _ = std::fs::remove_file(&pid_file);
    }
```

Add `libc = "0.2"` to `crates/abox-cli/Cargo.toml` dependencies if it isn't already present.

- [ ] **Step 4: Write an integration test for the detach argv-rebuild logic**

Add to `crates/abox-cli/src/commands/run.rs` (inline test):

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_rebuild_detach_free_argv() {
        let raw = vec![
            "run".to_string(),
            "--task".to_string(),
            "x".to_string(),
            "--detach".to_string(),
            "--".to_string(),
            "claude".to_string(),
        ];
        let rebuilt: Vec<String> = raw.into_iter().filter(|a| a != "--detach").collect();
        assert_eq!(rebuilt, vec!["run", "--task", "x", "--", "claude"]);
        assert!(!rebuilt.iter().any(|a| a == "--detach"));
    }
}
```

- [ ] **Step 5: Run all tests**

```bash
cargo test --workspace 2>&1 | tail -15
```
Expected: all pass.

- [ ] **Step 6: Smoke-test the detach path manually**

```bash
# In a scratch dir, run a sandbox detached; we expect the command to return
# almost immediately even though the sandbox is still "running".
# Because this requires a real VM we skip the full flow and just verify
# the flag parses and prints the expected message without erroring.
cargo run --release --bin abox -- run --help 2>&1 | grep -- --detach
```
Expected: the help text shows `--detach`.

- [ ] **Step 7: Commit**

```bash
git add crates/abox-cli/src/commands/run.rs crates/abox-cli/src/commands/stop.rs \
        crates/abox-core/src/sandbox.rs crates/abox-cli/Cargo.toml
git commit -m "feat(cli): add 'abox run --detach'

Re-execs the current binary without --detach, redirecting stdout/err
to <runtime>/console-<task>.log and writing the supervisor pid to
<runtime>/run-<task>.pid. 'abox stop <task>' reads the pid file and
SIGTERMs the supervisor before the usual orchestrator stop path.

Closes F1 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 6: S3 + S4 — Forward SSH agent env var; shim CWD via /proc/self/cwd

**Files:**
- Modify: `crates/abox-proxyd/src/cli_proxy.rs` (or `crates/abox-core/src/proxy_bridge.rs` wherever the child env is set)
- Modify: `crates/abox-shim/src/main.rs`

- [ ] **Step 1: Locate the child-process env block in the proxy bridge**

```bash
grep -n "Command::new\|env\b" crates/abox-core/src/proxy_bridge.rs | head -20
```

Identify the spot where the child command is configured (it's where the matched CliPolicy is executed).

- [ ] **Step 2: Wire `forward_ssh_agent`**

Add to the child construction, inside the match-policy block:

```rust
// If the matched policy says forward_ssh_agent=true, pass the host's
// SSH_AUTH_SOCK through so guest tools (git push to ssh remotes, etc.)
// can reach the host agent. Otherwise, unset it so the child can't
// accidentally inherit a parent's SSH_AUTH_SOCK.
if matched_policy.forward_ssh_agent {
    if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
        cmd.env("SSH_AUTH_SOCK", sock);
    }
} else {
    cmd.env_remove("SSH_AUTH_SOCK");
}
```

**Important:** the `CompiledCliPolicy` struct in `policy.rs` has `#[allow(dead_code)] forward_ssh_agent: bool`. Remove the `#[allow(dead_code)]` once this is wired up.

- [ ] **Step 3: Update `abox-shim` to prefer `/proc/self/cwd`**

In `crates/abox-shim/src/main.rs`, replace the `cwd` computation in `run()`:

```rust
    // CWD resolution order:
    //   1. ABOX_CWD env var (set by runner.sh, authoritative)
    //   2. /proc/self/cwd symlink target (more reliable than getcwd(2)
    //      on virtiofs mount points)
    //   3. getcwd(2) fallback
    //   4. hardcoded "/workspace"
    let cwd = std::env::var(CWD_OVERRIDE_ENV)
        .ok()
        .or_else(|| {
            std::fs::read_link("/proc/self/cwd")
                .ok()
                .and_then(|p| p.to_str().map(str::to_string))
        })
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| "/workspace".to_string());
```

- [ ] **Step 4: Add a policy test that confirms `forward_ssh_agent` is no longer dead code**

Add to `policy.rs` tests:

```rust
    #[test]
    fn test_forward_ssh_agent_is_compiled_from_policy() {
        let engine = PolicyEngine::from_policy_file(test_policy()).unwrap();
        // Find the compiled git policy and confirm the flag was preserved.
        let git_policy = engine
            .cli_policies
            .iter()
            .find(|p| p.command == "git")
            .expect("git policy present");
        assert!(git_policy.forward_ssh_agent);
    }
```

This requires making `cli_policies` and the struct field pub(crate). Add `pub(crate)` to both.

- [ ] **Step 5: Run all tests**

```bash
cargo test --workspace 2>&1 | tail -15
```
Expected: all pass.

- [ ] **Step 6: Rebuild the static-musl shim so the rootfs picks up the CWD fix on next bootstrap**

```bash
just build-shim 2>&1 | tail -10
```

(Users will re-run `just bootstrap-vm` to refresh their rootfs; no need to re-assemble in this plan.)

- [ ] **Step 7: Commit**

```bash
git add crates/abox-core/src/proxy_bridge.rs crates/abox-core/src/policy.rs \
        crates/abox-shim/src/main.rs
git commit -m "fix(proxy,shim): honor forward_ssh_agent; prefer /proc/self/cwd

- The forward_ssh_agent policy field was parsed but never applied to
  the child process env. Now, when the matched CliPolicy has it set,
  the host's SSH_AUTH_SOCK is passed through; otherwise it is removed
  from the child env so the child cannot inherit it by accident.
- abox-shim now prefers the target of /proc/self/cwd over getcwd(2),
  which returns the wrong path on some kernels when called from
  inside a virtiofs mount. ABOX_CWD remains the highest-priority
  source when the runner.sh sets it.

Closes S3 and S4 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 7: F4 — `abox template create` wiring (SKIPPED in this session)

**Status:** **SKIPPED** by user decision 2026-04-08. F4 stays in the backlog for a later pass. Task 15 updates the backlog entry to reflect this.

**Note:** This task is **optional**. It is P2 and requires hooking up snapshot/restore plumbing that has no e2e coverage yet.

**Files:**
- Modify: `crates/abox-cli/src/commands/template.rs`

- [ ] **Step 1: Read the current stub**

```bash
cat crates/abox-cli/src/commands/template.rs
```

- [ ] **Step 2: Replace the create stub body with a call to `SnapshotManager::create_snapshot`**

Inside the `create` match arm (whatever it currently prints), call:

```rust
            let snapshot_path = orchestrator.runtime_dir()
                .join("templates")
                .join(format!("{}.snapshot", args.name));
            orchestrator.pause_sandbox(&args.source_task).await?;
            let snapshot_mgr = abox_core::snapshot::SnapshotManager::new(
                orchestrator.runtime_dir().join("templates"),
            );
            snapshot_mgr.create_snapshot(&args.source_task, &snapshot_path).await?;
            orchestrator.resume_sandbox(&args.source_task).await?;
            println!("Template '{}' created at {}", args.name, snapshot_path.display());
```

Update `TemplateArgs` to carry `source_task`. If the `SnapshotManager` API differs, adjust accordingly — this is a wiring task, not a behavioral change.

- [ ] **Step 3: Run tests**

```bash
cargo test --workspace 2>&1 | tail -15
```

- [ ] **Step 4: Commit (only if implementation was viable — otherwise skip the task and document in backlog update Task 15)**

```bash
git add crates/abox-cli/src/commands/template.rs
git commit -m "feat(cli): wire 'abox template create' to SnapshotManager

Closes F4 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 8: F5 — TUI dashboard refresh (SKIPPED in this session)

**Status:** **SKIPPED** by user decision 2026-04-08. F5 stays in the backlog for a later pass. Task 15 updates the backlog entry to reflect this.

**Note:** Also optional. The dashboard is cosmetic in the MVP.

**Files:**
- Modify: `crates/abox-cli/src/tui/dashboard.rs`

- [ ] **Step 1: Read the current dashboard event loop**

```bash
cat crates/abox-cli/src/tui/dashboard.rs | head -100
```

- [ ] **Step 2: Add a `refresh_every: Duration` and a tokio interval**

Bind `r` in the event loop to trigger a manual refresh. Add an async tokio select that races between a keyboard event and a `tokio::time::interval`, calling `orchestrator.list_sandboxes()` on either trigger.

- [ ] **Step 3: Commit**

```bash
git add crates/abox-cli/src/tui/dashboard.rs
git commit -m "feat(tui): auto-refresh dashboard every 2s; bind 'r' to force refresh

Closes F5 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 9: D2 — Tunable timing constants (P2, small)

**Files:**
- Modify: `crates/abox-core/src/config.rs` (new `VmRuntimeTuning` struct)
- Modify: `crates/abox-core/src/sandbox.rs` (use it in `run_sandbox` poll)
- Modify: `crates/abox-core/src/console.rs` (use it in `tail_to_writer_until`)

- [ ] **Step 1: Add the tuning struct to `config.rs`**

```rust
/// Runtime timing knobs for the VM supervisor. All durations are in
/// milliseconds. Defaults match the pre-refactor hardcoded values.
#[derive(Debug, Clone)]
pub struct VmRuntimeTuning {
    /// How often `run_sandbox` polls `vm_manager.info()` for exit.
    pub vm_exit_poll_interval: std::time::Duration,
    /// How often the console tailer polls for new bytes.
    pub console_poll_interval: std::time::Duration,
    /// How long to wait for a socket file to appear before giving up.
    pub socket_wait_timeout: std::time::Duration,
}

impl Default for VmRuntimeTuning {
    fn default() -> Self {
        Self {
            vm_exit_poll_interval: std::time::Duration::from_millis(250),
            console_poll_interval: std::time::Duration::from_millis(50),
            socket_wait_timeout: std::time::Duration::from_secs(5),
        }
    }
}
```

- [ ] **Step 2: Thread it through `sandbox.rs` and `console.rs`**

Update `tail_to_writer_until` to accept a `poll_interval: Duration`, and update `run_sandbox` to pass `tuning.vm_exit_poll_interval` to the `sleep()` in its poll loop.

Since the default values are preserved, no external caller needs to change.

- [ ] **Step 3: Test + commit**

```bash
cargo test --workspace 2>&1 | tail -15
git add crates/abox-core/src/config.rs crates/abox-core/src/sandbox.rs \
        crates/abox-core/src/console.rs
git commit -m "refactor(core): lift timing constants into VmRuntimeTuning

Closes D2 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 10: D3 — Musl target opt-in for bootstrap

**Files:**
- Modify: `scripts/bootstrap_vm.sh`

- [ ] **Step 1: Add a `--yes` flag guard around the rustup target install**

Find the `rustup target add` block (around phase 4) and replace with:

```bash
if ! rustup target list --installed 2>/dev/null | grep -q '^x86_64-unknown-linux-musl$'; then
    if [[ "${BOOTSTRAP_YES:-0}" == "1" ]] || [[ "${CI:-0}" == "true" ]]; then
        echo "  adding x86_64-unknown-linux-musl rust target..."
        rustup target add x86_64-unknown-linux-musl
    else
        echo "ERROR: x86_64-unknown-linux-musl rust target is not installed."
        echo "       Re-run with BOOTSTRAP_YES=1 to install it automatically, or run:"
        echo "         rustup target add x86_64-unknown-linux-musl"
        exit 1
    fi
fi
```

Add `BOOTSTRAP_YES=1` to the default-case argument parsing from Task 4 as well:

```bash
        --yes|-y) BOOTSTRAP_YES=1 ;;
```

- [ ] **Step 2: Document the new behavior in the `--help` block**

Update the help text in the arg parser:

```
  --yes, -y        Non-interactive mode: install missing rust targets, etc.
```

- [ ] **Step 3: Commit**

```bash
git add scripts/bootstrap_vm.sh
git commit -m "feat(bootstrap): require --yes to install rust targets

Closes D3 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 11: D4 — Assert phase-6 console output reached stdout

**Files:**
- Modify: `scripts/e2e_test.sh`

- [ ] **Step 1: Replace the current phase-6 run with output capture + grep**

In phase 6, replace:

```bash
    if RUN_OUT=$(timeout 90 $ABOX run --task vm-e2e --base main -- \
        /usr/local/bin/git status 2>&1); then
```

with:

```bash
    RUN_OUT_FILE="$SCRATCH/vm-e2e-run.out"
    if timeout 90 $ABOX run --task vm-e2e --base main -- \
        /usr/local/bin/git status >"$RUN_OUT_FILE" 2>&1; then
        RUN_OUT=$(cat "$RUN_OUT_FILE")
```

Add a new assertion after the existing ones:

```bash
    if grep -q "abox guest init: online" "$RUN_OUT_FILE"; then
        pass "guest init banner reached host stdout"
    else
        fail "console streaming" "no 'guest init: online' banner in run output"
        tail -20 "$RUN_OUT_FILE" | sed "s/^/    /"
    fi
```

- [ ] **Step 2: Run the e2e script**

```bash
./scripts/e2e_test.sh 2>&1 | tail -20
```
Expected: `✓ e2e PASSED` with the new assertion passing.

- [ ] **Step 3: Commit**

```bash
git add scripts/e2e_test.sh
git commit -m "test(e2e): assert guest console banner reached host stdout

Closes D4 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 12: D6 — GitHub Actions CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `.gitignore`

- [ ] **Step 1: Remove `.github/workflows/` from `.gitignore`**

In `.gitignore`, delete the line `.github/workflows/`.

- [ ] **Step 2: Create `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main, develop]
  pull_request:
    branches: [main, develop]

jobs:
  check:
    name: fmt + clippy + test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
          targets: x86_64-unknown-linux-musl

      - name: Cache cargo registry + target
        uses: Swatinem/rust-cache@v2

      - name: cargo fmt --check
        run: cargo fmt --all -- --check

      - name: cargo clippy
        run: cargo clippy --workspace --all-targets -- -D warnings

      - name: cargo test
        run: cargo test --workspace

  e2e-phases-1-5:
    name: e2e (phases 1-5; phase 6 skipped — no KVM)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Cache cargo registry + target
        uses: Swatinem/rust-cache@v2

      - name: Install Python (for proxy_send helper)
        uses: actions/setup-python@v5
        with:
          python-version: '3.x'

      - name: Run e2e script
        run: ./scripts/e2e_test.sh
```

- [ ] **Step 3: Validate the yaml locally**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('ok')"
```
Expected: `ok`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml .gitignore
git commit -m "ci: add GitHub Actions workflow for fmt/clippy/test + e2e phases 1-5

Phase 6 (full VM e2e) remains gated on ~/.abox/vm artifacts and is
skipped on CI runners without /dev/kvm.

Closes D6 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 13: H3 — E2E script robustness

**Files:**
- Modify: `scripts/e2e_test.sh`

- [ ] **Step 1: Register the cleanup trap earlier**

Move the `trap cleanup EXIT` line to immediately after `SCRATCH` is defined, before any `set -u` sensitive expansions.

- [ ] **Step 2: Add a startup cleanup of stale scratch dirs older than 1 hour**

Near the top, after `SCRATCH=...`:

```bash
# Clean up any e2e scratch dirs from killed previous runs (>1 hour old).
find "$(dirname "$SCRATCH")" -maxdepth 1 -name 'e2e-run-*' -type d -mmin +60 \
    -exec rm -rf {} + 2>/dev/null || true
```

- [ ] **Step 3: Commit**

```bash
git add scripts/e2e_test.sh
git commit -m "test(e2e): register cleanup trap earlier; sweep stale scratch dirs

Closes H3 in docs/backlog/2026-04-08-vm-e2e-mvp-followups.md"
```

---

## Task 14: Documentation sweep + new ADR + retrospective

**Files:**
- Modify: `README.md` (install instructions reflect symlink default; link to tutorial + explainer)
- Modify: `docs/vm-setup.md` (expanded troubleshooting section)
- Modify: `CONTRIBUTING.md` (e2e + subagent workflow section)
- Modify: `docs/plans/2026-04-07-vm-end-to-end-mvp.md` (retrospective section at bottom)
- Create: `docs/decisions/002-aboxstatus-share.md` (new ADR)

- [ ] **Step 1: README.md — Getting Started update**

Change the install block from:

```bash
cargo build --release
just bootstrap-vm
```

to:

```bash
cargo build --release
just bootstrap-vm   # downloads VMM + kernel, symlinks binaries into ~/.local/bin
```

Add a "Next steps" block linking to:

- [`docs/tutorial.md`](docs/tutorial.md) — 10-minute quickstart with real commands
- [`docs/explainer.md`](docs/explainer.md) — architecture deep dive

- [ ] **Step 2: vm-setup.md — expand troubleshooting**

Add these entries to the Troubleshooting section:

- `No such file or directory: x86_64-unknown-linux-musl` — rust target missing; re-run with `BOOTSTRAP_YES=1 ./scripts/bootstrap_vm.sh` or install manually.
- `Download stalls at cloud-hypervisor` — GitHub rate-limit; wait a few minutes, delete the partial file under `vendor/`, re-run.
- `abox run: cannot find cloud-hypervisor` — symlinks not in PATH; add `~/.local/bin` to PATH or re-run bootstrap without `--no-symlink`.
- `guest init: failed to mount aboxstatus virtiofs` — stale sockets; delete `~/.abox/runtime/` and retry.

- [ ] **Step 3: CONTRIBUTING.md — add e2e + subagent workflow section**

After the existing "Common Workflows" section, add:

```markdown
### Running the E2E Test

The end-to-end test (`./scripts/e2e_test.sh` or `just e2e`) runs six
phases covering build, unit tests, git worktree operations, CLI
commands, the credential proxy daemon, and (when VM artifacts are
present) a full live-VM boot with guest `git` attribution.

Phase 6 is gated on `~/.abox/vm/cloud-hypervisor` and `rootfs.raw`
existing, so developers without the bootstrap stack can still run
the test in "phases 1-5" mode.

To add a new phase, append a `section "phase N — ..."` block to
`scripts/e2e_test.sh` with `step`/`how`/`expect`/`pass`/`fail`
assertions. The summary footer counts all `pass`/`fail` calls.

### Subagent-Driven Implementation

Multi-step work in this repo is typically done via
superpowers:subagent-driven-development: write a plan under
`docs/plans/YYYY-MM-DD-<topic>.md`, then dispatch one subagent per
task with implementer → spec review → code-quality review cycles.
This keeps the main context window clean and produces smaller,
focused commits. See `docs/plans/2026-04-08-vm-e2e-hardening.md`
for a recent example.
```

- [ ] **Step 4: Plan retrospective**

Append to the bottom of `docs/plans/2026-04-07-vm-end-to-end-mvp.md`:

```markdown
---

## Retrospective (added 2026-04-08)

**Plan vs. reality:**
- Task 9's scope absorbed real bug fixes in
  `crates/abox-core/src/adapters/cloud_hypervisor.rs`,
  `crates/abox-core/src/proxy_bridge.rs`,
  `crates/abox-core/src/sandbox.rs`,
  `crates/abox-core/src/boot_meta.rs`, and
  `crates/abox-shim/src/main.rs`. The plan had listed those files
  under Task 6/7/8. This is normal during final integration but the
  task-level breakdown didn't anticipate it.
- **F2 (exit code propagation)** was deliberately deferred from the
  original plan even though it's a core correctness issue. It is now
  addressed in `docs/plans/2026-04-08-vm-e2e-hardening.md` (Task 1)
  via a third `aboxstatus` virtiofs share. See
  `docs/decisions/002-aboxstatus-share.md`.
- **F3 (HTTPS credential injection)** also stayed deferred; it is a
  multi-day TLS-termination project and got its own standalone spec
  at `docs/plans/2026-04-08-credential-injection.md`.

**What worked well:**
- The per-VM vsock bridge attribution model — every request
  arriving on `<vsock>_5000` provably came from one guest. Zero
  ambiguity in the audit log for the CLI proxy path.
- The bootstrap-vm download caching. Re-running bootstrap is
  effectively instant after the first pull.
- The two-virtiofs shares (workspace + aboxmeta) was the right call
  vs. cramming boot metadata onto the cmdline.

**What didn't:**
- Phase 6 gating on the e2e script was the right call but it masks
  regressions in CI until someone runs it locally with a bootstrap.
  Task 12 adds CI but phase 6 still needs manual verification.
```

- [ ] **Step 5: Create `docs/decisions/002-aboxstatus-share.md`**

```markdown
# ADR-002: Exit Code Propagation via `aboxstatus` Virtiofs Share

**Status:** Accepted
**Date:** 2026-04-08
**Supersedes:** —
**Related:** ADR-001, `docs/plans/2026-04-08-vm-e2e-hardening.md` (Task 1)

## Context

The VM MVP (ADR-001) mounts two virtiofs shares into every guest:

1. `workspace` (RW) — the git worktree
2. `aboxmeta` (RO in practice) — `boot.json` + `runner.sh`

After the guest agent exits and the VM powers off, the host
orchestrator had no channel to read the agent's exit status. The
MVP's `run_sandbox` returned a hardcoded `Ok(0)`, silently masking
any non-zero exit from the guest command.

Two options were considered:

1. **Write `/run/abox-exit-code` into the worktree.** Simplest, but
   pollutes the user's source tree with an abox-internal file. Also
   race-prone if the agent writes large files at the same time.
2. **Use a third virtiofs share, writable, called `aboxstatus`.**
   One more `virtiofsd` process per sandbox; slight resource bump;
   cleanest separation of concerns.

## Decision

Adopt option 2. The orchestrator creates
`<runtime>/status-<id>/exit-code` (empty file at boot), exports it
via a third `virtiofsd` process, and passes a third `--fs
tag=aboxstatus,...` to cloud-hypervisor. `guest/init.sh` mounts it
at `/abox-status`, writes `$?` into `/abox-status/exit-code` after
the runner script exits, and powers off. The host reads the file
after detecting VM exit (before cleanup) and returns its contents
from `run_sandbox`.

## Consequences

**Positive:**
- No worktree pollution.
- Writable share has a clear, single-purpose use (one file, one int).
- Trivially extensible — future fields (crash dumps, resource
  metrics) can live in the same share without a protocol change.

**Negative:**
- One more `virtiofsd` process per sandbox (~1 MB RSS each) and one
  more unix socket in `runtime_dir/`. Immaterial for the MVP's
  expected scale (≤10 sandboxes per host).
- The cleanup flow is now two-phase: `info()` (which detects VM
  exit) must NOT remove `status_dir`, leaving it for `run_sandbox`
  to read; only `stop()` removes it. Missed this ordering in the
  first draft — made explicit with a `remove_status_dir` flag on
  `cleanup_vm_files`.

## Alternatives Considered

- **Serial console exit-code marker.** The guest init prints
  `__ABOX_EXIT__=N` on the last line; the host parses it out of the
  console log. Rejected: parsing stdout is fragile, and the console
  can be interleaved with agent output.
- **vsock exit notification.** Guest init sends a one-byte vsock
  message to a new port before poweroff. Rejected: adds protocol
  surface area and a second server in `proxy_bridge.rs` for what
  is ultimately a single integer.
- **ch-remote exit-status API.** Cloud Hypervisor does not expose
  guest-process exit status; this would require kernel-level
  hooking. Rejected as out of scope.
```

- [ ] **Step 6: Commit**

```bash
git add README.md docs/vm-setup.md CONTRIBUTING.md \
        docs/plans/2026-04-07-vm-end-to-end-mvp.md \
        docs/decisions/002-aboxstatus-share.md
git commit -m "docs: sweep READMEs + add ADR-002 + MVP retrospective

- README: note the PATH symlink default; link tutorial + explainer
- vm-setup: expand troubleshooting (rust target, PATH, downloads)
- CONTRIBUTING: add e2e + subagent-driven workflow section
- plan retrospective: note F2/F3 deferral and Task 9 scope creep
- ADR-002: aboxstatus virtiofs share for exit code propagation"
```

---

## Task 15: Update the backlog file with outcomes

**Files:**
- Modify: `docs/backlog/2026-04-08-vm-e2e-mvp-followups.md`

- [ ] **Step 1: For each landed task, strike through the entry and note the commit sha**

Use the format described in the session handoff: for each item that landed, keep the original entry (so it remains a running history) but wrap the title in `~~strikethrough~~` and append `**DONE in <commit-sha>**`. For F3 specifically, add `**DEFERRED — see docs/plans/2026-04-08-credential-injection.md**`.

Example for F2:

```markdown
### ~~F2. Agent exit code is always 0~~ — P0, S  **DONE in <sha>**
```

- [ ] **Step 2: Commit**

```bash
git add docs/backlog/2026-04-08-vm-e2e-mvp-followups.md
git commit -m "docs(backlog): mark landed/deferred follow-ups with commit shas"
```

---

## Task 16: Write the deferred F3 credential-injection spec

**Files:**
- Create: `docs/plans/2026-04-08-credential-injection.md`

- [ ] **Step 1: Write the full spec**

Write a plan document with the same header format as the other plans, covering:

1. **Goal** — replace the passthrough egress proxy with a TLS-terminating proxy that injects Authorization / x-api-key headers based on the destination domain.
2. **Architecture:**
   - First-run generation of a self-signed root CA under `~/.abox/ca/`.
   - CA cert baked into the guest rootfs at `/etc/ssl/certs/abox-ca.crt`.
   - Rebuild of `guest/init.sh` to run `update-ca-certificates` (or equivalent) on boot.
   - Egress proxy binds a per-sandbox TCP port. Guest HTTPS_PROXY=... set by runner.sh.
   - Proxy handles CONNECT, fakes a server cert signed by the abox CA, reads the rewritten request, forwards to the real upstream with injected headers.
   - Audit log entries tagged with the per-sandbox id (resolving S2 at the same time).
3. **Task breakdown (~10 tasks):** CA generation, rootfs rebuild, egress proxy MITM, header rewrite, per-sandbox binding, audit attribution, tests against a local echo server, end-to-end test against api.anthropic.com, documentation update, ADR-003.
4. **Risks:**
   - Certificate pinning in some clients will break (document which).
   - Egress proxy becomes a performance bottleneck for high-traffic agents.
   - Storing the CA private key on disk is a new sensitive asset.
5. **Explicit non-goals:** Rewriting the CLI proxy; supporting HTTP/3; egress to non-HTTPS endpoints.

Use real file paths, rough line counts, and code snippets where useful. Target length: ~250-400 lines. This is a spec someone else will implement, not a hand-wave.

- [ ] **Step 2: Commit**

```bash
git add docs/plans/2026-04-08-credential-injection.md
git commit -m "docs(plans): deferred spec for F3 HTTPS credential injection

Multi-day TLS-termination proxy project. Includes CA generation,
guest rootfs rebuild with the abox CA trusted, MITM egress proxy
with header injection, and per-sandbox binding for audit
attribution (also resolves S2).

Implementation intentionally deferred out of vm-e2e-hardening."
```

---

## Task 17: Write `docs/tutorial.md` (10-minute walkthrough)

**Files:**
- Create: `docs/tutorial.md`

- [ ] **Step 1: Draft the tutorial structure**

Target: new Rust-familiar developer who has never seen abox. Under 500 lines.

Sections:
1. **Prerequisites check** — `/dev/kvm`, rustup, ~1 GB disk
2. **Clone & build** — `cargo build --release`
3. **Bootstrap the VM stack** — `just bootstrap-vm` (what it downloads, how long, what gets symlinked)
4. **Your first sandbox** — create a scratch git repo, `abox run --task demo -- /bin/sh -c "..."`, watch boot output
5. **Inspect state** — `abox list`, `abox divergence`
6. **Merge and clean up** — `abox merge`, `abox stop --clean`
7. **What just happened?** — 2-paragraph recap pointing to explainer.md

- [ ] **Step 2: For each step, run the command yourself and paste real output**

Use a scratch directory under `.scratch/tutorial-capture/`. Save the real terminal output and embed the trimmed interesting lines into the tutorial.

- [ ] **Step 3: Commit**

```bash
git add docs/tutorial.md
git commit -m "docs: add 10-minute tutorial walkthrough

A zero-to-first-sandbox guide for developers new to abox. Uses
real captured commands and output from a scratch run."
```

---

## Task 18: Write `docs/explainer.md` (ELI5 deep dive)

**Files:**
- Create: `docs/explainer.md`

- [ ] **Step 1: Draft the 12-section structure**

Sections (each ~1 page):
1. The big picture (one diagram + one paragraph)
2. Git worktrees (what / why / how abox uses them)
3. microVMs and why not containers (kernel isolation, attack surface, why Cloud Hypervisor)
4. virtiofs (how the worktree gets into the guest, why not 9p/block)
5. vsock (how the guest talks to the host without networking)
6. The shim `abox-shim` (static musl, symlinks, request lifecycle)
7. The policy daemon and per-VM bridge (attribution, policy eval, real execution)
8. HTTPS egress proxy (what it does today, what it's supposed to — link F3 spec)
9. The orchestrator (`abox-core::sandbox`, state machine, supervision)
10. The bootstrap (`bootstrap_vm.sh`, why bash, what's checksummed)
11. The e2e test script (six phases, adding a seventh)
12. Putting it all together (full `abox run` lifecycle, every component in order)

Include: one ASCII architecture block diagram (section 1) and one ASCII sequence diagram (section 12).

- [ ] **Step 2: Write each section. For every component, answer:**
  - What is it
  - Why does it exist
  - What does it actually do
  - What breaks if it weren't there
  - Benefit over alternatives

Keep the tone "junior engineer who knows git" — not actually 5 years old.

- [ ] **Step 3: Commit**

```bash
git add docs/explainer.md
git commit -m "docs: add ELI5 architecture explainer

12-section deep dive covering git worktrees, microVMs, virtiofs,
vsock, the shim, the proxy bridge, the egress proxy, the
orchestrator state machine, the bootstrap script, and the e2e
test. Includes ASCII architecture + request-lifecycle diagrams."
```

---

## Task 19: Final verification + push

- [ ] **Step 1: Run `cargo fmt --check` + clippy + tests**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all green.

- [ ] **Step 2: Run the e2e script**

```bash
./scripts/e2e_test.sh 2>&1 | tail -20
```
Expected: `✓ e2e PASSED` with all new assertions.

- [ ] **Step 3: Push the branch**

```bash
git push -u origin vm-e2e-hardening
```

- [ ] **Step 4: Write the final report to the user**

Summarize what landed, what was deferred and why, what to do next. Do NOT merge into develop or main without explicit approval.

---
