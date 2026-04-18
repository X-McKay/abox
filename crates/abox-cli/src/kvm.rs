//! Shared KVM diagnostics used by both `abox init` and `abox doctor`.
//!
//! Detects *why* KVM is unavailable and returns actionable remediation
//! guidance tailored to the specific environment (bare metal, container,
//! WSL2, nested VM).

use std::path::Path;

/// Result of a KVM availability check.
#[derive(Debug)]
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
fn has_cpu_virt_extensions() -> bool {
    std::fs::read_to_string("/proc/cpuinfo").is_ok_and(|s| s.contains(" vmx") || s.contains(" svm"))
}

/// Detect WSL2 via `/proc/version`.
fn is_wsl2() -> bool {
    std::fs::read_to_string("/proc/version").is_ok_and(|s| s.to_lowercase().contains("microsoft"))
}

/// Detect container environment (Docker, Podman, etc.).
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
    fn cpu_virt_detection_does_not_panic() {
        // Just ensure the function doesn't panic on the current machine.
        let _ = has_cpu_virt_extensions();
    }

    #[test]
    fn container_detection_does_not_panic() {
        let _ = is_container();
    }

    #[test]
    fn wsl2_detection_does_not_panic() {
        let _ = is_wsl2();
    }

    #[test]
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
