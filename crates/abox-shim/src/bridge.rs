//! `abox-bridge` — Guest-side TCP↔vsock forwarder.
//!
//! Listens on a guest loopback TCP port and forwards each connection to the
//! host over AF_VSOCK (CID 2). Under the MicroSandbox runtime this replaces
//! the `socat` bridges the legacy guest init script used for:
//!
//! - the HTTPS egress proxy (`127.0.0.1:18443` → vsock 5001), and
//! - service sidecar ports (`127.0.0.1:<port>` → vsock 51xx).
//!
//! Usage: `abox-bridge <listen_port> <vsock_port> [<listen_port> <vsock_port> ...]`
//!
//! Synchronous, thread-per-connection, no dependencies beyond the `vsock`
//! crate — it ships in the same static musl binary set as `abox-shim`.

// The transport module is shared with `abox-shim`; each binary uses a
// different subset of it.
#[allow(dead_code)]
mod transport;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || !args.len().is_multiple_of(2) {
        eprintln!("usage: abox-bridge <listen_port> <vsock_port> [<listen_port> <vsock_port> ...]");
        return ExitCode::from(2);
    }

    let mut pairs = Vec::new();
    for chunk in args.chunks(2) {
        let Ok(listen) = chunk[0].parse::<u16>() else {
            eprintln!("abox-bridge: invalid listen port {:?}", chunk[0]);
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
        handles.push(std::thread::spawn(move || serve(listen, vsock_port)));
    }
    for handle in handles {
        if let Err(e) = handle.join().unwrap_or_else(|_| Err("bridge thread panicked".into())) {
            eprintln!("abox-bridge: {e}");
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
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

impl ShutdownWrite for transport::TransportStream {
    fn shutdown_write(&self) -> std::io::Result<()> {
        transport::TransportStream::shutdown_write(self)
    }
}
