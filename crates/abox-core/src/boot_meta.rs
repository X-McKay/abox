//! Boot metadata passed from host to guest via a per-VM virtiofs share.
//!
//! The orchestrator stages a directory containing `boot.json` and
//! `runner.sh`, mounts it as the `aboxmeta` virtiofs tag (read-only), and
//! the guest init reads them. This avoids kernel-cmdline length limits and
//! quoting issues, and never touches the user's worktree.

use crate::util::validate_env_key;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;

/// Home directory of the unprivileged guest agent user.
/// Baked into the rootfs by `scripts/build_rootfs.sh`. Referenced as the
/// target of `~/` expansion in guest paths. See ADR-004.
pub const GUEST_AGENT_HOME: &str = "/home/abox";

/// Dedicated VM-local scratch directory for the unprivileged guest agent.
/// Created by `guest/init.sh` before the runner drops privileges.
pub const GUEST_AGENT_TMPDIR: &str = "/run/abox-tmp";

/// Expand a guest-side path against [`GUEST_AGENT_HOME`].
///
/// Rules:
///   * `~/…`  → `/home/abox/…`
///   * `/…`   → absolute, unchanged
///   * anything else → [`Err`] with the offending entry in the message
pub fn expand_guest_path(raw: &str) -> Result<String> {
    if let Some(rest) = raw.strip_prefix("~/") {
        Ok(format!("{GUEST_AGENT_HOME}/{rest}"))
    } else if raw.starts_with('/') {
        Ok(raw.to_string())
    } else {
        anyhow::bail!(
            "invalid guest path {raw:?}: must start with '/' (absolute) or '~/' \
             (relative to agent home /home/abox)"
        )
    }
}

/// A credential file staged in the boot metadata directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedCredential {
    /// Index in the credentials directory (maps to `credentials/<index>`).
    pub index: usize,
    /// Absolute destination path inside the guest VM.
    pub guest_path: String,
    /// Unix permissions (e.g., "0600").
    pub mode: String,
}

/// Metadata the host injects into the guest at boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootMeta {
    /// Sandbox identifier — exported as `ABOX_SANDBOX_ID` inside the guest.
    pub sandbox_id: String,
    /// The agent command and its arguments (`argv`-style).
    pub agent_command: Vec<String>,
    /// Additional environment variables to export before exec.
    pub env: Vec<(String, String)>,
    /// Credential files staged in `<meta_dir>/credentials/`.
    #[serde(default)]
    pub credential_files: Vec<StagedCredential>,
}

impl BootMeta {
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// Generate the `runner.sh` script the guest init `exec`s. Each
    /// argument is wrapped in single quotes; embedded single quotes are
    /// escaped using the standard `'\''` shell idiom. Environment
    /// variables are exported before the `exec`.
    ///
    /// The script runs as root (inherited from init.sh), stages credentials,
    /// fixes ownership, then drops privileges via `su-exec` before exec-ing
    /// the agent. See ADR-004.
    ///
    /// # Panics
    ///
    /// Panics if any environment variable key in `self.env` fails
    /// [`validate_env_key`]. Keys must be validated at the CLI boundary
    /// (in `run.rs`) before being stored in `BootMeta`. This panic is a
    /// defence-in-depth guard against keys reaching this function via any
    /// call path that bypasses the CLI.
    pub fn runner_script(&self) -> String {
        let mut s = String::from("#!/bin/sh\n");
        s.push_str("set -e\n");
        // Pre-flight: fail fast if rootfs is missing the abox user.
        s.push_str(
            "getent passwd abox >/dev/null 2>&1 || {\n\
             \x20   echo \"ERROR: guest rootfs is missing the 'abox' user \
             — rootfs rebuild required\" >&2\n\
             \x20   exit 69\n\
             }\n",
        );
        // Change to the workspace mount so the agent's CWD is the git worktree.
        // Also export ABOX_CWD so the shim can use it directly if getcwd(2)
        // fails (e.g. virtiofs mount points can confuse getcwd on some kernels).
        s.push_str("cd /workspace 2>/dev/null || true\n");
        s.push_str("export PATH='/usr/local/bin:/usr/bin:/bin:/sbin'\n");
        s.push_str("export ABOX_CWD=/workspace\n");
        s.push_str("export ABOX_SANDBOX_ID='");
        s.push_str(&sh_escape(&self.sandbox_id));
        s.push_str("'\n");
        for (k, v) in &self.env {
            // Defence-in-depth: panic rather than emit a malformed export
            // statement. The CLI boundary (run.rs) must have already rejected
            // invalid keys; this guard catches any call path that bypasses it.
            validate_env_key(k).unwrap_or_else(|e| {
                panic!(
                    "BUG: invalid env key {k:?} reached runner_script(); \
                     validate at the CLI boundary before constructing BootMeta: {e}"
                )
            });
            s.push_str("export ");
            s.push_str(k);
            s.push_str("='");
            s.push_str(&sh_escape(v));
            s.push_str("'\n");
        }
        // Keep guest temp usage on the VM-local scratch mount even if callers
        // try to point TMPDIR somewhere host-backed like /workspace.
        s.push_str("export TMPDIR='");
        s.push_str(GUEST_AGENT_TMPDIR);
        s.push_str("'\n");
        // Fix ownership of agent home if it doesn't belong to the abox user
        // (uid 1000). Skipped on template-restored VMs where ownership is
        // already correct, avoiding a redundant recursive chown on every boot.
        s.push_str(
            "[ \"$(stat -c %u /home/abox)\" = \"1000\" ] || chown -R abox:abox /home/abox\n",
        );
        for cred in &self.credential_files {
            let parent = std::path::Path::new(&cred.guest_path)
                .parent()
                .unwrap_or(std::path::Path::new("/"))
                .display()
                .to_string();
            let _ = writeln!(s, "mkdir -p '{}'", sh_escape(&parent));
            // Chown the parent directory if it's under the agent home — tools
            // like Codex write config/lock files into their credential directory
            // and fail with EACCES if it's owned by root. Directories outside
            // /home/abox (e.g. /etc) are left alone to avoid privilege escalation.
            if parent.starts_with(GUEST_AGENT_HOME) {
                let _ = writeln!(s, "chown abox:abox '{}'", sh_escape(&parent));
            }
            let _ = writeln!(
                s,
                "cp '/abox-meta/credentials/{}' '{}'",
                cred.index,
                sh_escape(&cred.guest_path)
            );
            let _ =
                writeln!(s, "chmod {} '{}'", sh_escape(&cred.mode), sh_escape(&cred.guest_path));
            let _ = writeln!(s, "chown abox:abox '{}'", sh_escape(&cred.guest_path));
        }
        // Older experimental builds accidentally wrote the MITM CA PEM into
        // ~/.claude.json. Claude treats that path as JSON config and aborts on
        // subsequent multi-turn runs if the stale PEM is left behind.
        s.push_str(
            "if [ -f /home/abox/.claude.json ] && \
             grep -q '^-----BEGIN CERTIFICATE-----' /home/abox/.claude.json; then\n\
             \x20   rm -f /home/abox/.claude.json\n\
             fi\n",
        );
        s.push_str(
            "if [ -f /home/abox/.codex/config.toml ] && \
             grep -q '^-----BEGIN CERTIFICATE-----' /home/abox/.codex/config.toml; then\n\
             \x20   rm -f /home/abox/.codex/config.toml\n\
             fi\n",
        );
        // Drop privileges and exec agent. su-exec is Alpine's standard
        // atomic uid/gid-drop-and-exec tool (like setpriv but available in
        // BusyBox-based rootfs). It sets uid, gid, and supplementary groups
        // from /etc/group, then execs the command.
        s.push_str("exec su-exec abox:abox env HOME=/home/abox USER=abox");
        for arg in &self.agent_command {
            s.push_str(" '");
            s.push_str(&sh_escape(arg));
            s.push('\'');
        }
        s.push('\n');
        s
    }

    /// Stage the boot meta on disk: write `boot.json` and `runner.sh` into
    /// `dir`. The orchestrator points virtiofsd at `dir` and mounts it as
    /// `/abox-meta` in the guest.
    pub fn stage(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("boot.json"), self.to_json()?)?;
        let runner_path = dir.join("runner.sh");
        std::fs::write(&runner_path, self.runner_script())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runner_path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    }
}

/// Escape a string for safe inclusion inside single quotes in a POSIX
/// shell. Embedded single quotes become `'\''`.
fn sh_escape(s: &str) -> String {
    s.replace('\'', r"'\''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_boot_meta_roundtrip() {
        let meta = BootMeta {
            sandbox_id: "fix-auth".into(),
            agent_command: vec!["claude".into(), "--model".into(), "opus".into()],
            env: vec![("FOO".into(), "bar".into())],
            credential_files: vec![],
        };
        let json = meta.to_json().unwrap();
        let parsed = BootMeta::from_json(&json).unwrap();
        assert_eq!(parsed.sandbox_id, "fix-auth");
        assert_eq!(parsed.agent_command, vec!["claude", "--model", "opus"]);
        assert_eq!(parsed.env.len(), 1);
        assert_eq!(parsed.env[0], ("FOO".into(), "bar".into()));
    }

    #[test]
    fn test_runner_script_basic() {
        let meta = BootMeta {
            sandbox_id: "task-a".into(),
            agent_command: vec!["/bin/echo".into(), "hello".into()],
            env: vec![],
            credential_files: vec![],
        };
        let script = meta.runner_script();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("export PATH='/usr/local/bin:/usr/bin:/bin:/sbin'\n"));
        assert!(script.contains("export ABOX_SANDBOX_ID='task-a'\n"));
        assert!(script.contains("export TMPDIR='/run/abox-tmp'\n"));
        assert!(script
            .contains("su-exec abox:abox env HOME=/home/abox USER=abox '/bin/echo' 'hello'\n"));
    }

    #[test]
    fn test_runner_script_forces_vm_local_tmpdir_after_user_env() {
        let meta = BootMeta {
            sandbox_id: "tmpdir-test".into(),
            agent_command: vec!["/bin/true".into()],
            env: vec![("TMPDIR".into(), "/workspace".into())],
            credential_files: vec![],
        };

        let script = meta.runner_script();
        assert!(script.contains("export TMPDIR='/workspace'\nexport TMPDIR='/run/abox-tmp'\n"));
    }

    #[test]
    fn test_runner_script_quotes_metacharacters() {
        let meta = BootMeta {
            sandbox_id: "x".into(),
            agent_command: vec!["echo".into(), "hello world".into(), "$HOME".into(), "it's".into()],
            env: vec![("MSG".into(), "a 'quote' here".into())],
            credential_files: vec![],
        };
        let script = meta.runner_script();
        // Argument wrapping
        assert!(script.contains("'echo'"));
        assert!(script.contains("'hello world'"));
        assert!(script.contains("'$HOME'")); // dollar sign protected by single-quotes
                                             // Single-quote escape: it's -> 'it'\''s'
        assert!(script.contains(r"'it'\''s'"));
        // Env var with embedded quote
        assert!(script.contains(r"export MSG='a '\''quote'\'' here'"));
    }

    #[test]
    fn test_stage_writes_files() {
        let tmp = TempDir::new().unwrap();
        let meta = BootMeta {
            sandbox_id: "stage-test".into(),
            agent_command: vec!["true".into()],
            env: vec![],
            credential_files: vec![],
        };
        meta.stage(tmp.path()).unwrap();

        let boot_json = std::fs::read_to_string(tmp.path().join("boot.json")).unwrap();
        assert!(boot_json.contains("\"sandbox_id\": \"stage-test\""));

        let runner = std::fs::read_to_string(tmp.path().join("runner.sh")).unwrap();
        assert!(runner.starts_with("#!/bin/sh\n"));

        // runner.sh must be executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                std::fs::metadata(tmp.path().join("runner.sh")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn test_runner_script_with_credentials() {
        let meta = BootMeta {
            sandbox_id: "cred-test".into(),
            agent_command: vec!["claude".into(), "--print".into(), "hello".into()],
            env: vec![],
            credential_files: vec![StagedCredential {
                index: 0,
                guest_path: "/.claude/.credentials.json".into(),
                mode: "0600".into(),
            }],
        };
        let script = meta.runner_script();
        assert!(script.contains("mkdir -p '/.claude'"));
        assert!(script.contains("cp '/abox-meta/credentials/0' '/.claude/.credentials.json'"));
        assert!(script.contains("chmod 0600 '/.claude/.credentials.json'"));
        // Credential placement must come before exec
        let cred_pos = script.find("cp '/abox-meta/credentials/0'").unwrap();
        let exec_pos = script.find("\nexec ").unwrap();
        assert!(cred_pos < exec_pos, "credentials must be placed before exec");
    }

    #[test]
    fn test_runner_script_no_credentials() {
        let meta = BootMeta {
            sandbox_id: "no-cred".into(),
            agent_command: vec!["echo".into(), "hi".into()],
            env: vec![],
            credential_files: vec![],
        };
        let script = meta.runner_script();
        assert!(!script.contains("/abox-meta/credentials"));
    }

    #[test]
    fn test_runner_script_credential_path_escaping() {
        let meta = BootMeta {
            sandbox_id: "escape-test".into(),
            agent_command: vec!["true".into()],
            env: vec![],
            credential_files: vec![StagedCredential {
                index: 0,
                guest_path: "/root/.config/it's a test/creds.json".into(),
                mode: "0600".into(),
            }],
        };
        let script = meta.runner_script();
        assert!(script.contains(r"'/root/.config/it'\''s a test/creds.json'"));
    }

    #[test]
    fn test_stage_with_credentials() {
        let tmp = TempDir::new().unwrap();
        let meta = BootMeta {
            sandbox_id: "stage-cred".into(),
            agent_command: vec!["true".into()],
            env: vec![],
            credential_files: vec![StagedCredential {
                index: 0,
                guest_path: "/.claude/.credentials.json".into(),
                mode: "0600".into(),
            }],
        };
        meta.stage(tmp.path()).unwrap();
        let runner = std::fs::read_to_string(tmp.path().join("runner.sh")).unwrap();
        assert!(runner.contains("cp '/abox-meta/credentials/0'"));
    }

    #[test]
    fn runner_script_contains_abox_user_preflight() {
        let meta = BootMeta {
            sandbox_id: "t".into(),
            agent_command: vec!["/bin/true".into()],
            env: vec![],
            credential_files: vec![],
        };
        let script = meta.runner_script();
        assert!(
            script.contains("getent passwd abox"),
            "runner script must contain getent passwd abox preflight, got:\n{script}"
        );
        assert!(
            script.contains("exit 69"),
            "runner script must exit 69 on missing abox user, got:\n{script}"
        );
    }

    #[test]
    fn runner_script_execs_via_su_exec() {
        let meta = BootMeta {
            sandbox_id: "t".into(),
            agent_command: vec!["/bin/true".into()],
            env: vec![],
            credential_files: vec![],
        };
        let script = meta.runner_script();
        assert!(
            script.contains("exec su-exec abox:abox"),
            "runner script must exec via su-exec, got:\n{script}"
        );
        assert!(
            script.contains("env HOME=/home/abox USER=abox"),
            "runner script must set HOME and USER for the dropped-priv child, got:\n{script}"
        );
        assert!(script.contains("'/bin/true'"), "agent command missing, got:\n{script}");
    }

    #[test]
    fn runner_script_chowns_staged_credentials() {
        let meta = BootMeta {
            sandbox_id: "t".into(),
            agent_command: vec!["/bin/true".into()],
            env: vec![],
            credential_files: vec![StagedCredential {
                index: 0,
                guest_path: "/home/abox/.claude/.credentials.json".into(),
                mode: "0600".into(),
            }],
        };
        let script = meta.runner_script();
        // Directory under agent home gets chowned so tools can write config there.
        assert!(
            script.contains("chown abox:abox '/home/abox/.claude'"),
            "parent dir under agent home must be chowned"
        );
        let cp_pos = script.find("cp '/abox-meta/credentials/0'").expect("cp line missing");
        let chmod_pos = script.find("chmod 0600").expect("chmod line missing");
        // The file chown comes after cp + chmod.
        let file_chown_pos = script[cp_pos..].find("chown abox:abox").expect("file chown missing");
        let exec_pos = script.find("\nexec ").expect("exec line missing");
        assert!(cp_pos < chmod_pos, "cp must precede chmod");
        assert!(cp_pos + file_chown_pos < exec_pos, "file chown must precede exec");
    }

    #[test]
    fn runner_script_removes_stale_claude_pem_config() {
        let meta = BootMeta {
            sandbox_id: "t".into(),
            agent_command: vec!["/bin/true".into()],
            env: vec![],
            credential_files: vec![],
        };
        let script = meta.runner_script();
        assert!(script.contains("grep -q '^-----BEGIN CERTIFICATE-----' /home/abox/.claude.json"));
        assert!(script.contains("rm -f /home/abox/.claude.json"));
    }

    #[test]
    fn runner_script_removes_stale_codex_pem_config() {
        let meta = BootMeta {
            sandbox_id: "t".into(),
            agent_command: vec!["/bin/true".into()],
            env: vec![],
            credential_files: vec![],
        };
        let script = meta.runner_script();
        assert!(
            script.contains("grep -q '^-----BEGIN CERTIFICATE-----' /home/abox/.codex/config.toml")
        );
        assert!(script.contains("rm -f /home/abox/.codex/config.toml"));
    }

    #[test]
    fn expand_guest_path_tilde_prefix() {
        assert_eq!(
            expand_guest_path("~/.claude/.credentials.json").unwrap(),
            "/home/abox/.claude/.credentials.json"
        );
        assert_eq!(expand_guest_path("~/foo").unwrap(), "/home/abox/foo");
        assert_eq!(expand_guest_path("~/").unwrap(), "/home/abox/");
    }

    #[test]
    fn expand_guest_path_absolute_unchanged() {
        assert_eq!(expand_guest_path("/etc/foo").unwrap(), "/etc/foo");
        assert_eq!(
            expand_guest_path("/home/abox/.claude/.credentials.json").unwrap(),
            "/home/abox/.claude/.credentials.json"
        );
    }

    #[test]
    fn expand_guest_path_rejects_bare_relative() {
        for bad in ["foo", "./foo", "../foo", "~user/foo", "~"] {
            let result = expand_guest_path(bad);
            assert!(result.is_err(), "expected Err for {bad:?}, got {result:?}");
            let msg = format!("{}", result.err().unwrap());
            assert!(
                msg.contains(bad),
                "error message should cite offending entry {bad:?}, got: {msg}"
            );
        }
    }

    #[test]
    fn guest_init_mounts_private_tmpdir() {
        let init_sh = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../guest/init.sh"));
        assert!(init_sh.contains("ABOX_TMPDIR=/run/abox-tmp"));
        assert!(init_sh.contains(
            "mount -t tmpfs -o mode=0700,uid=\"$ABOX_UID\",gid=\"$ABOX_GID\",nodev,nosuid"
        ));
        assert!(init_sh.contains("boot_fail 70 \"failed to mount tmpfs scratch at $ABOX_TMPDIR\""));
        assert!(init_sh.contains("chown \"$ABOX_UID:$ABOX_GID\" \"$ABOX_TMPDIR\""));
        assert!(init_sh.contains("chmod 0700 \"$ABOX_TMPDIR\""));
    }

    #[test]
    fn guest_init_mounts_workspace_with_nodev_nosuid() {
        let init_sh = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../guest/init.sh"));
        // The workspace virtiofs share must be mounted with nodev and nosuid to
        // prevent the agent from creating device nodes or using setuid binaries
        // on the host-backed share.
        assert!(
            init_sh.contains("mount -t virtiofs -o nodev,nosuid workspace /workspace"),
            "workspace mount must include nodev,nosuid; init.sh:\n{init_sh}"
        );
    }

    #[test]
    fn guest_init_mounts_aboxmeta_read_only() {
        let init_sh = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../guest/init.sh"));
        // The aboxmeta share must be mounted read-only to prevent a compromised
        // guest process from modifying runner.sh or staged credentials after boot.
        // nodev and nosuid are also required.
        assert!(
            init_sh.contains("mount -t virtiofs -o ro,nodev,nosuid aboxmeta /abox-meta"),
            "aboxmeta mount must include ro,nodev,nosuid; init.sh:\n{init_sh}"
        );
    }

    #[test]
    fn guest_init_mounts_aboxstatus_with_nodev_nosuid() {
        let init_sh = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../guest/init.sh"));
        // The status share must be mounted with nodev and nosuid.
        assert!(
            init_sh.contains("mount -t virtiofs -o nodev,nosuid aboxstatus /abox-status"),
            "aboxstatus mount must include nodev,nosuid; init.sh:\n{init_sh}"
        );
    }
}
