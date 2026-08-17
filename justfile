# abox — justfile for common development tasks
#
# Install just: https://github.com/casey/just
# Run `just` to see all available recipes.

# Default recipe: run all checks
default: check

# ─── Development ─────────────────────────────────────────────────────────────

# Run all checks (format, lint, test)
check: fmt-check lint test

# Format all Rust code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Run clippy with strict warnings
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run all tests
test:
    cargo test --workspace

# Run tests with output shown
test-verbose:
    cargo test --workspace -- --nocapture

# Build all crates in debug mode
build:
    cargo build --workspace

# Build all crates in release mode
build-release:
    cargo build --workspace --release

# Build the static musl guest binaries (abox-shim + abox-bridge) for the
# HOST architecture. Used by the runtime e2e suite and by developers
# staging fresh guest binaries into <state_dir>/guest/<arch>/.
build-guest-bins:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -m)" in
        arm64|aarch64)  TARGET="aarch64-unknown-linux-musl" ;;
        x86_64|amd64)   TARGET="x86_64-unknown-linux-musl" ;;
        *) echo "unsupported host arch: $(uname -m)" >&2; exit 1 ;;
    esac
    RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes" \
        cargo build --release --target "$TARGET" -p abox-shim
    echo "guest binaries: target/$TARGET/release/{abox-shim,abox-bridge}"

# ─── Quality ─────────────────────────────────────────────────────────────────

# Run cargo-deny for supply chain auditing (install: cargo install cargo-deny)
deny:
    cargo deny check

# Run all quality checks (what CI runs)
ci: fmt-check lint test deny

# ─── Validation Tiers ────────────────────────────────────────────────────────

# Tier 1: CI-safe checks (fmt + clippy + test + supply-chain audit).
# No virtualization needed.
tier-ci: check deny

# Tier 2: Criterion microbenchmarks for policy evaluation and proxy
# serialization. No runtime needed.
tier-bench: bench

# Tier 3: live runtime end-to-end suite (real microVMs). Skips cleanly when
# the MicroSandbox runtime assets under $MSB_HOME are absent or hardware
# virtualization is missing.
e2e-runtime:
    ./scripts/local/msb_e2e_test.sh

# Tier 4: Agent smoke tests — real Claude/Codex API calls (requires the
# runtime + credentials). Costs tokens.
tier-smoke:
    ./scripts/local/agent_smoke_test.sh

# Run all pre-release validation tiers, attest what passes.
pre-release:
    ./scripts/pre_release.sh

# ─── Cleanup ─────────────────────────────────────────────────────────────────

# Remove build artifacts
clean:
    cargo clean

# ─── Documentation ───────────────────────────────────────────────────────────

# Generate and open rustdoc
doc:
    cargo doc --workspace --no-deps --open

# Generate rustdoc without opening
doc-build:
    cargo doc --workspace --no-deps

# ─── Utilities ───────────────────────────────────────────────────────────────

# Count lines of code (requires tokei: cargo install tokei)
loc:
    tokei crates/

# Show dependency tree
deps:
    cargo tree --workspace

# Check for outdated dependencies (requires cargo-outdated)
outdated:
    cargo outdated --workspace

# ─── Benchmarks ─────────────────────────────────────────────────────────────

# Run criterion microbenchmarks (policy, serialization). No runtime needed.
bench:
    cargo bench -p abox-core

# ─── Release ────────────────────────────────────────────────────────────────

# Cut a release: bump version, run all checks, update changelog, tag.
release version:
    ./scripts/release.sh {{version}}

# Dry-run a release (no commit, no tag — shows what would happen).
release-dry version:
    ./scripts/release.sh {{version}} --dry
