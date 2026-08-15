//! Reusable in-memory [`SandboxRuntimePort`] mock for tests.
//!
//! Available behind the `test-support` feature so that both `abox-core`
//! integration tests and `abox-cli` unit tests exercise the orchestrator
//! against one configurable mock instead of hand-rolling per-test adapters.

use super::{RuntimeExit, RuntimeInstance, RuntimeState, SandboxRuntimePort, SandboxRuntimeSpec};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Configurable behavior for [`MockRuntime`].
#[derive(Debug, Clone, Default)]
pub struct MockBehavior {
    /// `start()` fails with this message instead of starting.
    pub start_error: Option<String>,
    /// `start()` panics — for tests asserting the runtime is never used.
    pub panic_on_start: bool,
    /// Exit code `wait()` reports. `None` simulates a sandbox that died
    /// without reporting a code (crash before guest init).
    pub exit_code: Option<i32>,
    /// `wait()` sleeps this long before reporting the exit.
    pub exit_delay: Option<Duration>,
    /// `wait()` never returns — for timeout tests.
    pub never_exit: bool,
}

#[derive(Debug, Default)]
struct MockState {
    started: Vec<SandboxRuntimeSpec>,
    stopped: Vec<String>,
    killed: Vec<String>,
    paused: Vec<String>,
    resumed: Vec<String>,
}

/// In-memory mock runtime.
#[derive(Clone)]
pub struct MockRuntime {
    behavior: MockBehavior,
    control_dir: PathBuf,
    state: Arc<Mutex<MockState>>,
}

impl MockRuntime {
    /// Mock whose sandboxes exit immediately with code 0.
    pub fn new(control_dir: PathBuf) -> Self {
        Self::with_behavior(
            control_dir,
            MockBehavior { exit_code: Some(0), ..MockBehavior::default() },
        )
    }

    /// Mock with fully custom behavior.
    pub fn with_behavior(control_dir: PathBuf, behavior: MockBehavior) -> Self {
        Self { behavior, control_dir, state: Arc::new(Mutex::new(MockState::default())) }
    }

    /// Mock that panics if a sandbox is ever started.
    pub fn panicking(control_dir: PathBuf) -> Self {
        Self::with_behavior(
            control_dir,
            MockBehavior { panic_on_start: true, ..MockBehavior::default() },
        )
    }

    /// Specs passed to `start()`, in order.
    pub fn started(&self) -> Vec<SandboxRuntimeSpec> {
        self.state.lock().unwrap().started.clone()
    }

    /// Sandbox ids passed to `stop()`, in order.
    pub fn stopped(&self) -> Vec<String> {
        self.state.lock().unwrap().stopped.clone()
    }

    /// Sandbox ids passed to `kill()`, in order.
    pub fn killed(&self) -> Vec<String> {
        self.state.lock().unwrap().killed.clone()
    }

    /// Sandbox ids passed to `pause()`, in order.
    pub fn paused(&self) -> Vec<String> {
        self.state.lock().unwrap().paused.clone()
    }

    /// Sandbox ids passed to `resume()`, in order.
    pub fn resumed(&self) -> Vec<String> {
        self.state.lock().unwrap().resumed.clone()
    }

    fn is_running(&self, id: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.started.iter().any(|s| s.id == id) && !state.stopped.iter().any(|s| s == id)
    }
}

impl SandboxRuntimePort for MockRuntime {
    async fn start(&self, spec: SandboxRuntimeSpec) -> Result<RuntimeInstance> {
        assert!(!self.behavior.panic_on_start, "runtime should not be used in this test");
        if let Some(msg) = &self.behavior.start_error {
            anyhow::bail!("{msg}");
        }
        let id = spec.id.clone();
        self.state.lock().unwrap().started.push(spec);
        Ok(RuntimeInstance { id, state: RuntimeState::Running, pid: Some(12345) })
    }

    async fn stop(&self, id: &str) -> Result<()> {
        self.state.lock().unwrap().stopped.push(id.to_string());
        Ok(())
    }

    async fn kill(&self, id: &str) -> Result<()> {
        self.state.lock().unwrap().killed.push(id.to_string());
        Ok(())
    }

    async fn info(&self, id: &str) -> Result<RuntimeInstance> {
        if self.is_running(id) {
            Ok(RuntimeInstance {
                id: id.to_string(),
                state: RuntimeState::Running,
                pid: Some(12345),
            })
        } else {
            anyhow::bail!("sandbox '{id}' is not running")
        }
    }

    async fn list(&self) -> Result<Vec<RuntimeInstance>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .started
            .iter()
            .filter(|s| !state.stopped.iter().any(|id| id == &s.id))
            .map(|s| RuntimeInstance {
                id: s.id.clone(),
                state: RuntimeState::Running,
                pid: Some(12345),
            })
            .collect())
    }

    async fn wait(&self, _id: &str) -> Result<RuntimeExit> {
        if self.behavior.never_exit {
            std::future::pending::<()>().await;
        }
        if let Some(delay) = self.behavior.exit_delay {
            tokio::time::sleep(delay).await;
        }
        Ok(RuntimeExit { exit_code: self.behavior.exit_code })
    }

    fn control_socket(&self, id: &str, guest_port: u32) -> PathBuf {
        self.control_dir.join(format!("mock-{id}.sock_{guest_port}"))
    }

    async fn pause(&self, id: &str) -> Result<()> {
        self.state.lock().unwrap().paused.push(id.to_string());
        Ok(())
    }

    async fn resume(&self, id: &str) -> Result<()> {
        self.state.lock().unwrap().resumed.push(id.to_string());
        Ok(())
    }
}
