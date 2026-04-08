//! Workspace management port (trait).
//!
//! Defines the domain interface for git worktree operations. The adapter
//! implementation lives in `adapters::git2_workspace`.

use std::path::PathBuf;

/// The status of a file in a worktree relative to the base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed(String),
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added => write!(f, "Added"),
            Self::Modified => write!(f, "Modified"),
            Self::Deleted => write!(f, "Deleted"),
            Self::Renamed(old) => write!(f, "Renamed({old})"),
        }
    }
}

/// A single entry in the divergence report.
#[derive(Debug, Clone)]
pub struct DivergenceEntry {
    /// The file path relative to the repository root.
    pub file_path: String,
    /// The sandbox ID that modified this file.
    pub sandbox_id: String,
    /// How the file was changed.
    pub status: FileStatus,
}

/// Information about a worktree.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// The sandbox ID (also the worktree name).
    pub sandbox_id: String,
    /// The branch name (e.g., `agent/fix-auth`).
    pub branch: String,
    /// The absolute path to the worktree on disk.
    pub path: PathBuf,
    /// Number of commits ahead of the base branch.
    pub commits_ahead: usize,
}

/// Port (trait) for workspace management. Decoupled from any git implementation.
pub trait WorkspacePort: Send + Sync {
    /// Create a new worktree on a new branch forked from `base_branch`.
    /// Returns the absolute path to the worktree directory.
    fn create_worktree(&self, sandbox_id: &str, base_branch: &str) -> anyhow::Result<PathBuf>;

    /// Remove a worktree and optionally delete its branch.
    fn remove_worktree(&self, sandbox_id: &str, delete_branch: bool) -> anyhow::Result<()>;

    /// List all active worktrees managed by abox.
    fn list_worktrees(&self) -> anyhow::Result<Vec<WorktreeInfo>>;

    /// Compute the divergence matrix: which files are changed in which worktrees
    /// relative to the base branch.
    fn compute_divergence(&self, base_branch: &str) -> anyhow::Result<Vec<DivergenceEntry>>;

    /// Merge a sandbox's branch back into the base branch.
    /// Returns a list of conflict descriptions, if any.
    fn merge_branch(&self, sandbox_id: &str, base_branch: &str) -> anyhow::Result<Vec<String>>;
}
