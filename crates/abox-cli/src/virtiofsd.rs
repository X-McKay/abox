//! Shared virtiofsd capability diagnostics used by setup and doctor flows.

use std::io::IsTerminal;
use std::path::Path;
use std::process::{Command, Stdio};

const REQUIRED_CAPABILITY: &str = "cap_sys_admin+ep";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtiofsdCapsStatus {
    Ready,
    Missing { condition: String, remediation: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureVirtiofsdCapsOutcome {
    AlreadyPresent,
    Applied,
    NeedsManual { condition: String, remediation: String },
}

pub fn diagnose_virtiofsd_caps(path: &Path) -> VirtiofsdCapsStatus {
    if !path.exists() {
        return VirtiofsdCapsStatus::Missing {
            condition: format!("virtiofsd not found at {}", path.display()),
            remediation: "Run 'abox init' or 'just bootstrap-vm' to install VM assets.".into(),
        };
    }

    match Command::new("getcap").arg(path).output() {
        Ok(output) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            if has_required_capability(&combined) {
                VirtiofsdCapsStatus::Ready
            } else {
                VirtiofsdCapsStatus::Missing {
                    condition: format!(
                        "virtiofsd at {} lacks required capability {REQUIRED_CAPABILITY}",
                        path.display()
                    ),
                    remediation: manual_remediation(path),
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => VirtiofsdCapsStatus::Missing {
            condition: "getcap is not installed on this host".into(),
            remediation: format!(
                "Install libcap tooling (for example: 'sudo apt install libcap2-bin') and then run:\n\n  {}",
                setcap_command(path)
            ),
        },
        Err(err) => VirtiofsdCapsStatus::Missing {
            condition: format!("failed to probe virtiofsd capabilities: {err}"),
            remediation: manual_remediation(path),
        },
    }
}

pub fn ensure_virtiofsd_caps(path: &Path, allow_sudo_prompt: bool) -> EnsureVirtiofsdCapsOutcome {
    match diagnose_virtiofsd_caps(path) {
        VirtiofsdCapsStatus::Ready => EnsureVirtiofsdCapsOutcome::AlreadyPresent,
        VirtiofsdCapsStatus::Missing { .. } => {
            if (try_apply_setcap(path) || try_apply_with_sudo(path, allow_sudo_prompt))
                && matches!(diagnose_virtiofsd_caps(path), VirtiofsdCapsStatus::Ready)
            {
                return EnsureVirtiofsdCapsOutcome::Applied;
            }

            match diagnose_virtiofsd_caps(path) {
                VirtiofsdCapsStatus::Ready => EnsureVirtiofsdCapsOutcome::Applied,
                VirtiofsdCapsStatus::Missing { condition, remediation } => {
                    EnsureVirtiofsdCapsOutcome::NeedsManual { condition, remediation }
                }
            }
        }
    }
}

pub fn setcap_command(path: &Path) -> String {
    format!("sudo setcap '{REQUIRED_CAPABILITY}' {}", shell_quote(path))
}

fn manual_remediation(path: &Path) -> String {
    format!(
        "Grant the required file capability and re-run 'abox doctor':\n\n  {}",
        setcap_command(path)
    )
}

fn has_required_capability(text: &str) -> bool {
    text.contains("cap_sys_admin") && (text.contains("=ep") || text.contains("+ep"))
}

fn try_apply_setcap(path: &Path) -> bool {
    Command::new("setcap")
        .arg(REQUIRED_CAPABILITY)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn try_apply_with_sudo(path: &Path, allow_sudo_prompt: bool) -> bool {
    if !allow_sudo_prompt || !std::io::stdin().is_terminal() {
        return false;
    }

    Command::new("sudo")
        .arg("setcap")
        .arg(REQUIRED_CAPABILITY)
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

fn shell_quote(path: &Path) -> String {
    let raw = path_to_string(path);
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn missing_binary_reports_install_remediation() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("virtiofsd");
        match diagnose_virtiofsd_caps(&missing) {
            VirtiofsdCapsStatus::Ready => panic!("expected missing binary to fail"),
            VirtiofsdCapsStatus::Missing { condition, remediation } => {
                assert!(condition.contains("virtiofsd not found"));
                assert!(remediation.contains("abox init"));
            }
        }
    }

    #[test]
    fn setcap_command_quotes_paths() {
        let path = PathBuf::from("/tmp/path with spaces/virtiofsd");
        assert_eq!(
            setcap_command(&path),
            "sudo setcap 'cap_sys_admin+ep' '/tmp/path with spaces/virtiofsd'"
        );
    }

    #[test]
    fn capability_parser_accepts_getcap_output() {
        assert!(has_required_capability("/tmp/virtiofsd cap_sys_admin=ep"));
        assert!(has_required_capability("/tmp/virtiofsd cap_sys_admin+ep"));
        assert!(!has_required_capability("/tmp/virtiofsd"));
    }
}
