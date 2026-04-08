//! Stream a Cloud Hypervisor console log file to the orchestrator's stdout.
//!
//! `cloud-hypervisor --console file=<path>` writes the guest's serial
//! console output to a plain file. This module tails that file and writes
//! new bytes to the orchestrator's stdout so the user sees live guest
//! output.
//!
//! The pump exits gracefully when a `Notify` is signalled: it performs one
//! final read-to-EOF before returning so the last ~50 ms of output (the
//! window between the previous poll and the shutdown signal) is not
//! dropped — important on slow systems where the abort path used to lose
//! the guest's poweroff banner.

use crate::config::VmRuntimeTuning;
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Notify;

/// Tail `path`, streaming new bytes to stdout until `shutdown` is notified.
///
/// On shutdown notification, performs a final drain (read-to-EOF) before
/// returning so no bytes are lost.
pub async fn tail_to_stdout_until(path: &Path, shutdown: Arc<Notify>) -> Result<()> {
    let mut stdout = tokio::io::stdout();
    tail_to_writer_until(path, &mut stdout, shutdown).await
}

/// Generic variant used by tests. Tails `path` into any `AsyncWrite` sink.
pub async fn tail_to_writer_until<W>(path: &Path, sink: &mut W, shutdown: Arc<Notify>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    // Wait up to ~5 s for the file to appear.
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
    file.seek(std::io::SeekFrom::Start(0)).await?;

    let mut buf = [0u8; 8192];
    let poll_interval = VmRuntimeTuning::DEFAULT.console_poll_interval;
    loop {
        tokio::select! {
            biased;
            () = shutdown.notified() => {
                // Final drain: read remaining bytes to EOF before returning.
                drain_to_eof(&mut file, sink, &mut buf).await?;
                return Ok(());
            }
            read = file.read(&mut buf) => {
                let n = read?;
                if n == 0 {
                    // EOF: wait briefly and try again — `tail -f`-style poll.
                    tokio::time::sleep(poll_interval).await;
                    continue;
                }
                sink.write_all(&buf[..n]).await?;
                sink.flush().await?;
            }
        }
    }
}

async fn drain_to_eof<R, W>(file: &mut R, sink: &mut W, buf: &mut [u8]) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let n = file.read(buf).await?;
        if n == 0 {
            sink.flush().await?;
            return Ok(());
        }
        sink.write_all(&buf[..n]).await?;
    }
}

/// Legacy wrapper kept for callers that don't need a shutdown signal.
///
/// Calls `tail_to_stdout_until` with a `Notify` that is never triggered,
/// so it behaves exactly like the previous infinite-loop implementation.
/// New call sites should prefer `tail_to_stdout_until` so the pump can
/// drain cleanly on shutdown.
pub async fn tail_to_stdout(path: &Path) -> Result<()> {
    tail_to_stdout_until(path, Arc::new(Notify::new())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[tokio::test]
    async fn test_tail_drains_remaining_bytes_after_shutdown_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("console.log");
        std::fs::write(&log, b"initial ").unwrap();

        let notify = Arc::new(Notify::new());
        let notify_clone = notify.clone();
        let log_path = log.clone();
        let handle = tokio::spawn(async move {
            let mut out = Vec::<u8>::new();
            tail_to_writer_until(&log_path, &mut out, notify_clone).await.unwrap();
            out
        });

        // Let the pump read "initial ", then append "final", then signal shutdown.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(b"final").unwrap();
        drop(f);
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        notify.notify_one();

        let out = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("tail handle didn't finish after shutdown")
            .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("initial"), "missing initial; got: {s:?}");
        assert!(s.contains("final"), "drain lost trailing bytes; got: {s:?}");
    }

    #[tokio::test]
    async fn test_tail_returns_when_signalled_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("console.log");
        std::fs::write(&log, b"hello").unwrap();

        let notify = Arc::new(Notify::new());
        let notify_clone = notify.clone();
        let log_path = log.clone();

        // Signal shutdown before the pump even starts; it should still drain
        // the existing content and return without hanging.
        notify.notify_one();

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async move {
            let mut out = Vec::<u8>::new();
            tail_to_writer_until(&log_path, &mut out, notify_clone).await.unwrap();
            out
        })
        .await
        .expect("tail did not finish within 2s of shutdown signal");

        let s = String::from_utf8(result).unwrap();
        assert_eq!(s, "hello");
    }
}
