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

# Build the shim as a static musl binary (requires musl target)
build-shim:
    cargo build --release --target x86_64-unknown-linux-musl -p abox-shim

# Check whether the installed guest rootfs image is stale — i.e. whether
# guest/init.sh has changed on disk since the rootfs was last built. Does
# NOT rebuild; just reports and exits 1 if stale.
check-rootfs:
    #!/usr/bin/env bash
    set -euo pipefail
    STAMP="$HOME/.abox/vm/rootfs.raw.inputs"
    if [ ! -f "$STAMP" ]; then
        echo "no rootfs input stamp found at $STAMP — rebuild the rootfs to create one"
        exit 0
    fi
    RECORDED_INIT=$(grep '^init_sh=' "$STAMP" | cut -d= -f2)
    CURRENT_INIT=$(sha256sum guest/init.sh | cut -d' ' -f1)
    if [ "$RECORDED_INIT" != "$CURRENT_INIT" ]; then
        echo "⚠  rootfs is STALE: guest/init.sh has changed since last rebuild"
        echo "   recorded: $RECORDED_INIT"
        echo "   current:  $CURRENT_INIT"
        echo "   rebuild the rootfs to update"
        exit 1
    fi
    echo "✓ rootfs matches current guest/init.sh"

# ─── Quality ─────────────────────────────────────────────────────────────────

# Run cargo-deny for supply chain auditing (install: cargo install cargo-deny)
deny:
    cargo deny check

# Run all quality checks (what CI runs)
ci: fmt-check lint test deny

# ─── Pre-Release Validation ─────────────────────────────────────────────────

# Run all pre-release validation tiers, attest what passes.
pre-release:
    ./scripts/pre_release.sh

# Tier 1: CI-safe checks (fmt + clippy + test + supply-chain audit). No KVM needed.
tier-ci: check deny

# Tier 2: VM end-to-end tests (requires KVM + bootstrapped VM). Checks rootfs freshness first.
tier-vm: check-rootfs e2e-vm

# Tier 3: Benchmarks — criterion microbenchmarks + VM latency (requires KVM + bootstrapped VM).
tier-bench: bench
    just bench-vm-n 5

# Tier 4: Agent smoke tests — real Claude/Codex API calls (requires KVM + credentials). Costs tokens.
tier-smoke:
    ./scripts/local/agent_smoke_test.sh

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

# Run criterion microbenchmarks (policy, serialization, boot meta). No VM needed.
bench:
    cargo bench -p abox-core

# Run real VM latency benchmarks (requires bootstrap + /dev/kvm).
bench-vm:
    ./scripts/local/bench.sh

# Run VM latency benchmarks averaged over N runs.
bench-vm-n n="5":
    ./scripts/local/bench.sh --runs {{n}}

# ─── Release ────────────────────────────────────────────────────────────────

# Cut a release: bump version, run all checks + benchmarks, update README, tag.
release version:
    ./scripts/release.sh {{version}}

# Dry-run a release (no commit, no tag — shows what would happen).
release-dry version:
    ./scripts/release.sh {{version}} --dry

# ─── VM ──────────────────────────────────────────────────────────────────────

# Bootstrap the host: download cloud-hypervisor, virtiofsd, kernel, and rootfs.
bootstrap-vm:
    ./scripts/bootstrap_vm.sh

# Wipe the local VM install (does not touch the vendor cache).
clean-vm:
    rm -rf ~/.abox/vm

# Run the e2e test, including phase 6 (live VM) if the bootstrap is present.
e2e-vm:
    ./scripts/local/e2e_test.sh
