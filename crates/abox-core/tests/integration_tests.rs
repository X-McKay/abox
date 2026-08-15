//! Integration tests for abox-core.
//!
//! These tests exercise the public API of abox-core components in combination,
//! including the config loader, policy engine, workspace manager, snapshot
//! manager, and sandbox orchestrator (with mock VM).

use abox_core::adapters::git2_workspace::Git2Workspace;
use abox_core::config::AboxConfig;
use abox_core::policy::{CliPolicy, Decision, EgressRule, PolicyEngine, PolicyFile};
use abox_core::project::EnvironmentProfile;
use abox_core::runtime::testing::{MockBehavior, MockRuntime};
use abox_core::sandbox::{CreateSandboxParams, SandboxOrchestrator};
use abox_core::snapshot::SnapshotManager;
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
    // runtime_dir defaults to <state_dir>/r when not configured (short path to avoid
    // Unix domain socket 108-byte limit with per-sandbox suffixes).
    assert_eq!(config.runtime_dir(), PathBuf::from("/test/state/r"));
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
        bypass_tls: vec![],
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
        bypass_tls: vec![],
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
        bypass_tls: vec![],
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
        bypass_tls: vec![],
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
        bypass_tls: vec![],
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
                native_substitution: false,
                env_var: Some("ANTHROPIC_API_KEY".to_string()),
                credential_file: None,
                json_path: None,
                header_template: "{value}".to_string(),
                request_rules: vec![],
            },
            EgressRule {
                domain: "api.openai.com".to_string(),
                inject_header: "Authorization".to_string(),
                native_substitution: false,
                env_var: Some("OPENAI_API_KEY".to_string()),
                credential_file: None,
                json_path: None,
                header_template: "Bearer {value}".to_string(),
                request_rules: vec![],
            },
            EgressRule {
                domain: "*.amazonaws.com".to_string(),
                inject_header: "Authorization".to_string(),
                native_substitution: false,
                env_var: Some("AWS_TOKEN".to_string()),
                credential_file: None,
                json_path: None,
                header_template: "AWS4-HMAC-SHA256 {value}".to_string(),
                request_rules: vec![],
            },
        ],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
        bypass_tls: vec![],
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    // Anthropic
    let rule = engine.evaluate_egress("api.anthropic.com").unwrap().unwrap();
    assert_eq!(rule.env_var.as_deref(), Some("ANTHROPIC_API_KEY"));

    // OpenAI
    let rule = engine.evaluate_egress("api.openai.com").unwrap().unwrap();
    assert_eq!(rule.env_var.as_deref(), Some("OPENAI_API_KEY"));

    // AWS wildcard
    let rule = engine.evaluate_egress("s3.us-east-1.amazonaws.com").unwrap().unwrap();
    assert_eq!(rule.env_var.as_deref(), Some("AWS_TOKEN"));

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

    // Set repo-local user config so tests that shell out to `git` (e.g., merges,
    // which need a committer identity) work in environments with no global git
    // config, like fresh CI runners. git2's in-memory Signature is fine for
    // direct commits, but merge_branch() in the Git2Workspace adapter shells out.
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&repo_path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

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

    let mgr =
        SnapshotManager::new(template_dir.clone(), runtime_dir, tmp.path().to_path_buf()).unwrap();
    let templates = mgr.list_templates().unwrap();
    assert!(templates.is_empty());
}

#[test]
fn test_snapshot_manager_list_with_templates() {
    let tmp = TempDir::new().unwrap();
    let template_dir = tmp.path().join("templates");
    let runtime_dir = tmp.path().join("runtime");

    let mgr =
        SnapshotManager::new(template_dir.clone(), runtime_dir, tmp.path().to_path_buf()).unwrap();

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

    let mgr =
        SnapshotManager::new(template_dir.clone(), runtime_dir, tmp.path().to_path_buf()).unwrap();

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

    let mgr = SnapshotManager::new(template_dir, runtime_dir, tmp.path().to_path_buf()).unwrap();
    let result = mgr.delete_template("nonexistent");
    assert!(result.is_err());
}

// ─── Sandbox Orchestrator Tests (with MockRuntime) ─────────────────────────

#[tokio::test]
async fn test_orchestrator_create_sandbox() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
            resolved_prompt: None,
            cache_mount_dir: None,
            staged_prepare_script: None,
            environment_profile: EnvironmentProfile::Base,
            timeout_secs: None,
            ephemeral: false,
            ca_cert_pem: None,
            mount_excludes: vec![],
            service_bridges: Vec::new(),
            host_port_bridges: Vec::new(),
            input_files: Vec::new(),
            network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
            native_secrets: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(status.id, "fix-auth");
    assert_eq!(status.branch, "agent/fix-auth");
    assert_eq!(status.vm_state, "running");
    assert_eq!(status.vm_pid, 12345);
}

/// `abox path <task>` is a thin wrapper over `worktree_info`; this verifies the
/// lookup it relies on returns the real worktree path for a known sandbox and
/// `None` for an unknown one.
#[tokio::test]
async fn test_worktree_info_lookup() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockRuntime::new(tmp.path().to_path_buf());
    let orch = SandboxOrchestrator::new(config, ws, vm);

    let status = orch
        .create_sandbox(CreateSandboxParams {
            task_id: "collect-me".to_string(),
            base_branch: "main".to_string(),
            template: None,
            memory_mib: None,
            vcpus: None,
            user: None,
            env_vars: vec![],
            command: vec!["claude".to_string()],
            resolved_prompt: None,
            cache_mount_dir: None,
            staged_prepare_script: None,
            environment_profile: EnvironmentProfile::Base,
            timeout_secs: None,
            ephemeral: false,
            ca_cert_pem: None,
            mount_excludes: vec![],
            service_bridges: Vec::new(),
            host_port_bridges: Vec::new(),
            input_files: Vec::new(),
            network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
            native_secrets: Vec::new(),
        })
        .await
        .unwrap();

    let info = orch.worktree_info("collect-me").unwrap().expect("known task resolves");
    assert_eq!(info.sandbox_id, "collect-me");
    assert_eq!(info.path.to_string_lossy(), status.worktree_path);
    assert!(info.path.is_dir(), "worktree path should exist on disk");

    assert!(orch.worktree_info("no-such-task").unwrap().is_none());
}

#[tokio::test]
async fn test_orchestrator_create_multiple_sandboxes() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
            resolved_prompt: None,
            cache_mount_dir: None,
            staged_prepare_script: None,
            environment_profile: EnvironmentProfile::Base,
            timeout_secs: None,
            ephemeral: false,
            ca_cert_pem: None,
            mount_excludes: vec![],
            service_bridges: Vec::new(),
            host_port_bridges: Vec::new(),
            input_files: Vec::new(),
            network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
            native_secrets: Vec::new(),
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
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
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
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
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
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
            resolved_prompt: None,
            cache_mount_dir: None,
            staged_prepare_script: None,
            environment_profile: EnvironmentProfile::Base,
            timeout_secs: None,
            ephemeral: false,
            ca_cert_pem: None,
            mount_excludes: vec![],
            service_bridges: Vec::new(),
            host_port_bridges: Vec::new(),
            input_files: Vec::new(),
            network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
            native_secrets: Vec::new(),
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
    let vm = MockRuntime::new(tmp.path().to_path_buf());
    let orch = SandboxOrchestrator::new(config, ws, vm.clone());

    orch.create_sandbox(CreateSandboxParams {
        task_id: "task-custom".to_string(),
        base_branch: "main".to_string(),
        template: None,
        memory_mib: Some(8192),
        vcpus: Some(4),
        user: Some("agent-user".to_string()),
        env_vars: vec![("FOO".to_string(), "bar".to_string())],
        command: vec!["claude".to_string()],
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    })
    .await
    .unwrap();

    // Verify the sandbox was started with the overridden spec.
    let started = vm.started();
    assert_eq!(started.len(), 1);
    let spec = &started[0];
    assert_eq!(spec.id, "task-custom");
    assert_eq!(spec.resources.memory_mib, 8192);
    assert_eq!(spec.resources.vcpus, 4);
    assert_eq!(spec.user.as_deref(), Some("agent-user"));
    assert!(spec.env.contains(&("FOO".to_string(), "bar".to_string())));
    assert_eq!(spec.command, vec!["claude".to_string()]);
    assert_eq!(spec.workspace.host_path(), &wt_base.join("task-custom"));

    let statuses = orch.list_sandboxes().await.unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].id, "task-custom");
}

#[tokio::test]
async fn test_run_sandbox_polls_until_vm_exits() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // A sandbox whose guest agent exits promptly with code 0; the runtime
    // reports the clean exit through its exit channel (mirroring what a
    // real guest poweroff would produce).
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
            bypass_tls: vec![],
        })
        .unwrap(),
    );

    let exit = orchestrator
        .run_sandbox(params, policy, {
            let tmp_ca = tempfile::TempDir::new().unwrap();
            std::sync::Arc::new(abox_core::ca::RootCa::generate_and_persist(tmp_ca.path()).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(exit, 0);

    // The worktree should have been created on disk.
    assert!(wt_base.join("run-sandbox-test").exists());
}

#[tokio::test]
async fn test_silent_failure_missing_exit_code_returns_1_and_rolls_back() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // A sandbox that terminates without reporting an exit code — simulates
    // a crash before guest init completed.
    let vm = MockRuntime::with_behavior(
        tmp.path().to_path_buf(),
        MockBehavior { exit_code: None, ..MockBehavior::default() },
    );
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
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
            bypass_tls: vec![],
        })
        .unwrap(),
    );

    let exit = orchestrator
        .run_sandbox(params, policy, {
            let tmp_ca = tempfile::TempDir::new().unwrap();
            std::sync::Arc::new(abox_core::ca::RootCa::generate_and_persist(tmp_ca.path()).unwrap())
        })
        .await
        .unwrap();
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

#[tokio::test(start_paused = true)]
async fn test_run_sandbox_timeout_returns_124() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // A sandbox that never exits on its own — the orchestrator must enforce
    // the timeout, attempt a graceful stop, and force-kill after the grace
    // period. (Paused tokio time auto-advances through the grace wait.)
    let vm = MockRuntime::with_behavior(
        tmp.path().to_path_buf(),
        MockBehavior { never_exit: true, ..MockBehavior::default() },
    );
    let orchestrator = SandboxOrchestrator::new(config, workspace, vm.clone());

    let params = CreateSandboxParams {
        task_id: "timeout-test".into(),
        base_branch: "main".into(),
        template: None,
        memory_mib: None,
        vcpus: None,
        user: None,
        env_vars: vec![],
        command: vec!["sleep".into(), "infinity".into()],
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: Some(1), // 1-second timeout
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
            bypass_tls: vec![],
        })
        .unwrap(),
    );

    let exit = orchestrator
        .run_sandbox(params, policy, {
            let tmp_ca = tempfile::TempDir::new().unwrap();
            std::sync::Arc::new(abox_core::ca::RootCa::generate_and_persist(tmp_ca.path()).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(exit, 124, "timeout should produce exit code 124");

    // Graceful shutdown must have been attempted before force-killing.
    assert_eq!(vm.stopped(), vec!["timeout-test".to_string()]);
    assert_eq!(
        vm.killed(),
        vec!["timeout-test".to_string()],
        "sandbox that ignores stop must be force-killed after the grace period"
    );
}

#[tokio::test]
async fn test_run_sandbox_exits_before_timeout() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // A sandbox that exits with code 42 after a short delay, well before
    // the timeout fires.
    let vm = MockRuntime::with_behavior(
        tmp.path().to_path_buf(),
        MockBehavior {
            exit_code: Some(42),
            exit_delay: Some(std::time::Duration::from_millis(50)),
            ..MockBehavior::default()
        },
    );
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
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: Some(60), // generous timeout — should not fire
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
            bypass_tls: vec![],
        })
        .unwrap(),
    );

    let exit = orchestrator
        .run_sandbox(params, policy, {
            let tmp_ca = tempfile::TempDir::new().unwrap();
            std::sync::Arc::new(abox_core::ca::RootCa::generate_and_persist(tmp_ca.path()).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(exit, 42, "should return the guest's exit code, not 124");
}

// ─── Ephemeral Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_run_sandbox_ephemeral_cleans_up() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // A sandbox whose guest agent exits cleanly with code 0.
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: true,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
            bypass_tls: vec![],
        })
        .unwrap(),
    );

    let exit = orchestrator
        .run_sandbox(params, policy, {
            let tmp_ca = tempfile::TempDir::new().unwrap();
            std::sync::Arc::new(abox_core::ca::RootCa::generate_and_persist(tmp_ca.path()).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(exit, 0);

    // Worktree should be cleaned up in ephemeral mode.
    assert!(!wt_base.join("ephemeral-test").exists(), "ephemeral mode should remove the worktree");
}

#[tokio::test]
async fn test_run_sandbox_non_ephemeral_preserves_worktree() {
    let (tmp, repo_path) = setup_test_repo();
    let wt_base = tmp.path().join("worktrees");
    let workspace = Git2Workspace::new(&repo_path, &wt_base).unwrap();

    let config = AboxConfig { state_dir: tmp.path().to_path_buf(), ..Default::default() };
    config.ensure_dirs().unwrap();

    // A sandbox whose guest agent exits cleanly with code 0.
    let vm = MockRuntime::new(tmp.path().to_path_buf());
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
        resolved_prompt: None,
        cache_mount_dir: None,
        staged_prepare_script: None,
        environment_profile: EnvironmentProfile::Base,
        timeout_secs: None,
        ephemeral: false,
        ca_cert_pem: None,
        mount_excludes: vec![],
        service_bridges: Vec::new(),
        host_port_bridges: Vec::new(),
        input_files: Vec::new(),
        network_plan: abox_core::runtime::RuntimeNetworkPlan::HostMediated,
        native_secrets: Vec::new(),
    };

    let policy = std::sync::Arc::new(
        abox_core::policy::PolicyEngine::from_policy_file(abox_core::policy::PolicyFile {
            cli: vec![],
            egress: vec![],
            default_cli_action: "allow".into(),
            default_egress_action: "deny".into(),
            bypass_tls: vec![],
        })
        .unwrap(),
    );

    let exit = orchestrator
        .run_sandbox(params, policy, {
            let tmp_ca = tempfile::TempDir::new().unwrap();
            std::sync::Arc::new(abox_core::ca::RootCa::generate_and_persist(tmp_ca.path()).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(exit, 0);

    // Worktree should still exist when NOT ephemeral.
    assert!(
        wt_base.join("non-ephemeral-test").exists(),
        "non-ephemeral mode should preserve the worktree"
    );
}
