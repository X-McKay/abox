# Claude Code Agent Instructions for `abox`

## Project Overview

`abox` is a Rust workspace for parallel AI agent sandboxing using Cloud Hypervisor microVMs, git worktrees, and a dual-layer credential proxy. It follows Hexagonal Architecture (Ports & Adapters).

## Workspace Structure

| Crate | Type | Purpose |
|---|---|---|
| `abox-core` | Library | Domain logic: workspace, VM, policy, config, snapshot, protocol types |
| `abox-cli` | Binary (`abox`) | CLI commands + TUI dashboard |
| `abox-proxyd` | Binary | Host-side credential proxy daemon |
| `abox-shim` | Binary | Guest-side static musl binary (minimal deps, no tokio) |

## Development Workflow

### Before Making Changes

1. Read the relevant source files to understand the current implementation.
2. Read `.plans/implementation-plan.md` for architectural context and design decisions.
3. Read `docs/decisions/001-architecture.md` for the ADR.

### Making Changes

1. **Write code** following the patterns already established in the crate.
2. **Add tests** for all new functionality. Unit tests go in the same file (`#[cfg(test)] mod tests`). Integration tests go in `crates/<crate>/tests/`.
3. **Run the quality gate** before committing:
   ```bash
   just check   # or: cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
   ```

### Quality Standards

- **Zero warnings:** `cargo clippy --workspace --all-targets -- -D warnings` must pass clean.
- **Formatting:** `cargo fmt --all -- --check` must pass clean.
- **All tests pass:** `cargo test --workspace` must pass.
- **No `unwrap()` in library code** (`abox-core`). Use `?` with `anyhow::Result` or typed errors from `abox_core::error`.
- **`abox-shim` stays minimal.** It must not depend on `abox-core`, `tokio`, or any heavy crate. It compiles as a static musl binary.

### Commit Messages

Use conventional commits:
- `feat:` for new features
- `fix:` for bug fixes
- `refactor:` for code restructuring
- `test:` for adding tests
- `docs:` for documentation changes
- `chore:` for tooling and CI changes

### Key Patterns

**Hexagonal Architecture:** Domain logic lives in `abox-core` as traits (ports). Implementations (adapters) live in `abox_core::adapters`. The CLI and proxyd depend on `abox-core` but `abox-core` never depends on them.

**Shared Protocol Types:** The proxy request/response types are defined in `abox_core::protocol`. The shim duplicates these types locally (documented) because it cannot depend on `abox-core`.

**Policy Engine:** Credential policies are defined in TOML files (see `policies/default.toml`). The `PolicyEngine` compiles regex patterns at load time and evaluates them per-request.

**Error Types:** Typed errors are in `abox_core::error` using `thiserror`. Application code uses `anyhow::Result` for propagation.

**Utilities:** Shared helpers (format_size, sanitize_task_id, wait_for_socket) are in `abox_core::util`.

## Testing

### Running Tests
```bash
# All tests
cargo test --workspace

# Specific crate
cargo test -p abox-core

# Specific test
cargo test -p abox-core test_policy_deny_takes_precedence

# With output
cargo test --workspace -- --nocapture
```

### Test Categories
- **Unit tests** (`#[test]`): Run everywhere, no external dependencies.
- **Async tests** (`#[tokio::test]`): For orchestrator and proxy tests.
- **Integration tests** (`#[ignore]`): Require KVM/Cloud Hypervisor. Run with `cargo test -- --ignored`.

## Files You Should NOT Modify Without Good Reason

- `Cargo.toml` (workspace root): Lint policy, dependency versions.
- `rustfmt.toml`, `clippy.toml`: Formatting and lint configuration.
- `rust-toolchain.toml`: Pinned Rust version.
- `.pre-commit-config.yaml`: Pre-commit hooks.
- `deny.toml`: Supply chain audit policy.
