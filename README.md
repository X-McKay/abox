# abox — Parallel AI Agent Sandboxing

`abox` is a lightweight, secure tool for running multiple AI coding agents in parallel, isolated sandboxes. It combines **git worktrees** with **microVMs** (Cloud Hypervisor) to provide agents with independent workspaces, while securely proxying credentials via a dual-layer interception architecture.

## Why `abox`?

When running multiple autonomous agents on a single codebase, you face three problems:
1. **Workspace collisions:** Agents stepping on each other's git branches and files.
2. **Credential leaks:** Giving agents direct access to your AWS or GitHub tokens is dangerous.
3. **Host system risk:** Agents running `rm -rf /` or installing malware.

`abox` solves this by:
- Isolating each agent in a fast-booting **Cloud Hypervisor microVM**.
- Mounting independent **git worktrees** into the VM via `virtiofs`.
- Proxying commands and HTTP requests out of the VM through a **strict, TOML-configured policy engine**.

## Architecture

`abox` is built in Rust using a Hexagonal (Ports & Adapters) architecture.

1. **`abox-core`**: Domain logic (Workspace manager, VM lifecycle, Policy engine).
2. **`abox-cli`**: The user interface (CLI commands and TUI dashboard).
3. **`abox-proxyd`**: The host-side daemon that evaluates policies and executes allowed commands.
4. **`abox-shim`**: A static musl binary injected into the guest VM that intercepts commands (via symlinks) and forwards them to `proxyd`.

![Architecture](.plans/architecture-diagram.png)

## Getting Started

### Prerequisites

- Linux host with `/dev/kvm` accessible to your user
- Rust toolchain (`cargo`)
- `just` command runner (`cargo install just`)

### Installation

```bash
git clone https://github.com/X-McKay/abox.git
cd abox
cargo build --release
just bootstrap-vm     # downloads the VMM, kernel, and builds the guest rootfs
```

See [`docs/vm-setup.md`](docs/vm-setup.md) for the full setup walkthrough,
including how to boot a real sandbox with `abox run --task X -- claude`.

### Configuration

Copy the example configuration to your home directory:

```bash
mkdir -p ~/.abox/policies
cp templates/config.example.toml ~/.abox/config.toml
cp policies/default.toml ~/.abox/policies/default.toml
```

By default, abox stores all state under `~/.abox/` (worktrees, templates,
logs, and the runtime socket directory). No root access required.

### Usage

1. **Start an agent sandbox:**
   ```bash
   abox run --task fix-auth --base main --command "claude"
   ```

2. **List running sandboxes:**
   ```bash
   abox list
   ```

3. **Check divergence across agents:**
   ```bash
   abox divergence
   ```

4. **Merge a completed task:**
   ```bash
   abox merge fix-auth
   ```

## Development

We use `just` as our command runner. Install it with `cargo install just`.

- `just check`: Run formatting, lints, and tests.
- `just lint`: Run clippy with strict warnings.
- `just build-shim`: Build the guest shim (requires `x86_64-unknown-linux-musl` target).

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed development guidelines.

## License

Apache 2.0
