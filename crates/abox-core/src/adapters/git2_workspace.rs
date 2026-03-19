//! Git2-based implementation of the [`WorkspacePort`] trait.
//!
//! Uses the `git2` crate to manage worktrees, branches, and divergence computation.

use crate::workspace::{DivergenceEntry, FileStatus, WorkspacePort, WorktreeInfo};
use anyhow::{Context, Result};
use git2::{BranchType, Delta, Repository};
use std::path::{Path, PathBuf};

/// Adapter that implements workspace management using libgit2.
pub struct Git2Workspace {
    /// Path to the main git repository.
    repo_path: PathBuf,
    /// Base directory where worktrees are created (e.g., `~/.abox/worktrees/`).
    worktree_base: PathBuf,
}

impl Git2Workspace {
    /// Create a new adapter.
    ///
    /// # Arguments
    /// * `repo_path` - Path to the git repository.
    /// * `worktree_base` - Directory where worktrees will be created.
    pub fn new(repo_path: impl AsRef<Path>, worktree_base: impl AsRef<Path>) -> Result<Self> {
        let repo_path = repo_path.as_ref().to_path_buf();
        let worktree_base = worktree_base.as_ref().to_path_buf();

        // Validate that the repo exists
        Repository::open(&repo_path).context("Failed to open git repository")?;

        std::fs::create_dir_all(&worktree_base)?;

        Ok(Self { repo_path, worktree_base })
    }

    /// Return the branch name for a given sandbox ID.
    fn branch_name(sandbox_id: &str) -> String {
        format!("agent/{sandbox_id}")
    }
}

impl WorkspacePort for Git2Workspace {
    fn create_worktree(&self, sandbox_id: &str, base_branch: &str) -> Result<PathBuf> {
        let repo = Repository::open(&self.repo_path)?;
        let branch_name = Self::branch_name(sandbox_id);

        // Resolve the base branch to a commit
        let base_ref = repo
            .revparse_single(base_branch)
            .with_context(|| format!("Branch '{base_branch}' not found"))?;
        let commit = base_ref.peel_to_commit().context("Failed to peel to commit")?;

        // Create the new branch
        repo.branch(&branch_name, &commit, false)
            .with_context(|| format!("Failed to create branch '{branch_name}'"))?;

        // Determine the worktree path
        let wt_path = self.worktree_base.join(sandbox_id);
        if wt_path.exists() {
            anyhow::bail!(
                "Worktree directory already exists: {}. Use a different sandbox ID.",
                wt_path.display()
            );
        }
        // Ensure the parent directory exists (git2 creates the leaf directory itself)
        std::fs::create_dir_all(&self.worktree_base)?;

        // Add the worktree, checking out the new branch
        let reference = repo.find_branch(&branch_name, BranchType::Local)?;
        let mut opts = git2::WorktreeAddOptions::new();
        let git_ref = reference.into_reference();
        opts.reference(Some(&git_ref));
        repo.worktree(sandbox_id, &wt_path, Some(&opts))
            .with_context(|| format!("Failed to create worktree at {}", wt_path.display()))?;

        tracing::info!(
            sandbox_id,
            branch = %branch_name,
            path = %wt_path.display(),
            "Created worktree"
        );

        Ok(wt_path)
    }

    fn remove_worktree(&self, sandbox_id: &str, delete_branch: bool) -> Result<()> {
        let repo = Repository::open(&self.repo_path)?;

        // Prune the worktree from git's tracking
        if let Ok(wt) = repo.find_worktree(sandbox_id) {
            let mut prune_opts = git2::WorktreePruneOptions::new();
            prune_opts.working_tree(true);
            prune_opts.valid(true);
            wt.prune(Some(&mut prune_opts))
                .with_context(|| format!("Failed to prune worktree '{sandbox_id}'"))?;
        }

        // Remove the worktree directory from disk
        let wt_path = self.worktree_base.join(sandbox_id);
        if wt_path.exists() {
            std::fs::remove_dir_all(&wt_path)
                .with_context(|| format!("Failed to remove {}", wt_path.display()))?;
        }

        // Optionally delete the branch
        if delete_branch {
            let branch_name = Self::branch_name(sandbox_id);
            if let Ok(mut branch) = repo.find_branch(&branch_name, BranchType::Local) {
                branch.delete()?;
                tracing::info!(branch = %branch_name, "Deleted branch");
            }
        }

        tracing::info!(sandbox_id, "Removed worktree");
        Ok(())
    }

    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let repo = Repository::open(&self.repo_path)?;
        let wt_names = repo.worktrees()?;
        let mut infos = Vec::new();

        for name in &wt_names {
            let Some(name) = name else {
                continue;
            };

            let wt_path = self.worktree_base.join(name);
            if !wt_path.exists() {
                continue;
            }

            let branch = Self::branch_name(name);

            // Count commits ahead of main (best effort)
            let commits_ahead = count_commits_ahead(&repo, &branch, "main").unwrap_or(0);

            infos.push(WorktreeInfo {
                sandbox_id: name.to_string(),
                branch,
                path: wt_path,
                commits_ahead,
            });
        }

        Ok(infos)
    }

    fn compute_divergence(&self, base_branch: &str) -> Result<Vec<DivergenceEntry>> {
        let repo = Repository::open(&self.repo_path)?;

        // Resolve the base branch tree
        let base_commit = repo
            .revparse_single(base_branch)?
            .peel_to_commit()
            .context("Failed to resolve base branch")?;
        let base_tree = base_commit.tree()?;

        let mut entries = Vec::new();
        let wt_names = repo.worktrees()?;

        for name in &wt_names {
            let Some(name) = name else {
                continue;
            };

            // Only process abox worktrees
            let wt_path = self.worktree_base.join(name);
            if !wt_path.exists() {
                continue;
            }

            // Get the worktree's HEAD tree
            let branch_name = Self::branch_name(name);
            let head_commit = match repo.revparse_single(&branch_name) {
                Ok(obj) => match obj.peel_to_commit() {
                    Ok(c) => c,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            let head_tree = head_commit.tree()?;

            // Diff base tree against the worktree's HEAD tree
            let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)?;

            for delta in diff.deltas() {
                let status = match delta.status() {
                    Delta::Added => FileStatus::Added,
                    Delta::Modified => FileStatus::Modified,
                    Delta::Deleted => FileStatus::Deleted,
                    Delta::Renamed => {
                        let old = delta
                            .old_file()
                            .path()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();
                        FileStatus::Renamed(old)
                    }
                    _ => continue,
                };

                let file_path = delta
                    .new_file()
                    .path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                entries.push(DivergenceEntry { file_path, sandbox_id: name.to_string(), status });
            }
        }

        Ok(entries)
    }

    fn merge_branch(&self, sandbox_id: &str, base_branch: &str) -> Result<Vec<String>> {
        // We shell out to `git merge` because git2's merge API is notoriously
        // difficult to use correctly for real-world merges with conflict detection.
        let branch_name = Self::branch_name(sandbox_id);

        // First, checkout the base branch
        let checkout_output = std::process::Command::new("git")
            .args(["checkout", base_branch])
            .current_dir(&self.repo_path)
            .output()
            .context("Failed to run git checkout")?;

        if !checkout_output.status.success() {
            let stderr = String::from_utf8_lossy(&checkout_output.stderr);
            anyhow::bail!("Failed to checkout {base_branch}: {stderr}");
        }

        // Then merge the agent branch
        let merge_output = std::process::Command::new("git")
            .args(["merge", "--no-ff", &branch_name, "-m"])
            .arg(format!("Merge {branch_name} into {base_branch}"))
            .current_dir(&self.repo_path)
            .output()
            .context("Failed to run git merge")?;

        if merge_output.status.success() {
            tracing::info!(sandbox_id, base_branch, "Merged branch successfully");
            Ok(vec![])
        } else {
            let stderr = String::from_utf8_lossy(&merge_output.stderr);
            let conflicts: Vec<String> = stderr
                .lines()
                .filter(|l| l.contains("CONFLICT"))
                .map(ToString::to_string)
                .collect();

            // Abort the merge so the repo is in a clean state
            let _ = std::process::Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(&self.repo_path)
                .output();

            Ok(conflicts)
        }
    }
}

/// Count how many commits `branch` is ahead of `base`.
fn count_commits_ahead(repo: &Repository, branch: &str, base: &str) -> Result<usize> {
    let branch_oid = repo.revparse_single(branch)?.id();
    let base_oid = repo.revparse_single(base)?.id();
    let (ahead, _behind) = repo.graph_ahead_behind(branch_oid, base_oid)?;
    Ok(ahead)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: create a temporary git repo with an initial commit on `main`.
    fn setup_test_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().to_path_buf();

        let repo = Repository::init(&repo_path).unwrap();

        // Create an initial commit so `main` exists
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            // Write a file so the tree is not empty
            let file_path = repo_path.join("README.md");
            std::fs::write(&file_path, "# Test Repo\n").unwrap();
            index.add_path(Path::new("README.md")).unwrap();
            index.write().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[]).unwrap();

        // Rename the default branch to `main`
        let mut head = repo.find_branch("master", BranchType::Local).unwrap();
        head.rename("main", false).unwrap();

        (tmp, repo_path)
    }

    #[test]
    fn test_create_and_list_worktree() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");

        let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();

        // Create a worktree
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        assert!(wt_path.exists());
        assert!(wt_path.join("README.md").exists());

        // List worktrees
        let worktrees = ws.list_worktrees().unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].sandbox_id, "task-1");
        assert_eq!(worktrees[0].branch, "agent/task-1");
    }

    #[test]
    fn test_create_duplicate_fails() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");

        let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
        ws.create_worktree("task-1", "main").unwrap();

        // Creating the same sandbox again should fail
        let result = ws.create_worktree("task-1", "main");
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_worktree() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");

        let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        assert!(wt_path.exists());

        ws.remove_worktree("task-1", true).unwrap();
        assert!(!wt_path.exists());

        // Branch should be deleted too
        let repo = Repository::open(&repo_path).unwrap();
        assert!(repo.find_branch("agent/task-1", BranchType::Local).is_err());
    }

    #[test]
    fn test_compute_divergence() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");

        let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();

        // Make a change in the worktree
        std::fs::write(wt_path.join("new_file.txt"), "hello\n").unwrap();

        // Commit the change in the worktree
        let wt_repo = Repository::open(&wt_path).unwrap();
        let sig = git2::Signature::now("Agent", "agent@test.com").unwrap();
        let mut index = wt_repo.index().unwrap();
        index.add_path(Path::new("new_file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = wt_repo.find_tree(tree_id).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        wt_repo.commit(Some("HEAD"), &sig, &sig, "Add new file", &tree, &[&head]).unwrap();

        // Compute divergence
        let divergence = ws.compute_divergence("main").unwrap();
        assert!(!divergence.is_empty());
        assert!(divergence
            .iter()
            .any(|e| e.file_path == "new_file.txt" && e.status == FileStatus::Added));
    }
}
