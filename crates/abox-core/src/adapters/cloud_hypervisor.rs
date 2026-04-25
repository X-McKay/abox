//! Cloud Hypervisor adapter for the [`VmPort`] trait.
//!
//! Orchestrates `virtiofsd` and `cloud-hypervisor` processes to create
//! hardware-isolated MicroVMs with virtiofs-mounted git worktrees.

use crate::util::{max_task_id_len_for_runtime_dir, validate_task_id_for_runtime_dir};
use crate::vm::{StartMode, VmConfig, VmInfo, VmPort, VmState};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

fn host_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").map_or(0, |m| m.uid())
}

fn host_gid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").map_or(0, |m| m.gid())
}

fn workspace_virtiofsd_args(
    socket_path: &std::path::Path,
    shared_dir: &std::path::Path,
    uid: u32,
    gid: u32,
) -> Vec<String> {
    vec![
        format!("--socket-path={}", socket_path.display()),
        format!("--shared-dir={}", shared_dir.display()),
        "--cache=never".to_string(),
        // --sandbox=namespace confines virtiofsd via Linux user namespaces
        // (required for --uid-map/--gid-map and for security isolation).
        // Requires unprivileged user namespaces on the host (default on Linux 5.x+).
        "--sandbox=namespace".to_string(),
        "--thread-pool-size=4".to_string(),
        format!("--uid-map=:1000:{uid}:1:"),
        format!("--gid-map=:1000:{gid}:1:"),
        // Suppress verbose FUSE-level debug output; only warnings and errors
        // are operationally relevant and reducing log volume limits the
        // information available to an attacker who gains log access.
        "--log-level=warn".to_string(),
    ]
}

/// Build the virtiofsd argument list for the read-only meta and status shares.
///
/// These shares do not require UID/GID remapping (they are accessed by the
/// guest init process as root before privilege drop), but they still benefit
/// from `--sandbox=namespace` for process-level isolation: virtiofsd is
/// confined to its own user/mount/PID namespace, limiting the blast radius
/// of a virtiofsd vulnerability to the shared directory contents rather than
/// the full host filesystem.
fn auxiliary_virtiofsd_args(
    socket_path: &std::path::Path,
    shared_dir: &std::path::Path,
) -> Vec<String> {
    vec![
        format!("--socket-path={}", socket_path.display()),
        format!("--shared-dir={}", shared_dir.display()),
        "--cache=never".to_string(),
        // Namespace sandbox for process isolation (same rationale as workspace).
        "--sandbox=namespace".to_string(),
        "--log-level=warn".to_string(),
    ]
}

/// Manages Cloud Hypervisor and virtiofsd process lifecycles.
pub struct CloudHypervisorAdapter {
    /// Base directory for runtime files (sockets, PIDs).
    runtime_dir: PathBuf,
    /// Base state directory (e.g., `~/.abox`). Used to resolve VM binary
    /// paths via `state_dir/vm/<name>` for curl-pipe installs.
    state_dir: PathBuf,
    /// Active VMs indexed by sandbox ID.
    vms: Arc<Mutex<HashMap<String, RunningVm>>>,
}

struct RunningVm {
    ch_child: Child,
    virtiofsd_child: Child,
    meta_virtiofsd_child: Child,
    status_virtiofsd_child: Child,
    meta_dir: PathBuf,
    status_dir: PathBuf,
    api_socket: PathBuf,
    console_socket: PathBuf,
    /// Actual virtiofsd socket paths (may differ from standard naming in restore mode).
    virtiofs_sockets: Vec<PathBuf>,
    #[allow(dead_code)]
    config: VmConfig,
}

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

impl CloudHypervisorAdapter {
    /// Create a new adapter.
    ///
    /// # Arguments
    /// * `runtime_dir` - Directory for runtime sockets (e.g., `/run/abox`).
    /// * `state_dir` - Base abox state directory (e.g., `~/.abox`). Used to
    ///   resolve VM binary paths at `state_dir/vm/<name>`.
    pub fn new(runtime_dir: PathBuf, state_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&runtime_dir)?;
        anyhow::ensure!(
            max_task_id_len_for_runtime_dir(&runtime_dir) > 0,
            "runtime_dir '{}' is too deep: no valid task ID would fit within Linux's 108-byte \
             Unix socket path limit. Use a shorter --runtime-dir.",
            runtime_dir.display(),
        );
        Ok(Self { runtime_dir, state_dir, vms: Arc::new(Mutex::new(HashMap::new())) })
    }

    /// Resolve a VM binary (cloud-hypervisor, virtiofsd, ch-remote) using the
    /// standard search order: `state_dir/vm/<name>` then `$PATH`.
    fn resolve_binary(&self, name: &str) -> Result<PathBuf> {
        crate::binary_resolve::resolve_vm_binary(name, &self.state_dir)
    }

    /// Remove all runtime files associated with a VM (sockets, console log,
    /// vsock bridge socket, and staged meta directory).
    ///
    /// Called from both `stop()` (explicit teardown) and `info()` (natural
    /// exit detection) so cleanup always runs regardless of how the VM ends.
    ///
    /// `remove_status_dir` controls whether the staged status directory is
    /// also deleted. `info()` must pass `false` so `run_sandbox` can still
    /// read `exit-code` from it after the VM exits; `stop()` and
    /// `run_sandbox` (after reading the file) pass `true`.
    fn cleanup_vm_files(&self, id: &str, vm: &RunningVm, remove_status_dir: bool) {
        // Remove virtiofsd sockets (may differ from standard naming in restore mode).
        for sock in &vm.virtiofs_sockets {
            let _ = std::fs::remove_file(sock);
        }
        // Remove CH API and vsock sockets (always use current sandbox id).
        let _ = std::fs::remove_file(self.runtime_dir.join(format!("ch-api-{id}.sock")));
        let _ = std::fs::remove_file(self.runtime_dir.join(format!("vsock-{id}.sock")));
        // Console is a plain file (CH v44's --console file=...), not a socket.
        let _ = std::fs::remove_file(&vm.console_socket);
        let _ = std::fs::remove_file(self.runtime_dir.join(format!("vsock-{id}.sock_5000")));
        let _ = std::fs::remove_dir_all(&vm.meta_dir);
        if remove_status_dir {
            let _ = std::fs::remove_dir_all(&vm.status_dir);
        }
    }

    /// Wait for a Unix socket file to appear on disk.
    async fn wait_for_socket(path: &std::path::Path, timeout_ms: u64) -> Result<()> {
        let start = std::time::Instant::now();
        loop {
            if path.exists() {
                return Ok(());
            }
            if start.elapsed().as_millis() > u128::from(timeout_ms) {
                bail!("Timed out waiting for socket: {}", path.display());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

impl VmPort for CloudHypervisorAdapter {
    async fn start(&self, config: VmConfig) -> Result<VmInfo> {
        validate_task_id_for_runtime_dir(&config.id, &self.runtime_dir)
            .map_err(anyhow::Error::msg)?;

        let api_socket = self.runtime_dir.join(format!("ch-api-{}.sock", config.id));
        // cloud-hypervisor v44 supports file= for console but not socket=.
        let console_socket = self.runtime_dir.join(format!("console-{}.log", config.id));
        let vsock_socket = self.runtime_dir.join(format!("vsock-{}.sock", config.id));
        let meta_dir = self.runtime_dir.join(format!("meta-{}", config.id));
        let status_dir = self.runtime_dir.join(format!("status-{}", config.id));

        // In restore mode, virtiofsd sockets must match the original paths
        // baked into the snapshot. Read them from the template metadata.
        // In fresh mode, use the standard naming convention.
        let (virtiofs_socket, meta_socket, status_socket) = match &config.start_mode {
            StartMode::Fresh => (
                self.runtime_dir.join(format!("vfs-{}.sock", config.id)),
                self.runtime_dir.join(format!("vfs-meta-{}.sock", config.id)),
                self.runtime_dir.join(format!("vfs-status-{}.sock", config.id)),
            ),
            StartMode::Restore { template_path } => {
                let tmpl_meta = crate::snapshot::TemplateMeta::load(template_path)?;
                let ws = tmpl_meta
                    .virtiofs_sockets
                    .get("workspace")
                    .context("template metadata missing 'workspace' socket")?;
                let mt = tmpl_meta
                    .virtiofs_sockets
                    .get("meta")
                    .context("template metadata missing 'meta' socket")?;
                let st = tmpl_meta
                    .virtiofs_sockets
                    .get("status")
                    .context("template metadata missing 'status' socket")?;
                (self.runtime_dir.join(ws), self.runtime_dir.join(mt), self.runtime_dir.join(st))
            }
        };

        let t_meta = std::time::Instant::now();

        // Stage boot metadata into meta_dir.
        let staged_creds: Vec<crate::boot_meta::StagedCredential> = config
            .credential_files
            .iter()
            .map(|c| crate::boot_meta::StagedCredential {
                index: c.index,
                guest_path: c.guest_path.clone(),
                mode: c.mode.clone(),
            })
            .collect();
        let meta = crate::boot_meta::BootMeta {
            sandbox_id: config.id.clone(),
            agent_command: config.agent_command.clone(),
            env: config.env_vars.clone(),
            credential_files: staged_creds,
        };
        meta.stage(&meta_dir)
            .with_context(|| format!("Failed to stage boot metadata in {}", meta_dir.display()))?;

        // Write credential file contents into meta_dir/credentials/.
        if !config.credential_files.is_empty() {
            let creds_dir = meta_dir.join("credentials");
            std::fs::create_dir_all(&creds_dir).with_context(|| {
                format!("Failed to create credentials dir {}", creds_dir.display())
            })?;
            for cred in &config.credential_files {
                let dest = creds_dir.join(cred.index.to_string());
                std::fs::write(&dest, &cred.content).with_context(|| {
                    format!("Failed to write credential file {}", dest.display())
                })?;
            }
        }

        // Stage the root CA certificate so guest/init.sh can inject it
        // into the guest trust store at boot. This decouples the rootfs
        // image from any specific CA, enabling CI-built rootfs distribution.
        if let Some(ref pem) = config.ca_cert_pem {
            std::fs::write(meta_dir.join("root.crt"), pem).with_context(|| {
                format!("Failed to write CA cert to {}", meta_dir.join("root.crt").display())
            })?;
        }

        // Stage the status dir for the writable aboxstatus virtiofs share.
        std::fs::create_dir_all(&status_dir)
            .with_context(|| format!("Failed to create status dir {}", status_dir.display()))?;
        // Pre-create an empty exit-code file so virtiofsd has something to serve
        // and the guest can truncate it without permission errors.
        let _ = std::fs::write(status_dir.join("exit-code"), "");

        tracing::info!(
            sandbox_id = %config.id,
            elapsed_ms = t_meta.elapsed().as_millis() as u64,
            "boot metadata staged"
        );

        // Clean up any stale sockets/files from a previous run
        for sock in [&virtiofs_socket, &meta_socket, &status_socket, &api_socket, &vsock_socket] {
            let _ = std::fs::remove_file(sock);
        }
        let _ = std::fs::remove_file(&console_socket);

        // ── Step 1: Start all three virtiofsd instances ──
        // Spawn all three processes up front, then wait for their sockets in
        // parallel via tokio::join!(). This collapses ~3× the single-socket
        // wait time down to ~1× (saving ~100-200ms on the critical path).
        let t_vfs = std::time::Instant::now();

        // Workspace virtiofsd: serves the git worktree to the VM.
        // --sandbox=namespace confines virtiofsd via Linux user namespaces
        // (required for --uid-map/--gid-map and for security).
        // --cache=never avoids host page-cache pressure at scale.
        // --uid-map / --gid-map remap host uid/gid → guest uid 1000.
        let uid = host_uid();
        let gid = host_gid();
        let virtiofsd_args =
            workspace_virtiofsd_args(&virtiofs_socket, &config.worktree_path, uid, gid);
        let mut cmd = Command::new(self.resolve_binary("virtiofsd")?);
        for a in &virtiofsd_args {
            cmd.arg(a);
        }
        let virtiofsd_child = cmd.kill_on_drop(true).spawn().context(
            "Failed to start workspace virtiofsd. Run scripts/bootstrap_vm.sh to install it.",
        )?;

        // Meta virtiofsd (read-only in practice; serves boot metadata).
        // Uses auxiliary_virtiofsd_args which adds --sandbox=namespace for
        // process-level isolation and --log-level=warn to reduce log noise.
        let meta_args = auxiliary_virtiofsd_args(&meta_socket, &meta_dir);
        let mut meta_cmd = Command::new(self.resolve_binary("virtiofsd")?);
        for a in &meta_args {
            meta_cmd.arg(a);
        }
        let meta_virtiofsd_child =
            meta_cmd.kill_on_drop(true).spawn().context("Failed to start meta virtiofsd")?;

        // Status virtiofsd (read-write; for exit-code reporting).
        // Uses auxiliary_virtiofsd_args which adds --sandbox=namespace for
        // process-level isolation and --log-level=warn to reduce log noise.
        let status_args = auxiliary_virtiofsd_args(&status_socket, &status_dir);
        let mut status_cmd = Command::new(self.resolve_binary("virtiofsd")?);
        for a in &status_args {
            status_cmd.arg(a);
        }
        let status_virtiofsd_child =
            status_cmd.kill_on_drop(true).spawn().context("Failed to start status virtiofsd")?;

        // Wait for all three sockets concurrently instead of sequentially.
        let (ws_res, meta_res, status_res) = tokio::join!(
            Self::wait_for_socket(&virtiofs_socket, 5000),
            Self::wait_for_socket(&meta_socket, 5000),
            Self::wait_for_socket(&status_socket, 5000),
        );
        ws_res.context("workspace virtiofsd socket did not appear within 5 seconds")?;
        meta_res.context("meta virtiofsd socket did not appear within 5 seconds")?;
        status_res.context("status virtiofsd socket did not appear within 5 seconds")?;

        tracing::info!(
            sandbox_id = %config.id,
            elapsed_ms = t_vfs.elapsed().as_millis() as u64,
            "virtiofsd ready"
        );

        // ── Step 2: Start Cloud Hypervisor ──
        let t_ch = std::time::Instant::now();
        let ch_child = match &config.start_mode {
            StartMode::Fresh => {
                // --memory shared=on is REQUIRED for virtiofs (enables shared memory mapping).
                // --fs connects virtiofsd sockets as virtio-fs devices.
                // --console file= captures the VM's serial console for debugging.
                // --vsock allows the guest shim to communicate with the host proxy daemon.
                let child = Command::new(self.resolve_binary("cloud-hypervisor")?)
                    .arg("--api-socket")
                    .arg(api_socket.display().to_string())
                    .arg("--cpus")
                    .arg(format!("boot={}", config.vcpus))
                    .arg("--memory")
                    .arg(format!("size={}M,shared=on", config.memory_mib))
                    .arg("--disk")
                    .arg(format!("path={}", config.image_path.display()))
                    .arg("--kernel")
                    .arg(config.kernel_path.display().to_string())
                    .arg("--cmdline")
                    .arg("console=hvc0 root=/dev/vda rw quiet nomodeset noresume nokaslr raid=noautodetect")
                    // cloud-hypervisor v44+ requires multiple --fs values as separate
                    // positional values after a single --fs flag (not repeated --fs flags).
                    .arg("--fs")
                    .arg(format!(
                        "tag=workspace,socket={},num_queues=1,queue_size=1024",
                        virtiofs_socket.display()
                    ))
                    .arg(format!(
                        "tag=aboxmeta,socket={},num_queues=1,queue_size=512",
                        meta_socket.display()
                    ))
                    .arg(format!(
                        "tag=aboxstatus,socket={},num_queues=1,queue_size=256",
                        status_socket.display()
                    ))
                    .arg("--vsock")
                    .arg(format!("cid=3,socket={}", vsock_socket.display()))
                    .arg("--console")
                    .arg(format!("file={}", console_socket.display()))
                    .kill_on_drop(true)
                    .spawn()
                    .context(
                        "Failed to start cloud-hypervisor. Run scripts/bootstrap_vm.sh to install it.",
                    )?;
                child
            }
            StartMode::Restore { template_path } => {
                // Restore from snapshot: CH starts paused, then we resume.
                let child = Command::new(self.resolve_binary("cloud-hypervisor")?)
                    .arg("--api-socket")
                    .arg(api_socket.display().to_string())
                    .arg("--restore")
                    .arg(format!("source_url=file://{}", template_path.display()))
                    .kill_on_drop(true)
                    .spawn()
                    .context("Failed to start cloud-hypervisor in restore mode")?;
                child
            }
        };

        Self::wait_for_socket(&api_socket, 10000)
            .await
            .context("Cloud Hypervisor API socket did not appear within 10 seconds")?;

        // In restore mode the VM comes up paused; resume it now that
        // virtiofsd instances are listening on the expected socket paths.
        if matches!(&config.start_mode, StartMode::Restore { .. }) {
            let status = Command::new(self.resolve_binary("ch-remote")?)
                .arg("--api-socket")
                .arg(api_socket.display().to_string())
                .arg("resume")
                .status()
                .await
                .context("Failed to resume restored VM")?;
            if !status.success() {
                bail!("ch-remote resume failed after snapshot restore for '{}'", config.id);
            }
        }

        let pid = ch_child.id().unwrap_or(0);

        tracing::info!(
            sandbox_id = %config.id,
            pid,
            memory_mib = config.memory_mib,
            vcpus = config.vcpus,
            elapsed_ms = t_ch.elapsed().as_millis() as u64,
            "MicroVM started"
        );

        let running = RunningVm {
            ch_child,
            virtiofsd_child,
            meta_virtiofsd_child,
            status_virtiofsd_child,
            meta_dir: meta_dir.clone(),
            status_dir: status_dir.clone(),
            api_socket: api_socket.clone(),
            console_socket: console_socket.clone(),
            virtiofs_sockets: vec![virtiofs_socket, meta_socket, status_socket],
            config,
        };

        let id = running.config.id.clone();
        self.vms.lock().await.insert(id.clone(), running);

        Ok(VmInfo { id, pid, state: VmState::Running, api_socket, console_socket })
    }

    async fn stop(&self, id: &str) -> Result<()> {
        let mut vms = self.vms.lock().await;
        if let Some(mut vm) = vms.remove(id) {
            // Kill processes (in production, send shutdown via CH API first)
            let _ = vm.ch_child.kill().await;
            let _ = vm.virtiofsd_child.kill().await;
            let _ = vm.meta_virtiofsd_child.kill().await;
            let _ = vm.status_virtiofsd_child.kill().await;

            // Clean up all runtime files (sockets, console log, meta dir,
            // and the status dir — this is an explicit teardown).
            self.cleanup_vm_files(id, &vm, true);

            tracing::info!(sandbox_id = id, "MicroVM stopped");
        } else {
            bail!("No running VM with id '{id}'");
        }
        Ok(())
    }

    async fn pause(&self, id: &str) -> Result<()> {
        let vms = self.vms.lock().await;
        let vm = vms.get(id).context("VM not found")?;

        // Use ch-remote to pause via the API socket
        let status = Command::new("ch-remote")
            .arg("--api-socket")
            .arg(vm.api_socket.display().to_string())
            .arg("pause")
            .status()
            .await
            .context("Failed to run ch-remote")?;

        if !status.success() {
            bail!("ch-remote pause failed for '{id}'");
        }

        tracing::info!(sandbox_id = id, "MicroVM paused");
        Ok(())
    }

    async fn resume(&self, id: &str) -> Result<()> {
        let vms = self.vms.lock().await;
        let vm = vms.get(id).context("VM not found")?;

        let status = Command::new("ch-remote")
            .arg("--api-socket")
            .arg(vm.api_socket.display().to_string())
            .arg("resume")
            .status()
            .await
            .context("Failed to run ch-remote")?;

        if !status.success() {
            bail!("ch-remote resume failed for '{id}'");
        }

        tracing::info!(sandbox_id = id, "MicroVM resumed");
        Ok(())
    }

    async fn info(&self, id: &str) -> Result<VmInfo> {
        let mut vms = self.vms.lock().await;
        let vm = vms.get_mut(id).context("VM not found")?;
        // If the cloud-hypervisor process has exited, treat the VM as gone.
        if let Ok(Some(_)) = vm.ch_child.try_wait() {
            // Run file cleanup while we still hold the vm reference (before
            // removing it from the map drops the RunningVm). Note: we keep
            // the status dir around so `run_sandbox` can still read the
            // guest's exit code after this call tears the VM entry down.
            self.cleanup_vm_files(id, vm, false);
            drop(vms);

            // Reacquire and remove the entry (this drops the RunningVm, which
            // kills the virtiofsd children via kill_on_drop).
            let mut vms = self.vms.lock().await;
            vms.remove(id);

            bail!("VM '{id}' has exited");
        }
        Ok(VmInfo {
            id: id.to_string(),
            pid: vm.ch_child.id().unwrap_or(0),
            state: VmState::Running,
            api_socket: vm.api_socket.clone(),
            console_socket: vm.console_socket.clone(),
        })
    }

    async fn list(&self) -> Result<Vec<VmInfo>> {
        let vms = self.vms.lock().await;
        Ok(vms
            .iter()
            .map(|(id, vm)| VmInfo {
                id: id.clone(),
                pid: vm.ch_child.id().unwrap_or(0),
                state: VmState::Running,
                api_socket: vm.api_socket.clone(),
                console_socket: vm.console_socket.clone(),
            })
            .collect())
    }

    async fn wait_for_exit(&self, id: &str) -> Result<()> {
        loop {
            {
                let mut vms = self.vms.lock().await;
                match vms.get_mut(id) {
                    Some(vm) => {
                        if let Ok(Some(_)) = vm.ch_child.try_wait() {
                            self.cleanup_vm_files(id, vm, false);
                            drop(vms);
                            self.vms.lock().await.remove(id);
                            return Ok(());
                        }
                    }
                    None => return Ok(()),
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    fn status_dir(&self, id: &str) -> Option<PathBuf> {
        Some(self.runtime_dir.join(format!("status-{id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_len_rejects_deep_runtime_dir() {
        // Build a path long enough that even a one-character task ID would
        // exceed the Unix socket path limit.
        let tmp = tempfile::tempdir().unwrap();
        // Pad with nested directories to push the base path well past the
        // point where any runtime socket name would fit.
        let deep = tmp.path().join("a".repeat(90));
        std::fs::create_dir_all(&deep).unwrap();
        let result = CloudHypervisorAdapter::new(deep, tmp.path().to_path_buf());
        assert!(result.is_err(), "expected Err for deep runtime_dir");
        let msg = format!("{}", result.err().unwrap());
        assert!(msg.contains("too deep"), "error should mention 'too deep', got: {msg}");
    }

    #[test]
    fn sun_len_accepts_short_runtime_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // A short path should succeed.
        let short = tmp.path().join("r");
        let result = CloudHypervisorAdapter::new(short, tmp.path().to_path_buf());
        assert!(result.is_ok(), "expected Ok for short runtime_dir, got: {:?}", result.err());
    }

    #[test]
    fn sun_len_accepts_runtime_dir_with_small_nonzero_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("r");
        let pad = 90usize.saturating_sub(base.as_os_str().len());
        let runtime = tmp.path().join("a".repeat(pad));
        std::fs::create_dir_all(&runtime).unwrap();
        let result = CloudHypervisorAdapter::new(runtime.clone(), tmp.path().to_path_buf());
        assert!(
            result.is_ok(),
            "expected Ok for runtime_dir with small budget, got: {:?}",
            result.err()
        );
        let budget = max_task_id_len_for_runtime_dir(&runtime);
        assert!(budget > 0 && budget <= 2, "expected a small non-zero budget, got {budget}");
    }

    #[test]
    fn workspace_virtiofsd_args_include_uid_gid_map() {
        let sock = std::path::Path::new("/tmp/vfs-workspace.sock");
        let dir = std::path::Path::new("/tmp/wt");
        let args = super::workspace_virtiofsd_args(sock, dir, 1000, 1000);
        assert!(args.iter().any(|a| a == "--uid-map=:1000:1000:1:"));
        assert!(args.iter().any(|a| a == "--gid-map=:1000:1000:1:"));
        assert!(args.iter().any(|a| a == "--cache=never"));
        assert!(args.iter().any(|a| a == "--sandbox=namespace"));
    }

    #[test]
    fn workspace_virtiofsd_args_uid_propagates() {
        let sock = std::path::Path::new("/tmp/s.sock");
        let dir = std::path::Path::new("/tmp/d");
        let args = super::workspace_virtiofsd_args(sock, dir, 2000, 3000);
        assert!(args.iter().any(|a| a == "--uid-map=:1000:2000:1:"));
        assert!(args.iter().any(|a| a == "--gid-map=:1000:3000:1:"));
    }

    #[test]
    fn workspace_virtiofsd_args_include_log_level_warn() {
        let sock = std::path::Path::new("/tmp/vfs-workspace.sock");
        let dir = std::path::Path::new("/tmp/wt");
        let args = super::workspace_virtiofsd_args(sock, dir, 1000, 1000);
        assert!(
            args.iter().any(|a| a == "--log-level=warn"),
            "workspace virtiofsd must include --log-level=warn; got: {args:?}"
        );
    }

    #[test]
    fn auxiliary_virtiofsd_args_include_sandbox_namespace() {
        let sock = std::path::Path::new("/tmp/vfs-meta.sock");
        let dir = std::path::Path::new("/tmp/meta");
        let args = super::auxiliary_virtiofsd_args(sock, dir);
        assert!(
            args.iter().any(|a| a == "--sandbox=namespace"),
            "auxiliary virtiofsd must include --sandbox=namespace; got: {args:?}"
        );
    }

    #[test]
    fn auxiliary_virtiofsd_args_include_log_level_warn() {
        let sock = std::path::Path::new("/tmp/vfs-status.sock");
        let dir = std::path::Path::new("/tmp/status");
        let args = super::auxiliary_virtiofsd_args(sock, dir);
        assert!(
            args.iter().any(|a| a == "--log-level=warn"),
            "auxiliary virtiofsd must include --log-level=warn; got: {args:?}"
        );
    }

    #[test]
    fn auxiliary_virtiofsd_args_include_cache_never() {
        let sock = std::path::Path::new("/tmp/vfs-meta.sock");
        let dir = std::path::Path::new("/tmp/meta");
        let args = super::auxiliary_virtiofsd_args(sock, dir);
        assert!(
            args.iter().any(|a| a == "--cache=never"),
            "auxiliary virtiofsd must include --cache=never; got: {args:?}"
        );
    }

    #[test]
    fn auxiliary_virtiofsd_args_no_uid_gid_map() {
        // The meta and status shares do not remap UIDs — the guest init
        // process reads them as root before privilege drop.
        let sock = std::path::Path::new("/tmp/vfs-meta.sock");
        let dir = std::path::Path::new("/tmp/meta");
        let args = super::auxiliary_virtiofsd_args(sock, dir);
        assert!(
            !args.iter().any(|a| a.starts_with("--uid-map")),
            "auxiliary virtiofsd must NOT include --uid-map; got: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("--gid-map")),
            "auxiliary virtiofsd must NOT include --gid-map; got: {args:?}"
        );
    }
}
