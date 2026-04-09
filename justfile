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

# ─── Quality ─────────────────────────────────────────────────────────────────

# Run cargo-deny for supply chain auditing (install: cargo install cargo-deny)
deny:
    cargo deny check

# Run all quality checks (what CI runs)
ci: fmt-check lint test deny

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
    ./scripts/bench.sh

# Run VM latency benchmarks averaged over N runs.
bench-vm-n n="5":
    ./scripts/bench.sh --runs {{n}}

# ─── VM ──────────────────────────────────────────────────────────────────────

# Bootstrap the host: download cloud-hypervisor, virtiofsd, kernel, and rootfs.
bootstrap-vm:
    ./scripts/bootstrap_vm.sh

# Wipe the local VM install (does not touch the vendor cache).
clean-vm:
    rm -rf ~/.abox/vm

# Run the e2e test, including phase 6 (live VM) if the bootstrap is present.
e2e-vm:
    ./scripts/e2e_test.sh
