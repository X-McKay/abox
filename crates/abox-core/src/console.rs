//! Stream a Cloud Hypervisor console log file to the orchestrator's stdout.
//!
//! `cloud-hypervisor --console file=<path>` writes the guest's serial
//! console output to a plain file. This module tails that file (similar
//! to `tail -f`) and writes new bytes to the orchestrator's stdout so
//! the user sees live guest output.
//!
//! The pump returns when the parent orchestrator task is aborted or when
//! reading fails.

use anyhow::{Context, Result};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

/// Tail a console log file and stream newly-appended bytes to stdout.
///
/// Waits up to ~5 seconds for the file to appear (CH may take a moment
/// to create it after boot). Once open, polls for new bytes every 50 ms
/// and writes them out. Returns if the file disappears or reads return
/// persistent errors.
pub async fn tail_to_stdout(path: &Path) -> Result<()> {
    // Wait for the file to appear.
    for _ in 0..200 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    if !path.exists() {
        anyhow::bail!("console log never appeared: {}", path.display());
    }

    let mut file = File::open(path)
        .await
        .with_context(|| format!("opening console log: {}", path.display()))?;
    // Start reading from position 0 so early boot output is shown.
    file.seek(std::io::SeekFrom::Start(0)).await?;

    let mut stdout = tokio::io::stdout();
    let mut buf = [0u8; 8192];

    loop {
        use tokio::io::AsyncReadExt;
        let n = file.read(&mut buf).await?;
        if n == 0 {
            // EOF: wait a bit and try again — this is a `tail -f`-style poll.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        stdout.write_all(&buf[..n]).await?;
        stdout.flush().await?;
    }
}
