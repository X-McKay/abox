//! Guest→host transport resolution and connection for the abox shim tools.
//!
//! Two transports exist:
//!
//! - **Unix socket** (`/run/abox-proxy.sock`, the default) — the shim
//!   connects to the persistent `abox-bridge` process, which multiplexes
//!   every exchange over one long-lived vsock uplink to the host command
//!   broker (see `bridge.rs` for why per-process vsock connects are
//!   avoided).
//! - **Direct AF_VSOCK** — the shim connects straight to
//!   `VMADDR_CID_HOST` (CID 2) at the declared port. The runtime routes
//!   that port to a per-sandbox host Unix socket, so sandbox attribution
//!   derives from the host-side route, never from anything the guest
//!   asserts.
//!
//! The transport is declared by host-staged **immutable** configuration:
//! `/etc/abox/transport`, patched into the guest rootfs before boot. The
//! guest cannot escalate by editing it — pointing the shim at a different
//! vsock port only reaches whatever the host routed there (nothing, for
//! unrouted ports).
//!
//! File format (first non-comment line wins):
//!
//! ```text
//! unix:/run/abox-proxy.sock
//! vsock:5000            # AF_VSOCK to CID 2, port 5000
//! ```

use std::io::{Read, Write};

/// Host-staged transport declaration.
pub const TRANSPORT_CONFIG_PATH: &str = "/etc/abox/transport";

/// Default guest Unix socket (served by the persistent `abox-bridge`).
pub const DEFAULT_PROXY_SOCKET: &str = "/run/abox-proxy.sock";

/// vsock CID of the host from a guest's perspective.
#[cfg(target_os = "linux")]
pub const VSOCK_HOST_CID: u32 = 2;

/// A resolved guest→host transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// Connect to a guest-local Unix socket.
    Unix(String),
    /// Connect to the host over AF_VSOCK at this port.
    Vsock(u32),
}

/// A connected duplex stream, whichever transport backs it.
pub enum TransportStream {
    Unix(std::os::unix::net::UnixStream),
    #[cfg(target_os = "linux")]
    Vsock(vsock::VsockStream),
}

impl TransportStream {
    /// Half-close the write side so the peer sees EOF.
    ///
    /// No-op for vsock: the request/response protocol is newline-framed, so
    /// the peer never needs the EOF — and the vsock forwarding path (libkrun
    /// host routing) does not propagate half-close reliably, tearing down
    /// the whole connection before the response arrives.
    pub fn shutdown_write(&self) -> std::io::Result<()> {
        match self {
            Self::Unix(s) => s.shutdown(std::net::Shutdown::Write),
            #[cfg(target_os = "linux")]
            Self::Vsock(_) => Ok(()),
        }
    }
}

impl Read for TransportStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(s) => s.read(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(s) => s.read(buf),
        }
    }
}

impl Write for TransportStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(s) => s.write(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Unix(s) => s.flush(),
            #[cfg(target_os = "linux")]
            Self::Vsock(s) => s.flush(),
        }
    }
}

/// Parse a transport declaration document. Returns `None` when no valid
/// declaration is present.
pub fn parse_transport(content: &str) -> Option<Transport> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(port) = line.strip_prefix("vsock:") {
            if let Ok(port) = port.trim().parse::<u32>() {
                return Some(Transport::Vsock(port));
            }
            return None;
        }
        if let Some(path) = line.strip_prefix("unix:") {
            return Some(Transport::Unix(path.trim().to_string()));
        }
        return None;
    }
    None
}

/// Resolve the transport: host-staged config first, the default guest Unix
/// socket otherwise.
pub fn resolve_transport() -> Transport {
    match std::fs::read_to_string(TRANSPORT_CONFIG_PATH) {
        Ok(content) => parse_transport(&content)
            .unwrap_or_else(|| Transport::Unix(DEFAULT_PROXY_SOCKET.to_string())),
        Err(_) => Transport::Unix(DEFAULT_PROXY_SOCKET.to_string()),
    }
}

/// Connect over the given transport. For `Transport::Vsock`, `port` is the
/// declared broker port unless the caller overrides it.
pub fn connect(transport: &Transport) -> Result<TransportStream, Box<dyn std::error::Error>> {
    match transport {
        Transport::Unix(path) => {
            let stream = std::os::unix::net::UnixStream::connect(path).map_err(|e| {
                format!("failed to connect to proxy at {path}: {e}. Is the proxy daemon running?")
            })?;
            Ok(TransportStream::Unix(stream))
        }
        Transport::Vsock(port) => connect_vsock(*port),
    }
}

/// Connect to the host over AF_VSOCK.
#[cfg(target_os = "linux")]
pub fn connect_vsock(port: u32) -> Result<TransportStream, Box<dyn std::error::Error>> {
    let stream = vsock::VsockStream::connect_with_cid_port(VSOCK_HOST_CID, port)
        .map_err(|e| format!("failed to connect to host vsock port {port}: {e}"))?;
    Ok(TransportStream::Vsock(stream))
}

/// AF_VSOCK is only available inside Linux guests.
#[cfg(not(target_os = "linux"))]
pub fn connect_vsock(port: u32) -> Result<TransportStream, Box<dyn std::error::Error>> {
    Err(format!("vsock transport (port {port}) is only available inside Linux guests").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vsock_declaration() {
        assert_eq!(parse_transport("vsock:5000\n"), Some(Transport::Vsock(5000)));
    }

    #[test]
    fn parse_unix_declaration() {
        assert_eq!(
            parse_transport("unix:/run/abox-proxy.sock\n"),
            Some(Transport::Unix("/run/abox-proxy.sock".to_string()))
        );
    }

    #[test]
    fn parse_skips_comments_and_blank_lines() {
        assert_eq!(
            parse_transport("# transport staged by abox\n\nvsock:5000\n"),
            Some(Transport::Vsock(5000))
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_transport("tcp:127.0.0.1:80\n"), None);
        assert_eq!(parse_transport("vsock:not-a-port\n"), None);
        assert_eq!(parse_transport(""), None);
    }
}
