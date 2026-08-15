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

# Rebuild the guest rootfs from the current init.sh + musl shim.
# Requires `just bootstrap-vm` to have populated ~/.abox/vm first.
rebuild-rootfs: build-shim
    #!/usr/bin/env bash
    set -euo pipefail
    ABOX_VM_DIR="${ABOX_VM_DIR:-$HOME/.abox/vm}"
    SHIM_BIN="$PWD/target/x86_64-unknown-linux-musl/release/abox-shim"
    GUEST_INIT="$PWD/guest/init.sh"
    if [ ! -f "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" ]; then
        echo "ERROR: Alpine minirootfs not found at $ABOX_VM_DIR/alpine-minirootfs.tar.gz" >&2
        echo "Run 'just bootstrap-vm' first." >&2
        exit 1
    fi
    ABOX_VM_DIR="$ABOX_VM_DIR" SHIM_BIN="$SHIM_BIN" GUEST_INIT="$GUEST_INIT" \
        ./scripts/build_rootfs.sh

# Check whether the installed guest rootfs image is stale — i.e. whether
# guest/init.sh, the shim, or the rootfs builder inputs have changed on
# disk since the rootfs was last built. Does NOT rebuild; just reports and
# exits 1 if stale.
check-rootfs:
    #!/usr/bin/env bash
    set -euo pipefail
    STAMP="$HOME/.abox/vm/rootfs.raw.inputs"
    if [ ! -f "$STAMP" ]; then
        echo "no rootfs input stamp found at $STAMP — rebuild the rootfs to create one"
        exit 0
    fi
    RECORDED_INIT=$(grep '^init_sh=' "$STAMP" | cut -d= -f2)
    RECORDED_SHIM=$(grep '^shim=' "$STAMP" | cut -d= -f2)
    RECORDED_BUILD=$(grep '^build_rootfs_sh=' "$STAMP" | cut -d= -f2)
    RECORDED_DOCKERFILE=$(grep '^rootfs_builder_dockerfile=' "$STAMP" | cut -d= -f2)
    CURRENT_INIT=$(sha256sum guest/init.sh | cut -d' ' -f1)
    CURRENT_SHIM=$(sha256sum target/x86_64-unknown-linux-musl/release/abox-shim | cut -d' ' -f1)
    CURRENT_BUILD=$(sha256sum scripts/build_rootfs.sh | cut -d' ' -f1)
    CURRENT_DOCKERFILE=$(sha256sum scripts/rootfs-builder.Dockerfile | cut -d' ' -f1)
    if [ "$RECORDED_INIT" != "$CURRENT_INIT" ] || \
       [ "$RECORDED_SHIM" != "$CURRENT_SHIM" ] || \
       [ "$RECORDED_BUILD" != "$CURRENT_BUILD" ] || \
       [ "$RECORDED_DOCKERFILE" != "$CURRENT_DOCKERFILE" ]; then
        echo "⚠  rootfs is STALE: guest/init.sh, the shim, or the rootfs builder changed since last rebuild"
        echo "   init_sh:                 recorded: $RECORDED_INIT"
        echo "                            current:  $CURRENT_INIT"
        echo "   shim:                    recorded: $RECORDED_SHIM"
        echo "                            current:  $CURRENT_SHIM"
        echo "   build_rootfs.sh:         recorded: $RECORDED_BUILD"
        echo "                            current:  $CURRENT_BUILD"
        echo "   rootfs-builder.Dockerfile recorded: $RECORDED_DOCKERFILE"
        echo "                            current:  $CURRENT_DOCKERFILE"
        echo "   rebuild the rootfs to update"
        exit 1
    fi
    echo "✓ rootfs matches current guest/init.sh, shim, and builder inputs"

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

# MicroSandbox-era additions (ADR-008): `just e2e-runtime` runs the live
# MicroSandbox e2e suite (scripts/local/msb_e2e_test.sh; skips cleanly when
# the msb runtime assets are absent), and `just build-guest-bins` builds the
# static musl guest binaries (abox-shim + abox-bridge) it stages into guests.

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

# Run the MicroSandbox live e2e suite (real microVMs; skips cleanly when the
# msb runtime assets under $MSB_HOME are absent or virtualization is missing).
e2e-runtime:
    ./scripts/local/msb_e2e_test.sh

# Build the static musl guest binaries (abox-shim + abox-bridge) for the
# HOST architecture. Used by the MicroSandbox e2e suite and by developers
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
