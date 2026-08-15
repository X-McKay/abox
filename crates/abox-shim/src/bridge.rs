//! `abox-bridge` — Guest-side TCP/Unix↔vsock forwarder.
//!
//! Listens on guest loopback TCP ports and/or guest Unix sockets and
//! forwards each connection to the host over AF_VSOCK (CID 2):
//!
//! - the command broker (`/run/abox-proxy.sock` → vsock 5000),
//! - the HTTPS egress proxy (`127.0.0.1:18443` → vsock 5001), and
//! - service sidecar ports (`127.0.0.1:<port>` → vsock 51xx).
//!
//! Usage: `abox-bridge <listen> <vsock_port> [<listen> <vsock_port> ...]`
//! where `<listen>` is a TCP port number or an absolute Unix socket path.
//!
//! The command broker deliberately rides this long-lived process rather
//! than having each short-lived shim invocation open AF_VSOCK itself: the
//! current MicroSandbox VMM loses host→guest delivery for vsock
//! connections opened by fresh guest processes after the first one, while
//! connections from a persistent process work reliably. Attribution is
//! unaffected — it derives from the per-sandbox host socket the route
//! points at.
//!
//! Synchronous, thread-per-connection, no dependencies beyond the `vsock`
//! crate — it ships in the same static musl binary set as `abox-shim`.

// The transport module is shared with `abox-shim`; each binary uses a
// different subset of it.
#[allow(dead_code)]
mod transport;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::ExitCode;

/// One listen endpoint for the bridge.
enum Listen {
    Tcp(u16),
    Unix(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || !args.len().is_multiple_of(2) {
        eprintln!(
            "usage: abox-bridge <listen: tcp-port|unix-path> <vsock_port>              [<listen> <vsock_port> ...]"
        );
        return ExitCode::from(2);
    }

    let mut pairs = Vec::new();
    for chunk in args.chunks(2) {
        let listen = if chunk[0].starts_with('/') {
            Listen::Unix(chunk[0].clone())
        } else if let Ok(port) = chunk[0].parse::<u16>() {
            Listen::Tcp(port)
        } else {
            eprintln!("abox-bridge: invalid listen endpoint {:?}", chunk[0]);
            return ExitCode::from(2);
        };
        let Ok(vsock_port) = chunk[1].parse::<u32>() else {
            eprintln!("abox-bridge: invalid vsock port {:?}", chunk[1]);
            return ExitCode::from(2);
        };
        pairs.push((listen, vsock_port));
    }

    let mut handles = Vec::new();
    for (listen, vsock_port) in pairs {
        handles.push(std::thread::spawn(move || match listen {
            Listen::Tcp(port) => serve(port, vsock_port),
            Listen::Unix(path) => serve_unix(&path, vsock_port),
        }));
    }
    for handle in handles {
        if let Err(e) = handle.join().unwrap_or_else(|_| Err("bridge thread panicked".into())) {
            eprintln!("abox-bridge: {e}");
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

/// Accept loop for one unix-socket → vsock-port pair.
fn serve_unix(path: &str, vsock_port: u32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path).map_err(|e| format!("failed to bind {path}: {e}"))?;
    // The agent runs as uid 1000; let it (and only it) connect.
    let _ = std::os::unix::fs::chown(path, Some(1000), Some(1000));
    let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    loop {
        let (client, _addr) = match listener.accept() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("abox-bridge: accept error on {path}: {e}");
                continue;
            }
        };
        std::thread::spawn(move || {
            if let Err(e) = forward_unix(client, vsock_port) {
                eprintln!("abox-bridge: forward error (unix → vsock:{vsock_port}): {e}");
            }
        });
    }
}

/// Splice one Unix connection to a fresh vsock connection, both directions.
fn forward_unix(client: UnixStream, vsock_port: u32) -> Result<(), Box<dyn std::error::Error>> {
    let upstream = transport::connect_vsock(vsock_port)?;
    let (mut client_read, mut client_write) = (client.try_clone()?, client);
    let (mut upstream_read, mut upstream_write) = split_stream(upstream)?;

    let up = std::thread::spawn(move || {
        let _ = copy_then_shutdown(&mut client_read, &mut upstream_write);
    });
    let _ = copy_then_shutdown(&mut upstream_read, &mut client_write);
    let _ = up.join();
    Ok(())
}

/// Accept loop for one listen-port → vsock-port pair.
fn serve(
    listen_port: u16,
    vsock_port: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(("127.0.0.1", listen_port))
        .map_err(|e| format!("failed to bind 127.0.0.1:{listen_port}: {e}"))?;
    loop {
        let (client, _addr) = match listener.accept() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("abox-bridge: accept error on port {listen_port}: {e}");
                continue;
            }
        };
        std::thread::spawn(move || {
            if let Err(e) = forward(client, vsock_port) {
                eprintln!("abox-bridge: forward error (:{listen_port} → vsock:{vsock_port}): {e}");
            }
        });
    }
}

/// Splice one TCP connection to a fresh vsock connection, both directions.
fn forward(client: TcpStream, vsock_port: u32) -> Result<(), Box<dyn std::error::Error>> {
    let upstream = transport::connect_vsock(vsock_port)?;
    let (mut client_read, mut client_write) = (client.try_clone()?, client);
    let (mut upstream_read, mut upstream_write) = split_stream(upstream)?;

    let up = std::thread::spawn(move || {
        let _ = copy_then_shutdown(&mut client_read, &mut upstream_write);
    });
    let _ = copy_then_shutdown(&mut upstream_read, &mut client_write);
    let _ = up.join();
    Ok(())
}

/// Duplicate a transport stream into independent read/write halves.
fn split_stream(
    stream: transport::TransportStream,
) -> Result<(transport::TransportStream, transport::TransportStream), Box<dyn std::error::Error>> {
    match stream {
        transport::TransportStream::Unix(s) => {
            let clone = s.try_clone()?;
            Ok((transport::TransportStream::Unix(s), transport::TransportStream::Unix(clone)))
        }
        #[cfg(target_os = "linux")]
        transport::TransportStream::Vsock(s) => {
            let clone = s.try_clone()?;
            Ok((transport::TransportStream::Vsock(s), transport::TransportStream::Vsock(clone)))
        }
    }
}

/// Copy until EOF, then propagate the EOF with a write-half shutdown.
fn copy_then_shutdown<R: Read, W: Write + ShutdownWrite>(
    reader: &mut R,
    writer: &mut W,
) -> std::io::Result<u64> {
    let copied = std::io::copy(reader, writer);
    let _ = writer.shutdown_write();
    copied
}

/// Write-half shutdown, abstracted over stream kinds.
trait ShutdownWrite {
    fn shutdown_write(&self) -> std::io::Result<()>;
}

impl ShutdownWrite for TcpStream {
    fn shutdown_write(&self) -> std::io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

impl ShutdownWrite for UnixStream {
    fn shutdown_write(&self) -> std::io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

impl ShutdownWrite for transport::TransportStream {
    fn shutdown_write(&self) -> std::io::Result<()> {
        transport::TransportStream::shutdown_write(self)
    }
}
