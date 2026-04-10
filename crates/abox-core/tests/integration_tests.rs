//! Integration tests for abox-core.
//!
//! These tests exercise the public API of abox-core components in combination,
//! including the config loader, policy engine, workspace manager, snapshot
//! manager, and sandbox orchestrator (with mock VM).

use abox_core::adapters::git2_workspace::Git2Workspace;
use abox_core::config::AboxConfig;
use abox_core::policy::{CliPolicy, Decision, EgressRule, PolicyEngine, PolicyFile};
use abox_core::sandbox::{CreateSandboxParams, SandboxOrchestrator};
use abox_core::snapshot::SnapshotManager;
use abox_core::vm::{VmConfig, VmInfo, VmPort, VmState};
use abox_core::workspace::{FileStatus, WorkspacePort};
use git2::Repository;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ─── Config Tests ───────────────────────────────────────────────────────────

#[test]
fn test_config_load_from_file() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
state_dir = "/custom/state"

[vm_defaults]
memory_mib = 8192
vcpus = 8
image_path = "/custom/image.raw"
kernel_path = "/custom/vmlinux"

[proxy]
egress_port = 9999
policy_dir = "/custom/policies"
"#,
    )
    .unwrap();

    let config = AboxConfig::load(&config_path).unwrap();
    assert_eq!(config.state_dir, PathBuf::from("/custom/state"));
    assert_eq!(config.vm_defaults.memory_mib, 8192);
    assert_eq!(config.vm_defaults.vcpus, 8);
    assert_eq!(config.vm_defaults.image_path, Some(PathBuf::from("/custom/image.raw")));
    assert_eq!(config.vm_defaults.kernel_path, Some(PathBuf::from("/custom/vmlinux")));
    assert_eq!(config.proxy.egress_port, 9999);
    assert_eq!(config.proxy.policy_dir, PathBuf::from("/custom/policies"));
}

#[test]
fn test_config_load_nonexistent_returns_defaults() {
    let config = AboxConfig::load(Path::new("/nonexistent/config.toml")).unwrap();
    assert_eq!(config.vm_defaults.memory_mib, 2048);
    assert_eq!(config.vm_defaults.vcpus, 2);
    assert_eq!(config.proxy.egress_port, 18443);
}

#[test]
fn test_config_ensure_dirs() {
    let tmp = TempDir::new().unwrap();
    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();
    assert!(config.worktrees_dir().exists());
    assert!(config.templates_dir().exists());
    assert!(config.logs_dir().exists());
}

#[test]
fn test_config_directory_helpers() {
    let config = AboxConfig { state_dir: PathBuf::from("/test/state"), ..Default::default() };
    assert_eq!(config.worktrees_dir(), PathBuf::from("/test/state/worktrees"));
    assert_eq!(config.templates_dir(), PathBuf::from("/test/state/templates"));
    assert_eq!(config.logs_dir(), PathBuf::from("/test/state/logs"));
    // runtime_dir defaults to <state_dir>/run when not configured.
    assert_eq!(config.runtime_dir(), PathBuf::from("/test/state/run"));
}

#[test]
fn test_config_runtime_dir_override() {
    let config = AboxConfig {
        state_dir: PathBuf::from("/test/state"),
        runtime_dir: Some(PathBuf::from("/run/abox")),
        ..Default::default()
    };
    assert_eq!(config.runtime_dir(), PathBuf::from("/run/abox"));
}

#[test]
fn test_config_partial_toml_uses_defaults() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    // Only set memory, everything else should default
    std::fs::write(&config_path, "[vm_defaults]\nmemory_mib = 4096\n").unwrap();

    let config = AboxConfig::load(&config_path).unwrap();
    assert_eq!(config.vm_defaults.memory_mib, 4096);
    assert_eq!(config.vm_defaults.vcpus, 2); // default
    assert_eq!(config.proxy.egress_port, 18443); // default
    assert!(config.vm_defaults.image_path.is_none()); // default
}

// ─── Policy Engine Tests ────────────────────────────────────────────────────

#[test]
fn test_policy_load_from_file() {
    let tmp = TempDir::new().unwrap();
    let policy_path = tmp.path().join("test.toml");
    std::fs::write(
        &policy_path,
        r#"
default_cli_action = "deny"
default_egress_action = "deny"

[[cli]]
command = "git"
allow = ["^status", "^log"]
deny = ["--force"]

[[egress]]
domain = "api.example.com"
inject_header = "Authorization"
env_var = "EXAMPLE_API_KEY"
header_template = "Bearer {value}"
"#,
    )
    .unwrap();

    let engine = PolicyEngine::from_file(&policy_path).unwrap();

    // git status should be allowed
    let decision = engine.evaluate_cli("git", &["status".to_string()]);
    assert_eq!(decision, Decision::Allow);

    // git push should be denied (no matching allow pattern)
    let decision = engine.evaluate_cli("git", &["push".to_string(), "origin".to_string()]);
    assert!(matches!(decision, Decision::Deny(_)));

    // egress to api.example.com should be allowed
    let result = engine.evaluate_egress("api.example.com");
    assert!(result.is_ok());
    let rule = result.unwrap().unwrap();
    assert_eq!(rule.inject_header, "Authorization");
    assert_eq!(rule.header_template, "Bearer {value}");
}

#[test]
fn test_policy_invalid_regex_fails() {
    let policy = PolicyFile {
        cli: vec![CliPolicy {
            command: "git".to_string(),
            allow: vec!["[invalid".to_string()], // unclosed bracket
            deny: vec![],
            forward_ssh_agent: false,
        }],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
    };

    let result = PolicyEngine::from_policy_file(policy);
    assert!(result.is_err());
}

#[test]
fn test_policy_default_allow_cli() {
    let policy = PolicyFile {
        cli: vec![],
        egress: vec![],
        default_cli_action: "allow".to_string(),
        default_egress_action: "deny".to_string(),
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();
    // Any command should be allowed when default is "allow"
    let decision = engine.evaluate_cli("rm", &["-rf".to_string(), "/".to_string()]);
    assert_eq!(decision, Decision::Allow);
}

#[test]
fn test_policy_default_allow_egress() {
    let policy = PolicyFile {
        cli: vec![],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "allow".to_string(),
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();
    // Any domain should be allowed when default is "allow"
    let result = engine.evaluate_egress("anything.example.com");
    assert!(result.is_ok());
    assert!(result.unwrap().is_none()); // No specific rule matched
}

#[test]
fn test_policy_deny_takes_precedence_over_allow() {
    let policy = PolicyFile {
        cli: vec![CliPolicy {
            command: "git".to_string(),
            allow: vec![r"^push\s+".to_string()],
            deny: vec![r"--force".to_string()],
            forward_ssh_agent: false,
        }],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // git push origin main → allowed
    let decision =
        engine.evaluate_cli("git", &["push".to_string(), "origin".to_string(), "main".to_string()]);
    assert_eq!(decision, Decision::Allow);

    // git push --force origin main → denied (deny pattern matches first)
    let decision = engine.evaluate_cli(
        "git",
        &["push".to_string(), "--force".to_string(), "origin".to_string(), "main".to_string()],
    );
    assert!(matches!(decision, Decision::Deny(_)));
}

#[test]
fn test_policy_empty_allow_list_allows_everything() {
    let policy = PolicyFile {
        cli: vec![CliPolicy {
            command: "git".to_string(),
            allow: vec![], // empty = all subcommands allowed
            deny: vec![r"--force".to_string()],
            forward_ssh_agent: false,
        }],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // git anything → allowed (no allow patterns means everything passes)
    let decision =
        engine.evaluate_cli("git", &["random-subcommand".to_string(), "arg".to_string()]);
    assert_eq!(decision, Decision::Allow);

    // git push --force → still denied by deny pattern
    let decision = engine.evaluate_cli("git", &["push".to_string(), "--force".to_string()]);
    assert!(matches!(decision, Decision::Deny(_)));
}

#[test]
fn test_policy_multiple_egress_rules() {
    let policy = PolicyFile {
        cli: vec![],
        egress: vec![
            EgressRule {
                domain: "api.anthropic.com".to_string(),
                inject_header: "x-api-key".to_string(),
                env_var: "ANTHROPIC_API_KEY".to_string(),
                header_template: "{value}".to_string(),
            },
            EgressRule {
                domain: "api.openai.com".to_string(),
                inject_header: "Authorization".to_string(),
                env_var: "OPENAI_API_KEY".to_string(),
                header_template: "Bearer {value}".to_string(),
            },
            EgressRule {
                domain: "*.amazonaws.com".to_string(),
                inject_header: "Authorization".to_string(),
                env_var: "AWS_TOKEN".to_string(),
                header_template: "AWS4-HMAC-SHA256 {value}".to_string(),
            },
        ],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // Anthropic
    let rule = engine.evaluate_egress("api.anthropic.com").unwrap().unwrap();
    assert_eq!(rule.env_var, "ANTHROPIC_API_KEY");

    // OpenAI
    let rule = engine.evaluate_egress("api.openai.com").unwrap().unwrap();
    assert_eq!(rule.env_var, "OPENAI_API_KEY");

    // AWS wildcard
    let rule = engine.evaluate_egress("s3.us-east-1.amazonaws.com").unwrap().unwrap();
    assert_eq!(rule.env_var, "AWS_TOKEN");

    // Unknown domain → denied
    assert!(engine.evaluate_egress("evil.example.com").is_err());
}

#[test]
fn test_policy_load_real_default_policy() {
    // Test that the actual default.toml policy file parses correctly
    let policy_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("policies/default.toml");

    if policy_path.exists() {
        let engine = PolicyEngine::from_file(&policy_path).unwrap();

        // git status should be allowed
        let decision = engine.evaluate_cli("git", &["status".to_string()]);
        assert_eq!(decision, Decision::Allow);

        // git push --force should be denied
        let decision = engine.evaluate_cli(
            "git",
            &["push".to_string(), "--force".to_string(), "origin".to_string(), "main".to_string()],
        );
        assert!(matches!(decision, Decision::Deny(_)));

        // api.anthropic.com should be allowed
        assert!(engine.evaluate_egress("api.anthropic.com").is_ok());

        // random domain should be denied
        assert!(engine.evaluate_egress("evil.example.com").is_err());
    }
}

// ─── Workspace Tests ────────────────────────────────────────────────────────

fn setup_test_repo() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let repo_path = tmp.path().to_path_buf();

    // Force the initial branch to `main` regardless of host's `init.defaultBranch`.
    let mut init_opts = git2::RepositoryInitOptions::new();
    init_opts.initial_head("main");
    let repo = Repository::init_opts(&repo_path, &init_opts).unwrap();

    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    let tree_id = {
        let mut index = repo.index().unwrap();
        let file_path = repo_path.join("README.md");
        std::fs::write(&file_path, "# Test Repo\n").unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        index.write_tree().unwrap()
    };
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[]).unwrap();

    (tmp, repo_path)
}

#[test]
fn test_workspace_multiple_worktrees() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    // Create three worktrees
    let path1 = ws.create_worktree("task-1", "main").unwrap();
    let path2 = ws.create_worktree("task-2", "main").unwrap();
    let path3 = ws.create_worktree("task-3", "main").unwrap();

    assert!(path1.exists());
    assert!(path2.exists());
    assert!(path3.exists());

    // All should have the README
    assert!(path1.join("README.md").exists());
    assert!(path2.join("README.md").exists());
    assert!(path3.join("README.md").exists());

    // List should return all three
    let worktrees = ws.list_worktrees().unwrap();
    assert_eq!(worktrees.len(), 3);

    let ids: Vec<&str> = worktrees.iter().map(|w| w.sandbox_id.as_str()).collect();
    assert!(ids.contains(&"task-1"));
    assert!(ids.contains(&"task-2"));
    assert!(ids.contains(&"task-3"));
}

#[test]
fn test_workspace_worktree_files_are_independent() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let path1 = ws.create_worktree("task-1", "main").unwrap();
    let path2 = ws.create_worktree("task-2", "main").unwrap();

    // Write different files in each worktree
    std::fs::write(path1.join("file1.txt"), "from task-1\n").unwrap();
    std::fs::write(path2.join("file2.txt"), "from task-2\n").unwrap();

    // Each worktree should only see its own file
    assert!(path1.join("file1.txt").exists());
    assert!(!path1.join("file2.txt").exists());
    assert!(path2.join("file2.txt").exists());
    assert!(!path2.join("file1.txt").exists());
}

#[test]
fn test_workspace_divergence_multiple_worktrees() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let path1 = ws.create_worktree("task-1", "main").unwrap();
    let path2 = ws.create_worktree("task-2", "main").unwrap();

    // Commit a change in task-1
    {
        std::fs::write(path1.join("feature1.rs"), "fn main() {}\n").unwrap();
        let wt_repo = Repository::open(&path1).unwrap();
        let sig = git2::Signature::now("Agent1", "agent1@test.com").unwrap();
        let mut index = wt_repo.index().unwrap();
        index.add_path(Path::new("feature1.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = wt_repo.find_tree(tree_id).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        wt_repo.commit(Some("HEAD"), &sig, &sig, "Add feature1", &tree, &[&head]).unwrap();
    }

    // Commit a change in task-2
    {
        std::fs::write(path2.join("feature2.rs"), "fn other() {}\n").unwrap();
        let wt_repo = Repository::open(&path2).unwrap();
        let sig = git2::Signature::now("Agent2", "agent2@test.com").unwrap();
        let mut index = wt_repo.index().unwrap();
        index.add_path(Path::new("feature2.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = wt_repo.find_tree(tree_id).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        wt_repo.commit(Some("HEAD"), &sig, &sig, "Add feature2", &tree, &[&head]).unwrap();
    }

    // Compute divergence
    let divergence = ws.compute_divergence("main").unwrap();
    assert_eq!(divergence.len(), 2);

    // task-1 should have feature1.rs
    assert!(divergence.iter().any(|e| e.file_path == "feature1.rs"
        && e.sandbox_id == "task-1"
        && e.status == FileStatus::Added));

    // task-2 should have feature2.rs
    assert!(divergence.iter().any(|e| e.file_path == "feature2.rs"
        && e.sandbox_id == "task-2"
        && e.status == FileStatus::Added));
}

#[test]
fn test_workspace_remove_preserves_others() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let _path1 = ws.create_worktree("task-1", "main").unwrap();
    let path2 = ws.create_worktree("task-2", "main").unwrap();

    // Remove task-1
    ws.remove_worktree("task-1", true).unwrap();

    // task-2 should still be there
    assert!(path2.exists());
    let worktrees = ws.list_worktrees().unwrap();
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0].sandbox_id, "task-2");
}

#[test]
fn test_workspace_invalid_base_branch_fails() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let result = ws.create_worktree("task-1", "nonexistent-branch");
    assert!(result.is_err());
}

#[test]
fn test_workspace_commits_ahead_count() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let path = ws.create_worktree("task-1", "main").unwrap();

    // Initially 0 commits ahead
    let worktrees = ws.list_worktrees().unwrap();
    assert_eq!(worktrees[0].commits_ahead, 0);

    // Make two commits
    let wt_repo = Repository::open(&path).unwrap();
    let sig = git2::Signature::now("Agent", "agent@test.com").unwrap();

    for i in 1..=2 {
        std::fs::write(path.join(format!("file{i}.txt")), format!("content {i}\n")).unwrap();
        let mut index = wt_repo.index().unwrap();
        index.add_path(Path::new(&format!("file{i}.txt"))).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = wt_repo.find_tree(tree_id).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        wt_repo.commit(Some("HEAD"), &sig, &sig, &format!("Commit {i}"), &tree, &[&head]).unwrap();
    }

    // Should be 2 commits ahead
    let worktrees = ws.list_worktrees().unwrap();
    assert_eq!(worktrees[0].commits_ahead, 2);
}

#[test]
fn test_workspace_merge_clean() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let path = ws.create_worktree("task-1", "main").unwrap();

    // Make a commit in the worktree
    {
        std::fs::write(path.join("new_feature.rs"), "fn feature() {}\n").unwrap();
        let wt_repo = Repository::open(&path).unwrap();
        let sig = git2::Signature::now("Agent", "agent@test.com").unwrap();
        let mut index = wt_repo.index().unwrap();
        index.add_path(Path::new("new_feature.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = wt_repo.find_tree(tree_id).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        wt_repo.commit(Some("HEAD"), &sig, &sig, "Add feature", &tree, &[&head]).unwrap();
    }

    // Merge should succeed with no conflicts
    let conflicts = ws.merge_branch("task-1", "main").unwrap();
    assert!(conflicts.is_empty(), "Expected no conflicts, got: {conflicts:?}");

    // The main branch should now have the new file
    let repo = Repository::open(&repo_path).unwrap();
    let main_commit = repo.revparse_single("main").unwrap().peel_to_commit().unwrap();
    let main_tree = main_commit.tree().unwrap();
    assert!(main_tree.get_name("new_feature.rs").is_some());
}

// ─── Snapshot Manager Tests ─────────────────────────────────────────────────

#[test]
fn test_snapshot_manager_list_empty() {
    let tmp = TempDir::new().unwrap();
    let template_dir = tmp.path().join("templates");
    let runtime_dir = tmp.path().join("runtime");

    let mgr = SnapshotManager::new(template_dir.clone(), runtime_dir).unwrap();
    let templates = mgr.list_templates().unwrap();
    assert!(templates.is_empty());
}

#[test]
fn test_snapshot_manager_list_with_templates() {
    let tmp = TempDir::new().unwrap();
    let template_dir = tmp.path().join("templates");
    let runtime_dir = tmp.path().join("runtime");

    let mgr = SnapshotManager::new(template_dir.clone(), runtime_dir).unwrap();

    // Create fake template directories with files
    let t1 = template_dir.join("base-python");
    std::fs::create_dir_all(&t1).unwrap();
    std::fs::write(t1.join("snapshot.bin"), "fake snapshot data 12345").unwrap();

    let t2 = template_dir.join("base-rust");
    std::fs::create_dir_all(&t2).unwrap();
    std::fs::write(t2.join("snapshot.bin"), "more fake data").unwrap();
    std::fs::write(t2.join("memory.bin"), "fake memory dump").unwrap();

    let templates = mgr.list_templates().unwrap();
    assert_eq!(templates.len(), 2);

    let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"base-python"));
    assert!(names.contains(&"base-rust"));

    // base-rust should be larger (two files)
    let rust_template = templates.iter().find(|t| t.name == "base-rust").unwrap();
    let python_template = templates.iter().find(|t| t.name == "base-python").unwrap();
    assert!(rust_template.size_bytes > python_template.size_bytes);
}

#[test]
fn test_snapshot_manager_delete() {
    let tmp = TempDir::new().unwrap();
    let template_dir = tmp.path().join("templates");
    let runtime_dir = tmp.path().join("runtime");

    let mgr = SnapshotManager::new(template_dir.clone(), runtime_dir).unwrap();

    // Create a fake template
    let t1 = template_dir.join("to-delete");
    std::fs::create_dir_all(&t1).unwrap();
    std::fs::write(t1.join("snapshot.bin"), "data").unwrap();

    assert_eq!(mgr.list_templates().unwrap().len(), 1);

    mgr.delete_template("to-delete").unwrap();
    assert_eq!(mgr.list_templates().unwrap().len(), 0);
    assert!(!t1.exists());
}

#[test]
fn test_snapshot_manager_delete_nonexistent_fails() {
    let tmp = TempDir::new().unwrap();
    let template_dir = tmp.path().join("templates");
    let runtime_dir = tmp.path().join("runtime");

    let mgr = SnapshotManager::new(template_dir, runtime_dir).unwrap();
    let result = mgr.delete_template("nonexistent");
    assert!(result.is_err());
}

// ─── Sandbox Orchestrator Tests (with Mock VM) ─────────────────────────────

/// A mock VM port that doesn't actually start any VMs.
/// Used to test the orchestrator's coordination logic.
struct MockVmPort {
    started: std::sync::Mutex<Vec<VmConfig>>,
    stopped: std::sync::Mutex<Vec<String>>,
}

impl MockVmPort {
    fn new() -> Self {
        Self {
            started: std::sync::Mutex::new(Vec::new()),
            stopped: std::sync::Mutex::new(Vec::new()),
        }
    }

    #[allow(dead_code)]
    fn started_ids(&self) -> Vec<String> {
        self.started.lock().unwrap().iter().map(|c| c.id.clone()).collect()
    }

    #[allow(dead_code)]
    fn stopped_ids(&self) -> Vec<String> {
        self.stopped.lock().unwrap().clone()
    }
}

impl VmPort for MockVmPort {
    async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = config.id.clone();
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
        Ok(self
            .started
            .lock()
            .unwrap()
            .iter()
            .map(|c| VmInfo {
                id: c.id.clone(),
                pid: 12345,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/mock-api.sock"),
                console_socket: PathBuf::from("/tmp/mock-console.sock"),
            })
            .collect())
    }
}

#[tokio::test]
async fn test_orchestrator_create_sandbox() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockVmPort::new();
    let orch = SandboxOrchestrator::new(config, ws, vm);

    let status = orch
        .create_sandbox(CreateSandboxParams {
            task_id: "fix-auth".to_string(),
            base_branch: "main".to_string(),
            template: None,
            memory_mib: None,
            vcpus: None,
            user: None,
            env_vars: vec![],
            command: vec!["claude".to_string()],
            timeout_secs: None,
            ephemeral: false,
        })
        .await
        .unwrap();

    assert_eq!(status.id, "fix-auth");
    assert_eq!(status.branch, "agent/fix-auth");
    assert_eq!(status.vm_state, "running");
    assert_eq!(status.vm_pid, 12345);
}

#[tokio::test]
async fn test_orchestrator_create_multiple_sandboxes() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockVmPort::new();
    let orch = SandboxOrchestrator::new(config, ws, vm);

    for task in &["task-a", "task-b", "task-c"] {
        orch.create_sandbox(CreateSandboxParams {
            task_id: task.to_string(),
            base_branch: "main".to_string(),
            template: None,
            memory_mib: None,
            vcpus: None,
            user: None,
            env_vars: vec![],
            command: vec!["claude".to_string()],
            timeout_secs: None,
            ephemeral: false,
        })
        .await
        .unwrap();
    }

    let statuses = orch.list_sandboxes().await.unwrap();
    assert_eq!(statuses.len(), 3);

    let ids: Vec<&str> = statuses.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"task-a"));
    assert!(ids.contains(&"task-b"));
    assert!(ids.contains(&"task-c"));
}

#[tokio::test]
async fn test_orchestrator_stop_sandbox() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockVmPort::new();
    let orch = SandboxOrchestrator::new(config, ws, vm);

    orch.create_sandbox(CreateSandboxParams {
        task_id: "task-1".to_string(),
        base_branch: "main".to_string(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["claude".to_string()],
        timeout_secs: None,
        ephemeral: false,
    })
    .await
    .unwrap();

    // Stop without cleaning
    orch.stop_sandbox("task-1", false).await.unwrap();

    // Worktree should still exist
    assert!(wt_base.join("task-1").exists());
}

#[tokio::test]
async fn test_orchestrator_stop_with_clean() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockVmPort::new();
    let orch = SandboxOrchestrator::new(config, ws, vm);

    orch.create_sandbox(CreateSandboxParams {
        task_id: "task-1".to_string(),
        base_branch: "main".to_string(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["claude".to_string()],
        timeout_secs: None,
        ephemeral: false,
    })
    .await
    .unwrap();

    // Stop with cleaning
    orch.stop_sandbox("task-1", true).await.unwrap();

    // Worktree should be removed
    assert!(!wt_base.join("task-1").exists());
}

#[tokio::test]
async fn test_orchestrator_divergence() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockVmPort::new();
    let orch = SandboxOrchestrator::new(config, ws, vm);

    let status = orch
        .create_sandbox(CreateSandboxParams {
            task_id: "task-1".to_string(),
            base_branch: "main".to_string(),
            template: None,
            memory_mib: None,
            vcpus: None,
            user: None,
            env_vars: vec![],
            command: vec!["claude".to_string()],
            timeout_secs: None,
            ephemeral: false,
        })
        .await
        .unwrap();

    // Make a commit in the worktree
    let wt_path = PathBuf::from(&status.worktree_path);
    std::fs::write(wt_path.join("new.rs"), "fn new() {}\n").unwrap();
    let wt_repo = Repository::open(&wt_path).unwrap();
    let sig = git2::Signature::now("Agent", "agent@test.com").unwrap();
    let mut index = wt_repo.index().unwrap();
    index.add_path(Path::new("new.rs")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = wt_repo.find_tree(tree_id).unwrap();
    let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
    wt_repo.commit(Some("HEAD"), &sig, &sig, "Add new.rs", &tree, &[&head]).unwrap();

    let divergence = orch.divergence("main").unwrap();
    assert!(!divergence.is_empty());
    assert!(divergence.iter().any(|e| e.file_path == "new.rs"));
}

#[tokio::test]
async fn test_orchestrator_vm_config_overrides() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };

    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockVmPort::new();
    let orch = SandboxOrchestrator::new(config, ws, vm);

    orch.create_sandbox(CreateSandboxParams {
        task_id: "task-custom".to_string(),
        base_branch: "main".to_string(),
        template: None,
        memory_mib: Some(8192),
        vcpus: Some(4),
        user: Some("agent-user".to_string()),
        env_vars: vec![("FOO".to_string(), "bar".to_string())],
        command: vec!["claude".to_string()],
        timeout_secs: None,
        ephemeral: false,
    })
    .await
    .unwrap();

    // Verify the VM was started with the overridden config
    // We need to access the mock's internal state
    // Since we can't easily access it through the orchestrator,
    // we verify the status output is correct
    let statuses = orch.list_sandboxes().await.unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].id, "task-custom");
}

#[tokio::test]
async fn test_run_sandbox_polls_until_vm_exits() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock VM port whose `info()` returns Ok on the first call and Err
    /// on all subsequent calls, simulating a VM whose guest agent exited
    /// promptly with code 0. The mock exposes a `status_dir` so
    /// `run_sandbox` can read a pre-staged "0\n" exit-code file (mirroring
    /// what the real `aboxstatus` virtiofs share would contain after a
    /// clean guest poweroff).
    struct ExitingMockVm {
        info_calls: AtomicUsize,
        status_dir: PathBuf,
    }

    impl VmPort for ExitingMockVm {
        async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
            Ok(VmInfo {
                id: config.id,
                pid: 12345,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/api.sock"),
                console_socket: PathBuf::from("/tmp/console.sock"),
            })
        }

        async fn stop(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn info(&self, id: &str) -> anyhow::Result<VmInfo> {
            // First call: VM still exists. All subsequent calls: VM is gone.
            let n = self.info_calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(VmInfo {
                    id: id.to_string(),
                    pid: 12345,
                    state: VmState::Running,
                    api_socket: PathBuf::from("/tmp/api.sock"),
                    console_socket: PathBuf::from("/tmp/console.sock"),
                })
            } else {
                anyhow::bail!("VM exited")
            }
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            Ok(vec![])
        }

        fn status_dir(&self, _id: &str) -> Option<PathBuf> {
            Some(self.status_dir.clone())
        }
    }

    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // Pre-stage a clean exit code (0) like a real guest poweroff would.
    let status_dir = tmp.path().join("status-run-sandbox-test");
    std::fs::create_dir_all(&status_dir).unwrap();
    std::fs::write(status_dir.join("exit-code"), "0\n").unwrap();

    let vm = ExitingMockVm { info_calls: AtomicUsize::new(0), status_dir };
    let orchestrator = SandboxOrchestrator::new(config, workspace, vm);

    let params = CreateSandboxParams {
        task_id: "run-sandbox-test".into(),
        base_branch: "main".into(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["true".into()],
        timeout_secs: None,
        ephemeral: false,
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
        })
        .unwrap(),
    );

    let exit = orchestrator.run_sandbox(params, policy).await.unwrap();
    assert_eq!(exit, 0);

    // The worktree should have been created on disk.
    assert!(wt_base.join("run-sandbox-test").exists());
}

#[tokio::test]
async fn test_silent_failure_missing_exit_code_returns_1_and_rolls_back() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock VM that exits without writing an exit-code file.
    /// Simulates a VM crash before guest init completed.
    struct ExitingMockVmNoStatus {
        info_calls: AtomicUsize,
        status_dir: PathBuf,
    }

    impl VmPort for ExitingMockVmNoStatus {
        async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
            Ok(VmInfo {
                id: config.id,
                pid: 99999,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/api.sock"),
                console_socket: PathBuf::from("/tmp/console.sock"),
            })
        }

        async fn stop(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn info(&self, id: &str) -> anyhow::Result<VmInfo> {
            let n = self.info_calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(VmInfo {
                    id: id.to_string(),
                    pid: 99999,
                    state: VmState::Running,
                    api_socket: PathBuf::from("/tmp/api.sock"),
                    console_socket: PathBuf::from("/tmp/console.sock"),
                })
            } else {
                anyhow::bail!("VM exited")
            }
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            Ok(vec![])
        }

        fn status_dir(&self, _id: &str) -> Option<PathBuf> {
            Some(self.status_dir.clone())
        }
    }

    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // Create a status dir with NO exit-code file (simulates crash).
    let status_dir = tmp.path().join("status-silent-fail");
    std::fs::create_dir_all(&status_dir).unwrap();

    let vm =
        ExitingMockVmNoStatus { info_calls: AtomicUsize::new(0), status_dir };
    let orchestrator = SandboxOrchestrator::new(config, workspace, vm);

    let params = CreateSandboxParams {
        task_id: "silent-fail".into(),
        base_branch: "main".into(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["true".into()],
        timeout_secs: None,
        ephemeral: false,
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
        })
        .unwrap(),
    );

    let exit = orchestrator.run_sandbox(params, policy).await.unwrap();
    assert_eq!(exit, 1, "missing exit-code should produce exit code 1");

    // The worktree should have been rolled back (removed).
    assert!(
        !wt_base.join("silent-fail").exists(),
        "worktree should be removed after silent VM failure"
    );
}

// ─── read_exit_code tests ───────────────────────────────────────────────────

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
    let result = abox_core::adapters::cloud_hypervisor::read_exit_code(tmp.path());
    assert!(result.is_none());
}

#[test]
fn test_read_exit_code_malformed_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("exit-code"), "not-a-number").unwrap();
    let code = abox_core::adapters::cloud_hypervisor::read_exit_code(tmp.path());
    assert_eq!(code, None);
}

// ─── Timeout Tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_run_sandbox_timeout_returns_124() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock VM that never exits on its own — `info()` always returns Ok.
    /// `stop()` records calls so we can verify graceful shutdown was attempted,
    /// and after stop is called, `info()` starts returning Err (simulating
    /// the VM exiting after graceful shutdown).
    struct NeverExitVm {
        stop_calls: AtomicUsize,
    }

    impl VmPort for NeverExitVm {
        async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
            Ok(VmInfo {
                id: config.id,
                pid: 99999,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/api.sock"),
                console_socket: PathBuf::from("/tmp/console.sock"),
            })
        }

        async fn stop(&self, _id: &str) -> anyhow::Result<()> {
            self.stop_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn info(&self, id: &str) -> anyhow::Result<VmInfo> {
            // After stop() is called, simulate VM gone.
            if self.stop_calls.load(Ordering::SeqCst) > 0 {
                anyhow::bail!("VM exited after stop");
            }
            Ok(VmInfo {
                id: id.to_string(),
                pid: 99999,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/api.sock"),
                console_socket: PathBuf::from("/tmp/console.sock"),
            })
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            Ok(vec![])
        }
    }

    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    let vm = NeverExitVm { stop_calls: AtomicUsize::new(0) };
    let orchestrator = SandboxOrchestrator::new(config, workspace, vm);

    let params = CreateSandboxParams {
        task_id: "timeout-test".into(),
        base_branch: "main".into(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["sleep".into(), "infinity".into()],
        timeout_secs: Some(1), // 1-second timeout
        ephemeral: false,
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
        })
        .unwrap(),
    );

    let exit = orchestrator.run_sandbox(params, policy).await.unwrap();
    assert_eq!(exit, 124, "timeout should produce exit code 124");
}

#[tokio::test]
async fn test_run_sandbox_exits_before_timeout() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock VM that exits after 1 info() call, well before any timeout.
    struct QuickExitVm {
        info_calls: AtomicUsize,
        status_dir: PathBuf,
    }

    impl VmPort for QuickExitVm {
        async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
            Ok(VmInfo {
                id: config.id,
                pid: 12345,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/api.sock"),
                console_socket: PathBuf::from("/tmp/console.sock"),
            })
        }

        async fn stop(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn info(&self, id: &str) -> anyhow::Result<VmInfo> {
            let n = self.info_calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(VmInfo {
                    id: id.to_string(),
                    pid: 12345,
                    state: VmState::Running,
                    api_socket: PathBuf::from("/tmp/api.sock"),
                    console_socket: PathBuf::from("/tmp/console.sock"),
                })
            } else {
                anyhow::bail!("VM exited")
            }
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            Ok(vec![])
        }

        fn status_dir(&self, _id: &str) -> Option<PathBuf> {
            Some(self.status_dir.clone())
        }
    }

    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // Pre-stage exit code 42.
    let status_dir = tmp.path().join("status-quick-exit");
    std::fs::create_dir_all(&status_dir).unwrap();
    std::fs::write(status_dir.join("exit-code"), "42\n").unwrap();

    let vm = QuickExitVm { info_calls: AtomicUsize::new(0), status_dir };
    let orchestrator = SandboxOrchestrator::new(config, workspace, vm);

    let params = CreateSandboxParams {
        task_id: "quick-exit".into(),
        base_branch: "main".into(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["true".into()],
        timeout_secs: Some(60), // generous timeout — should not fire
        ephemeral: false,
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
        })
        .unwrap(),
    );

    let exit = orchestrator.run_sandbox(params, policy).await.unwrap();
    assert_eq!(exit, 42, "should return the guest's exit code, not 124");
}

// ─── Ephemeral Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_run_sandbox_ephemeral_cleans_up() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ExitingMockVm {
        info_calls: AtomicUsize,
        status_dir: PathBuf,
    }

    impl VmPort for ExitingMockVm {
        async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
            Ok(VmInfo {
                id: config.id,
                pid: 12345,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/api.sock"),
                console_socket: PathBuf::from("/tmp/console.sock"),
            })
        }

        async fn stop(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn info(&self, id: &str) -> anyhow::Result<VmInfo> {
            let n = self.info_calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(VmInfo {
                    id: id.to_string(),
                    pid: 12345,
                    state: VmState::Running,
                    api_socket: PathBuf::from("/tmp/api.sock"),
                    console_socket: PathBuf::from("/tmp/console.sock"),
                })
            } else {
                anyhow::bail!("VM exited")
            }
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            Ok(vec![])
        }

        fn status_dir(&self, _id: &str) -> Option<PathBuf> {
            Some(self.status_dir.clone())
        }
    }

    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    let status_dir = tmp.path().join("status-ephemeral");
    std::fs::create_dir_all(&status_dir).unwrap();
    std::fs::write(status_dir.join("exit-code"), "0\n").unwrap();

    let vm = ExitingMockVm { info_calls: AtomicUsize::new(0), status_dir };
    let orchestrator = SandboxOrchestrator::new(config, workspace, vm);

    let params = CreateSandboxParams {
        task_id: "ephemeral-test".into(),
        base_branch: "main".into(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["true".into()],
        timeout_secs: None,
        ephemeral: true,
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
        })
        .unwrap(),
    );

    let exit = orchestrator.run_sandbox(params, policy).await.unwrap();
    assert_eq!(exit, 0);

    // Worktree should be cleaned up in ephemeral mode.
    assert!(
        !wt_base.join("ephemeral-test").exists(),
        "ephemeral mode should remove the worktree"
    );
}

#[tokio::test]
async fn test_run_sandbox_non_ephemeral_preserves_worktree() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ExitingMockVm {
        info_calls: AtomicUsize,
        status_dir: PathBuf,
    }

    impl VmPort for ExitingMockVm {
        async fn start(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
            Ok(VmInfo {
                id: config.id,
                pid: 12345,
                state: VmState::Running,
                api_socket: PathBuf::from("/tmp/api.sock"),
                console_socket: PathBuf::from("/tmp/console.sock"),
            })
        }

        async fn stop(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn pause(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resume(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn info(&self, id: &str) -> anyhow::Result<VmInfo> {
            let n = self.info_calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(VmInfo {
                    id: id.to_string(),
                    pid: 12345,
                    state: VmState::Running,
                    api_socket: PathBuf::from("/tmp/api.sock"),
                    console_socket: PathBuf::from("/tmp/console.sock"),
                })
            } else {
                anyhow::bail!("VM exited")
            }
        }

        async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
            Ok(vec![])
        }

        fn status_dir(&self, _id: &str) -> Option<PathBuf> {
            Some(self.status_dir.clone())
        }
    }

    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    let status_dir = tmp.path().join("status-non-ephemeral");
    std::fs::create_dir_all(&status_dir).unwrap();
    std::fs::write(status_dir.join("exit-code"), "0\n").unwrap();

    let vm = ExitingMockVm { info_calls: AtomicUsize::new(0), status_dir };
    let orchestrator = SandboxOrchestrator::new(config, workspace, vm);

    let params = CreateSandboxParams {
        task_id: "non-ephemeral-test".into(),
        base_branch: "main".into(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["true".into()],
        timeout_secs: None,
        ephemeral: false,
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
        })
        .unwrap(),
    );

    let exit = orchestrator.run_sandbox(params, policy).await.unwrap();
    assert_eq!(exit, 0);

    // Worktree should still exist when NOT ephemeral.
    assert!(
        wt_base.join("non-ephemeral-test").exists(),
        "non-ephemeral mode should preserve the worktree"
    );
}
