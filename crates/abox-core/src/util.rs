//! Shared utility functions used across `abox` crates.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Maximum allowed length for a task ID.
///
/// This is a general sanity cap for user-facing sandbox IDs. The exact
/// effective maximum also depends on `runtime_dir`, because abox embeds the
/// task ID into several Unix socket paths and Linux caps those at 108 bytes.
/// Use [`validate_task_id_for_runtime_dir`] when the runtime directory is
/// known and you need the full safety check.
pub const TASK_ID_MAX_LEN: usize = 64;

/// Maximum length of a Unix-domain socket path on Linux (`SUN_LEN`).
pub const UNIX_SOCKET_PATH_MAX_LEN: usize = 108;

const RUNTIME_SOCKET_NAME_PARTS: &[(&str, &str)] = &[
    ("ch-api-", ".sock"),
    ("vsock-", ".sock"),
    ("vfs-", ".sock"),
    ("vfs-meta-", ".sock"),
    ("vfs-status-", ".sock"),
    ("vsock-", ".sock_5000"),
    ("vsock-", ".sock_5001"),
];

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
/// - Does not exceed [`TASK_ID_MAX_LEN`] characters.
///
/// This validates the **syntax** of a task ID. To also verify that the ID fits
/// inside the runtime socket paths for a particular `runtime_dir`, use
/// [`validate_task_id_for_runtime_dir`].
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
                    "task ID contains invalid character {c:?} at position {i}; \
                     only ASCII letters, digits, hyphens, underscores, and dots are allowed"
                ));
            }
        }
    }
    if id.starts_with('.') || id.ends_with('.') {
        return Err("task ID must not start or end with a dot ('.')".to_string());
    }
    if id.contains("..") {
        return Err("task ID must not contain consecutive dots ('..')".to_string());
    }
    Ok(())
}

/// Return the maximum task ID length supported by a specific runtime dir.
///
/// abox embeds the sandbox ID in several runtime socket names, with the
/// longest currently being `vfs-status-<id>.sock` and `vsock-<id>.sock_5000`.
/// This helper computes how many characters remain for `<id>` once the
/// runtime dir, path separator, and longest static suffix are accounted for.
pub fn max_task_id_len_for_runtime_dir(runtime_dir: &Path) -> usize {
    let runtime_len = runtime_dir.as_os_str().len();
    let longest_overhead = RUNTIME_SOCKET_NAME_PARTS
        .iter()
        .map(|(prefix, suffix)| prefix.len() + suffix.len())
        .max()
        .unwrap_or(0);

    UNIX_SOCKET_PATH_MAX_LEN
        .saturating_sub(runtime_len.saturating_add(1).saturating_add(longest_overhead))
}

fn runtime_socket_paths(task_id: &str, runtime_dir: &Path) -> Vec<PathBuf> {
    RUNTIME_SOCKET_NAME_PARTS
        .iter()
        .map(|(prefix, suffix)| runtime_dir.join(format!("{prefix}{task_id}{suffix}")))
        .collect()
}

/// Validate a task ID against both the syntax rules and the current runtime dir.
///
/// This is the complete task-ID safety check used before abox creates
/// worktrees, runtime sockets, detached-console logs, or per-sandbox bridges.
pub fn validate_task_id_for_runtime_dir(id: &str, runtime_dir: &Path) -> Result<(), String> {
    validate_task_id(id)?;

    let longest_path = runtime_socket_paths(id, runtime_dir)
        .into_iter()
        .max_by_key(|path| path.as_os_str().len())
        .expect("runtime socket path list must not be empty");
    let longest_len = longest_path.as_os_str().len();

    if longest_len > UNIX_SOCKET_PATH_MAX_LEN {
        return Err(format!(
            "task ID is too long for runtime_dir '{}': socket path '{}' would be {} bytes \
             (limit is {}). Use a shorter task ID or a shorter runtime_dir. \
             This runtime_dir supports task IDs up to {} characters.",
            runtime_dir.display(),
            longest_path.display(),
            longest_len,
            UNIX_SOCKET_PATH_MAX_LEN,
            max_task_id_len_for_runtime_dir(runtime_dir)
        ));
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
            "environment variable key {key:?} is invalid: must start with an ASCII letter or \
             underscore (got {first:?})"
        ));
    }
    for (i, c) in key.char_indices().skip(1) {
        if !c.is_ascii_alphanumeric() && c != '_' {
            return Err(format!(
                "environment variable key {key:?} contains invalid character {c:?} at position {i}; \
                 only ASCII letters, digits, and underscores are allowed"
            ));
        }
    }
    Ok(())
}

/// Fill `buf` with cryptographically secure random bytes from the OS CSPRNG.
///
/// Backed by the `getrandom` crate (which uses `getrandom(2)`/`/dev/urandom`
/// on Linux, `getentropy` on macOS, etc.). Returns an error rather than
/// silently falling back to a weak source: callers that need secrecy
/// (PKCE verifiers, generated passwords) must fail loudly if the OS CSPRNG
/// is unavailable instead of emitting guessable output.
pub fn secure_random_bytes(buf: &mut [u8]) -> Result<(), String> {
    getrandom::getrandom(buf).map_err(|e| format!("OS CSPRNG unavailable: {e}"))
}

/// Validate a user-supplied resource name that will be used as a single path
/// component (token names, snapshot names, etc.).
///
/// Rejects empty names, names longer than 64 chars, and anything outside
/// `[A-Za-z0-9._-]`. This blocks path-traversal (`..`, `/`) and absolute-path
/// injection when the name is later joined onto a state directory.
pub fn validate_resource_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name must not be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!("name {name:?} is too long (max 64 characters)"));
    }
    if name == "." || name == ".." {
        return Err(format!("name {name:?} is reserved"));
    }
    for (i, c) in name.char_indices() {
        if !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-') {
            return Err(format!(
                "name {name:?} contains invalid character {c:?} at position {i}; only ASCII \
                 letters, digits, '.', '_', and '-' are allowed"
            ));
        }
    }
    Ok(())
}

/// Sanitize an arbitrary string into a safe single-path-component resource name.
///
/// Replaces every character outside `[A-Za-z0-9._-]` with `-`, collapses the
/// result, and guarantees the output passes [`validate_resource_name`]. Used to
/// derive a default name (e.g. from a hostname) before validation.
pub fn sanitize_resource_name(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    if out.is_empty() || out == "." || out == ".." {
        out = "default".to_string();
    }
    out.truncate(64);
    out
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

/// Percent-decode a URL component once: each `%XX` (two hex digits) becomes the
/// corresponding byte; any `%` not followed by two hex digits is left literal.
/// The result is interpreted as UTF-8 with lossy replacement.
///
/// This decodes `%XX` only — it does **not** treat `+` as a space. That `+`
/// rule is specific to `application/x-www-form-urlencoded` query strings; path
/// matching needs `+` left literal. Callers that want query semantics should
/// replace `+` with a space before calling (see `mcp_oauth::percent_decode`).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode a single ASCII hex digit, or `None` if it is not one.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percent_decode_xx_only() {
        assert_eq!(percent_decode("%61dmin"), "admin");
        assert_eq!(percent_decode("a%2fb"), "a/b");
        // `+` is left literal (path semantics, not query).
        assert_eq!(percent_decode("a+b"), "a+b");
        // Invalid / truncated escapes are left literal.
        assert_eq!(percent_decode("a%xxb"), "a%xxb");
        assert_eq!(percent_decode("trailing%2"), "trailing%2");
        assert_eq!(percent_decode("plain"), "plain");
    }

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

    // ── secure_random_bytes tests ─────────────────────────────────────────

    #[test]
    fn secure_random_bytes_fills_buffer() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        secure_random_bytes(&mut a).unwrap();
        secure_random_bytes(&mut b).unwrap();
        // Two independent draws of 32 bytes should differ with overwhelming
        // probability; a constant result would indicate a broken RNG.
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
    }

    // ── validate_resource_name / sanitize_resource_name tests ─────────────

    #[test]
    fn validate_resource_name_accepts_safe_names() {
        assert!(validate_resource_name("github").is_ok());
        assert!(validate_resource_name("mcp.example.com").is_ok());
        assert!(validate_resource_name("snap_2024-01-01").is_ok());
    }

    #[test]
    fn validate_resource_name_rejects_traversal_and_separators() {
        assert!(validate_resource_name("").is_err());
        assert!(validate_resource_name(".").is_err());
        assert!(validate_resource_name("..").is_err());
        assert!(validate_resource_name("../../etc/passwd").is_err());
        assert!(validate_resource_name("a/b").is_err());
        assert!(validate_resource_name("a\\b").is_err());
        assert!(validate_resource_name("name with space").is_err());
        assert!(validate_resource_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn sanitize_resource_name_produces_valid_names() {
        assert_eq!(sanitize_resource_name("mcp.example.com"), "mcp.example.com");
        assert_eq!(sanitize_resource_name("a/b c"), "a-b-c");
        assert!(validate_resource_name(&sanitize_resource_name("../../weird")).is_ok());
        assert!(validate_resource_name(&sanitize_resource_name("")).is_ok());
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
            assert!(validate_task_id(bad).is_err(), "expected rejection of {bad:?}");
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

    #[test]
    fn validate_task_id_for_runtime_dir_accepts_default_layout_at_max_length() {
        let runtime = Path::new("/home/username/.abox/r");
        let id = "a".repeat(TASK_ID_MAX_LEN);
        assert!(validate_task_id_for_runtime_dir(&id, runtime).is_ok());
    }

    #[test]
    fn validate_task_id_for_runtime_dir_rejects_when_socket_path_would_overflow() {
        let runtime = PathBuf::from("/tmp").join("x".repeat(80));
        let err =
            validate_task_id_for_runtime_dir(&"a".repeat(TASK_ID_MAX_LEN), &runtime).unwrap_err();
        assert!(err.contains("too long for runtime_dir"), "unexpected error: {err}");
    }

    #[test]
    fn validate_task_id_for_runtime_dir_accepts_shorter_id_on_deep_runtime_dir() {
        let runtime = PathBuf::from("/tmp").join("x".repeat(80));
        assert!(validate_task_id_for_runtime_dir("short", &runtime).is_ok());
    }

    #[test]
    fn max_task_id_len_for_runtime_dir_matches_default_budget() {
        let runtime = Path::new("/home/username/.abox/r");
        assert!(max_task_id_len_for_runtime_dir(runtime) >= TASK_ID_MAX_LEN);
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
        let payloads = ["FOO=bar; malicious", "FOO'\nmalicious", "FOO$(cmd)", "FOO`cmd`"];
        for key in &payloads {
            assert!(validate_env_key(key).is_err(), "expected rejection of env key {key:?}");
        }
    }
}
