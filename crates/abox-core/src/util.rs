//! Shared utility functions used across `abox` crates.

use std::path::Path;
use std::time::Duration;

/// Maximum allowed length for a task ID.
///
/// Chosen to keep the longest virtiofsd socket path (which embeds the task ID)
/// well within the 108-byte Unix socket path limit even for deeply nested
/// runtime directories. See `CloudHypervisorAdapter::LONGEST_SOCKET_SUFFIX`.
pub const TASK_ID_MAX_LEN: usize = 64;

/// Format a byte count into a human-readable string (KiB, MiB, GiB).
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    }
}

/// Wait for a Unix socket to appear on the filesystem, polling at intervals.
///
/// Returns `Ok(())` when the socket exists, or `Err` if the timeout expires.
pub async fn wait_for_socket(
    path: &Path,
    timeout: Duration,
    poll_interval: Duration,
) -> std::io::Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return Ok(());
        }
        tokio::time::sleep(poll_interval).await;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("socket {} did not appear within {:?}", path.display(), timeout),
    ))
}

/// Validate a task ID and return an error if it is not safe to use.
///
/// A valid task ID:
/// - Is non-empty.
/// - Contains only ASCII alphanumeric characters, hyphens (`-`), underscores
///   (`_`), and dots (`.`).
/// - Does not start or end with a dot (to avoid hidden-file and git-ref
///   ambiguity).
/// - Does not contain consecutive dots (`..`) which would create an ambiguous
///   git ref.
/// - Does not exceed [`TASK_ID_MAX_LEN`] characters (to keep socket paths
///   within the 108-byte Unix limit).
///
/// This function is the **single enforcement point** for task ID safety.
/// Call it at the CLI boundary (in `run.rs`) before the ID is forwarded to
/// any internal subsystem (workspace adapter, VM adapter, runtime paths).
pub fn validate_task_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("task ID must not be empty".to_string());
    }
    if id.len() > TASK_ID_MAX_LEN {
        return Err(format!(
            "task ID is too long ({} chars); maximum is {} characters",
            id.len(),
            TASK_ID_MAX_LEN
        ));
    }
    for (i, c) in id.char_indices() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => {}
            _ => {
                return Err(format!(
                    "task ID contains invalid character {:?} at position {i}; \
                     only ASCII letters, digits, hyphens, underscores, and dots are allowed",
                    c
                ));
            }
        }
    }
    if id.starts_with('.') || id.ends_with('.') {
        return Err(
            "task ID must not start or end with a dot ('.')".to_string()
        );
    }
    if id.contains("..") {
        return Err(
            "task ID must not contain consecutive dots ('..')".to_string()
        );
    }
    Ok(())
}

/// Validate an environment variable key and return an error if it is not safe.
///
/// A valid key conforms to the POSIX shell identifier grammar:
/// - Starts with an ASCII letter or underscore.
/// - Contains only ASCII letters, digits, or underscores.
///
/// This is enforced at the CLI boundary so that unvalidated keys can never
/// reach `BootMeta::runner_script()`, which interpolates them directly into
/// shell `export` statements executed as root inside the guest.
pub fn validate_env_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("environment variable key must not be empty".to_string());
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(format!(
            "environment variable key {:?} is invalid: must start with an ASCII letter or \
             underscore (got {:?})",
            key, first
        ));
    }
    for (i, c) in key.char_indices().skip(1) {
        if !c.is_ascii_alphanumeric() && c != '_' {
            return Err(format!(
                "environment variable key {:?} contains invalid character {:?} at position {i}; \
                 only ASCII letters, digits, and underscores are allowed",
                key, c
            ));
        }
    }
    Ok(())
}

/// Sanitize a task ID for use as a branch name and directory name.
///
/// Replaces characters that are invalid in git branch names with hyphens.
/// Prefer [`validate_task_id`] at trust boundaries; this function is kept for
/// internal use where a best-effort normalization is acceptable (e.g., display
/// or legacy paths).
pub fn sanitize_task_id(id: &str) -> String {
    id.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '-',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn test_format_size_kib() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
    }

    #[test]
    fn test_format_size_mib() {
        assert_eq!(format_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_size(2 * 1024 * 1024 + 512 * 1024), "2.5 MiB");
    }

    #[test]
    fn test_format_size_gib() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
    }

    #[test]
    fn test_sanitize_task_id() {
        assert_eq!(sanitize_task_id("fix-auth"), "fix-auth");
        assert_eq!(sanitize_task_id("feature/new thing"), "feature-new-thing");
        assert_eq!(sanitize_task_id("task@123!"), "task-123-");
    }

    #[test]
    fn test_sanitize_preserves_valid_chars() {
        assert_eq!(sanitize_task_id("My.Task_v2"), "My.Task_v2");
    }

    // ── validate_task_id tests ────────────────────────────────────────────

    #[test]
    fn validate_task_id_accepts_valid_ids() {
        assert!(validate_task_id("fix-auth").is_ok());
        assert!(validate_task_id("feature_v2").is_ok());
        assert!(validate_task_id("task.123").is_ok());
        assert!(validate_task_id("A").is_ok());
        assert!(validate_task_id("abc-def_ghi.jkl").is_ok());
    }

    #[test]
    fn validate_task_id_rejects_empty() {
        assert!(validate_task_id("").is_err());
    }

    #[test]
    fn validate_task_id_rejects_too_long() {
        let long = "a".repeat(TASK_ID_MAX_LEN + 1);
        let err = validate_task_id(&long).unwrap_err();
        assert!(err.contains("too long"), "unexpected error: {err}");
    }

    #[test]
    fn validate_task_id_accepts_max_length() {
        let ok = "a".repeat(TASK_ID_MAX_LEN);
        assert!(validate_task_id(&ok).is_ok());
    }

    #[test]
    fn validate_task_id_rejects_slash() {
        let err = validate_task_id("feature/new").unwrap_err();
        assert!(err.contains("invalid character"), "unexpected error: {err}");
    }

    #[test]
    fn validate_task_id_rejects_space() {
        let err = validate_task_id("my task").unwrap_err();
        assert!(err.contains("invalid character"), "unexpected error: {err}");
    }

    #[test]
    fn validate_task_id_rejects_shell_metacharacters() {
        for bad in &["task;evil", "task$(cmd)", "task`cmd`", "task&bg", "task|pipe"] {
            assert!(
                validate_task_id(bad).is_err(),
                "expected rejection of {:?}",
                bad
            );
        }
    }

    #[test]
    fn validate_task_id_rejects_leading_dot() {
        let err = validate_task_id(".hidden").unwrap_err();
        assert!(err.contains("start or end with a dot"), "unexpected error: {err}");
    }

    #[test]
    fn validate_task_id_rejects_trailing_dot() {
        let err = validate_task_id("task.").unwrap_err();
        assert!(err.contains("start or end with a dot"), "unexpected error: {err}");
    }

    #[test]
    fn validate_task_id_rejects_double_dot() {
        let err = validate_task_id("task..evil").unwrap_err();
        assert!(err.contains("consecutive dots"), "unexpected error: {err}");
    }

    #[test]
    fn validate_task_id_rejects_path_traversal() {
        assert!(validate_task_id("../../etc/passwd").is_err());
    }

    // ── validate_env_key tests ────────────────────────────────────────────

    #[test]
    fn validate_env_key_accepts_valid_keys() {
        assert!(validate_env_key("FOO").is_ok());
        assert!(validate_env_key("FOO_BAR").is_ok());
        assert!(validate_env_key("_PRIVATE").is_ok());
        assert!(validate_env_key("A1").is_ok());
        assert!(validate_env_key("MY_VAR_123").is_ok());
    }

    #[test]
    fn validate_env_key_rejects_empty() {
        assert!(validate_env_key("").is_err());
    }

    #[test]
    fn validate_env_key_rejects_digit_start() {
        let err = validate_env_key("1FOO").unwrap_err();
        assert!(err.contains("must start with"), "unexpected error: {err}");
    }

    #[test]
    fn validate_env_key_rejects_hyphen() {
        let err = validate_env_key("FOO-BAR").unwrap_err();
        assert!(err.contains("invalid character"), "unexpected error: {err}");
    }

    #[test]
    fn validate_env_key_rejects_space() {
        let err = validate_env_key("FOO BAR").unwrap_err();
        assert!(err.contains("invalid character"), "unexpected error: {err}");
    }

    #[test]
    fn validate_env_key_rejects_shell_injection_payload() {
        // A crafted key that would break `export <key>='value'` syntax.
        let payloads = [
            "FOO=bar; malicious",
            "FOO'\nmalicious",
            "FOO$(cmd)",
            "FOO`cmd`",
        ];
        for key in &payloads {
            assert!(
                validate_env_key(key).is_err(),
                "expected rejection of env key {:?}",
                key
            );
        }
    }
}
