//! Runtime contract suite for [`SandboxRuntimePort`] implementations.
//!
//! The `contract` module holds runtime-generic assertions written as
//! `async fn runtime_contract_*<R: SandboxRuntimePort>(runtime: &R)` helpers.
//! The `mock` module instantiates them against [`MockRuntime`], so the
//! contract compiles and runs everywhere (no hypervisor needed). A live
//! runtime binding (e.g. a gated MicroSandbox suite) can reuse the same
//! generic functions by constructing its runtime and calling into
//! `contract::*` with an appropriate spec-observer.

use abox_core::project::EnvironmentProfile;
use abox_core::runtime::{
    ControlChannel, RuntimeEnvironment, RuntimeLifecycle, RuntimeMount, RuntimeNetworkPlan,
    RuntimeResources, SandboxRuntimeSpec, WorkspaceMount, COMMAND_BROKER_PORT, HTTPS_EGRESS_PORT,
};

/// Runtime-generic contract assertions. Each function takes any
/// [`SandboxRuntimePort`] implementation and asserts one behavioral clause
/// of the port contract.
mod contract {
    use super::*;
    use abox_core::runtime::{RuntimeState, SandboxRuntimePort};

    /// A minimal but fully-populated spec with distinctive values, so
    /// passthrough assertions can detect field mix-ups.
    pub(crate) fn contract_spec(id: &str) -> SandboxRuntimeSpec {
        SandboxRuntimeSpec {
            id: id.to_string(),
            workspace: WorkspaceMount::ReadWrite(format!("/tmp/contract-ws-{id}").into()),
            environment: RuntimeEnvironment::Profile(EnvironmentProfile::Base),
            resources: RuntimeResources { memory_mib: 768, vcpus: 3 },
            user: Some("1000:1000".into()),
            env: vec![
                ("CONTRACT_MARKER".into(), format!("marker-{id}")),
                ("SECOND_VAR".into(), "second-value".into()),
            ],
            command: vec!["sh".into(), "-c".into(), format!("echo contract-{id}")],
            resolved_prompt: None,
            staged_prepare_script: None,
            credential_files: vec![],
            ca_cert_pem: None,
            inputs: vec![],
            caches: vec![RuntimeMount {
                host_path: format!("/tmp/contract-cache-{id}").into(),
                guest_path: "/var/cache/contract".into(),
                read_only: false,
            }],
            mount_excludes: vec![],
            services: vec![],
            control_channels: vec![
                ControlChannel { name: "command-broker".into(), guest_port: COMMAND_BROKER_PORT },
                ControlChannel { name: "https-egress".into(), guest_port: HTTPS_EGRESS_PORT },
            ],
            network: RuntimeNetworkPlan::HostMediated,
            native_secrets: vec![],
            lifecycle: RuntimeLifecycle::default(),
        }
    }

    /// `start()` returns an instance with the spec's id in the `Running`
    /// (or at least non-`Stopped`) state.
    pub(crate) async fn runtime_contract_start_produces_running_instance<R: SandboxRuntimePort>(
        runtime: &R,
    ) {
        let instance = runtime.start(contract_spec("start-a")).await.expect("start must succeed");
        assert_eq!(instance.id, "start-a", "instance id must match the spec id");
        assert_ne!(
            instance.state,
            RuntimeState::Stopped,
            "freshly started sandbox must not be Stopped"
        );
    }

    /// Every started sandbox appears in `list()`, and `info()` agrees with
    /// the listing (same id, live state). `info()` on an unknown id errors.
    pub(crate) async fn runtime_contract_list_info_consistency<R: SandboxRuntimePort>(runtime: &R) {
        runtime.start(contract_spec("li-a")).await.expect("start li-a");
        runtime.start(contract_spec("li-b")).await.expect("start li-b");

        let listed = runtime.list().await.expect("list must succeed");
        for id in ["li-a", "li-b"] {
            let row = listed
                .iter()
                .find(|i| i.id == id)
                .unwrap_or_else(|| panic!("started sandbox '{id}' missing from list()"));
            let info = runtime.info(id).await.expect("info on a listed sandbox must succeed");
            assert_eq!(info.id, row.id, "info/list id mismatch for '{id}'");
            assert_eq!(info.state, row.state, "info/list state mismatch for '{id}'");
        }

        assert!(
            runtime.info("li-never-started").await.is_err(),
            "info() on an unknown id must error"
        );
    }

    /// `stop()` removes the sandbox from `list()` and makes `info()` fail,
    /// while leaving other sandboxes untouched.
    pub(crate) async fn runtime_contract_stop_removes_from_list<R: SandboxRuntimePort>(
        runtime: &R,
    ) {
        runtime.start(contract_spec("stop-a")).await.expect("start stop-a");
        runtime.start(contract_spec("stop-b")).await.expect("start stop-b");

        runtime.stop("stop-a").await.expect("stop must succeed for a running sandbox");

        let listed = runtime.list().await.expect("list after stop");
        assert!(
            !listed.iter().any(|i| i.id == "stop-a"),
            "stopped sandbox must not appear in list()"
        );
        assert!(
            listed.iter().any(|i| i.id == "stop-b"),
            "stopping one sandbox must not remove others"
        );
        assert!(runtime.info("stop-a").await.is_err(), "info() on a stopped sandbox must error");
    }

    /// `wait()` reports the guest exit code the runtime observed.
    pub(crate) async fn runtime_contract_wait_returns_exit_code<R: SandboxRuntimePort>(
        runtime: &R,
        expected: i32,
    ) {
        runtime.start(contract_spec("wait-a")).await.expect("start wait-a");
        let exit = runtime.wait("wait-a").await.expect("wait must succeed");
        assert_eq!(exit.exit_code, Some(expected), "wait() must report the guest exit code");
    }

    /// `wait()` on an unknown (never started / already reaped) id succeeds
    /// with `exit_code: None` — it must not hang or error.
    pub(crate) async fn runtime_contract_wait_unknown_id_returns_none<R: SandboxRuntimePort>(
        runtime: &R,
    ) {
        let exit =
            runtime.wait("wait-never-started").await.expect("wait on an unknown id must not error");
        assert_eq!(
            exit.exit_code, None,
            "wait on an unknown id must report exit_code None (no fabricated code)"
        );
    }

    /// `control_socket()` is deterministic (same inputs → same path) and
    /// unique per sandbox and per guest port, so host-side attribution can
    /// derive from the socket path alone. (Plain fn: `control_socket` is the
    /// port's only synchronous method.)
    pub(crate) fn runtime_contract_control_socket_determinism<R: SandboxRuntimePort>(runtime: &R) {
        let a1 = runtime.control_socket("cs-a", COMMAND_BROKER_PORT);
        let a2 = runtime.control_socket("cs-a", COMMAND_BROKER_PORT);
        assert_eq!(a1, a2, "control_socket must be deterministic for (id, port)");

        let b = runtime.control_socket("cs-b", COMMAND_BROKER_PORT);
        assert_ne!(a1, b, "control_socket must be unique per sandbox id");

        let a_egress = runtime.control_socket("cs-a", HTTPS_EGRESS_PORT);
        assert_ne!(a1, a_egress, "control_socket must be unique per guest port");

        // Cross-check: different id AND different port never collide.
        let b_egress = runtime.control_socket("cs-b", HTTPS_EGRESS_PORT);
        for (label, other) in [("a broker", &a1), ("a egress", &a_egress), ("b broker", &b)] {
            assert_ne!(&b_egress, other, "socket collision between b egress and {label}");
        }
    }

    /// The spec handed to `start()` reaches the runtime unmodified in the
    /// fields the orchestrator relies on: resources, env, command, workspace
    /// path, caches, and control channels.
    ///
    /// `observed` retrieves the spec the runtime captured for a given id —
    /// for [`abox_core::runtime::testing::MockRuntime`] that is
    /// `started()`; a live binding can substitute its own introspection.
    pub(crate) async fn runtime_contract_spec_field_passthrough<R, F>(runtime: &R, observed: F)
    where
        R: SandboxRuntimePort,
        F: Fn(&str) -> Option<SandboxRuntimeSpec>,
    {
        let spec = contract_spec("pass-a");
        runtime.start(spec.clone()).await.expect("start pass-a");

        let got = observed("pass-a").expect("runtime must have captured the spec for 'pass-a'");

        assert_eq!(got.id, spec.id);
        assert_eq!(got.resources.memory_mib, spec.resources.memory_mib, "memory passthrough");
        assert_eq!(got.resources.vcpus, spec.resources.vcpus, "vcpus passthrough");
        assert_eq!(got.env, spec.env, "env passthrough (values and order)");
        assert_eq!(got.command, spec.command, "command passthrough (argv order)");
        assert_eq!(
            got.workspace.host_path(),
            spec.workspace.host_path(),
            "workspace host path passthrough"
        );

        assert_eq!(got.caches.len(), spec.caches.len(), "cache count passthrough");
        for (g, s) in got.caches.iter().zip(spec.caches.iter()) {
            assert_eq!(g.host_path, s.host_path, "cache host path passthrough");
            assert_eq!(g.guest_path, s.guest_path, "cache guest path passthrough");
            assert_eq!(g.read_only, s.read_only, "cache read_only passthrough");
        }

        assert_eq!(
            got.control_channels.len(),
            spec.control_channels.len(),
            "control channel count passthrough"
        );
        for (g, s) in got.control_channels.iter().zip(spec.control_channels.iter()) {
            assert_eq!(g.name, s.name, "control channel name passthrough");
            assert_eq!(g.guest_port, s.guest_port, "control channel guest port passthrough");
        }
    }
}

/// The contract instantiated against [`MockRuntime`] — runs everywhere.
mod mock {
    use super::contract;
    use abox_core::runtime::testing::{MockBehavior, MockRuntime};
    use tempfile::TempDir;

    fn mock(control_dir: &TempDir) -> MockRuntime {
        MockRuntime::new(control_dir.path().to_path_buf())
    }

    #[tokio::test]
    async fn start_produces_running_instance() {
        let dir = TempDir::new().unwrap();
        contract::runtime_contract_start_produces_running_instance(&mock(&dir)).await;
    }

    #[tokio::test]
    async fn list_info_consistency() {
        let dir = TempDir::new().unwrap();
        contract::runtime_contract_list_info_consistency(&mock(&dir)).await;
    }

    #[tokio::test]
    async fn stop_removes_from_list() {
        let dir = TempDir::new().unwrap();
        contract::runtime_contract_stop_removes_from_list(&mock(&dir)).await;
    }

    #[tokio::test]
    async fn wait_returns_exit_code() {
        let dir = TempDir::new().unwrap();
        // Configure the mock's guest to "exit 7" so the contract observes a
        // distinctive code (0 would also pass a broken always-zero runtime).
        let runtime = MockRuntime::with_behavior(
            dir.path().to_path_buf(),
            MockBehavior { exit_code: Some(7), ..MockBehavior::default() },
        );
        contract::runtime_contract_wait_returns_exit_code(&runtime, 7).await;
    }

    #[tokio::test]
    async fn wait_unknown_id_returns_none() {
        let dir = TempDir::new().unwrap();
        // exit_code: None mirrors "no guest exit observed" — the contract
        // requires unknown ids to surface as None rather than error.
        let runtime = MockRuntime::with_behavior(
            dir.path().to_path_buf(),
            MockBehavior { exit_code: None, ..MockBehavior::default() },
        );
        contract::runtime_contract_wait_unknown_id_returns_none(&runtime).await;
    }

    #[test]
    fn control_socket_determinism_and_uniqueness() {
        let dir = TempDir::new().unwrap();
        contract::runtime_contract_control_socket_determinism(&mock(&dir));
    }

    #[tokio::test]
    async fn spec_field_passthrough() {
        let dir = TempDir::new().unwrap();
        let runtime = mock(&dir);
        let observer = runtime.clone();
        contract::runtime_contract_spec_field_passthrough(&runtime, move |id| {
            observer.started().into_iter().find(|s| s.id == id)
        })
        .await;
    }
}
