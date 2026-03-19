# Contributing to `abox`

Welcome! We're excited you want to contribute to `abox`. This document outlines our development process, architecture, and quality standards.

## Development Environment

### 1. Prerequisites
- **Rust Toolchain:** Install via [rustup](https://rustup.rs/). We use the `1.75` edition.
- **just:** Command runner. Install with `cargo install just`.
- **cargo-deny:** Supply chain auditing. Install with `cargo install cargo-deny`.
- **Pre-commit:** Git hooks. Install via `pip install pre-commit && pre-commit install`.

### 2. Initial Setup
```bash
git clone https://github.com/X-McKay/abox.git
cd abox
pre-commit install
```

### 3. Common Workflows
We use `just` to simplify common commands. Run `just` to see all available recipes.

- `just check`: Runs the full quality suite (formatting, clippy, tests).
- `just build`: Builds all crates in debug mode.
- `just build-shim`: Builds the guest shim as a static musl binary (requires `x86_64-unknown-linux-musl` target).
- `just doc`: Generates and opens the rustdoc.

## Architecture

`abox` follows a Hexagonal (Ports & Adapters) architecture, implemented as a Cargo workspace with four crates:

1. **`abox-core`**: The domain layer. Defines ports (`WorkspacePort`, `VmPort`) and implements adapters (`Git2Workspace`, `CloudHypervisorAdapter`). Also contains the policy engine and config parsing.
2. **`abox-cli`**: The user interface. Implements the `abox` CLI commands and the `ratatui` dashboard.
3. **`abox-proxyd`**: The host-side daemon. Listens on Unix sockets, evaluates policies, and executes allowed commands.
4. **`abox-shim`**: The guest-side binary. Injected into the VM to intercept commands and forward them to `proxyd`. Must remain a small, static, synchronous binary.

### Key Design Principles

1. **Agent Inside the VM:** Agents run unmodified inside the Cloud Hypervisor VM. The sandbox boundary is invisible to them.
2. **Dual-Layer Proxy:** Credentials never enter the VM. CLI commands are proxied via `abox-shim` over VSock. HTTP API requests are proxied via an egress CONNECT tunnel that injects authorization headers.
3. **Git Worktrees:** Each sandbox gets its own git worktree, mounted into the VM via `virtiofs`. This provides instant filesystem access without copying.
4. **Sub-second Boot:** We use Cloud Hypervisor snapshots to boot new agent sandboxes from a pre-warmed template in milliseconds.

## Code Quality Standards

We enforce strict code quality via CI and pre-commit hooks:

- **Formatting:** `cargo fmt` must pass cleanly.
- **Linting:** `cargo clippy` runs with `-D warnings`. We use a workspace-level lint policy defined in the root `Cargo.toml`.
- **Error Handling:** Use `thiserror` for library error types (in `abox-core::error`). Use `anyhow` for application-level error propagation (in `abox-cli` and `abox-proxyd`). Never `unwrap()` or `expect()` in library code unless mathematically provable.
- **Testing:** All new functionality must include unit tests. Integration tests should be added to `abox-core/tests/` or `abox-proxyd/tests/`.
- **Documentation:** Public APIs must be documented with rustdoc (`///`). We enforce `missing_docs` (currently suppressed while the API stabilizes, but will be enabled soon).

## Pull Request Process

1. Create a feature branch from `develop`: `git checkout -b feature/my-new-thing`
2. Write your code and tests.
3. Run `just check` locally to ensure all quality gates pass.
4. Push your branch and open a PR against `develop`.
5. CI will run formatting, lints, tests, and `cargo-deny`. All checks must pass before merging.

Thank you for contributing!
