//! Shared host-virtualization diagnostics used by both `abox init` and
//! `abox doctor`.
//!
//! On Linux this detects *why* KVM is unavailable and returns actionable
//! remediation guidance tailored to the specific environment (bare metal,
//! container, WSL2, nested VM). On macOS it checks Hypervisor.framework
//! support (`sysctl kern.hv_support`) on Apple Silicon, which is what the
//! MicroSandbox (libkrun) runtime uses.

#[cfg(not(target_os = "macos"))]
use std::path::Path;

/// Result of a host-virtualization check (KVM on Linux,
/// Hypervisor.framework on macOS).
#[derive(Debug)]
pub enum HostVirtStatus {
    /// Hardware virtualization is usable.
    Available { detail: String },
    /// Virtualization is not usable; includes condition and remediation.
    Unavailable { condition: String, remediation: String },
}

/// Diagnose host virtualization for the MicroSandbox (libkrun) runtime:
/// `/dev/kvm` on Linux, Hypervisor.framework on macOS.
pub fn diagnose_host_virtualization() -> HostVirtStatus {
    #[cfg(target_os = "macos")]
    {
        diagnose_macos_hypervisor()
    }
    #[cfg(not(target_os = "macos"))]
    {
        match diagnose_kvm() {
            KvmStatus::Available => {
                HostVirtStatus::Available { detail: "/dev/kvm is accessible".into() }
            }
            KvmStatus::Unavailable { condition, remediation } => {
                HostVirtStatus::Unavailable { condition, remediation }
            }
        }
    }
}

/// macOS: libkrun requires Apple Silicon and Hypervisor.framework
/// (`kern.hv_support` = 1).
#[cfg(target_os = "macos")]
fn diagnose_macos_hypervisor() -> HostVirtStatus {
    if std::env::consts::ARCH != "aarch64" {
        return HostVirtStatus::Unavailable {
            condition: format!(
                "macOS on {} — the MicroSandbox runtime requires Apple Silicon (arm64)",
                std::env::consts::ARCH
            ),
            remediation: "Run abox on an Apple Silicon Mac or a Linux host with KVM.".into(),
        };
    }
    match std::process::Command::new("sysctl").args(["-n", "kern.hv_support"]).output() {
        Ok(out) if out.status.success() => {
            let value = String::from_utf8_lossy(&out.stdout);
            if parse_hv_support(&value) {
                HostVirtStatus::Available {
                    detail: "Hypervisor.framework available (kern.hv_support = 1, arm64)".into(),
                }
            } else {
                HostVirtStatus::Unavailable {
                    condition: format!(
                        "Hypervisor.framework unavailable (kern.hv_support = {})",
                        value.trim()
                    ),
                    remediation:
                        "Virtualization is disabled on this Mac. If this is a VM, enable\n\
                         nested virtualization on the host hypervisor; on bare metal this\n\
                         usually indicates an MDM restriction."
                            .into(),
                }
            }
        }
        Ok(out) => HostVirtStatus::Unavailable {
            condition: format!(
                "sysctl kern.hv_support failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            remediation: "Run 'sysctl kern.hv_support' manually; expected value is 1.".into(),
        },
        Err(err) => HostVirtStatus::Unavailable {
            condition: format!("failed to run sysctl: {err}"),
            remediation: "Run 'sysctl kern.hv_support' manually; expected value is 1.".into(),
        },
    }
}

/// Parse `sysctl -n kern.hv_support` output: virtualization is available
/// when the trimmed value is exactly "1".
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_hv_support(output: &str) -> bool {
    output.trim() == "1"
}

/// Result of a KVM availability check.
#[derive(Debug)]
#[cfg(not(target_os = "macos"))]
pub enum KvmStatus {
    /// `/dev/kvm` exists and is read-write accessible.
    Available,
    /// KVM is not usable; includes a human-readable condition and remediation.
    Unavailable { condition: String, remediation: String },
}

/// Diagnose KVM availability with environment-aware guidance.
///
/// The checks are ordered from most specific to most general so the
/// remediation message is as actionable as possible.
#[cfg(not(target_os = "macos"))]
pub fn diagnose_kvm() -> KvmStatus {
    // ── WSL2 ────────────────────────────────────────────────────────────
    if is_wsl2() && !Path::new("/dev/kvm").exists() {
        return KvmStatus::Unavailable {
            condition: "WSL2 detected, /dev/kvm not found".into(),
            remediation: "Enable nested virtualisation in .wslconfig:\n\n\
                          \x20 [wsl2]\n\
                          \x20 nestedVirtualization=true\n\n\
                          Then restart WSL: wsl --shutdown"
                .into(),
        };
    }

    // ── Container ───────────────────────────────────────────────────────
    if is_container() && !Path::new("/dev/kvm").exists() {
        return KvmStatus::Unavailable {
            condition: "Running inside a container, /dev/kvm not found".into(),
            remediation: "Pass the KVM device when launching the container:\n\n\
                          \x20 docker run --device /dev/kvm ..."
                .into(),
        };
    }

    // ── /dev/kvm existence ──────────────────────────────────────────────
    let kvm = Path::new("/dev/kvm");
    if !kvm.exists() {
        // Check CPU virtualisation extensions to distinguish "module not
        // loaded" from "hardware doesn't support it".
        return if has_cpu_virt_extensions() {
            KvmStatus::Unavailable {
                condition: "CPU supports virtualisation but /dev/kvm is missing".into(),
                remediation: "Load the KVM kernel module:\n\n\
                              \x20 sudo modprobe kvm_intel   # Intel CPUs\n\
                              \x20 sudo modprobe kvm_amd     # AMD CPUs"
                    .into(),
            }
        } else {
            KvmStatus::Unavailable {
                condition: "CPU does not support hardware virtualisation".into(),
                remediation: "Enable VT-x (Intel) or AMD-V in your BIOS/UEFI settings,\n\
                     or run abox on a bare-metal host with virtualisation support.\n\n\
                     If you're inside a VM, enable nested virtualisation on the\n\
                     host hypervisor."
                    .into(),
            }
        };
    }

    // ── Permission check ────────────────────────────────────────────────
    match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
        Ok(_) => KvmStatus::Available,
        Err(_) => KvmStatus::Unavailable {
            condition: "Permission denied on /dev/kvm".into(),
            remediation: "Add yourself to the kvm group and log out/in:\n\n\
                          \x20 sudo usermod -aG kvm $USER\n\
                          \x20 newgrp kvm   # or log out and back in"
                .into(),
        },
    }
}

/// Check `/proc/cpuinfo` for vmx (Intel) or svm (AMD) flags.
#[cfg(not(target_os = "macos"))]
fn has_cpu_virt_extensions() -> bool {
    std::fs::read_to_string("/proc/cpuinfo").is_ok_and(|s| s.contains(" vmx") || s.contains(" svm"))
}

/// Detect WSL2 via `/proc/version`.
#[cfg(not(target_os = "macos"))]
fn is_wsl2() -> bool {
    std::fs::read_to_string("/proc/version").is_ok_and(|s| s.to_lowercase().contains("microsoft"))
}

/// Detect container environment (Docker, Podman, etc.).
#[cfg(not(target_os = "macos"))]
fn is_container() -> bool {
    // /.dockerenv is created by Docker; /run/.containerenv by Podman.
    if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
        return true;
    }
    // cgroup-based detection: look for container-specific cgroup paths.
    std::fs::read_to_string("/proc/1/cgroup")
        .is_ok_and(|s| s.contains("/docker/") || s.contains("/lxc/") || s.contains("/kubepods/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn cpu_virt_detection_does_not_panic() {
        // Just ensure the function doesn't panic on the current machine.
        let _ = has_cpu_virt_extensions();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn container_detection_does_not_panic() {
        let _ = is_container();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn wsl2_detection_does_not_panic() {
        let _ = is_wsl2();
    }

    #[test]
    fn hv_support_parsing() {
        assert!(parse_hv_support("1"));
        assert!(parse_hv_support("1\n"));
        assert!(parse_hv_support("  1  "));
        assert!(!parse_hv_support("0"));
        assert!(!parse_hv_support("0\n"));
        assert!(!parse_hv_support(""));
        assert!(!parse_hv_support("11"));
    }

    #[test]
    fn host_virtualization_diagnostic_is_concrete() {
        match diagnose_host_virtualization() {
            HostVirtStatus::Available { detail } => assert!(!detail.is_empty()),
            HostVirtStatus::Unavailable { condition, remediation } => {
                assert!(!condition.is_empty());
                assert!(!remediation.is_empty());
            }
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn diagnose_returns_some_status() {
        // On any Linux machine this should return a concrete status.
        let status = diagnose_kvm();
        match status {
            KvmStatus::Available => {} // fine
            KvmStatus::Unavailable { condition, remediation } => {
                assert!(!condition.is_empty());
                assert!(!remediation.is_empty());
            }
        }
    }
}
