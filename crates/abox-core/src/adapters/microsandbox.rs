//! MicroSandbox implementation of [`SandboxRuntimePort`] (ADR-008).
//!
//! Translates the runtime-neutral [`SandboxRuntimeSpec`] into a MicroSandbox
//! microVM (libkrun: KVM on Linux, Hypervisor.framework on macOS):
//!
//! - environment profiles resolve to pinned OCI images
//!   ([`crate::runtime::images`]);
//! - the task worktree bind-mounts read-write at `/workspace`; guest
//!   ownership is granted through the mount's metadata overlay (a root
//!   `chown` before the agent runs), so host inodes keep their owner exactly
//!   like the legacy virtiofsd uid-map did;
//! - prompt/prepare/credential-stub/input files are staged as pre-boot
//!   rootfs patches under `/abox-meta` (read-only modes);
//! - control channels (command broker, HTTPS egress, service bridges) are
//!   vsock routes from well-known guest ports to per-sandbox host Unix
//!   sockets — attribution stays host-controlled;
//! - the agent runs as a non-root exec through the guest agent, so its exit
//!   code propagates directly (no status-share file protocol);
//! - `mount_excludes` become tmpfs volumes shadowing workspace
//!   subdirectories.
//!
//! Security invariants owned here: the guest never receives real
//! credentials (only stubs staged by the orchestrator), the network plan is
//! compiled by abox and never widened (`HostMediated` ⇒ networking fully
//! disabled; all egress rides the vsock control channels), and unsupported
//! spec combinations fail at start time rather than degrade.

use crate::runtime::images::ImageManifest;
use crate::runtime::spec::NativeNetworkPlan;
use crate::runtime::{
    RuntimeExit, RuntimeInstance, RuntimeNetworkPlan, RuntimeStart, RuntimeState,
    SandboxRuntimePort, SandboxRuntimeSpec, COMMAND_BROKER_PORT,
};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

/// Fixed guest loopback port the agent's `HTTPS_PROXY` points at; bridged to
/// the HTTPS egress control channel by `abox-bridge`.
const GUEST_EGRESS_TCP_PORT: u16 = 18443;

/// Guest Unix socket the shim connects to; bridged to the command-broker
/// control channel by `abox-bridge`. The broker deliberately rides the
/// long-lived bridge process: the current MicroSandbox VMM drops host→guest
/// delivery for vsock connections opened by short-lived guest processes
/// after the first, while a persistent process's connections are reliable.
/// Attribution is unaffected (per-sandbox host route).
const GUEST_BROKER_SOCKET: &str = "/run/abox-proxy.sock";

/// Where the shim's transport declaration is staged in the guest.
const GUEST_TRANSPORT_PATH: &str = "/etc/abox/transport";

/// Default guest user (uid:gid) the agent runs as — matches the `abox` user
/// baked into the official guest images.
const DEFAULT_GUEST_USER: &str = "1000:1000";

/// Parameters for the deferred agent launch.
#[derive(Debug, Clone)]
struct AgentLaunch {
    command: Vec<String>,
    env: Vec<(String, String)>,
    user: String,
}

/// Book-keeping for one running sandbox.
struct TaskEntry {
    sandbox: microsandbox::Sandbox,
    pid: Option<u32>,
    /// Agent launch parameters; consumed by the first `wait()`. The agent is
    /// deliberately NOT started in `start()` so the orchestrator can bind the
    /// command-broker/egress listeners on the control sockets first.
    launch: Option<AgentLaunch>,
}

/// MicroSandbox runtime adapter.
pub struct MicrosandboxRuntime {
    runtime_dir: PathBuf,
    manifest: ImageManifest,
    /// Host-staged guest binaries (abox-shim, abox-bridge), when installed
    /// under `<state_dir>/guest/<arch>/`. Patched into every guest at start
    /// so shim protocol stays in lockstep with the host binary even when the
    /// OCI image bakes an older copy.
    guest_bin_dir: Option<PathBuf>,
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
}

impl MicrosandboxRuntime {
    /// Create the runtime adapter.
    pub fn new(config: &crate::config::AboxConfig) -> Result<Self> {
        let manifest = ImageManifest::embedded()?.with_overrides(config.images.overrides.clone());
        let runtime_dir = config.runtime_dir();
        std::fs::create_dir_all(&runtime_dir)
            .with_context(|| format!("Failed to create runtime dir {}", runtime_dir.display()))?;
        let guest_bin_dir = guest_binaries_dir(&config.state_dir);
        Ok(Self {
            runtime_dir,
            manifest,
            guest_bin_dir,
            tasks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The MicroSandbox sandbox name for a task. Prefixed so abox sandboxes
    /// are recognizable in `msb`'s own state.
    fn sandbox_name(id: &str) -> String {
        format!("abox-{id}")
    }
}

/// Locate host-staged guest binaries for the guest architecture (which
/// always matches the host architecture under both KVM and
/// Hypervisor.framework). Returns `None` unless both binaries are present.
fn guest_binaries_dir(state_dir: &std::path::Path) -> Option<PathBuf> {
    let dir = state_dir.join("guest").join(std::env::consts::ARCH);
    if dir.join("abox-shim").is_file() && dir.join("abox-bridge").is_file() {
        Some(dir)
    } else {
        None
    }
}

/// Escape a string for single-quoted POSIX shell embedding.
fn sh_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Validate a workspace mount-exclude path: relative, no traversal.
fn validate_mount_exclude(exclude: &str) -> Result<()> {
    anyhow::ensure!(!exclude.is_empty(), "mount exclude must not be empty");
    anyhow::ensure!(!exclude.starts_with('/'), "mount exclude must be relative: {exclude:?}");
    anyhow::ensure!(
        exclude.split('/').all(|c| !c.is_empty() && c != "." && c != ".."),
        "mount exclude must not contain traversal components: {exclude:?}"
    );
    Ok(())
}

/// Build the root setup script that runs once before the agent:
/// ownership overlay for writable mounts, scratch tmpdir, CA trust,
/// credential-stub placement.
fn guest_setup_script(spec: &SandboxRuntimeSpec, chown_user: &str) -> String {
    use std::fmt::Write as _;
    let mut script = String::from("set -e\n");

    // Writable-mount ownership. With host_permissions=Private this lands in
    // the mount's metadata overlay; host inodes keep their owner.
    script.push_str("mkdir -p /home/abox\n");
    let _ = writeln!(script, "chown {chown_user} /home/abox");
    let _ = writeln!(script, "chown -R {chown_user} /workspace");
    for cache in &spec.caches {
        if !cache.read_only {
            let _ = writeln!(script, "chown -R {chown_user} {}", sh_escape(&cache.guest_path));
        }
    }

    // Proxied host commands resolve to the shim. Official images bake these
    // symlinks; create them for any image that lacks them.
    script.push_str(
        "if [ -x /usr/local/bin/abox-shim ]; then\n\
           for c in git gh aws; do\n\
             [ -e \"/usr/local/bin/$c\" ] || ln -s abox-shim \"/usr/local/bin/$c\"\n\
           done\n\
         fi\n",
    );

    // Private scratch tmpdir (exec allowed; see ADR-005 rationale).
    script.push_str("mkdir -p /run/abox-tmp\n");
    let _ = writeln!(script, "chown {chown_user} /run/abox-tmp");
    script.push_str("chmod 0700 /run/abox-tmp\n");

    // Root CA trust for the host-mediated HTTPS egress proxy.
    if spec.ca_cert_pem.is_some() {
        script.push_str(
            "mkdir -p /etc/ssl/certs\n\
             cp /abox-meta/root.crt /etc/ssl/certs/abox-ca.pem\n\
             chmod 0444 /etc/ssl/certs/abox-ca.pem\n\
             if [ -f /etc/ssl/certs/ca-certificates.crt ]; then\n\
               cat /abox-meta/root.crt >> /etc/ssl/certs/ca-certificates.crt\n\
             else\n\
               cp /abox-meta/root.crt /etc/ssl/certs/ca-certificates.crt\n\
             fi\n",
        );
    }

    // Credential stubs: placeholders only — the orchestrator never hands the
    // adapter a real secret.
    for cred in &spec.credential_files {
        let dest = sh_escape(&cred.guest_path);
        let src = sh_escape(&format!("/abox-meta/credentials/{}", cred.index));
        let mode = sh_escape(&cred.mode);
        let _ = write!(
            script,
            "d=$(dirname {dest}); mkdir -p \"$d\"\n\
             case {dest} in /home/abox/*) chown -R {chown_user} \"$d\" ;; esac\n\
             cp {src} {dest}\n\
             chmod {mode} {dest}\n\
             chown {chown_user} {dest}\n"
        );
    }

    script
}

/// Compile a [`NativeNetworkPlan`] into a MicroSandbox `NetworkPolicy`.
///
/// The plan's invariants are implemented as explicit first-match deny rules
/// so they can never be shadowed: loopback, private ranges, link-local,
/// cloud metadata, multicast, and the host itself are denied before any
/// allow rule. Egress is TCP 443 plus gateway DNS only; ingress is denied
/// entirely; defaults are deny.
fn compile_msb_network_policy(
    plan: &NativeNetworkPlan,
) -> anyhow::Result<microsandbox::NetworkPolicy> {
    use microsandbox_network::policy::{
        Action, Destination, DestinationGroup, Direction, DomainName, PortRange, Protocol, Rule,
    };

    let mut rules = Vec::new();
    // Narrow gateway-only DNS first (UDP/TCP 53 to the gateway forwarder):
    // it must precede the Host-group denial below, or first-match-wins would
    // block every DNS query. Required for hostname resolution under a
    // deny-by-default policy; the runtime pins resolved IPs to names.
    rules.push(Rule::allow_dns());
    for group in [
        DestinationGroup::Loopback,
        DestinationGroup::Private,
        DestinationGroup::LinkLocal,
        DestinationGroup::Metadata,
        DestinationGroup::Multicast,
        DestinationGroup::Host,
    ] {
        rules.push(Rule::deny_egress(Destination::Group(group)));
    }

    let https = |destination: Destination| Rule {
        direction: Direction::Egress,
        destination,
        protocols: vec![Protocol::Tcp],
        ports: vec![PortRange::single(443)],
        action: Action::Allow,
    };

    if plan.allow_public {
        rules.push(https(Destination::Group(DestinationGroup::Public)));
    } else {
        for host in &plan.allowed_hosts {
            let name = DomainName::try_from(host.as_str())
                .map_err(|e| anyhow::anyhow!("invalid allowed host {host:?}: {e}"))?;
            rules.push(https(Destination::Domain(name)));
        }
    }

    Ok(microsandbox::NetworkPolicy {
        default_egress: Action::Deny,
        default_ingress: Action::Deny,
        rules,
    })
}

/// Assemble `abox-bridge` arguments for the guest-side loopback bridges:
/// the HTTPS egress port plus one per service/host-port bridge.
fn bridge_args(spec: &SandboxRuntimeSpec) -> Vec<String> {
    let mut args = vec![
        GUEST_BROKER_SOCKET.to_string(),
        COMMAND_BROKER_PORT.to_string(),
        GUEST_EGRESS_TCP_PORT.to_string(),
        crate::runtime::HTTPS_EGRESS_PORT.to_string(),
    ];
    for svc in &spec.services {
        args.push(svc.guest_port.to_string());
        args.push(svc.vsock_port.to_string());
    }
    args
}

impl SandboxRuntimePort for MicrosandboxRuntime {
    async fn start(&self, spec: SandboxRuntimeSpec) -> Result<RuntimeInstance> {
        if let RuntimeStart::RestoreTemplate { .. } = spec.start {
            anyhow::bail!(
                "memory-snapshot templates are not supported by the MicroSandbox runtime; \
                 use environment warming ('abox env warm') instead"
            );
        }

        let id = spec.id.clone();
        let image =
            self.manifest.image_for_profile(spec.environment.profile()).with_context(|| {
                format!("no guest image for profile '{}'", spec.environment.profile())
            })?;

        let mut builder = microsandbox::Sandbox::builder(Self::sandbox_name(&id))
            .image(image.pull_reference().as_str())
            .cpus(spec.resources.vcpus)
            .memory(spec.resources.memory_mib)
            // abox owns durable task state (worktree, audit); MicroSandbox
            // state is disposable. Ephemeral also guarantees no spec data
            // outlives the sandbox in msb's database.
            .ephemeral(true)
            .replace();

        // Network plan compiled by abox — never MicroSandbox defaults.
        match &spec.network {
            RuntimeNetworkPlan::HostMediated => {
                builder = builder.disable_network();
            }
            RuntimeNetworkPlan::Native(plan) => {
                let policy = compile_msb_network_policy(plan)?;
                builder = builder.network(|n| n.policy(policy));
            }
        }

        // Native secret substitution: host-held source references only —
        // real values are resolved by the runtime at spawn from the host
        // environment and never persist in durable sandbox state.
        for secret in &spec.native_secrets {
            anyhow::ensure!(
                matches!(spec.network, RuntimeNetworkPlan::Native(_)),
                "native secret substitution for '{}' requires a native network plan",
                secret.allowed_host
            );
            let env_var = secret.env_var.clone();
            let source = secret.source_env_var.clone();
            let host = secret.allowed_host.clone();
            builder = builder.secret(move |s| {
                let s = s
                    .env(env_var)
                    .source(microsandbox::SecretSource::Env { var: source })
                    .require_tls_identity(true);
                if let Some(suffix) = host.strip_prefix("*.") {
                    s.allow_host_pattern(format!("*.{suffix}"))
                } else {
                    s.allow_host(host)
                }
            });
        }

        // Native max-duration as defense in depth behind the orchestrator's
        // own timeout handling (which produces the user-visible exit 124).
        if let Some(timeout) = spec.lifecycle.timeout_secs {
            builder = builder.max_duration(timeout.saturating_add(60));
        }

        // Workspace + caches. Bind paths must be symlink-free — the runtime's
        // filesystem broker rejects symlinked ancestors (e.g. macOS /tmp).
        let workspace_host =
            spec.workspace.host_path().canonicalize().with_context(|| {
                format!("worktree path {:?} not found", spec.workspace.host_path())
            })?;
        builder = builder.volume("/workspace", |m| m.bind(workspace_host.display().to_string()));
        for cache in &spec.caches {
            let host = cache
                .host_path
                .canonicalize()
                .with_context(|| format!("cache path {:?} not found", cache.host_path))?
                .display()
                .to_string();
            let read_only = cache.read_only;
            builder = builder.volume(cache.guest_path.clone(), move |m| {
                let m = m.bind(host);
                if read_only {
                    m.readonly()
                } else {
                    m
                }
            });
        }

        // Workspace shadows (tmpfs over exclusions).
        for exclude in &spec.mount_excludes {
            validate_mount_exclude(exclude)?;
            builder = builder.volume(
                format!("/workspace/{exclude}"),
                microsandbox::sandbox::MountBuilder::tmpfs,
            );
        }

        // Control channels: guest vsock port → per-sandbox host socket.
        for channel in &spec.control_channels {
            let socket = self.control_socket(&id, channel.guest_port);
            builder = builder.vsock(&socket, channel.guest_port);
        }

        // Staged guest files (pre-boot rootfs patches).
        let transport_decl =
            format!("# staged by abox — do not edit\nunix:{GUEST_BROKER_SOCKET}\n");
        let guest_bin_dir = self.guest_bin_dir.clone();
        if guest_bin_dir.is_none() {
            tracing::debug!(
                task_id = %id,
                "no host-staged guest binaries; relying on the image's own \
                 abox-shim/abox-bridge"
            );
        }
        builder = builder.patch(|mut p| {
            p = p.mkdir("/abox-meta", Some(0o755));
            if let Some(dir) = &guest_bin_dir {
                p = p
                    .copy_file(dir.join("abox-shim"), "/usr/local/bin/abox-shim", Some(0o755), true)
                    .copy_file(
                        dir.join("abox-bridge"),
                        "/usr/local/bin/abox-bridge",
                        Some(0o755),
                        true,
                    );
            }
            p = p.text(GUEST_TRANSPORT_PATH, transport_decl.clone(), Some(0o444), true);
            if let Some(prompt) = &spec.resolved_prompt {
                p = p.text("/abox-meta/prompt.md", prompt.clone(), Some(0o444), true);
            }
            if let Some(prepare) = &spec.staged_prepare_script {
                p = p.text("/abox-meta/prepare.sh", prepare.clone(), Some(0o755), true);
            }
            if let Some(ca_pem) = &spec.ca_cert_pem {
                p = p.text("/abox-meta/root.crt", ca_pem.clone(), Some(0o444), true);
            }
            for cred in &spec.credential_files {
                p = p.file(
                    format!("/abox-meta/credentials/{}", cred.index),
                    cred.content.clone(),
                    Some(0o600),
                    true,
                );
            }
            for input in &spec.inputs {
                p = p.copy_file(
                    input.host_path.clone(),
                    format!("/abox-meta/inputs/{}", input.guest_name),
                    Some(0o444),
                    true,
                );
            }
            p
        });

        let sandbox = builder
            .create()
            .await
            .with_context(|| format!("Failed to start MicroSandbox sandbox for '{id}'"))?;

        let pid = match sandbox.local() {
            Some(local) => match &local.handle {
                Some(handle) => Some(handle.lock().await.pid()),
                None => None,
            },
            None => None,
        };

        let user = spec.user.clone().unwrap_or_else(|| DEFAULT_GUEST_USER.to_string());

        // One-time root setup: ownership overlay, scratch dir, CA trust,
        // credential stubs. Fail closed if it doesn't succeed.
        let setup = guest_setup_script(&spec, &user);
        let setup_out = sandbox
            .exec_with("sh", |o| o.args(["-c", setup.as_str()]))
            .await
            .with_context(|| format!("guest setup exec failed for '{id}'"))?;
        if !setup_out.status().success {
            let _ = sandbox.stop().await;
            anyhow::bail!(
                "guest setup script failed for '{id}' (exit {}): {}",
                setup_out.status().code,
                setup_out.stderr().unwrap_or_default()
            );
        }

        // Guest-side loopback bridges (HTTPS egress + services) via
        // abox-bridge. Missing binary (non-abox guest image) is tolerated:
        // egress simply stays unavailable; command-broker traffic is direct
        // vsock and does not need it.
        let bridge = bridge_args(&spec);
        match sandbox
            .exec_stream_with("/usr/local/bin/abox-bridge", |o| {
                o.args(bridge.iter().map(String::as_str))
            })
            .await
        {
            Ok(mut handle) => {
                let bridge_id = id.clone();
                tokio::spawn(async move {
                    // Drain events for the lifetime of the sandbox; the
                    // stream ends when the sandbox stops.
                    while let Some(event) = handle.recv().await {
                        match event {
                            microsandbox::ExecEvent::Stderr(bytes) => {
                                tracing::warn!(
                                    task_id = %bridge_id,
                                    msg = %String::from_utf8_lossy(&bytes),
                                    "abox-bridge stderr"
                                );
                            }
                            microsandbox::ExecEvent::Failed(e) => {
                                tracing::warn!(
                                    task_id = %bridge_id,
                                    error = ?e,
                                    "abox-bridge unavailable in guest; host-mediated \
                                     HTTPS egress will not work for this sandbox"
                                );
                            }
                            microsandbox::ExecEvent::Exited { code } => {
                                tracing::debug!(task_id = %bridge_id, code, "abox-bridge exited");
                            }
                            _ => {}
                        }
                    }
                });
            }
            Err(e) => {
                tracing::warn!(task_id = %id, error = %e, "failed to launch abox-bridge");
            }
        }

        // Agent env: fixed guest contract first, then user env, then forced
        // overrides (TMPDIR must win over user-supplied values).
        let mut env: Vec<(String, String)> = vec![
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin:/sbin".into()),
            ("HOME".into(), "/home/abox".into()),
            ("USER".into(), "abox".into()),
            ("ABOX_CWD".into(), "/workspace".into()),
            ("ABOX_SANDBOX_ID".into(), id.clone()),
        ];
        env.extend(spec.env.iter().cloned());
        env.push(("TMPDIR".into(), "/run/abox-tmp".into()));

        let launch = AgentLaunch { command: spec.command.clone(), env, user };

        self.tasks
            .lock()
            .unwrap()
            .insert(id.clone(), TaskEntry { sandbox, pid, launch: Some(launch) });

        Ok(RuntimeInstance { id, state: RuntimeState::Running, pid })
    }

    async fn stop(&self, id: &str) -> Result<()> {
        let entry = self.tasks.lock().unwrap().remove(id);
        match entry {
            Some(entry) => {
                entry.sandbox.stop().await.with_context(|| format!("Failed to stop '{id}'"))
            }
            None => anyhow::bail!("sandbox '{id}' is not managed by this process"),
        }
    }

    async fn kill(&self, id: &str) -> Result<()> {
        let entry = self.tasks.lock().unwrap().remove(id);
        match entry {
            Some(entry) => {
                entry.sandbox.kill().await.with_context(|| format!("Failed to kill '{id}'"))
            }
            None => anyhow::bail!("sandbox '{id}' is not managed by this process"),
        }
    }

    async fn info(&self, id: &str) -> Result<RuntimeInstance> {
        let tasks = self.tasks.lock().unwrap();
        match tasks.get(id) {
            Some(entry) => Ok(RuntimeInstance {
                id: id.to_string(),
                state: RuntimeState::Running,
                pid: entry.pid,
            }),
            None => anyhow::bail!("sandbox '{id}' is not running"),
        }
    }

    async fn list(&self) -> Result<Vec<RuntimeInstance>> {
        let tasks = self.tasks.lock().unwrap();
        Ok(tasks
            .iter()
            .map(|(id, entry)| RuntimeInstance {
                id: id.clone(),
                state: RuntimeState::Running,
                pid: entry.pid,
            })
            .collect())
    }

    async fn wait(&self, id: &str) -> Result<RuntimeExit> {
        // Take the pending agent launch (first waiter runs the agent).
        let (sandbox, launch) = {
            let mut tasks = self.tasks.lock().unwrap();
            match tasks.get_mut(id) {
                Some(entry) => (entry.sandbox.clone(), entry.launch.take()),
                // Unknown/already-reaped: nothing to wait for.
                None => return Ok(RuntimeExit { exit_code: None }),
            }
        };

        let exit_code = if let Some(launch) = launch {
            run_agent(&sandbox, id, launch).await
        } else {
            // Agent already consumed (e.g. grace wait after a timeout
            // stop): wait for the sandbox itself to terminate.
            let _ = sandbox.wait().await;
            None
        };

        // Agent exit ends the sandbox (parity with the legacy guest init,
        // which powered the VM off after the agent finished).
        let _ = sandbox.stop().await;
        self.tasks.lock().unwrap().remove(id);

        Ok(RuntimeExit { exit_code })
    }

    fn control_socket(&self, id: &str, guest_port: u32) -> PathBuf {
        self.runtime_dir.join(format!("msb-{id}.sock_{guest_port}"))
    }
}

/// Run the agent command in the guest, streaming output to the host's
/// stdio. Returns the agent's exit code, or `None` if it could not be
/// spawned or the stream ended without an exit event (sandbox died).
async fn run_agent(sandbox: &microsandbox::Sandbox, id: &str, launch: AgentLaunch) -> Option<i32> {
    let (cmd, args) = launch.command.split_first()?;

    let mut handle = match sandbox
        .exec_stream_with(cmd, |mut o| {
            o = o.args(args.iter().map(String::as_str)).cwd("/workspace").user(launch.user.clone());
            for (k, v) in &launch.env {
                o = o.env(k.clone(), v.clone());
            }
            o
        })
        .await
    {
        Ok(handle) => handle,
        Err(e) => {
            tracing::warn!(task_id = %id, error = %e, "failed to spawn agent in guest");
            return None;
        }
    };

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut exit_code = None;
    while let Some(event) = handle.recv().await {
        match event {
            microsandbox::ExecEvent::Stdout(bytes) => {
                let _ = stdout.write_all(&bytes).await;
                let _ = stdout.flush().await;
            }
            microsandbox::ExecEvent::Stderr(bytes) => {
                let _ = stderr.write_all(&bytes).await;
                let _ = stderr.flush().await;
            }
            microsandbox::ExecEvent::Exited { code } => {
                exit_code = Some(code);
            }
            microsandbox::ExecEvent::Failed(e) => {
                tracing::warn!(task_id = %id, error = ?e, "agent exec failed in guest");
            }
            _ => {}
        }
    }
    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::EnvironmentProfile;
    use crate::runtime::{
        ControlChannel, CredentialToStage, RuntimeEnvironment, RuntimeLifecycle, RuntimeResources,
        WorkspaceMount,
    };

    fn test_spec() -> SandboxRuntimeSpec {
        SandboxRuntimeSpec {
            id: "t1".into(),
            workspace: WorkspaceMount::ReadWrite("/tmp/ws".into()),
            environment: RuntimeEnvironment::Profile(EnvironmentProfile::Base),
            resources: RuntimeResources { memory_mib: 512, vcpus: 1 },
            user: None,
            env: vec![],
            command: vec!["true".into()],
            resolved_prompt: None,
            staged_prepare_script: None,
            credential_files: vec![],
            ca_cert_pem: None,
            inputs: vec![],
            caches: vec![],
            mount_excludes: vec![],
            services: vec![],
            control_channels: vec![ControlChannel {
                name: "command-broker".into(),
                guest_port: COMMAND_BROKER_PORT,
            }],
            network: RuntimeNetworkPlan::HostMediated,
            native_secrets: vec![],
            start: RuntimeStart::Fresh,
            lifecycle: RuntimeLifecycle::default(),
        }
    }

    #[test]
    fn sh_escape_handles_quotes() {
        assert_eq!(sh_escape("a'b"), "'a'\\''b'");
        assert_eq!(sh_escape("plain"), "'plain'");
    }

    #[test]
    fn mount_exclude_validation() {
        assert!(validate_mount_exclude("node_modules").is_ok());
        assert!(validate_mount_exclude("a/b/c").is_ok());
        assert!(validate_mount_exclude("/abs").is_err());
        assert!(validate_mount_exclude("../up").is_err());
        assert!(validate_mount_exclude("a/../b").is_err());
        assert!(validate_mount_exclude("").is_err());
        assert!(validate_mount_exclude("a//b").is_err());
    }

    #[test]
    fn setup_script_covers_workspace_and_credentials() {
        let mut spec = test_spec();
        spec.ca_cert_pem = Some("PEM".into());
        spec.credential_files.push(CredentialToStage {
            index: 0,
            guest_path: "/home/abox/.claude/.credentials.json".into(),
            mode: "0600".into(),
            content: b"{}".to_vec(),
        });
        let script = guest_setup_script(&spec, "1000:1000");
        assert!(script.contains("chown -R 1000:1000 /workspace"));
        assert!(script.contains("cp /abox-meta/root.crt /etc/ssl/certs/abox-ca.pem"));
        assert!(script.contains("'/abox-meta/credentials/0'"));
        assert!(script.contains("chmod '0600'"));
        assert!(script.contains("mkdir -p /run/abox-tmp"));
    }

    #[test]
    fn bridge_args_include_egress_and_services() {
        let mut spec = test_spec();
        spec.services.push(crate::services::GuestServiceBridge {
            name: "postgres".into(),
            guest_port: 5432,
            vsock_port: 5100,
        });
        let args = bridge_args(&spec);
        assert_eq!(args, vec!["/run/abox-proxy.sock", "5000", "18443", "5001", "5432", "5100"]);
    }

    #[test]
    fn sandbox_name_is_prefixed() {
        assert_eq!(MicrosandboxRuntime::sandbox_name("fix-auth"), "abox-fix-auth");
    }

    // ─── Native network plan compilation: SSRF invariants ──────────────────
    //
    // These are release gates (ADR-008 §5.5, plan §Phase 6): loopback,
    // private ranges, link-local, cloud metadata, multicast, and the host
    // must be denied by explicit first-match rules in every compiled plan,
    // egress is TCP 443 + gateway DNS only, ingress and defaults deny.

    use microsandbox_network::policy::{Action, Destination, DestinationGroup, Direction};

    fn assert_baseline_invariants(policy: &microsandbox::NetworkPolicy) {
        assert_eq!(policy.default_egress, Action::Deny, "default egress must deny");
        assert_eq!(policy.default_ingress, Action::Deny, "default ingress must deny");

        // Rule 0 is the narrow gateway DNS allow (must precede the Host
        // denial); rules 1-6 are the group denials — before ANY other allow.
        let dns = &policy.rules[0];
        assert_eq!(dns.action, Action::Allow);
        assert_eq!(dns.ports, vec![microsandbox_network::policy::PortRange::single(53)]);
        let expected_denies = [
            DestinationGroup::Loopback,
            DestinationGroup::Private,
            DestinationGroup::LinkLocal,
            DestinationGroup::Metadata,
            DestinationGroup::Multicast,
            DestinationGroup::Host,
        ];
        for (i, group) in expected_denies.iter().enumerate() {
            let rule = &policy.rules[i + 1];
            assert_eq!(rule.action, Action::Deny, "rule {} must deny", i + 1);
            assert!(
                matches!(&rule.destination, Destination::Group(g) if g == group),
                "rule {} must deny group {group:?}, got {:?}",
                i + 1,
                rule.destination
            );
        }

        // No allow rule may target loopback/private/link-local/metadata/
        // multicast ranges or arbitrary ports.
        for rule in &policy.rules {
            if rule.action != Action::Allow {
                continue;
            }
            match &rule.destination {
                Destination::Group(DestinationGroup::Host) => {
                    // Only the narrow gateway DNS rule may target Host.
                    assert_eq!(
                        rule.ports,
                        vec![microsandbox_network::policy::PortRange::single(53)],
                        "Host-directed allow must be DNS-only"
                    );
                }
                Destination::Group(DestinationGroup::Public) | Destination::Domain(_) => {
                    assert_eq!(rule.direction, Direction::Egress);
                    assert_eq!(
                        rule.ports,
                        vec![microsandbox_network::policy::PortRange::single(443)],
                        "non-DNS allows must be limited to TCP 443"
                    );
                }
                other => panic!("unexpected allow destination {other:?}"),
            }
        }
    }

    #[test]
    fn open_plan_is_public_only_never_unrestricted() {
        let policy = compile_msb_network_policy(&NativeNetworkPlan {
            allow_public: true,
            allowed_hosts: vec![],
        })
        .unwrap();
        assert_baseline_invariants(&policy);
        assert!(
            policy.rules.iter().any(|r| r.action == Action::Allow
                && matches!(r.destination, Destination::Group(DestinationGroup::Public))),
            "open must allow public egress"
        );
    }

    #[test]
    fn scoped_plan_allows_only_listed_hosts() {
        let policy = compile_msb_network_policy(&NativeNetworkPlan {
            allow_public: false,
            allowed_hosts: vec!["registry.npmjs.org".into(), "api.example.com".into()],
        })
        .unwrap();
        assert_baseline_invariants(&policy);
        let allowed_domains: Vec<String> = policy
            .rules
            .iter()
            .filter(|r| r.action == Action::Allow)
            .filter_map(|r| match &r.destination {
                Destination::Domain(d) => Some(d.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(allowed_domains, vec!["registry.npmjs.org", "api.example.com"]);
        assert!(
            !policy.rules.iter().any(|r| r.action == Action::Allow
                && matches!(r.destination, Destination::Group(DestinationGroup::Public))),
            "scoped must not allow public egress"
        );
    }

    #[test]
    fn scoped_plan_rejects_invalid_hostnames() {
        assert!(compile_msb_network_policy(&NativeNetworkPlan {
            allow_public: false,
            allowed_hosts: vec!["not a hostname".into()],
        })
        .is_err());
    }

    #[tokio::test]
    async fn native_secrets_require_native_network_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let config =
            crate::config::AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
        let runtime = MicrosandboxRuntime::new(&config).unwrap();

        let mut spec = test_spec();
        spec.native_secrets.push(crate::runtime::spec::NativeSecretSpec {
            env_var: "OPENAI_API_KEY".into(),
            source_env_var: "OPENAI_API_KEY".into(),
            allowed_host: "api.openai.com".into(),
        });
        // Network stays HostMediated → start() must fail closed before any
        // runtime interaction (the guard runs before sandbox creation).
        let err = runtime.start(spec).await.unwrap_err();
        assert!(
            err.to_string().contains("requires a native network plan"),
            "unexpected error: {err:#}"
        );
    }
}
