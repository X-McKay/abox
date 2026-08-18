//! Git2-based implementation of the [`WorkspacePort`] trait.
//!
//! Uses the `git2` crate to manage worktrees, branches, and divergence computation.

use crate::config::MergeValidationConfig;
use crate::workspace::{
    approved_path_set, normalize_repo_relative_path, DivergenceEntry, FileStatus, MergeBlocked,
    MergeOptions, MergeOutcome, MergeValidationPolicy, MergeValidationViolation, WorkspacePort,
    WorktreeInfo,
};
use anyhow::{Context, Result};
use git2::{BranchType, Delta, FileMode, ObjectType, Oid, Repository};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Compiled host merge policy, or the error text from an invalid host config.
///
/// Compilation is attempted once at construction, but a failure is *stored*
/// rather than propagated: an invalid `[merge.validation]` section must not
/// brick unrelated commands (`abox stop`, `abox list`, …) that also construct
/// the workspace. The error is surfaced when a merge is actually attempted
/// (fail-closed: the merge does not proceed) and proactively by `abox doctor`.
#[derive(Debug, Clone)]
enum MergePolicyState {
    Compiled(MergeValidationPolicy),
    Invalid(String),
}

/// Adapter that implements workspace management using libgit2.
pub struct Git2Workspace {
    /// Path to the main git repository.
    repo_path: PathBuf,
    /// Base directory where worktrees are created (e.g., `~/.abox/worktrees/`).
    worktree_base: PathBuf,
    /// Host-owned policy compiled once when the adapter is constructed.
    merge_validation: MergePolicyState,
}

impl Git2Workspace {
    /// Create a new adapter.
    ///
    /// # Arguments
    /// * `repo_path` - Path to the git repository.
    /// * `worktree_base` - Directory where worktrees will be created.
    pub fn new(repo_path: impl AsRef<Path>, worktree_base: impl AsRef<Path>) -> Result<Self> {
        Self::new_with_merge_validation(repo_path, worktree_base, &MergeValidationConfig::default())
    }

    /// Create an adapter with a host-owned merge validation policy.
    ///
    /// The policy is compiled eagerly, but a compile error is retained and
    /// only reported when a merge is attempted, so an invalid host config
    /// cannot brick unrelated commands that build the workspace.
    pub fn new_with_merge_validation(
        repo_path: impl AsRef<Path>,
        worktree_base: impl AsRef<Path>,
        merge_validation: &MergeValidationConfig,
    ) -> Result<Self> {
        let repo_path = repo_path.as_ref().to_path_buf();
        let worktree_base = worktree_base.as_ref().to_path_buf();

        // Validate that the repo exists
        Repository::open(&repo_path).context("Failed to open git repository")?;

        std::fs::create_dir_all(&worktree_base)?;

        let merge_validation = match MergeValidationPolicy::compile(merge_validation) {
            Ok(policy) => MergePolicyState::Compiled(policy),
            Err(error) => MergePolicyState::Invalid(format!("{error:#}")),
        };

        Ok(Self { repo_path, worktree_base, merge_validation })
    }

    /// Return the branch name for a given sandbox ID.
    fn branch_name(sandbox_id: &str) -> String {
        format!("agent/{sandbox_id}")
    }

    fn validate_merge(
        repo: &Repository,
        policy: &MergeValidationPolicy,
        branch_name: &str,
        base_branch: &str,
        options: &MergeOptions,
    ) -> Result<std::result::Result<MergeSnapshot, MergeBlocked>> {
        let approved_paths = match approved_path_set(options) {
            Ok(paths) => paths,
            Err(blocked) => return Ok(Err(blocked)),
        };

        let base_oid = resolve_branch_oid(repo, base_branch)
            .with_context(|| format!("base branch '{base_branch}' not found"))?;
        let agent_oid = resolve_branch_oid(repo, branch_name)
            .with_context(|| format!("agent branch '{branch_name}' not found"))?;
        let snapshot = MergeSnapshot { base_oid, agent_oid };

        if policy.is_empty() {
            return Ok(Ok(snapshot));
        }

        let merge_base = repo
            .merge_base(base_oid, agent_oid)
            .context("failed to find the merge base for validation")?;
        let base_tree = repo.find_commit(merge_base)?.tree()?;
        let agent_tree = repo.find_commit(agent_oid)?.tree()?;
        let mut diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&agent_tree), None)?;
        diff.find_similar(None)?;

        let mut violations = Vec::new();
        for delta in diff.deltas() {
            Self::validate_delta(repo, policy, &delta, &approved_paths, &mut violations)?;
        }

        if violations.is_empty() {
            Ok(Ok(snapshot))
        } else {
            Ok(Err(MergeBlocked::new(violations)))
        }
    }

    fn validate_delta(
        repo: &Repository,
        policy: &MergeValidationPolicy,
        delta: &git2::DiffDelta<'_>,
        approved_paths: &BTreeSet<PathBuf>,
        violations: &mut Vec<MergeValidationViolation>,
    ) -> Result<()> {
        let old_path =
            normalize_diff_path(delta.old_file().path(), "Git diff old path", violations);
        let new_path =
            normalize_diff_path(delta.new_file().path(), "Git diff new path", violations);

        let mut paths = BTreeSet::new();
        if let Some(path) = old_path.as_ref() {
            paths.insert(path.clone());
        }
        if let Some(path) = new_path.as_ref() {
            paths.insert(path.clone());
        }

        for path in paths {
            if let Some(pattern) = policy.denied_pattern(&path) {
                violations.push(MergeValidationViolation::DeniedPath {
                    path,
                    pattern: pattern.to_string(),
                });
            } else if let Some(pattern) = policy.review_pattern(&path) {
                if !approved_paths.contains(&path) {
                    violations.push(MergeValidationViolation::ReviewRequired {
                        path,
                        pattern: pattern.to_string(),
                    });
                }
            }
        }

        // An exact `--approve-path` acknowledgement clears the executable-bit
        // and size rules for that path too, so a reviewed executable or large
        // file has an escape hatch that does not require weakening the global
        // rule for every other path. `deny_patterns` remain absolute.
        let new_file = delta.new_file();
        let old_file = delta.old_file();
        let approved = new_path.as_ref().is_some_and(|path| approved_paths.contains(path));
        if !approved
            && policy.denies_new_executables()
            && new_file.mode() == FileMode::BlobExecutable
            && old_file.mode() != FileMode::BlobExecutable
        {
            if let Some(path) = new_path.as_ref() {
                violations.push(MergeValidationViolation::NewExecutable { path: path.clone() });
            }
        }

        if let (false, Some(max_size_kib), Some(path)) =
            (approved, policy.max_file_size_kib(), new_path.as_ref())
        {
            if matches!(
                new_file.mode(),
                FileMode::Blob
                    | FileMode::BlobExecutable
                    | FileMode::BlobGroupWritable
                    | FileMode::Link
            ) {
                let max_size_bytes = max_size_kib.saturating_mul(1024);
                // Read only the object header (type + size) instead of
                // inflating the whole blob into memory, which for the multi-GB
                // blobs this rule targets would defeat the purpose of the rule.
                match repo.odb().and_then(|odb| odb.read_header(new_file.id())) {
                    Ok((size, ObjectType::Blob)) => {
                        let size_bytes = u64::try_from(size).unwrap_or(u64::MAX);
                        if size_bytes > max_size_bytes {
                            violations.push(MergeValidationViolation::FileTooLarge {
                                path: path.clone(),
                                size_bytes,
                                max_size_kib,
                            });
                        }
                    }
                    // A non-blob object at a file path is unexpected; skip it
                    // rather than fail, matching the previous mode filter.
                    Ok(_) => {}
                    Err(error) => {
                        violations.push(MergeValidationViolation::UninspectableBlob {
                            path: path.clone(),
                            context: error.to_string(),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    fn snapshot_still_current(
        &self,
        base_branch: &str,
        agent_branch: &str,
        snapshot: MergeSnapshot,
    ) -> Result<Option<MergeBlocked>> {
        // Reopen the repository so this check observes a ref update made by
        // another process while the diff was being inspected.
        let repo = Repository::open(&self.repo_path)?;
        let mut violations = Vec::new();
        add_stale_reference_violation(&mut violations, &repo, base_branch, snapshot.base_oid);
        add_stale_reference_violation(&mut violations, &repo, agent_branch, snapshot.agent_oid);

        Ok((!violations.is_empty()).then(|| MergeBlocked::new(violations)))
    }
}

#[derive(Debug, Clone, Copy)]
struct MergeSnapshot {
    base_oid: Oid,
    agent_oid: Oid,
}

/// Resolve a *local branch* to its commit OID.
///
/// This deliberately resolves `refs/heads/<branch>` via `find_branch` rather
/// than `revparse_single`, whose gitrevisions precedence lets a tag named after
/// the branch shadow it. Merge validation must bind to the same object the
/// subsequent `git checkout <branch>` / `git merge <oid>` operate on, so a
/// `refs/tags/<branch>` an agent creates inside its worktree cannot make
/// validation inspect a different commit than the one that is merged.
fn resolve_branch_oid(repo: &Repository, branch: &str) -> Result<Oid> {
    let reference = repo.find_branch(branch, BranchType::Local)?;
    Ok(reference.get().peel_to_commit()?.id())
}

/// Where HEAD points, so it can be restored after a checkout that must be
/// rolled back.
enum HeadPosition {
    Branch(String),
    Detached(String),
}

/// Capture the current HEAD as either a branch name or a detached OID.
fn capture_head(repo_path: &Path) -> Result<HeadPosition> {
    let symbolic = Command::new("git")
        .args(["symbolic-ref", "-q", "--short", "HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to read HEAD")?;
    if symbolic.status.success() {
        let name = String::from_utf8_lossy(&symbolic.stdout).trim().to_string();
        return Ok(HeadPosition::Branch(name));
    }

    let detached = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to read detached HEAD")?;
    let oid = String::from_utf8_lossy(&detached.stdout).trim().to_string();
    Ok(HeadPosition::Detached(oid))
}

/// Best-effort restore of a previously captured HEAD. Used only on a blocked
/// path where no merge has started, so a plain `git checkout` is sufficient.
fn restore_head(repo_path: &Path, head: &HeadPosition) {
    let target = match head {
        HeadPosition::Branch(name) => name,
        HeadPosition::Detached(oid) => oid,
    };
    let _ = Command::new("git").args(["checkout", target]).current_dir(repo_path).output();
}

fn normalize_diff_path(
    path: Option<&Path>,
    context: &str,
    violations: &mut Vec<MergeValidationViolation>,
) -> Option<PathBuf> {
    let path = path?;

    match normalize_repo_relative_path(path, context) {
        Ok(path) => Some(path),
        Err(violation) => {
            violations.push(violation);
            None
        }
    }
}

fn add_stale_reference_violation(
    violations: &mut Vec<MergeValidationViolation>,
    repo: &Repository,
    reference: &str,
    expected: Oid,
) {
    let actual = resolve_branch_oid(repo, reference).ok();
    if actual != Some(expected) {
        violations.push(MergeValidationViolation::StaleReference {
            reference: reference.to_string(),
            expected: expected.to_string(),
            actual: actual.map(|oid| oid.to_string()),
        });
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

        // Resolve a sensible base branch once for all worktrees in this listing.
        let base_branch = resolve_default_branch(&repo);

        for name in &wt_names {
            let Ok(Some(name)) = name else {
                continue;
            };

            let wt_path = self.worktree_base.join(name);
            if !wt_path.exists() {
                continue;
            }

            let branch = Self::branch_name(name);
            let commits_ahead = count_commits_ahead(&repo, &branch, &base_branch).unwrap_or(0);

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
            let Ok(Some(name)) = name else {
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

    fn merge_branch(
        &self,
        sandbox_id: &str,
        base_branch: &str,
        options: &MergeOptions,
    ) -> Result<MergeOutcome> {
        // We shell out to `git merge` because git2's merge API is notoriously
        // difficult to use correctly for real-world merges with conflict detection.
        let branch_name = Self::branch_name(sandbox_id);
        let repo = Repository::open(&self.repo_path)?;

        // An invalid host `[merge.validation]` config was retained at
        // construction so it would not brick unrelated commands. It fails
        // closed here: the merge does not proceed until the config is fixed.
        let policy = match &self.merge_validation {
            MergePolicyState::Compiled(policy) => policy,
            MergePolicyState::Invalid(error) => anyhow::bail!(
                "invalid [merge.validation] host configuration; merge refused until it is fixed: {error}"
            ),
        };

        // No checkout happens before the complete agent diff is validated.
        // A denied result therefore leaves HEAD, MERGE_HEAD, and the working
        // tree exactly as the host user left them.
        let snapshot =
            match Self::validate_merge(&repo, policy, &branch_name, base_branch, options)? {
                Ok(snapshot) => snapshot,
                Err(blocked) => return Ok(MergeOutcome::Blocked(blocked)),
            };

        // The branch can move while an agent is still running or while a host
        // user is reviewing. Recheck both exact OIDs after validation and
        // again immediately before `git merge`; the merge itself uses the
        // reviewed agent commit rather than the mutable branch name.
        if let Some(blocked) = self.snapshot_still_current(base_branch, &branch_name, snapshot)? {
            return Ok(MergeOutcome::Blocked(blocked));
        }

        // Record where HEAD points so a block detected *after* the checkout can
        // restore the host user's original checkout — a blocked result must
        // leave the repository as it was found.
        let original_head = capture_head(&self.repo_path)?;

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

        if let Some(blocked) = self.snapshot_still_current(base_branch, &branch_name, snapshot)? {
            restore_head(&self.repo_path, &original_head);
            return Ok(MergeOutcome::Blocked(blocked));
        }

        // Then merge the reviewed agent commit. Passing the OID rather than
        // the ref prevents a late ref update from changing the content that
        // is integrated after the review-to-merge recheck above.
        let agent_oid = snapshot.agent_oid.to_string();
        let merge_output = std::process::Command::new("git")
            .args(["merge", "--no-ff", &agent_oid, "-m"])
            .arg(format!("Merge {branch_name} into {base_branch}"))
            .current_dir(&self.repo_path)
            .output()
            .context("Failed to run git merge")?;

        if merge_output.status.success() {
            tracing::info!(sandbox_id, base_branch, "Merged branch successfully");
            Ok(MergeOutcome::Merged)
        } else {
            let stdout = String::from_utf8_lossy(&merge_output.stdout);
            let stderr = String::from_utf8_lossy(&merge_output.stderr);
            let combined_output = format!("{stdout}\n{stderr}");
            let conflicts: Vec<String> = combined_output
                .lines()
                .filter(|l| l.contains("CONFLICT"))
                .map(ToString::to_string)
                .collect();

            // Abort the merge so the repo is in a clean state
            let _ = std::process::Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(&self.repo_path)
                .output();

            if conflicts.is_empty() {
                anyhow::bail!(
                    "git merge failed for {branch_name} into {base_branch}: {}",
                    combined_output.trim()
                );
            }

            Ok(MergeOutcome::Conflicts(conflicts))
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

/// Best-effort resolution of a repository's "default" branch for ahead/behind
/// computations. Tries `main` first, then `master`, then falls back to HEAD.
fn resolve_default_branch(repo: &Repository) -> String {
    for candidate in ["main", "master"] {
        if repo.revparse_single(candidate).is_ok() {
            return candidate.to_string();
        }
    }
    "HEAD".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WorkspaceError;
    use tempfile::TempDir;

    /// Helper: create a temporary git repo with an initial commit on `main`.
    ///
    /// Works regardless of the host's `init.defaultBranch` setting by forcing
    /// the initial branch to `main` via `init_opts`.
    fn setup_test_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().to_path_buf();

        let mut init_opts = git2::RepositoryInitOptions::new();
        init_opts.initial_head("main");
        let repo = Repository::init_opts(&repo_path, &init_opts).unwrap();

        // Shell-based merges need a repo-local committer identity in test envs.
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

        // Create an initial commit so `main` exists as a real ref
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

    fn commit_file(repo_path: &Path, file_name: &str, contents: &str, message: &str) {
        commit_bytes(repo_path, file_name, contents.as_bytes(), message);
    }

    fn commit_bytes(repo_path: &Path, file_name: &str, contents: &[u8], message: &str) {
        let path = repo_path.join(file_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();

        let repo = Repository::open(repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(file_name)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head]).unwrap();
    }

    fn commit_rename(repo_path: &Path, old_name: &str, new_name: &str, message: &str) {
        let new_path = repo_path.join(new_name);
        if let Some(parent) = new_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::rename(repo_path.join(old_name), &new_path).unwrap();

        let repo = Repository::open(repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new(old_name)).unwrap();
        index.add_path(Path::new(new_name)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head]).unwrap();
    }

    fn commit_executable(repo_path: &Path, file_name: &str, message: &str) {
        std::fs::write(repo_path.join(file_name), "#!/bin/sh\necho agent\n").unwrap();
        let add = std::process::Command::new("git")
            .args(["add", file_name])
            .current_dir(repo_path)
            .status()
            .unwrap();
        assert!(add.success());
        let chmod = std::process::Command::new("git")
            .args(["update-index", "--chmod=+x", file_name])
            .current_dir(repo_path)
            .status()
            .unwrap();
        assert!(chmod.success());
        let commit = std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(repo_path)
            .status()
            .unwrap();
        assert!(commit.success());
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

    #[test]
    fn test_merge_conflict_returns_conflicts_and_aborts_merge() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");

        let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();

        commit_file(&wt_path, "README.md", "# Agent change\n", "Agent edits README");
        commit_file(&repo_path, "README.md", "# Main change\n", "Main edits README");

        let outcome = ws.merge_branch("task-1", "main", &MergeOptions::default()).unwrap();
        let MergeOutcome::Conflicts(conflicts) = outcome else {
            panic!("expected merge conflicts");
        };

        assert!(!conflicts.is_empty(), "expected merge conflict details");
        assert!(conflicts.iter().any(|line| line.contains("README.md")));
        assert_eq!(
            std::fs::read_to_string(repo_path.join("README.md")).unwrap(),
            "# Main change\n"
        );
        assert!(!repo_path.join(".git").join("MERGE_HEAD").exists(), "merge should be aborted");
    }

    #[test]
    fn test_merge_non_conflict_failure_is_error() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");

        let ws = Git2Workspace::new(&repo_path, &wt_base).unwrap();

        let err = ws.merge_branch("missing-task", "main", &MergeOptions::default()).unwrap_err();

        assert!(err.downcast_ref::<WorkspaceError>().is_none());
        assert!(format!("{err:#}").contains("agent branch 'agent/missing-task' not found"));
        assert!(
            !repo_path.join(".git").join("MERGE_HEAD").exists(),
            "failed merge should not leave state behind"
        );
    }

    #[test]
    fn merge_validation_denies_before_checkout_or_merge() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                deny_patterns: vec![".claude/**".to_string()],
                ..MergeValidationConfig::default()
            },
        )
        .unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        commit_file(&wt_path, ".claude/settings.json", "{\"agent\": true}\n", "Add agent settings");

        let original_head = Repository::open(&repo_path).unwrap().head().unwrap().target();
        let outcome = ws.merge_branch("task-1", "main", &MergeOptions::default()).unwrap();

        assert!(
            matches!(
            outcome,
            MergeOutcome::Blocked(MergeBlocked {
                ref violations,
                }) if matches!(violations.as_slice(), [MergeValidationViolation::DeniedPath { path, pattern }]
                    if path == Path::new(".claude/settings.json") && pattern == ".claude/**")
            ),
            "unexpected outcome: {outcome:?}"
        );
        let repo = Repository::open(&repo_path).unwrap();
        assert_eq!(repo.head().unwrap().target(), original_head, "validation must not checkout");
        assert!(
            !repo_path.join(".git").join("MERGE_HEAD").exists(),
            "validation must not start a merge"
        );
        assert!(!repo_path.join(".claude/settings.json").exists());
    }

    #[test]
    fn merge_validation_requires_each_exact_path_acknowledgement() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                require_review_paths: vec!["Cargo.toml".to_string()],
                ..MergeValidationConfig::default()
            },
        )
        .unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        commit_file(&wt_path, "Cargo.toml", "[package]\nname = \"agent\"\n", "Add package");

        let blocked = ws.merge_branch("task-1", "main", &MergeOptions::default()).unwrap();
        assert!(matches!(
            blocked,
            MergeOutcome::Blocked(MergeBlocked { violations })
                if matches!(violations.as_slice(), [MergeValidationViolation::ReviewRequired { path, .. }]
                    if path == Path::new("Cargo.toml"))
        ));

        let approved = MergeOptions::with_approved_paths(vec![PathBuf::from("Cargo.toml")]);
        let merged = ws.merge_branch("task-1", "main", &approved).unwrap();
        assert_eq!(merged, MergeOutcome::Merged);
        assert!(repo_path.join("Cargo.toml").exists());
    }

    #[test]
    fn merge_validation_checks_both_rename_paths() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");
        commit_file(&repo_path, "sensitive.txt", "review me\n", "Add sensitive file");
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                require_review_paths: vec!["sensitive.txt".to_string()],
                ..MergeValidationConfig::default()
            },
        )
        .unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        commit_rename(&wt_path, "sensitive.txt", "ordinary.txt", "Rename sensitive file");

        let outcome = ws.merge_branch("task-1", "main", &MergeOptions::default()).unwrap();
        assert!(
            matches!(
                outcome,
                MergeOutcome::Blocked(MergeBlocked { ref violations })
                    if violations.iter().any(|violation| matches!(
                        violation,
                        MergeValidationViolation::ReviewRequired { path, .. }
                            if path == Path::new("sensitive.txt")
                    ))
            ),
            "unexpected outcome: {outcome:?}"
        );
    }

    #[test]
    fn merge_validation_blocks_new_executables_and_large_blobs() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                deny_new_executables: true,
                max_file_size_kib: Some(1),
                ..MergeValidationConfig::default()
            },
        )
        .unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        commit_executable(&wt_path, "agent.sh", "Add executable");
        commit_bytes(&wt_path, "large.bin", &vec![b'x'; 1025], "Add large blob");

        let outcome = ws.merge_branch("task-1", "main", &MergeOptions::default()).unwrap();
        assert!(
            matches!(
                outcome,
                MergeOutcome::Blocked(MergeBlocked { ref violations })
                    if violations.iter().any(|violation| matches!(
                        violation,
                        MergeValidationViolation::NewExecutable { path } if path == Path::new("agent.sh")
                    )) && violations.iter().any(|violation| matches!(
                        violation,
                        MergeValidationViolation::FileTooLarge { path, size_bytes: 1025, max_size_kib: 1 }
                            if path == Path::new("large.bin")
                    ))
            ),
            "unexpected outcome: {outcome:?}"
        );
    }

    #[test]
    fn merge_validation_detects_a_ref_that_changed_after_review() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                require_review_paths: vec!["README.md".to_string()],
                ..MergeValidationConfig::default()
            },
        )
        .unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        commit_file(&wt_path, "README.md", "reviewed\n", "Reviewed change");
        let repo = Repository::open(&repo_path).unwrap();
        let policy = MergeValidationPolicy::compile(&MergeValidationConfig {
            require_review_paths: vec!["README.md".to_string()],
            ..MergeValidationConfig::default()
        })
        .unwrap();
        let snapshot = Git2Workspace::validate_merge(
            &repo,
            &policy,
            "agent/task-1",
            "main",
            &MergeOptions::with_approved_paths(vec![PathBuf::from("README.md")]),
        )
        .unwrap()
        .unwrap();
        commit_file(&wt_path, "README.md", "late change\n", "Late change");

        let blocked = ws
            .snapshot_still_current("main", "agent/task-1", snapshot)
            .unwrap()
            .expect("agent ref should have moved");
        assert!(matches!(
            blocked.violations.as_slice(),
            [MergeValidationViolation::StaleReference { reference, .. }]
                if reference == "agent/task-1"
        ));
    }

    #[test]
    fn merge_validation_is_not_bypassed_by_a_shadowing_tag() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                deny_patterns: vec![".claude/**".to_string()],
                ..MergeValidationConfig::default()
            },
        )
        .unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        commit_file(&wt_path, ".claude/backdoor.json", "{\"evil\": true}\n", "Add denied file");

        // Simulate the agent creating a tag named after the base branch that
        // points at its own commit. `revparse_single("main")` would resolve to
        // this tag, but validation must bind to the real branch.
        let tag = std::process::Command::new("git")
            .args(["tag", "main", "agent/task-1"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        assert!(tag.status.success());

        let original_head = Repository::open(&repo_path).unwrap().head().unwrap().target();
        let outcome = ws.merge_branch("task-1", "main", &MergeOptions::default()).unwrap();

        assert!(
            matches!(
                outcome,
                MergeOutcome::Blocked(MergeBlocked { ref violations })
                    if violations.iter().any(|violation| matches!(
                        violation,
                        MergeValidationViolation::DeniedPath { path, .. }
                            if path == Path::new(".claude/backdoor.json")
                    ))
            ),
            "shadowing tag must not bypass validation: {outcome:?}"
        );
        let repo = Repository::open(&repo_path).unwrap();
        assert_eq!(
            repo.find_branch("main", BranchType::Local).unwrap().get().target(),
            original_head,
            "base branch must be untouched"
        );
        assert!(!repo_path.join(".claude/backdoor.json").exists());
    }

    #[test]
    fn invalid_merge_policy_is_deferred_to_merge_and_fails_closed() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");

        // Construction must succeed even with an invalid glob, so unrelated
        // commands that build the workspace are not bricked.
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                deny_patterns: vec!["/etc/**".to_string()],
                ..MergeValidationConfig::default()
            },
        )
        .expect("construction must not fail on invalid merge config");

        // A merge, however, must refuse to proceed.
        let err = ws.merge_branch("task-1", "main", &MergeOptions::default()).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid [merge.validation]"),
            "merge should fail closed on invalid policy: {err:#}"
        );
    }

    #[test]
    fn capture_and_restore_head_round_trips_a_branch() {
        let (_tmp, repo_path) = setup_test_repo();
        // Create and switch to a second branch, then capture it.
        std::process::Command::new("git")
            .args(["checkout", "-b", "feature-x"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        let head = capture_head(&repo_path).unwrap();
        assert!(matches!(&head, HeadPosition::Branch(name) if name == "feature-x"));

        // Move HEAD elsewhere, then restore.
        std::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        restore_head(&repo_path, &head);

        let restored = capture_head(&repo_path).unwrap();
        assert!(
            matches!(restored, HeadPosition::Branch(name) if name == "feature-x"),
            "restore_head must return to the captured branch"
        );
    }

    #[test]
    fn approved_path_clears_executable_and_size_violations() {
        let (tmp, repo_path) = setup_test_repo();
        let wt_base = tmp.path().join("worktrees");
        let ws = Git2Workspace::new_with_merge_validation(
            &repo_path,
            &wt_base,
            &MergeValidationConfig {
                deny_new_executables: true,
                max_file_size_kib: Some(1),
                ..MergeValidationConfig::default()
            },
        )
        .unwrap();
        let wt_path = ws.create_worktree("task-1", "main").unwrap();
        commit_executable(&wt_path, "agent.sh", "Add executable");
        commit_bytes(&wt_path, "large.bin", &vec![b'x'; 2048], "Add large blob");

        let approved = MergeOptions::with_approved_paths(vec![
            PathBuf::from("agent.sh"),
            PathBuf::from("large.bin"),
        ]);
        let outcome = ws.merge_branch("task-1", "main", &approved).unwrap();
        assert_eq!(outcome, MergeOutcome::Merged, "approved exec/size changes should merge");
    }
}
