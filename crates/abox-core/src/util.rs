//! Shared utility functions used across `abox` crates.

use std::path::Path;
use std::time::Duration;

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

/// Sanitize a task ID for use as a branch name and directory name.
///
/// Replaces characters that are invalid in git branch names with hyphens.
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
}
