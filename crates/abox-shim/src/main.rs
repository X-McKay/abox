//! abox-shim: Guest-side credential proxy shim.
//!
//! This binary is installed inside the VM as a symlink for proxied commands
//! (e.g., /usr/local/bin/git -> /usr/local/bin/abox-shim). When invoked, it:
//!
//! 1. Reads `argv[0]` to determine which command was requested.
//! 2. Serializes the command + args into a JSON request.
//! 3. Sends the request to the host proxy daemon over a Unix socket
//!    (connected via VSock from the guest side).
//! 4. Reads the JSON response (exit code, stdout, stderr).
//! 5. Prints stdout/stderr and exits with the proxied exit code.
//!
//! This binary is compiled as a static musl binary for maximum portability.
//! It has zero async dependencies -- everything is synchronous for the smallest
//! possible binary size.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;

/// The socket path inside the VM where the VSock-to-Unix bridge listens.
/// The guest init script sets up `socat` to bridge VSock CID 2 port 5000
/// to this Unix socket.
const PROXY_SOCKET: &str = "/run/abox-proxy.sock";

/// Request sent from the shim to the host proxy daemon.
#[derive(Debug, Serialize)]
struct ProxyRequest {
    /// The command name (derived from argv[0], e.g., "git").
    command: String,
    /// The full argument list (argv[1..]).
    args: Vec<String>,
    /// Current working directory inside the VM.
    cwd: String,
}

/// Response received from the host proxy daemon.
#[derive(Debug, Deserialize)]
struct ProxyResponse {
    /// The exit code of the proxied command.
    exit_code: i32,
    /// Standard output.
    #[serde(default)]
    stdout: String,
    /// Standard error.
    #[serde(default)]
    stderr: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("abox-shim: error: {}", e);
            ExitCode::from(127)
        }
    }
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Determine the command from argv[0].
    // When symlinked (e.g., /usr/local/bin/git -> abox-shim), argv[0] is "git".
    let argv0 = &args[0];
    let command =
        Path::new(argv0).file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

    // If invoked directly as "abox-shim", the first arg is the command.
    let (command, cmd_args) = if command == "abox-shim" {
        if args.len() < 2 {
            return Err("Usage: abox-shim <command> [args...]".into());
        }
        (args[1].clone(), args[2..].to_vec())
    } else {
        (command, args[1..].to_vec())
    };

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/workspace".to_string());

    let request = ProxyRequest { command, args: cmd_args, cwd };

    // Connect to the proxy daemon
    let mut stream = UnixStream::connect(PROXY_SOCKET).map_err(|e| {
        format!(
            "Failed to connect to proxy at {}: {}. Is the proxy daemon running?",
            PROXY_SOCKET, e
        )
    })?;

    // Send the request as a single JSON line
    let request_json = serde_json::to_string(&request)?;
    stream.write_all(request_json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    // Shut down the write half to signal we are done sending
    stream.shutdown(std::net::Shutdown::Write)?;

    // Read the response
    let mut reader = BufReader::new(&stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;

    // There may be streaming output after the JSON line (for large stdout).
    // Read remaining data as additional stdout.
    let mut remaining = String::new();
    reader.read_to_string(&mut remaining)?;

    let response: ProxyResponse = serde_json::from_str(&response_line)?;

    // Print stdout and stderr
    if !response.stdout.is_empty() {
        print!("{}", response.stdout);
    }
    if !remaining.is_empty() {
        print!("{}", remaining);
    }
    if !response.stderr.is_empty() {
        eprint!("{}", response.stderr);
    }

    Ok(response.exit_code)
}
