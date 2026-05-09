//! `abox env` — durable per-project caches and prepare flows.

use abox_core::config::AboxConfig;
use abox_core::project::{
    clear_environment_state, default_prepare_base_branch, environment_record_path,
    environments_dir, is_approved, load_environment_state, project_cache_root, record_approval,
    record_environment_state, rootfs_token_for_profile, EnvironmentProfile, EnvironmentStateRecord,
    ProjectConfig, ResolvedProjectConfig,
};
use abox_core::sandbox::{CreateSandboxParams, SandboxOrchestrator};
use abox_core::vm::VmPort;
use abox_core::workspace::WorkspacePort;
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use sha2::{Digest, Sha256};
use std::io::{IsTerminal, Write};
use std::path::Path;

struct WarmEvaluation {
    rootfs_token: String,
    environment_fingerprint: String,
    current_state: Option<EnvironmentStateRecord>,
}

#[derive(Debug, Args)]
pub struct EnvArgs {
    #[command(subcommand)]
    pub action: EnvAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum EnvAction {
    /// Show durable cache and prepare status for this repo.
    Status,
    /// Run the repo prepare flow inside a fresh guest to warm durable caches.
    Warm {
        /// Re-run the warm flow even if the current inputs already match the last warm state.
        #[arg(long)]
        force: bool,
    },
    /// Clear this repo's recorded warm state without deleting caches.
    Reset,
    /// Report cache usage and remove empty env/cache bookkeeping directories.
    Prune,
}

pub fn execute_without_orchestrator(
    args: &EnvArgs,
    repo_root: &Path,
    config: &AboxConfig,
) -> Result<bool> {
    match &args.action {
        EnvAction::Status => {
            status(repo_root, config)?;
            Ok(true)
        }
        EnvAction::Reset => {
            reset(repo_root, config)?;
            Ok(true)
        }
        EnvAction::Prune => {
            prune(config)?;
            Ok(true)
        }
        EnvAction::Warm { .. } => Ok(false),
    }
}

pub async fn execute_warm<W: WorkspacePort, V: VmPort>(
    args: &EnvArgs,
    repo_root: &Path,
    orchestrator: &SandboxOrchestrator<W, V>,
    policy: std::sync::Arc<abox_core::policy::PolicyEngine>,
    root_ca: std::sync::Arc<abox_core::ca::RootCa>,
) -> Result<()> {
    let EnvAction::Warm { force } = &args.action else {
        anyhow::bail!("internal error: execute_warm called for non-warm env action");
    };

    let resolved = load_resolved_project(repo_root)?;
    if !resolved.has_durable_caches() {
        anyhow::bail!(
            "No durable caches are configured for this repo.\n\n\
             Add [environment].caches in .abox/project.toml before using `abox env warm`."
        );
    }
    if !resolved.has_prepare_flow() {
        anyhow::bail!(
            "No prepare script is configured for this repo.\n\n\
             Add [environment].prepare in .abox/project.toml before using `abox env warm`."
        );
    }

    ensure_project_trusted(&resolved, policy.as_ref(), &orchestrator.config().state_dir)?;
    let evaluation = evaluate_warm_state(repo_root, orchestrator.config(), &resolved)?;

    if !force {
        if let Some(state) = &evaluation.current_state {
            if state.environment_fingerprint == evaluation.environment_fingerprint {
                println!("Environment is already warm for the current inputs.");
                println!("Warmed at: {}", state.warmed_at);
                return Ok(());
            }
        }
    }

    let cache_root = project_cache_root(&orchestrator.config().state_dir, &resolved.project_id);
    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("Creating {}", cache_root.display()))?;
    for cache in &resolved.caches {
        let path = cache_root.join(cache);
        std::fs::create_dir_all(&path).with_context(|| format!("Creating {}", path.display()))?;
    }

    if let Some(state) = &evaluation.current_state {
        println!("Refreshing environment warm state...");
        println!(
            "Reason: {}",
            stale_reason(
                state,
                resolved.environment_profile,
                &evaluation.environment_fingerprint,
                &evaluation.rootfs_token,
            )
        );
    } else {
        println!("Warming environment caches for the first time...");
    }

    let mut effective_policy = policy;
    if let Ok(scope) = resolved.effective_network_scope(None) {
        println!("Network mode: {}", scope.mode);
        effective_policy =
            std::sync::Arc::new(effective_policy.as_ref().with_network_scope(scope)?);
    }
    let path = warm_environment(
        repo_root,
        &resolved,
        orchestrator,
        effective_policy,
        root_ca,
        &evaluation,
    )
    .await?;
    println!("Environment warm complete.");
    println!("Cache root: {}", cache_root.display());
    println!("State record: {}", path.display());
    Ok(())
}

pub(crate) async fn ensure_warm_environment_for_run<W: WorkspacePort, V: VmPort>(
    repo_root: &Path,
    resolved: Option<&ResolvedProjectConfig>,
    orchestrator: &SandboxOrchestrator<W, V>,
    policy: std::sync::Arc<abox_core::policy::PolicyEngine>,
    root_ca: std::sync::Arc<abox_core::ca::RootCa>,
) -> Result<()> {
    let Some(resolved) = resolved else {
        return Ok(());
    };
    if !resolved.is_warmable() {
        return Ok(());
    }

    let evaluation = evaluate_warm_state(repo_root, orchestrator.config(), resolved)?;
    if let Some(state) = &evaluation.current_state {
        if state.environment_fingerprint == evaluation.environment_fingerprint {
            return Ok(());
        }
    }

    if let Some(state) = &evaluation.current_state {
        println!("Environment warm state: stale");
        println!(
            "Reason: {}",
            stale_reason(
                state,
                resolved.environment_profile,
                &evaluation.environment_fingerprint,
                &evaluation.rootfs_token,
            )
        );
    } else {
        println!("Environment warm state: cold");
    }
    println!("Refreshing guest-native environment before launch...");

    warm_environment(repo_root, resolved, orchestrator, policy, root_ca, &evaluation).await?;
    println!("Environment refresh complete.");
    Ok(())
}

fn status(repo_root: &Path, config: &AboxConfig) -> Result<()> {
    let Some(resolved) = load_optional_resolved_project(repo_root)? else {
        println!("No project config found at {}", ProjectConfig::default_path(repo_root).display());
        return Ok(());
    };

    println!("Project config: {}", resolved.config_path.display());
    println!("Project identity: {}", resolved.project_id);
    println!(
        "Environment profile: {} ({})",
        resolved.environment_profile,
        resolved.environment_profile.toolchain_summary()
    );
    if resolved.caches.is_empty() {
        println!("Durable caches: none");
    } else {
        println!("Durable caches: {}", resolved.caches.join(", "));
    }
    println!(
        "Prepare flow: {}",
        resolved
            .prepare_path
            .as_ref()
            .map_or_else(|| "not configured".to_string(), |path| path.display().to_string())
    );

    let cache_root = project_cache_root(&config.state_dir, &resolved.project_id);
    for cache in &resolved.caches {
        let path = cache_root.join(cache);
        let size = directory_size(&path)?;
        println!("Cache {cache}: {} ({})", path.display(), format_bytes(size));
    }

    if !resolved.has_prepare_flow() {
        println!("Warm state: not applicable (no prepare script configured)");
        return Ok(());
    }
    if !resolved.has_durable_caches() {
        println!("Warm state: not durable yet (configure at least one cache family)");
        return Ok(());
    }

    match evaluate_warm_state(repo_root, config, &resolved) {
        Ok(evaluation) => match evaluation.current_state {
            Some(record)
                if record.environment_fingerprint == evaluation.environment_fingerprint =>
            {
                println!("Warm state: ready");
                println!("Warmed at: {}", record.warmed_at);
            }
            Some(record) => {
                println!("Warm state: stale");
                println!(
                    "Reason: {}",
                    stale_reason(
                        &record,
                        resolved.environment_profile,
                        &evaluation.environment_fingerprint,
                        &evaluation.rootfs_token
                    )
                );
                println!(
                    "State record: {}",
                    environment_record_path(&config.state_dir, &resolved.project_id).display()
                );
            }
            None => {
                println!("Warm state: cold");
            }
        },
        Err(err) => {
            println!("Warm state: unavailable ({err:#})");
        }
    }

    Ok(())
}

fn reset(repo_root: &Path, config: &AboxConfig) -> Result<()> {
    let Some(resolved) = load_optional_resolved_project(repo_root)? else {
        println!("No project config found at {}", ProjectConfig::default_path(repo_root).display());
        return Ok(());
    };

    if clear_environment_state(&config.state_dir, &resolved.project_id)? {
        println!("Cleared warm-state record for {}.", resolved.project_id);
    } else {
        println!("No warm-state record exists for {}.", resolved.project_id);
    }
    println!(
        "Caches remain at {}",
        project_cache_root(&config.state_dir, &resolved.project_id).display()
    );
    Ok(())
}

fn prune(config: &AboxConfig) -> Result<()> {
    let env_root = environments_dir(&config.state_dir);
    let cache_root = config.state_dir.join("cache").join("projects");
    let removed = prune_empty_dirs(&env_root)? + prune_empty_dirs(&cache_root)?;
    let cache_bytes = directory_size(&cache_root)?;
    let cache_projects = count_immediate_dirs(&cache_root)?;

    println!("Project cache root: {}", cache_root.display());
    println!("Project cache directories: {cache_projects}");
    println!("Total cache size: {}", format_bytes(cache_bytes));
    println!("Removed {removed} empty bookkeeping directories.");
    Ok(())
}

fn load_optional_resolved_project(repo_root: &Path) -> Result<Option<ResolvedProjectConfig>> {
    let Some(config) = ProjectConfig::load(repo_root)? else {
        return Ok(None);
    };
    Ok(Some(config.resolve(repo_root)?))
}

fn load_resolved_project(repo_root: &Path) -> Result<ResolvedProjectConfig> {
    load_optional_resolved_project(repo_root)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No project config found at {}",
            ProjectConfig::default_path(repo_root).display()
        )
    })
}

fn evaluate_warm_state(
    repo_root: &Path,
    config: &AboxConfig,
    resolved: &ResolvedProjectConfig,
) -> Result<WarmEvaluation> {
    let rootfs_token = rootfs_token_for_profile(config, resolved.environment_profile)?;
    let environment_fingerprint = resolved.environment_fingerprint(repo_root, &rootfs_token)?;
    let mut current_state = load_environment_state(&config.state_dir, &resolved.project_id)?;
    if current_state.is_some() && !cache_dirs_present(&config.state_dir, resolved) {
        current_state = None;
    }
    Ok(WarmEvaluation { rootfs_token, environment_fingerprint, current_state })
}

async fn warm_environment<W: WorkspacePort, V: VmPort>(
    repo_root: &Path,
    resolved: &ResolvedProjectConfig,
    orchestrator: &SandboxOrchestrator<W, V>,
    policy: std::sync::Arc<abox_core::policy::PolicyEngine>,
    root_ca: std::sync::Arc<abox_core::ca::RootCa>,
    evaluation: &WarmEvaluation,
) -> Result<std::path::PathBuf> {
    let cache_root = project_cache_root(&orchestrator.config().state_dir, &resolved.project_id);
    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("Creating {}", cache_root.display()))?;
    for cache in &resolved.caches {
        let path = cache_root.join(cache);
        std::fs::create_dir_all(&path).with_context(|| format!("Creating {}", path.display()))?;
    }

    let prepare_script = String::from_utf8(
        resolved
            .prepare_bytes
            .clone()
            .context("prepare flow missing staged bytes despite being configured")?,
    )
    .context("environment.prepare must be valid UTF-8 text")?;

    let params = CreateSandboxParams {
        task_id: warm_task_id(&resolved.project_id),
        base_branch: default_prepare_base_branch(repo_root),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: resolved.cache_env_vars(),
        command: vec!["sh".to_string(), "/abox-meta/prepare.sh".to_string()],
        resolved_prompt: None,
        cache_mount_dir: Some(cache_root),
        staged_prepare_script: Some(prepare_script),
        environment_profile: resolved.environment_profile,
        timeout_secs: None,
        ephemeral: true,
        ca_cert_pem: None,
    };

    let exit_code = orchestrator.run_sandbox(params, policy, root_ca).await?;
    if exit_code != 0 {
        anyhow::bail!("Prepare flow exited with code {exit_code}");
    }

    let record = EnvironmentStateRecord {
        version: 1,
        project_id: resolved.project_id.clone(),
        environment_fingerprint: evaluation.environment_fingerprint.clone(),
        rootfs_token: evaluation.rootfs_token.clone(),
        environment_profile: Some(resolved.environment_profile.to_string()),
        caches: resolved.caches.clone(),
        prepare_path: resolved.prepare_path.as_ref().map(|path| path.display().to_string()),
        warmed_at: chrono::Utc::now().to_rfc3339(),
    };
    record_environment_state(&orchestrator.config().state_dir, &record)
}

fn ensure_project_trusted(
    resolved: &ResolvedProjectConfig,
    policy: &abox_core::policy::PolicyEngine,
    state_dir: &Path,
) -> Result<()> {
    if is_approved(state_dir, resolved) {
        return Ok(());
    }

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Repo-owned abox behavior is not yet trusted for this fingerprint.\n\n\
             Run `abox project explain` to review it, then `abox project trust` to approve it."
        );
    }

    eprintln!("Repo-owned abox behavior is not yet trusted:");
    for line in resolved.summary_lines(&policy.managed_egress_domains()) {
        eprintln!("  {line}");
    }
    eprint!("Trust this repo config and continue? [y/N]: ");
    std::io::stderr().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let accepted = matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if !accepted {
        anyhow::bail!(
            "Environment warm cancelled. Run `abox project trust` after reviewing the repo config."
        );
    }

    let record_path = record_approval(state_dir, resolved)?;
    eprintln!("Trusted current repo behavior.");
    eprintln!("Approval record: {}", record_path.display());
    Ok(())
}

fn warm_task_id(project_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("warm-{}", &digest[..12])
}

fn stale_reason(
    record: &EnvironmentStateRecord,
    current_profile: EnvironmentProfile,
    current_fingerprint: &str,
    current_rootfs: &str,
) -> String {
    if record.environment_profile.as_deref() != Some(current_profile.as_str()) {
        "environment profile changed".to_string()
    } else if record.rootfs_token != current_rootfs {
        "guest rootfs changed".to_string()
    } else if record.environment_fingerprint != current_fingerprint {
        "prepare inputs or watched dependency files changed".to_string()
    } else {
        "current state differs".to_string()
    }
}

fn directory_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return Ok(std::fs::metadata(path)?.len());
    }

    let mut total = 0u64;
    for entry in std::fs::read_dir(path).with_context(|| format!("Reading {}", path.display()))? {
        let entry = entry?;
        total += directory_size(&entry.path())?;
    }
    Ok(total)
}

fn count_immediate_dirs(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(path).with_context(|| format!("Reading {}", path.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += 1;
        }
    }
    Ok(count)
}

fn prune_empty_dirs(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in std::fs::read_dir(path).with_context(|| format!("Reading {}", path.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            removed += prune_empty_dirs(&entry.path())?;
        }
    }

    let is_empty = std::fs::read_dir(path)
        .with_context(|| format!("Reading {}", path.display()))?
        .next()
        .is_none();
    if is_empty {
        std::fs::remove_dir(path).with_context(|| format!("Removing {}", path.display()))?;
        removed += 1;
    }

    Ok(removed)
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_f = bytes as f64;

    if bytes_f >= GIB {
        format!("{:.1} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.1} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn cache_dirs_present(state_dir: &Path, resolved: &ResolvedProjectConfig) -> bool {
    let root = project_cache_root(state_dir, &resolved.project_id);
    if !root.exists() {
        return false;
    }

    resolved.caches.iter().all(|cache| root.join(cache).exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use abox_core::ca::RootCa;
    use abox_core::config::VmDefaults;
    use abox_core::policy::{PolicyEngine, PolicyFile};
    use abox_core::project::{rootfs_token, EnvironmentProfile};
    use abox_core::vm::{VmConfig, VmInfo, VmState};
    use abox_core::workspace::{DivergenceEntry, WorkspacePort, WorktreeInfo};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    struct PanicWorkspace;

    impl WorkspacePort for PanicWorkspace {
        fn create_worktree(
            &self,
            _sandbox_id: &str,
            _base_branch: &str,
        ) -> anyhow::Result<PathBuf> {
            panic!("workspace should not be used in this test")
        }

        fn remove_worktree(&self, _sandbox_id: &str, _delete_branch: bool) -> anyhow::Result<()> {
            panic!("workspace should not be used in this test")
        }

        fn list_worktrees(&self) -> anyhow::Result<Vec<WorktreeInfo>> {
            panic!("workspace should not be used in this test")
        }

        fn compute_divergence(&self, _base_branch: &str) -> anyhow::Result<Vec<DivergenceEntry>> {
            panic!("workspace should not be used in this test")
        }

        fn merge_branch(
            &self,
            _sandbox_id: &str,
            _base_branch: &str,
        ) -> anyhow::Result<Vec<String>> {
            panic!("workspace should not be used in this test")
        }
    }

    struct PanicVm;

    impl VmPort for PanicVm {
        async fn start(&self, _config: abox_core::vm::VmConfig) -> anyhow::Result<VmInfo> {
            panic!("vm should not be started in this test")
        }

        async fn stop(&self, _id: &str) -> anyhow::Result<()> {
            panic!("vm should not be used in this test")
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            panic!("vm should not be used in this test")
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            panic!("vm should not be used in this test")
        }

        async fn info(&self, _id: &str) -> anyhow::Result<VmInfo> {
            panic!("vm should not be used in this test")
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            panic!("vm should not be used in this test")
        }
    }

    struct RecordingWorkspace {
        worktree_base: PathBuf,
        created: Arc<Mutex<Vec<String>>>,
        removed: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingWorkspace {
        fn new(worktree_base: PathBuf) -> Self {
            Self {
                worktree_base,
                created: Arc::new(Mutex::new(Vec::new())),
                removed: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl WorkspacePort for RecordingWorkspace {
        fn create_worktree(&self, sandbox_id: &str, _base_branch: &str) -> anyhow::Result<PathBuf> {
            let path = self.worktree_base.join(sandbox_id);
            std::fs::create_dir_all(&path)?;
            self.created.lock().unwrap().push(sandbox_id.to_string());
            Ok(path)
        }

        fn remove_worktree(&self, sandbox_id: &str, _delete_branch: bool) -> anyhow::Result<()> {
            let path = self.worktree_base.join(sandbox_id);
            if path.exists() {
                std::fs::remove_dir_all(&path)?;
            }
            self.removed.lock().unwrap().push(sandbox_id.to_string());
            Ok(())
        }

        fn list_worktrees(&self) -> anyhow::Result<Vec<WorktreeInfo>> {
            Ok(vec![])
        }

        fn compute_divergence(&self, _base_branch: &str) -> anyhow::Result<Vec<DivergenceEntry>> {
            Ok(vec![])
        }

        fn merge_branch(
            &self,
            _sandbox_id: &str,
            _base_branch: &str,
        ) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
    }

    struct RecordingVm {
        started: Arc<Mutex<Vec<VmConfig>>>,
        stopped: Arc<Mutex<Vec<String>>>,
        status_root: PathBuf,
    }

    impl RecordingVm {
        fn new(status_root: PathBuf) -> Self {
            Self {
                started: Arc::new(Mutex::new(Vec::new())),
                stopped: Arc::new(Mutex::new(Vec::new())),
                status_root,
            }
        }
    }

    impl VmPort for RecordingVm {
        async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
            let id = config.id.clone();
            let status_dir = self.status_root.join(&id);
            std::fs::create_dir_all(&status_dir)?;
            std::fs::write(status_dir.join("exit-code"), "0\n")?;
            self.started.lock().unwrap().push(config);
            Ok(VmInfo {
                id,
                pid: 12345,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/mock-api.sock"),
                console_socket: PathBuf::from("/tmp/mock-console.sock"),
            })
        }

        async fn stop(&self, id: &str) -> anyhow::Result<()> {
            self.stopped.lock().unwrap().push(id.to_string());
            Ok(())
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn info(&self, id: &str) -> anyhow::Result<VmInfo> {
            Ok(VmInfo {
                id: id.to_string(),
                pid: 12345,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/mock-api.sock"),
                console_socket: PathBuf::from("/tmp/mock-console.sock"),
            })
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            Ok(vec![])
        }

        async fn wait_for_exit(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn status_dir(&self, id: &str) -> Option<PathBuf> {
            Some(self.status_root.join(id))
        }
    }

    fn write_project_files(repo_root: &Path) -> ResolvedProjectConfig {
        let prepare_path = repo_root.join(".abox/prepare.sh");
        let config_path = repo_root.join(".abox/project.toml");
        std::fs::create_dir_all(prepare_path.parent().unwrap()).unwrap();
        std::fs::write(&prepare_path, "#!/bin/sh\necho warm\n").unwrap();
        std::fs::write(repo_root.join("package-lock.json"), "{ \"lock\": 1 }\n").unwrap();

        std::fs::write(
            &config_path,
            "[project]\nid = \"demo-repo\"\n\n[network]\nmode = \"safe\"\n\n[environment]\n\
             caches = [\"npm\"]\nprepare = \".abox/prepare.sh\"\nwatch = [\"package-lock.json\"]\n",
        )
        .unwrap();

        ProjectConfig::load(repo_root).unwrap().unwrap().resolve(repo_root).unwrap()
    }

    fn test_config(state_dir: &Path) -> AboxConfig {
        let rootfs = state_dir.join("vm/rootfs.raw");
        let kernel = state_dir.join("vm/vmlinux");
        std::fs::create_dir_all(rootfs.parent().unwrap()).unwrap();
        std::fs::write(&rootfs, b"rootfs").unwrap();
        std::fs::write(&kernel, b"kernel").unwrap();

        AboxConfig {
            state_dir: state_dir.to_path_buf(),
            vm_defaults: VmDefaults {
                memory_mib: 2048,
                vcpus: 2,
                image_path: Some(rootfs),
                kernel_path: Some(kernel),
            },
            ..Default::default()
        }
    }

    #[test]
    fn evaluate_warm_state_treats_missing_cache_dirs_as_cold() {
        let tmp = tempdir().unwrap();
        let repo_root = tmp.path();
        let resolved = write_project_files(repo_root);
        let config = test_config(tmp.path());

        let rootfs = rootfs_token(&config).unwrap();
        let fingerprint = resolved.environment_fingerprint(repo_root, &rootfs).unwrap();
        record_environment_state(
            &config.state_dir,
            &EnvironmentStateRecord {
                version: 1,
                project_id: resolved.project_id.clone(),
                environment_fingerprint: fingerprint,
                rootfs_token: rootfs,
                environment_profile: Some(resolved.environment_profile.to_string()),
                caches: resolved.caches.clone(),
                prepare_path: resolved.prepare_path.as_ref().map(|path| path.display().to_string()),
                warmed_at: "2026-05-09T00:00:00Z".into(),
            },
        )
        .unwrap();

        let evaluation = evaluate_warm_state(repo_root, &config, &resolved).unwrap();
        assert!(evaluation.current_state.is_none(), "missing cache dirs should be treated as cold");
    }

    #[test]
    fn cache_dirs_present_requires_all_declared_caches() {
        let tmp = tempdir().unwrap();
        let state_dir = tmp.path();
        let resolved = ResolvedProjectConfig {
            config_path: PathBuf::from(".abox/project.toml"),
            project_id: "repo".into(),
            default_network_mode: abox_core::project::NetworkMode::Safe,
            bundles: vec![],
            domains: vec![],
            environment_profile: EnvironmentProfile::Base,
            caches: vec!["npm".into(), "cargo".into()],
            prepare_path: None,
            prepare_bytes: None,
            watch_paths: vec![],
            default_prompt_path: None,
            default_prompt_bytes: None,
            notes: vec![],
            approval_fingerprint: "abc".into(),
        };

        let root = project_cache_root(state_dir, &resolved.project_id);
        std::fs::create_dir_all(root.join("npm")).unwrap();
        assert!(!cache_dirs_present(state_dir, &resolved));
        std::fs::create_dir_all(root.join("cargo")).unwrap();
        assert!(cache_dirs_present(state_dir, &resolved));
    }

    #[tokio::test]
    async fn ensure_warm_environment_skips_when_ready() {
        let tmp = tempdir().unwrap();
        let repo_root = tmp.path();
        let resolved = write_project_files(repo_root);
        let config = test_config(tmp.path());
        config.ensure_dirs().unwrap();

        let cache_root = project_cache_root(&config.state_dir, &resolved.project_id);
        for cache in &resolved.caches {
            std::fs::create_dir_all(cache_root.join(cache)).unwrap();
        }

        let rootfs = rootfs_token(&config).unwrap();
        let fingerprint = resolved.environment_fingerprint(repo_root, &rootfs).unwrap();
        record_environment_state(
            &config.state_dir,
            &EnvironmentStateRecord {
                version: 1,
                project_id: resolved.project_id.clone(),
                environment_fingerprint: fingerprint,
                rootfs_token: rootfs,
                environment_profile: Some(resolved.environment_profile.to_string()),
                caches: resolved.caches.clone(),
                prepare_path: resolved.prepare_path.as_ref().map(|path| path.display().to_string()),
                warmed_at: "2026-05-09T00:00:00Z".into(),
            },
        )
        .unwrap();

        let orchestrator = SandboxOrchestrator::new(config, PanicWorkspace, PanicVm);
        let policy = std::sync::Arc::new(
            PolicyEngine::from_policy_file(PolicyFile {
                cli: vec![],
                egress: vec![],
                default_cli_action: "allow".into(),
                default_egress_action: "deny".into(),
                bypass_tls: vec![],
            })
            .unwrap(),
        );
        let ca_dir = tempdir().unwrap();
        let root_ca = std::sync::Arc::new(RootCa::generate_and_persist(ca_dir.path()).unwrap());

        ensure_warm_environment_for_run(repo_root, Some(&resolved), &orchestrator, policy, root_ca)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn warm_flow_writes_state_then_reuses_it() {
        let tmp = tempdir().unwrap();
        let repo_root = tmp.path();
        let resolved = write_project_files(repo_root);
        let config = test_config(tmp.path());
        config.ensure_dirs().unwrap();

        let workspace = RecordingWorkspace::new(config.worktrees_dir());
        let workspace_created = Arc::clone(&workspace.created);
        let workspace_removed = Arc::clone(&workspace.removed);
        let vm = RecordingVm::new(config.runtime_dir().join("status-mock"));
        let started = Arc::clone(&vm.started);
        let stopped = Arc::clone(&vm.stopped);
        let orchestrator = SandboxOrchestrator::new(config.clone(), workspace, vm);

        let policy = std::sync::Arc::new(
            PolicyEngine::from_policy_file(PolicyFile {
                cli: vec![],
                egress: vec![],
                default_cli_action: "allow".into(),
                default_egress_action: "deny".into(),
                bypass_tls: vec![],
            })
            .unwrap(),
        );
        let ca_dir = tempdir().unwrap();
        let root_ca = std::sync::Arc::new(RootCa::generate_and_persist(ca_dir.path()).unwrap());

        ensure_warm_environment_for_run(
            repo_root,
            Some(&resolved),
            &orchestrator,
            std::sync::Arc::clone(&policy),
            std::sync::Arc::clone(&root_ca),
        )
        .await
        .unwrap();

        let started_configs = started.lock().unwrap().clone();
        assert_eq!(started_configs.len(), 1, "cold warm flow should run exactly one prepare VM");
        let warm_config = &started_configs[0];
        assert!(warm_config.id.starts_with("warm-"));
        assert_eq!(warm_config.agent_command, vec!["sh", "/abox-meta/prepare.sh"]);
        assert!(warm_config
            .env_vars
            .contains(&("NPM_CONFIG_CACHE".to_string(), "/abox-cache/npm".to_string())));
        assert_eq!(
            warm_config.cache_mount_dir,
            Some(project_cache_root(&config.state_dir, &resolved.project_id))
        );
        assert!(warm_config
            .staged_prepare_script
            .as_deref()
            .is_some_and(|script| script.contains("echo warm")));

        let state = load_environment_state(&config.state_dir, &resolved.project_id).unwrap();
        assert!(state.is_some(), "warm flow should persist environment state");
        assert!(project_cache_root(&config.state_dir, &resolved.project_id).join("npm").exists());
        assert_eq!(workspace_created.lock().unwrap().len(), 1);
        assert_eq!(
            workspace_removed.lock().unwrap().len(),
            1,
            "ephemeral warm flow should clean up its worktree"
        );
        assert_eq!(stopped.lock().unwrap().len(), 1, "ephemeral warm flow should stop the VM");

        ensure_warm_environment_for_run(repo_root, Some(&resolved), &orchestrator, policy, root_ca)
            .await
            .unwrap();

        assert_eq!(
            started.lock().unwrap().len(),
            1,
            "ready warm state should be reused without starting another VM"
        );
    }
}
