# Agent Skills for `abox`

This file defines structured development skills that any AI coding agent should follow when working on the `abox` codebase. These skills ensure consistency, quality, and safety.

---

## Skill: Implement a New Feature

### Trigger
When asked to add new functionality to `abox`.

### Process
1. **Understand the architecture.** Read `.plans/implementation-plan.md` and identify which crate(s) the feature belongs in.
2. **Check the port/adapter pattern.** If the feature involves a new external dependency (e.g., a new VM backend, a new VCS), define a port trait in `abox-core` and implement the adapter separately.
3. **Write the implementation.** Follow existing patterns in the crate.
4. **Write tests.** Unit tests in the same file, integration tests in `crates/<crate>/tests/`.
5. **Run quality checks.** `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
6. **Commit with a conventional commit message.**

---

## Skill: Fix a Bug

### Trigger
When asked to fix a bug or failing test.

### Process
1. **Reproduce the bug.** Write a failing test first.
2. **Fix the code.** Make the minimal change needed.
3. **Verify the fix.** Run `cargo test --workspace` to ensure the new test passes and no existing tests regress.
4. **Run the full quality gate.** `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
5. **Commit with `fix:` prefix.**

---

## Skill: Refactor Code

### Trigger
When asked to simplify, restructure, or improve code quality.

### Process
1. **Run the full test suite first** to establish a baseline: `cargo test --workspace`.
2. **Make the refactoring changes.**
3. **Run the full test suite again** to verify no regressions.
4. **Run clippy** to verify no new warnings: `cargo clippy --workspace --all-targets -- -D warnings`.
5. **Run formatting**: `cargo fmt --all`.
6. **Commit with `refactor:` prefix.**

---

## Skill: Add a New Crate to the Workspace

### Trigger
When a new crate needs to be added to the `abox` workspace.

### Process
1. Create the crate directory under `crates/`.
2. Add the crate to the `members` list in the root `Cargo.toml`.
3. Add `[lints] workspace = true` to the new crate's `Cargo.toml`.
4. Use `version.workspace = true`, `edition.workspace = true`, `license.workspace = true`, `repository.workspace = true` for package metadata.
5. Prefer workspace dependencies (`dep.workspace = true`) over inline version specs.
6. Verify with `cargo check --workspace`.

---

## Skill: Modify the Policy Engine

### Trigger
When changing how credential policies are evaluated.

### Process
1. Read `abox_core::policy` and `policies/default.toml` to understand the current model.
2. Make changes to the `PolicyEngine` or `PolicyFile` structs.
3. Update `policies/default.toml` if the schema changed.
4. Add or update tests in `abox_core::policy::tests` and `crates/abox-core/tests/integration_tests.rs`.
5. Verify the `test_policy_load_real_default_policy` test still passes (it loads the actual `policies/default.toml` file).
6. Run the full quality gate.

---

## Skill: Update the Guest Shim

### Trigger
When modifying `abox-shim`.

### Process
1. **Remember the constraints:** The shim must remain a small, static, synchronous binary. No `tokio`, no `abox-core` dependency, no heavy crates.
2. If you change the protocol (request/response JSON format), you must also update `abox_core::protocol` to keep them in sync. The shim's types are documented as mirrors.
3. Test by building with the musl target: `cargo build --release --target x86_64-unknown-linux-musl -p abox-shim`.
4. Run the full quality gate.

---

## Quality Gate (Run Before Every Commit)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must pass with zero errors and zero warnings.
