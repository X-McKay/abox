//! Transitional runtime selection (ADR-008).
//!
//! Dispatches between the MicroSandbox runtime (default) and the deprecated
//! Cloud Hypervisor fallback based on host configuration (`[runtime]
//! backend`) or the `ABOX_RUNTIME_BACKEND` environment variable. Deleted
//! together with the legacy backend, at which point the CLI constructs
//! [`MicrosandboxRuntime`] directly.

use crate::adapters::cloud_hypervisor_runtime::CloudHypervisorRuntime;
use crate::adapters::microsandbox::MicrosandboxRuntime;
use crate::config::{AboxConfig, RuntimeBackend};
use crate::runtime::{
    MemorySnapshotHandles, RuntimeExit, RuntimeInstance, SandboxRuntimePort, SandboxRuntimeSpec,
};
use anyhow::Result;
use std::path::PathBuf;

/// The runtime backend selected by host configuration.
#[allow(clippy::large_enum_variant)] // two-variant transitional enum, one instance per process
pub enum SelectedRuntime {
    Microsandbox(MicrosandboxRuntime),
    CloudHypervisor(CloudHypervisorRuntime),
}

impl SelectedRuntime {
    /// Construct the configured backend.
    pub fn from_config(config: &AboxConfig) -> Result<Self> {
        match config.runtime.effective_backend()? {
            RuntimeBackend::Microsandbox => {
                Ok(Self::Microsandbox(MicrosandboxRuntime::new(config)?))
            }
            RuntimeBackend::CloudHypervisor => {
                tracing::warn!(
                    "the cloud-hypervisor runtime backend is deprecated and will be removed; \
                     see ADR-008"
                );
                Ok(Self::CloudHypervisor(CloudHypervisorRuntime::new(config.clone())?))
            }
        }
    }

    /// The backend's name, for diagnostics.
    pub fn backend(&self) -> RuntimeBackend {
        match self {
            Self::Microsandbox(_) => RuntimeBackend::Microsandbox,
            Self::CloudHypervisor(_) => RuntimeBackend::CloudHypervisor,
        }
    }
}

macro_rules! delegate {
    ($self:ident, $inner:ident => $expr:expr) => {
        match $self {
            SelectedRuntime::Microsandbox($inner) => $expr,
            SelectedRuntime::CloudHypervisor($inner) => $expr,
        }
    };
}

impl SandboxRuntimePort for SelectedRuntime {
    async fn start(&self, spec: SandboxRuntimeSpec) -> Result<RuntimeInstance> {
        delegate!(self, r => r.start(spec).await)
    }

    async fn stop(&self, id: &str) -> Result<()> {
        delegate!(self, r => r.stop(id).await)
    }

    async fn kill(&self, id: &str) -> Result<()> {
        delegate!(self, r => r.kill(id).await)
    }

    async fn info(&self, id: &str) -> Result<RuntimeInstance> {
        delegate!(self, r => r.info(id).await)
    }

    async fn list(&self) -> Result<Vec<RuntimeInstance>> {
        delegate!(self, r => r.list().await)
    }

    async fn wait(&self, id: &str) -> Result<RuntimeExit> {
        delegate!(self, r => r.wait(id).await)
    }

    fn control_socket(&self, id: &str, guest_port: u32) -> PathBuf {
        delegate!(self, r => r.control_socket(id, guest_port))
    }

    fn console_output(&self, id: &str) -> Option<PathBuf> {
        delegate!(self, r => r.console_output(id))
    }

    async fn pause(&self, id: &str) -> Result<()> {
        delegate!(self, r => r.pause(id).await)
    }

    async fn resume(&self, id: &str) -> Result<()> {
        delegate!(self, r => r.resume(id).await)
    }

    fn memory_snapshot_handles(&self, id: &str) -> Option<MemorySnapshotHandles> {
        delegate!(self, r => r.memory_snapshot_handles(id))
    }
}
