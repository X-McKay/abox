//! Cloud Hypervisor adapter for the [`VmPort`] trait.
//!
//! Orchestrates `virtiofsd` and `cloud-hypervisor` processes to create
//! hardware-isolated MicroVMs with virtiofs-mounted git worktrees.

use crate::vm::{StartMode, VmConfig, VmInfo, VmPort, VmState};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Manages Cloud Hypervisor and virtiofsd process lifecycles.
pub struct CloudHypervisorAdapter {
    /// Base directory for runtime files (sockets, PIDs).
    runtime_dir: PathBuf,
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
    pub fn new(runtime_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&runtime_dir)?;
        Ok(Self { runtime_dir, vms: Arc::new(Mutex::new(HashMap::new())) })
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
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

impl VmPort for CloudHypervisorAdapter {
    async fn start(&self, config: VmConfig) -> Result<VmInfo> {
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
                (
                    self.runtime_dir.join(ws),
                    self.runtime_dir.join(mt),
                    self.runtime_dir.join(st),
                )
            }
        };

        // Stage boot metadata into meta_dir.
        let meta = crate::boot_meta::BootMeta {
            sandbox_id: config.id.clone(),
            agent_command: config.agent_command.clone(),
            env: config.env_vars.clone(),
        };
        meta.stage(&meta_dir)
            .with_context(|| format!("Failed to stage boot metadata in {}", meta_dir.display()))?;

        // Stage the status dir for the writable aboxstatus virtiofs share.
        std::fs::create_dir_all(&status_dir)
            .with_context(|| format!("Failed to create status dir {}", status_dir.display()))?;
        // Pre-create an empty exit-code file so virtiofsd has something to serve
        // and the guest can truncate it without permission errors.
        let _ = std::fs::write(status_dir.join("exit-code"), "");

        // Clean up any stale sockets/files from a previous run
        for sock in [&virtiofs_socket, &meta_socket, &status_socket, &api_socket, &vsock_socket] {
            let _ = std::fs::remove_file(sock);
        }
        let _ = std::fs::remove_file(&console_socket);

        // ── Step 1: Start workspace virtiofsd ──
        // virtiofsd serves the git worktree to the VM via the vhost-user protocol.
        // --sandbox=none avoids namespace restrictions that require elevated privileges.
        // --cache=never avoids consuming host page cache (important at scale).
        let virtiofsd_child = Command::new("virtiofsd")
            .arg(format!("--socket-path={}", virtiofs_socket.display()))
            .arg(format!("--shared-dir={}", config.worktree_path.display()))
            .arg("--cache=never")
            .arg("--sandbox=none")
            .arg("--thread-pool-size=4")
            .kill_on_drop(true)
            .spawn()
            .context(
                "Failed to start workspace virtiofsd. Run scripts/bootstrap_vm.sh to install it.",
            )?;

        Self::wait_for_socket(&virtiofs_socket, 5000)
            .await
            .context("workspace virtiofsd socket did not appear within 5 seconds")?;

        // ── Step 1b: Start meta virtiofsd ──
        // Note: virtiofsd 1.x removed the --readonly flag; the guest only reads
        // this mount in practice.
        let meta_virtiofsd_child = Command::new("virtiofsd")
            .arg(format!("--socket-path={}", meta_socket.display()))
            .arg(format!("--shared-dir={}", meta_dir.display()))
            .arg("--cache=never")
            .arg("--sandbox=none")
            .kill_on_drop(true)
            .spawn()
            .context("Failed to start meta virtiofsd")?;

        Self::wait_for_socket(&meta_socket, 5000)
            .await
            .context("meta virtiofsd socket did not appear within 5 seconds")?;

        // ── Step 1c: Start status virtiofsd (read-write) ──
        // This share is writable from inside the guest so `init.sh` can
        // report the agent's exit code back to the host via a staged file.
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

        tracing::debug!(
            sandbox_id = %config.id,
            workspace_socket = %virtiofs_socket.display(),
            meta_socket = %meta_socket.display(),
            status_socket = %status_socket.display(),
            "virtiofsd up"
        );

        // ── Step 2: Start Cloud Hypervisor ──
        let ch_child = match &config.start_mode {
            StartMode::Fresh => {
                // --memory shared=on is REQUIRED for virtiofs (enables shared memory mapping).
                // --fs connects virtiofsd sockets as virtio-fs devices.
                // --console file= captures the VM's serial console for debugging.
                // --vsock allows the guest shim to communicate with the host proxy daemon.
                let child = Command::new("cloud-hypervisor")
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
                    .arg("console=hvc0 root=/dev/vda rw quiet")
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
                let child = Command::new("cloud-hypervisor")
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
            let status = Command::new("ch-remote")
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
            virtiofs_sockets: vec![
                virtiofs_socket,
                meta_socket,
                status_socket,
            ],
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

    fn status_dir(&self, id: &str) -> Option<PathBuf> {
        Some(self.runtime_dir.join(format!("status-{id}")))
    }
}
