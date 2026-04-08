//! Stream a Cloud Hypervisor console Unix socket to the orchestrator's
//! standard streams.
//!
//! `cloud-hypervisor --console socket=<path>` exposes the guest serial
//! console as a Unix socket on the host. To give the user live agent
//! output (and the ability to type into the guest), `abox run` connects
//! to that socket and pumps bytes between it and the orchestrator's
//! stdio. The pump returns when the socket closes — i.e. when the VM
//! powers off.

use anyhow::Result;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Connect to a console Unix socket and pump bytes between it and the
/// orchestrator's stdin/stdout. Returns when either direction closes.
///
/// This function is intentionally unbuffered on the read side so output
/// reaches the user immediately.
pub async fn stream_to_stdio(socket_path: &Path) -> Result<()> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut sock_r, mut sock_w) = stream.into_split();

    let mut so = tokio::io::stdout();
    let mut si = tokio::io::stdin();

    let from_guest = async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = sock_r.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            so.write_all(&buf[..n]).await?;
            so.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    };

    let to_guest = async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = si.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            sock_w.write_all(&buf[..n]).await?;
            sock_w.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        r = from_guest => r?,
        r = to_guest => r?,
    }
    Ok(())
}
