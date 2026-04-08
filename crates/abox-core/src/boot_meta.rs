//! Boot metadata passed from host to guest via a per-VM virtiofs share.
//!
//! The orchestrator stages a directory containing `boot.json` and
//! `runner.sh`, mounts it as the `aboxmeta` virtiofs tag (read-only), and
//! the guest init reads them. This avoids kernel-cmdline length limits and
//! quoting issues, and never touches the user's worktree.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Metadata the host injects into the guest at boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootMeta {
    /// Sandbox identifier — exported as `ABOX_SANDBOX_ID` inside the guest.
    pub sandbox_id: String,
    /// The agent command and its arguments (`argv`-style).
    pub agent_command: Vec<String>,
    /// Additional environment variables to export before exec.
    pub env: Vec<(String, String)>,
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
    pub fn runner_script(&self) -> String {
        let mut s = String::from("#!/bin/sh\n");
        // Change to the workspace mount so the agent's CWD is the git worktree.
        // Also export ABOX_CWD so the shim can use it directly if getcwd(2)
        // fails (e.g. virtiofs mount points can confuse getcwd on some kernels).
        s.push_str("cd /workspace 2>/dev/null || true\n");
        s.push_str("export ABOX_CWD=/workspace\n");
        s.push_str("export ABOX_SANDBOX_ID='");
        s.push_str(&sh_escape(&self.sandbox_id));
        s.push_str("'\n");
        for (k, v) in &self.env {
            s.push_str("export ");
            s.push_str(k);
            s.push_str("='");
            s.push_str(&sh_escape(v));
            s.push_str("'\n");
        }
        s.push_str("exec");
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
        };
        let script = meta.runner_script();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("export ABOX_SANDBOX_ID='task-a'\n"));
        assert!(script.contains("\nexec '/bin/echo' 'hello'\n"));
    }

    #[test]
    fn test_runner_script_quotes_metacharacters() {
        let meta = BootMeta {
            sandbox_id: "x".into(),
            agent_command: vec!["echo".into(), "hello world".into(), "$HOME".into(), "it's".into()],
            env: vec![("MSG".into(), "a 'quote' here".into())],
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
}
