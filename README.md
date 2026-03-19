# abox

**Parallel AI Agent Sandboxing with MicroVMs**

`abox` is a lightweight, self-hosted tool for running multiple AI coding agents in parallel, each in a hardware-isolated MicroVM with its own git worktree, secure credential proxying, and a central management interface.

## Key Features

- **Hardware Isolation:** Each agent runs inside a Cloud Hypervisor MicroVM with KVM. No agent action can impact the host.
- **Git Worktree Integration:** Each sandbox gets its own branch and worktree, shared with the VM via `virtiofs` for instant, bidirectional filesystem access.
- **Dual-Layer Credential Proxy:** CLI tools (git, aws) are proxied via a shim over VSock. HTTP API keys are injected by an egress proxy. Credentials never enter the VM.
- **Agent Agnostic:** Run Claude Code, Cursor, custom Python agents, or anything else — unmodified — inside the sandbox.
- **Central Management:** CLI (`abox`) and TUI dashboard for managing all sandboxes, viewing divergence across branches, and reviewing audit logs.
- **Snapshot Templates:** Pre-build VM images with your tools installed, then fork new sandboxes in sub-second time.

## Architecture

```
Host: abox CLI/TUI → abox-core (Workspace + VM Manager) → Cloud Hypervisor + virtiofsd
                   → abox-proxyd (CLI Proxy + Egress Proxy + Policy Engine)

Guest: Agent (unmodified) → abox-shim (intercepts git/aws) → VSock → Host Proxy
                          → HTTPS_PROXY → Host Egress Proxy
```

## Quick Start

```bash
# Start 3 parallel agents
abox run --task fix-auth -- claude --print "Fix the auth bug"
abox run --task add-tests -- claude --print "Add unit tests for payments"
abox run --task refactor-db -- claude --print "Refactor DB to use connection pooling"

# See what files each agent is changing
abox divergence

# Attach to a sandbox terminal
abox attach fix-auth

# Merge completed work
abox merge fix-auth

# Open the TUI dashboard
abox tui
```

## Prerequisites

- Linux with KVM support (`/dev/kvm`)
- [Cloud Hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor) installed
- [virtiofsd](https://gitlab.com/virtio-fs/virtiofsd) installed
- Git

## Project Structure

```
abox/
├── crates/
│   ├── abox-cli/        # CLI + TUI (clap, ratatui)
│   ├── abox-core/       # Domain logic: workspace, VM, snapshot, config
│   ├── abox-proxyd/     # Host-side credential proxy daemon
│   └── abox-shim/       # Guest-side credential shim (static musl binary)
├── policies/            # TOML policy templates for credential proxy
├── templates/           # Guest image build scripts
└── .plans/              # Architecture and implementation plans
```

## Development

```bash
# Build all crates
cargo build --workspace

# Run unit tests
cargo test --workspace --lib

# Run clippy
cargo clippy --workspace -- -D warnings

# Format code
cargo fmt --all
```

## License

Apache-2.0
