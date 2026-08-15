# Contributing to `abox`

Welcome! We're excited you want to contribute to `abox`. This document outlines our development process, architecture, and quality standards.

## Development Environment

### 1. Prerequisites
- **Rust Toolchain:** Install via [rustup](https://rustup.rs/). abox tracks the **latest stable** Rust (`rust-toolchain.toml` pins `channel = "stable"`); there is no fixed MSRV. CI runs `clippy -D warnings` on stable, so validate on stable before pushing — note that a `RUSTUP_TOOLCHAIN` env var, if set, overrides `rust-toolchain.toml` and can mask new-stable lints (run `rustup update stable` and `RUSTUP_TOOLCHAIN=stable just check`).
- **just:** Command runner. Install with `cargo install just`.
- **libcap-ng headers (Linux only):** the libkrun device layer links against libcap-ng. Install `libcap-ng-dev` (Debian/Ubuntu) or `libcap-ng-devel` (Fedora) before building.
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
- `just build-guest-bins`: Builds the guest binaries (`abox-shim` + `abox-bridge`) as static musl binaries for the host architecture.
- `just doc`: Generates and opens the rustdoc.

## Architecture

`abox` follows a Hexagonal (Ports & Adapters) architecture, implemented as a Cargo workspace with four crates:

1. **`abox-core`**: The domain layer. Defines ports (`WorkspacePort`, `SandboxRuntimePort`) and implements adapters (`Git2Workspace` and `adapters::microsandbox`, the MicroSandbox runtime). Also contains the policy engine (including network-plan compilation), the command/request brokers, config parsing, and the embedded guest image manifest.
2. **`abox-cli`**: The user interface. Implements the `abox` CLI commands and the `ratatui` dashboard.
3. **`abox-proxyd`**: The host-side daemon. Listens on Unix sockets, evaluates policies, and executes allowed commands.
4. **`abox-shim`**: The guest-side binaries. `abox-shim` is injected into the sandbox to intercept commands and forward them to the host broker; `abox-bridge` forwards guest loopback/Unix-socket traffic over vsock. Both must remain small, static, synchronous binaries.

### Key Design Principles

1. **Agent Inside the microVM:** Agents run unmodified inside a hardware-isolated MicroSandbox microVM (ADR-008). The sandbox boundary is invisible to them.
2. **Dual-Layer Proxy:** Credentials never enter the sandbox. CLI commands are proxied via `abox-shim` over vsock. HTTP API requests are proxied via an egress CONNECT tunnel that injects authorization headers.
3. **Git Worktrees:** Each sandbox gets its own git worktree, bind-mounted into the guest at `/workspace`. This provides instant filesystem access without copying.
4. **Policy-Owned Security Semantics:** abox compiles network modes and credential rules into runtime plans itself; the runtime translates them mechanically and never widens them. Unrepresentable configurations fail closed.

## Code Quality Standards

We enforce strict code quality via CI and pre-commit hooks:

- **Formatting:** `cargo fmt` must pass cleanly.
- **Linting:** `cargo clippy` runs with `-D warnings`. We use a workspace-level lint policy defined in the root `Cargo.toml`.
- **Error Handling:** Use `thiserror` for library error types (in `abox-core::error`). Use `anyhow` for application-level error propagation (in `abox-cli` and `abox-proxyd`). Never `unwrap()` or `expect()` in library code unless mathematically provable.
- **Testing:** All new functionality must include unit tests. Integration tests should be added to `abox-core/tests/` or `abox-proxyd/tests/`.
- **Documentation:** Public APIs must be documented with rustdoc (`///`). We enforce `missing_docs` (currently suppressed while the API stabilizes, but will be enabled soon).

## Running the Test Tiers

Tests are organized into tiers by what the host must provide (the canonical
table lives in [`AGENTS.md`](AGENTS.md#test-tiers)):

- **`just tier-ci`** — fmt + clippy + tests + `cargo deny`. Runs anywhere;
  this is what CI runs, and the minimum bar for every PR.
- **`just e2e-runtime`** — the live MicroSandbox end-to-end suite
  (`scripts/local/msb_e2e_test.sh`): boots real microVMs and exercises exit
  codes, workspace isolation, the command broker (allow/deny/audit), HTTPS
  egress, and filesystem escape attempts. Requires virtualization (KVM or
  Hypervisor.framework on Apple Silicon) + the msb runtime assets under
  `$MSB_HOME`; skips cleanly otherwise. Run it after any
  runtime/guest/broker change.
- **`just tier-smoke`** — real Claude/Codex API calls through the MITM proxy
  (requires the runtime + credentials; costs tokens).

To add an assertion to the e2e script, append a `section "..."` block
with `step` / `how` / `expect` / `pass` / `fail` calls — the summary footer
counts every `pass`/`fail` invocation automatically.

## Subagent-Driven Implementation

Multi-step work in this repo is typically done via the
`superpowers:subagent-driven-development` workflow: write a plan
under `docs/plans/YYYY-MM-DD-<topic>.md`, then execute it
task-by-task with TDD and frequent commits. This keeps the change
log readable and lets the project evolve in small, reviewable steps.

An example: `docs/plans/2026-04-08-vm-e2e-hardening.md`,
which drained 13 P0/P1/P2 backlog items in one session, every
behavior change gated on TDD and the quality gates of the time.

## Pull Request Process

Branching truth lives in [`docs/contributing/branching.md`](docs/contributing/branching.md) and [`AGENTS.md`](AGENTS.md): every change reaches `main` through a typed feature branch and a reviewed PR.

1. Create a typed feature branch from an up-to-date `main`: `git checkout -b feat/my-new-thing` (see the prefix table in `branching.md`).
2. Write your code and tests.
3. Run `just check` locally, then walk [`docs/contributing/pre-pr-checklist.md`](docs/contributing/pre-pr-checklist.md) — it covers the runtime-attestation requirement for runtime/guest/proxy diffs.
4. Push your branch and open a PR against `main`.
5. CI will run formatting, lints, tests, `cargo-deny`, and the `runtime-attestation` label gate. All required checks must pass before merging (squash-merge).

Thank you for contributing!
