//! Typed error types for `abox-core`.
//!
//! Uses `thiserror` to provide structured, matchable errors instead of
//! opaque `anyhow::Error`. The CLI and proxy crates can convert these into
//! user-facing messages or appropriate exit codes.

use std::path::PathBuf;
use thiserror::Error;

/// Errors from workspace (git worktree) operations.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("worktree '{task_id}' already exists at {path}")]
    AlreadyExists { task_id: String, path: PathBuf },

    #[error("worktree '{task_id}' not found")]
    NotFound { task_id: String },

    #[error("branch 'agent/{task_id}' already exists")]
    BranchExists { task_id: String },

    #[error("base branch '{branch}' does not exist")]
    BaseBranchNotFound { branch: String },

    #[error("merge conflict in {count} file(s)")]
    MergeConflict { count: usize, files: Vec<String> },

    #[error("git operation failed: {0}")]
    Git(#[from] git2::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors from policy evaluation.
#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("invalid regex in policy for '{command}': {source}")]
    InvalidRegex { command: String, source: regex::Error },

    #[error("failed to parse policy file: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("IO error reading policy: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors from configuration loading.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse config: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("config file not readable: {0}")]
    Io(#[from] std::io::Error),

    #[error("home directory not found")]
    NoHomeDir,
}
