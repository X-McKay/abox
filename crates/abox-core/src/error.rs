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

/// Errors from VM lifecycle operations.
#[derive(Debug, Error)]
pub enum VmError {
    #[error("VM '{id}' not found")]
    NotFound { id: String },

    #[error("VM '{id}' is in state '{state}', expected '{expected}'")]
    InvalidState { id: String, state: String, expected: String },

    #[error("failed to start VM '{id}': {reason}")]
    StartFailed { id: String, reason: String },

    #[error("failed to stop VM '{id}': {reason}")]
    StopFailed { id: String, reason: String },

    #[error("Cloud Hypervisor API error: {0}")]
    ApiError(String),

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

/// Errors from snapshot/template operations.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("template '{name}' not found")]
    NotFound { name: String },

    #[error("template '{name}' already exists")]
    AlreadyExists { name: String },

    #[error("Cloud Hypervisor snapshot API failed: {0}")]
    ApiError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
