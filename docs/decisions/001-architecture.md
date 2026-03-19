# ADR-001: Core Architecture Decisions

**Status:** Accepted
**Date:** 2026-03-18

## Context

We are building `abox`, a tool for running parallel AI coding agents in isolated sandboxes with git worktree integration, secure credential passing, and a central management interface.

## Decisions

### 1. Agent Inside the Sandbox

**Decision:** The AI agent (Claude Code, Cursor, etc.) runs inside the VM, not outside it.

**Rationale:** Running the agent inside the sandbox is both simpler and safer. The agent gets direct shell and filesystem access — exactly what it's designed for — and the sandbox boundary becomes invisible to it. This eliminates the need for an MCP intermediary layer and removes an entire class of latency and complexity. The tradeoff is higher per-sandbox resource usage (the agent runtime itself consumes memory/CPU), but this is a linear cost.

**Alternatives Considered:**
- Agent outside, reaching in via MCP tools (rejected: adds latency, re-implements agent tool interfaces)
- Agent outside, reaching in via SSH (rejected: similar overhead, requires SSH daemon management)

### 2. Cloud Hypervisor over Firecracker

**Decision:** Use Cloud Hypervisor as the VMM instead of Firecracker.

**Rationale:** Cloud Hypervisor natively supports virtiofs for filesystem sharing, which is essential for mounting git worktrees into the VM with full read-write access. Firecracker does not support virtiofs (as of 2026), requiring workarounds like 9p over vsock or block device snapshots. Cloud Hypervisor also supports VSock, snapshot/restore, and has a similar security profile to Firecracker.

**Alternatives Considered:**
- Firecracker (rejected: no virtiofs support, would need filesystem workarounds)
- QEMU (rejected: larger attack surface, slower boot times)
- Docker containers (rejected: weaker isolation boundary, shared kernel attack surface)

### 3. Dual-Layer Credential Proxy

**Decision:** Implement both a CLI proxy (for commands like git, aws, gh) and an HTTP egress proxy (for API keys).

**Rationale:** AI agents interact with external services through two channels: CLI tools and HTTP APIs. The CLI proxy intercepts commands via symlinks (abox-shim), evaluates them against policy, and executes them on the host with real credentials. The HTTP egress proxy intercepts outbound HTTPS traffic and injects API keys based on destination domain. Together, they ensure no credential ever enters the VM.

**Alternatives Considered:**
- Environment variables inside the VM (rejected: credentials visible to any process)
- Single HTTP-only proxy (rejected: doesn't cover CLI tools like git with SSH)
- OneCLI/Vault-style secret manager (considered for future integration)

### 4. Hexagonal Architecture

**Decision:** Use ports-and-adapters (hexagonal) architecture with trait-based abstraction.

**Rationale:** The core domain logic (workspace management, VM lifecycle, policy evaluation) is defined as traits (ports). Concrete implementations (git2, Cloud Hypervisor, etc.) are adapters. This enables testing domain logic without real VMs, and allows swapping backends (e.g., Docker for development, Cloud Hypervisor for production) without changing application code.

### 5. Git Worktrees for Workspace Isolation

**Decision:** Each sandbox gets its own git worktree on a dedicated branch.

**Rationale:** Git worktrees provide true filesystem isolation between parallel agents while sharing the same repository history. Each agent works on its own `agent/<task-id>` branch, and the divergence matrix shows which files each agent has modified, enabling early conflict detection before merge.

**Alternatives Considered:**
- Full git clones (rejected: wastes disk space, slow for large repos)
- Overlay filesystems (rejected: doesn't integrate with git branching model)
