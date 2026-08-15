//! Legacy Cloud Hypervisor implementation of [`SandboxRuntimePort`].
//!
//! Transitional adapter (ADR-008): translates the runtime-neutral
//! [`SandboxRuntimeSpec`] back into the raw-image/kernel/virtiofsd machinery
//! of [`CloudHypervisorAdapter`]. All hypervisor-specific concepts — kernel
//! and rootfs paths, API sockets, virtiofsd shares, the status-share
//! exit-code protocol, vsock socket naming — live here so the orchestrator
//! no longer sees them. Deleted together with the legacy runtime once the
//! MicroSandbox backend reaches parity.

use crate::adapters::cloud_hypervisor::{read_exit_code, CloudHypervisorAdapter};
use crate::config::AboxConfig;
use crate::project::{image_path_for_profile, kernel_path_for_profile, EnvironmentProfile};
use crate::runtime::{
    MemorySnapshotHandles, RuntimeExit, RuntimeInstance, RuntimeStart, RuntimeState,
    SandboxRuntimePort, SandboxRuntimeSpec,
};
use crate::vm::{StartMode, VmConfig, VmInfo, VmPort, VmState};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

/// Runtime adapter wrapping the legacy Cloud Hypervisor VM stack.
pub struct CloudHypervisorRuntime {
    config: AboxConfig,
    inner: CloudHypervisorAdapter,
}

impl CloudHypervisorRuntime {
    /// Create the legacy runtime. Fails if the runtime directory is too deep
    /// for per-sandbox Unix socket paths.
    pub fn new(config: AboxConfig) -> Result<Self> {
        let inner = CloudHypervisorAdapter::new(config.runtime_dir(), config.state_dir.clone())?;
        Ok(Self { config, inner })
    }

    /// Resolve the rootfs image for a profile, failing with an actionable
    /// message when the artifact was never bootstrapped.
    fn resolve_image(&self, profile: EnvironmentProfile) -> Result<PathBuf> {
        let image_path = image_path_for_profile(&self.config, profile);
        if !image_path.exists() {
            if profile == EnvironmentProfile::Base {
                anyhow::bail!(
                    "VM rootfs image not found at {}\n\n\
                     Run 'abox init' or 'just bootstrap-vm' to download and assemble\n\
                     the VM stack, then try again.",
                    image_path.display()
                );
            }
            anyhow::bail!(
                "VM rootfs image for profile '{profile}' not found at {}\n\n\
                 Install or build the '{profile}' guest profile under \
                 ~/.abox/vm/profiles/{profile}/, then try again.",
                image_path.display(),
            );
        }
        Ok(image_path)
    }

    fn resolve_kernel(&self) -> Result<PathBuf> {
        let kernel_path = kernel_path_for_profile(&self.config);
        if !kernel_path.exists() {
            anyhow::bail!(
                "VM kernel not found at {}\n\n\
                 Run 'abox init' or 'just bootstrap-vm' to download and assemble\n\
                 the VM stack, then try again.",
                kernel_path.display()
            );
        }
        Ok(kernel_path)
    }
}

fn map_state(state: &VmState) -> RuntimeState {
    match state {
        VmState::Starting => RuntimeState::Starting,
        VmState::Running => RuntimeState::Running,
        VmState::Paused => RuntimeState::Paused,
        VmState::Stopped => RuntimeState::Stopped,
    }
}

fn map_info(info: &VmInfo) -> RuntimeInstance {
    RuntimeInstance { id: info.id.clone(), state: map_state(&info.state), pid: Some(info.pid) }
}

impl SandboxRuntimePort for CloudHypervisorRuntime {
    async fn start(&self, spec: SandboxRuntimeSpec) -> Result<RuntimeInstance> {
        let image_path = self.resolve_image(spec.environment.profile())?;
        let kernel_path = self.resolve_kernel()?;

        let start_mode = match spec.start {
            RuntimeStart::Fresh => StartMode::Fresh,
            RuntimeStart::RestoreTemplate { template_path } => StartMode::Restore { template_path },
        };

        // The legacy stack supports exactly one cache mount at /abox-cache.
        let cache_mount_dir = spec.caches.first().map(|c| c.host_path.clone());

        let vm_config = VmConfig {
            id: spec.id.clone(),
            worktree_path: spec.workspace.host_path().clone(),
            image_path,
            kernel_path,
            memory_mib: spec.resources.memory_mib,
            vcpus: spec.resources.vcpus,
            user: spec.user,
            env_vars: spec.env,
            agent_command: spec.command,
            resolved_prompt: spec.resolved_prompt,
            cache_mount_dir,
            staged_prepare_script: spec.staged_prepare_script,
            start_mode,
            credential_files: spec.credential_files,
            ca_cert_pem: spec.ca_cert_pem,
            mount_excludes: spec.mount_excludes,
            services: spec.services,
            input_files: spec.inputs,
        };

        let info = self.inner.start(vm_config).await?;
        Ok(map_info(&info))
    }

    async fn stop(&self, id: &str) -> Result<()> {
        self.inner.stop(id).await
    }

    async fn kill(&self, id: &str) -> Result<()> {
        // Cloud Hypervisor's stop path (`shutdown-vmm` + child kill) is
        // already forceful; a second stop is the best available escalation.
        self.inner.stop(id).await
    }

    async fn info(&self, id: &str) -> Result<RuntimeInstance> {
        Ok(map_info(&self.inner.info(id).await?))
    }

    async fn list(&self) -> Result<Vec<RuntimeInstance>> {
        Ok(self.inner.list().await?.iter().map(map_info).collect())
    }

    async fn wait(&self, id: &str) -> Result<RuntimeExit> {
        self.inner.wait_for_exit(id).await?;
        // Read the exit code the guest wrote into the status share, then
        // tear the share down: it exists only to carry this one value.
        let exit_code = match self.inner.status_dir(id) {
            Some(status_dir) => {
                let code = read_exit_code(&status_dir);
                let _ = std::fs::remove_dir_all(&status_dir);
                code
            }
            None => None,
        };
        Ok(RuntimeExit { exit_code })
    }

    fn control_socket(&self, id: &str, guest_port: u32) -> PathBuf {
        // Cloud Hypervisor materializes guest vsock port traffic on
        // `<vsock-backend-socket>_<port>`.
        self.config.runtime_dir().join(format!("vsock-{id}.sock_{guest_port}"))
    }

    fn console_output(&self, id: &str) -> Option<PathBuf> {
        Some(self.config.runtime_dir().join(format!("console-{id}.log")))
    }

    async fn pause(&self, id: &str) -> Result<()> {
        self.inner.pause(id).await
    }

    async fn resume(&self, id: &str) -> Result<()> {
        self.inner.resume(id).await
    }

    fn memory_snapshot_handles(&self, id: &str) -> Option<MemorySnapshotHandles> {
        let runtime_dir = self.config.runtime_dir();
        let virtiofs_sockets = HashMap::from([
            ("workspace".to_string(), format!("vfs-{id}.sock")),
            ("meta".to_string(), format!("vfs-meta-{id}.sock")),
            ("status".to_string(), format!("vfs-status-{id}.sock")),
        ]);
        Some(MemorySnapshotHandles {
            api_socket: runtime_dir.join(format!("ch-api-{id}.sock")),
            virtiofs_sockets,
        })
    }
}
