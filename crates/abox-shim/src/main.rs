//! `abox-shim` — Guest-side credential proxy shim.
//!
//! Installed inside the VM as a symlink for proxied commands
//! (e.g., `/usr/local/bin/git` -> `/usr/local/bin/abox-shim`). When invoked:
//!
//! 1. Reads `argv[0]` to determine which command was requested
//! 2. Serializes the command + args as a JSON line
//! 3. Sends the request to the host command broker via the persistent
//!    `abox-bridge` uplink (or directly over AF_VSOCK). See the `transport`
//!    module.
//! 4. Reads the JSON response (exit code, stdout, stderr)
//! 5. Prints output and exits with the proxied exit code
//!
//! Compiled as a **static musl binary** for maximum portability. Uses zero
//! async dependencies — everything is synchronous for the smallest binary size.
//!
//! # Protocol
//!
//! Wire types are defined in the tiny `abox-protocol` crate (serde-only,
//! no transitive deps) and shared with `abox-proxyd` via `abox-core`.

mod transport;

use abox_protocol::{ProxyRequest, ProxyResponse};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::ExitCode;

/// Environment variable injected into the guest by the host so the shim can
/// attribute every request to the originating sandbox.
const SANDBOX_ID_ENV: &str = "ABOX_SANDBOX_ID";

/// Optional environment variable that overrides `getcwd(2)` for the CWD
/// passed to the proxy. Useful when virtiofs mount points confuse getcwd.
const CWD_OVERRIDE_ENV: &str = "ABOX_CWD";

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

    let cwd = resolve_cwd();
    let sandbox_id = std::env::var(SANDBOX_ID_ENV).ok();

    let request = ProxyRequest { command, args: cmd_args, cwd, sandbox_id };
    let response = send_request(&request)?;

    if !response.stdout.is_empty() {
        print!("{}", response.stdout);
    }
    if !response.stderr.is_empty() {
        eprint!("{}", response.stderr);
    }

    Ok(response.exit_code)
}

/// Resolve the current working directory.
///
/// Resolution order (most authoritative first):
///   1. `ABOX_CWD` env var (set by `/abox-meta/runner.sh`; host-known truth)
///   2. `/proc/self/cwd` symlink target -- kernel-maintained, more reliable
///      than `getcwd(2)` on some virtiofs kernels which can return the
///      wrong path when the process is inside a virtiofs mount.
///   3. `getcwd(2)` fallback
///   4. Hardcoded `"/workspace"` if everything else failed
fn resolve_cwd() -> String {
    std::env::var(CWD_OVERRIDE_ENV)
        .ok()
        .or_else(|| std::fs::read_link("/proc/self/cwd").ok().map(|p| p.display().to_string()))
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| "/workspace".to_string())
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
///
/// The transport is declared by host-staged immutable config; see
/// [`transport`].
fn send_request(request: &ProxyRequest) -> Result<ProxyResponse, Box<dyn std::error::Error>> {
    let mut stream = transport::connect(&transport::resolve_transport())?;

    // Send request as a single JSON line, then close the write half
    let json = serde_json::to_string(request)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    stream.shutdown_write()?;

    // Read the JSON response line (move the stream: writes are done). The
    // response is exactly one line; the shim must NOT read to EOF — the
    // broker keeps persistent connections open for multiplexing clients.
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let response: ProxyResponse = serde_json::from_str(&line)?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // Tests that touch environment variables or cwd must run serially to
    // avoid races with other tests in the same process.

    #[test]
    #[serial]
    fn resolve_cwd_returns_abox_cwd_when_set() {
        // Temporarily set ABOX_CWD and verify it wins.
        let _guard = EnvGuard::set(CWD_OVERRIDE_ENV, "/custom/cwd");
        let cwd = resolve_cwd();
        assert_eq!(cwd, "/custom/cwd");
    }

    #[test]
    #[serial]
    fn resolve_cwd_falls_back_to_proc_self_cwd() {
        // With ABOX_CWD unset and a valid cwd, /proc/self/cwd should
        // resolve to the process's actual working directory.
        let _guard = EnvGuard::remove(CWD_OVERRIDE_ENV);
        let cwd = resolve_cwd();
        // On a normal Linux host /proc/self/cwd is valid, so we should
        // get either the readlink result or getcwd — both are real paths.
        assert!(!cwd.is_empty());
        assert_ne!(cwd, "/workspace", "should not hit the hardcoded fallback on a real host");
    }

    #[test]
    #[serial]
    fn resolve_cwd_uses_real_current_dir() {
        let _guard = EnvGuard::remove(CWD_OVERRIDE_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let _cwd_guard = CwdGuard::set(tmp.path()).unwrap();

        let cwd = resolve_cwd();

        // The resolved cwd should contain the tmpdir's path (canonicalized).
        let canonical = tmp.path().canonicalize().unwrap();
        assert_eq!(cwd, canonical.display().to_string());
    }

    // ── Helper: RAII env‑var guard ──────────────────────────────────────────

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }

        fn remove(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    // ── Helper: RAII cwd guard ─────────────────────────────────────────────

    /// Saves the current working directory and restores it on drop,
    /// ensuring `set_current_dir` tests don't leak cwd changes on panic.
    struct CwdGuard {
        prev: std::path::PathBuf,
    }

    impl CwdGuard {
        fn set(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
            let prev = std::env::current_dir()?;
            std::env::set_current_dir(path)?;
            Ok(Self { prev })
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.prev);
        }
    }
}
