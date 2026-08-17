//! Workspace management port (trait).
//!
//! Defines the domain interface for git worktree operations. The adapter
//! implementation lives in `adapters::git2_workspace`.

use crate::config::MergeValidationConfig;
use anyhow::{bail, Context, Result};
use globset::{GlobBuilder, GlobMatcher};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

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

/// Caller acknowledgements for a merge operation.
///
/// Each path is checked by the workspace adapter as a repository-relative,
/// exact path. It is intentionally not a glob or a broad bypass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOptions {
    /// Paths explicitly reviewed by the host user.
    pub approved_paths: Vec<PathBuf>,
}

impl MergeOptions {
    /// Construct merge options with the supplied exact-path acknowledgements.
    pub fn with_approved_paths(approved_paths: Vec<PathBuf>) -> Self {
        Self { approved_paths }
    }
}

/// Result of attempting a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The agent branch was merged into the requested base branch.
    Merged,
    /// Git detected conflicts and the adapter aborted the merge.
    Conflicts(Vec<String>),
    /// Host-owned validation rejected the incoming change before merge.
    Blocked(MergeBlocked),
}

/// A structured explanation for a merge that was blocked before integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeBlocked {
    /// Every rule violation found during validation.
    pub violations: Vec<MergeValidationViolation>,
}

impl MergeBlocked {
    /// Construct a blocked result from one or more validation violations.
    pub fn new(violations: Vec<MergeValidationViolation>) -> Self {
        Self { violations }
    }
}

/// A specific reason a host-owned merge validation rule blocked integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeValidationViolation {
    /// The changed path matched a denied glob.
    DeniedPath { path: PathBuf, pattern: String },
    /// The path matched a review-required glob but was not explicitly
    /// acknowledged with `--approve-path`.
    ReviewRequired { path: PathBuf, pattern: String },
    /// A path changed from a non-executable mode to an executable mode.
    NewExecutable { path: PathBuf },
    /// An incoming blob exceeded the configured size limit.
    FileTooLarge { path: PathBuf, size_bytes: u64, max_size_kib: u64 },
    /// Validation could not inspect an incoming blob needed for a configured
    /// size check, so it failed closed.
    UninspectableBlob { path: PathBuf, context: String },
    /// A Git path or caller-provided approval could not be represented as a
    /// safe repository-relative UTF-8 path, so validation failed closed.
    UnrepresentablePath { path: PathBuf, context: String },
    /// A reference changed after validation and before merge.
    StaleReference { reference: String, expected: String, actual: Option<String> },
    /// The sandbox is still running and can mutate its agent branch.
    ActiveSandbox { task_id: String },
}

impl std::fmt::Display for MergeValidationViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeniedPath { path, pattern } => {
                write!(f, "{} matches denied pattern {pattern:?}", path.display())
            }
            Self::ReviewRequired { path, pattern } => write!(
                f,
                "{} matches review-required pattern {pattern:?}; rerun with --approve-path {} after review",
                path.display(),
                path.display()
            ),
            Self::NewExecutable { path } => {
                write!(f, "{} is newly executable", path.display())
            }
            Self::FileTooLarge { path, size_bytes, max_size_kib } => write!(
                f,
                "{} is {size_bytes} bytes, exceeding the {max_size_kib} KiB limit",
                path.display()
            ),
            Self::UninspectableBlob { path, context } => {
                write!(f, "could not inspect incoming blob {} ({context})", path.display())
            }
            Self::UnrepresentablePath { path, context } => {
                write!(f, "{} is not a safe repository-relative UTF-8 path ({context})", path.display())
            }
            Self::StaleReference { reference, expected, actual } => match actual {
                Some(actual) => write!(
                    f,
                    "{reference} changed during merge validation (expected {expected}, found {actual})"
                ),
                None => write!(
                    f,
                    "{reference} disappeared during merge validation (expected {expected})"
                ),
            },
            Self::ActiveSandbox { task_id } => {
                write!(f, "sandbox {task_id:?} is still active; stop it before merging")
            }
        }
    }
}

/// Host-owned, precompiled merge validation rules.
#[derive(Debug, Clone, Default)]
pub struct MergeValidationPolicy {
    deny_patterns: Vec<CompiledGlob>,
    require_review_paths: Vec<CompiledGlob>,
    deny_new_executables: bool,
    max_file_size_kib: Option<u64>,
}

#[derive(Debug, Clone)]
struct CompiledGlob {
    source: String,
    matcher: GlobMatcher,
}

impl MergeValidationPolicy {
    /// Compile and validate host-owned configuration once, before a merge can
    /// use it. Globs must be repository-relative and use `/` separators.
    pub fn compile(config: &MergeValidationConfig) -> Result<Self> {
        Ok(Self {
            deny_patterns: compile_globs("merge.validation.deny_patterns", &config.deny_patterns)?,
            require_review_paths: compile_globs(
                "merge.validation.require_review_paths",
                &config.require_review_paths,
            )?,
            deny_new_executables: config.deny_new_executables,
            max_file_size_kib: config.max_file_size_kib,
        })
    }

    /// Return whether no validation rule is configured.
    pub fn is_empty(&self) -> bool {
        self.deny_patterns.is_empty()
            && self.require_review_paths.is_empty()
            && !self.deny_new_executables
            && self.max_file_size_kib.is_none()
    }

    /// Return the denied glob matching `path`, if any.
    pub fn denied_pattern(&self, path: &Path) -> Option<&str> {
        self.deny_patterns
            .iter()
            .find(|glob| glob.matcher.is_match(path))
            .map(|glob| glob.source.as_str())
    }

    /// Return the review-required glob matching `path`, if any.
    pub fn review_pattern(&self, path: &Path) -> Option<&str> {
        self.require_review_paths
            .iter()
            .find(|glob| glob.matcher.is_match(path))
            .map(|glob| glob.source.as_str())
    }

    /// Return whether a mode transition to executable must be denied.
    pub fn denies_new_executables(&self) -> bool {
        self.deny_new_executables
    }

    /// Return the maximum allowed incoming blob size in KiB.
    pub fn max_file_size_kib(&self) -> Option<u64> {
        self.max_file_size_kib
    }
}

/// Normalize a user acknowledgement or Git path for exact path comparison.
///
/// Validation intentionally rejects paths that cannot safely take part in a
/// host policy decision instead of attempting a lossy conversion.
pub fn normalize_repo_relative_path(
    path: &Path,
    context: &str,
) -> std::result::Result<PathBuf, MergeValidationViolation> {
    if path.as_os_str().is_empty() || path.is_absolute() || path.to_str().is_none() {
        return Err(MergeValidationViolation::UnrepresentablePath {
            path: path.to_path_buf(),
            context: context.to_string(),
        });
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(MergeValidationViolation::UnrepresentablePath {
                    path: path.to_path_buf(),
                    context: context.to_string(),
                });
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(MergeValidationViolation::UnrepresentablePath {
            path: path.to_path_buf(),
            context: context.to_string(),
        });
    }

    Ok(normalized)
}

/// Normalize exact caller acknowledgements and return a deduplicated set.
pub fn approved_path_set(
    options: &MergeOptions,
) -> std::result::Result<BTreeSet<PathBuf>, MergeBlocked> {
    options
        .approved_paths
        .iter()
        .map(|path| normalize_repo_relative_path(path, "approval path"))
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(|violation| MergeBlocked::new(vec![violation]))
}

fn compile_globs(section: &str, patterns: &[String]) -> Result<Vec<CompiledGlob>> {
    patterns
        .iter()
        .map(|pattern| {
            validate_repo_relative_glob(pattern)
                .with_context(|| format!("invalid {section} entry {pattern:?}"))?;
            let matcher = GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .with_context(|| format!("invalid {section} entry {pattern:?}"))?
                .compile_matcher();
            Ok(CompiledGlob { source: pattern.clone(), matcher })
        })
        .collect()
}

fn validate_repo_relative_glob(pattern: &str) -> Result<()> {
    if pattern.is_empty() || pattern.starts_with('/') || pattern.contains('\\') {
        bail!("glob must be a non-empty repository-relative path using '/' separators");
    }

    if Path::new(pattern).components().any(|component| {
        matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
    }) {
        bail!("glob must not contain an absolute or parent-directory component");
    }

    Ok(())
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

    /// Merge a sandbox's branch back into the base branch after evaluating the
    /// host-owned merge policy. Validation must occur before checkout or
    /// merge so a blocked result leaves the repository untouched.
    fn merge_branch(
        &self,
        sandbox_id: &str,
        base_branch: &str,
        options: &MergeOptions,
    ) -> anyhow::Result<MergeOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_globs_are_relative_and_match_path_components() {
        let policy = MergeValidationPolicy::compile(&MergeValidationConfig {
            deny_patterns: vec![".github/**".to_string()],
            require_review_paths: vec!["Cargo.toml".to_string()],
            ..MergeValidationConfig::default()
        })
        .unwrap();

        assert_eq!(
            policy.denied_pattern(Path::new(".github/workflows/ci.yml")),
            Some(".github/**")
        );
        assert_eq!(policy.denied_pattern(Path::new("other/.github/ci.yml")), None);
        assert_eq!(policy.review_pattern(Path::new("Cargo.toml")), Some("Cargo.toml"));
    }

    #[test]
    fn invalid_globs_fail_before_merge() {
        for pattern in ["", "/etc/passwd", "../Cargo.toml", "dir\\file"] {
            let error = MergeValidationPolicy::compile(&MergeValidationConfig {
                deny_patterns: vec![pattern.to_string()],
                ..MergeValidationConfig::default()
            })
            .unwrap_err();
            assert!(format!("{error:#}").contains("invalid merge.validation.deny_patterns"));
        }
    }

    #[test]
    fn approval_paths_are_exact_and_cannot_escape_the_repo() {
        let approved = approved_path_set(&MergeOptions::with_approved_paths(vec![
            PathBuf::from("Cargo.toml"),
            PathBuf::from("Cargo.toml"),
        ]))
        .unwrap();
        assert_eq!(approved, BTreeSet::from([PathBuf::from("Cargo.toml")]));

        let blocked = approved_path_set(&MergeOptions::with_approved_paths(vec![PathBuf::from(
            "../Cargo.toml",
        )]))
        .unwrap_err();
        assert!(matches!(
            blocked.violations.as_slice(),
            [MergeValidationViolation::UnrepresentablePath { .. }]
        ));
    }
}
