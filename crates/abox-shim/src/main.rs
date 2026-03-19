//! `abox-shim` — Guest-side credential proxy shim.
//!
//! Installed inside the VM as a symlink for proxied commands
//! (e.g., `/usr/local/bin/git` -> `/usr/local/bin/abox-shim`). When invoked:
//!
//! 1. Reads `argv[0]` to determine which command was requested
//! 2. Serializes the command + args as a JSON line
//! 3. Sends the request to the host proxy daemon over a Unix socket
//!    (bridged from VSock by `socat` in the guest init script)
//! 4. Reads the JSON response (exit code, stdout, stderr)
//! 5. Prints output and exits with the proxied exit code
//!
//! Compiled as a **static musl binary** for maximum portability. Uses zero
//! async dependencies — everything is synchronous for the smallest binary size.
//!
//! # Protocol
//!
//! The request/response types here mirror `abox_core::protocol::{ProxyRequest, ProxyResponse}`.
//! They are duplicated (not imported) because this crate intentionally avoids depending
//! on `abox-core` to keep the binary small and free of transitive dependencies.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;

/// Unix socket path inside the VM. The guest init script bridges VSock CID 2
/// port 5000 to this path via `socat`.
const PROXY_SOCKET: &str = "/run/abox-proxy.sock";

// ─── Protocol types (mirrors abox_core::protocol) ──────────────────────────

#[derive(Debug, Serialize)]
struct ProxyRequest {
    command: String,
    args: Vec<String>,
    cwd: String,
}

#[derive(Debug, Deserialize)]
struct ProxyResponse {
    exit_code: i32,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
}

// ─── Entry point ────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("abox-shim: error: {e}");
            ExitCode::from(127)
        }
    }
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let (command, cmd_args) = parse_args()?;
    let cwd = std::env::current_dir()
        .map_or_else(|_| "/workspace".to_string(), |p| p.display().to_string());

    let request = ProxyRequest { command, args: cmd_args, cwd };
    let response = send_request(&request)?;

    if !response.stdout.is_empty() {
        print!("{}", response.stdout);
    }
    if !response.stderr.is_empty() {
        eprint!("{}", response.stderr);
    }

    Ok(response.exit_code)
}

/// Parse argv to determine the command and arguments.
///
/// When invoked via symlink (e.g., `argv[0]` = "git"), the command is the
/// basename of `argv[0]`. When invoked directly as "abox-shim", the first
/// positional argument is the command.
fn parse_args() -> Result<(String, Vec<String>), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let argv0 = &args[0];
    let basename =
        Path::new(argv0).file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

    if basename == "abox-shim" {
        if args.len() < 2 {
            return Err("usage: abox-shim <command> [args...]".into());
        }
        Ok((args[1].clone(), args[2..].to_vec()))
    } else {
        Ok((basename, args[1..].to_vec()))
    }
}

/// Connect to the proxy daemon, send the request, and read the response.
fn send_request(request: &ProxyRequest) -> Result<ProxyResponse, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(PROXY_SOCKET).map_err(|e| {
        format!("failed to connect to proxy at {PROXY_SOCKET}: {e}. Is the proxy daemon running?")
    })?;

    // Send request as a single JSON line, then close the write half
    let json = serde_json::to_string(request)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    stream.shutdown(std::net::Shutdown::Write)?;

    // Read the JSON response line
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let mut response: ProxyResponse = serde_json::from_str(&line)?;

    // Any remaining data is additional stdout (for large outputs)
    let mut remaining = String::new();
    reader.read_to_string(&mut remaining)?;
    if !remaining.is_empty() {
        response.stdout.push_str(&remaining);
    }

    Ok(response)
}
