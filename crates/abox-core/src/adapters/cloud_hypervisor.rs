//! Cloud Hypervisor adapter for the [`VmPort`] trait.
//!
//! Orchestrates `virtiofsd` and `cloud-hypervisor` processes to create
//! hardware-isolated MicroVMs with virtiofs-mounted git worktrees.

use crate::vm::{VmConfig, VmInfo, VmPort, VmState};
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
    meta_dir: PathBuf,
    api_socket: PathBuf,
    console_socket: PathBuf,
    #[allow(dead_code)]
    config: VmConfig,
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
        let virtiofs_socket = self.runtime_dir.join(format!("virtiofs-{}.sock", config.id));
        let meta_socket = self.runtime_dir.join(format!("virtiofs-meta-{}.sock", config.id));
        let api_socket = self.runtime_dir.join(format!("ch-api-{}.sock", config.id));
        // cloud-hypervisor v44 supports file= for console but not socket=.
        let console_socket = self.runtime_dir.join(format!("console-{}.log", config.id));
        let vsock_socket = self.runtime_dir.join(format!("vsock-{}.sock", config.id));
        let meta_dir = self.runtime_dir.join(format!("meta-{}", config.id));

        // Stage boot metadata into meta_dir.
        let meta = crate::boot_meta::BootMeta {
            sandbox_id: config.id.clone(),
            agent_command: config.agent_command.clone(),
            env: config.env_vars.clone(),
        };
        meta.stage(&meta_dir)
            .with_context(|| format!("Failed to stage boot metadata in {}", meta_dir.display()))?;

        // Clean up any stale sockets/files from a previous run
        for sock in [&virtiofs_socket, &meta_socket, &api_socket, &vsock_socket] {
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

        tracing::debug!(
            sandbox_id = %config.id,
            workspace_socket = %virtiofs_socket.display(),
            meta_socket = %meta_socket.display(),
            "virtiofsd up"
        );

        // ── Step 2: Start Cloud Hypervisor ──
        // --memory shared=on is REQUIRED for virtiofs (enables shared memory mapping).
        // --fs connects both virtiofsd sockets as virtio-fs devices.
        // --console file= captures the VM's serial console for debugging.
        // --vsock allows the guest shim to communicate with the host proxy daemon.
        let ch_child = Command::new("cloud-hypervisor")
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
            .arg("--vsock")
            .arg(format!("cid=3,socket={}", vsock_socket.display()))
            .arg("--console")
            .arg(format!("file={}", console_socket.display()))
            .kill_on_drop(true)
            .spawn()
            .context(
                "Failed to start cloud-hypervisor. Run scripts/bootstrap_vm.sh to install it.",
            )?;

        Self::wait_for_socket(&api_socket, 10000)
            .await
            .context("Cloud Hypervisor API socket did not appear within 10 seconds")?;

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
            meta_dir: meta_dir.clone(),
            api_socket: api_socket.clone(),
            console_socket: console_socket.clone(),
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

            // Clean up socket files
            for suffix in ["virtiofs", "virtiofs-meta", "ch-api", "console", "vsock"] {
                let sock = self.runtime_dir.join(format!("{suffix}-{id}.sock"));
                let _ = std::fs::remove_file(sock);
            }
            // Task 7 will bind a per-VM proxy bridge socket here; clean it up too.
            let _ = std::fs::remove_file(self.runtime_dir.join(format!("vsock-{id}.sock_5000")));

            // Remove the staged boot metadata directory.
            let _ = std::fs::remove_dir_all(&vm.meta_dir);

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
            drop(vms);
            // Remove from registry so the next info() call returns an error,
            // which is what sandbox.rs's polling loop watches for.
            let mut vms = self.vms.lock().await;
            vms.remove(id);
            bail!("VM '{}' has exited", id);
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
}
