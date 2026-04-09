#!/usr/bin/env bash
#
# abox release script.
#
# Bumps the workspace version, runs the full quality + test + benchmark
# suite, updates the benchmark table in README.md, generates a changelog
# entry, builds the release binary, and commits + tags the result.
#
# Usage:
#   ./scripts/release.sh 0.2.0          # bump to 0.2.0
#   ./scripts/release.sh 0.2.0 --dry    # show what would happen, don't commit
#
# Requirements:
#   - Clean git working tree (no uncommitted changes)
#   - Bootstrapped VM stack (~/.abox/vm/) + /dev/kvm for VM benchmarks
#   - just, cargo, python3 (for sed-free JSON extraction in the bench script)
#
# The script does NOT push. After it completes, review the commit and run:
#   git push origin main --tags

set -euo pipefail

# ─── Argument parsing ─────────────────────────────────────────────────────────
VERSION=""
DRY_RUN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry|--dry-run) DRY_RUN=1; shift ;;
        --help|-h)
            cat <<EOF
Usage: $(basename "$0") <version> [--dry]

  <version>    Semver version to release (e.g. 0.2.0)
  --dry        Show what would happen without committing or tagging
  -h, --help   This message

Steps performed:
  1. Validate version format and working tree cleanliness
  2. Bump workspace version in Cargo.toml + Cargo.lock
  3. cargo fmt --check + cargo clippy + cargo test
  4. ./scripts/e2e_test.sh (all phases)
  5. cargo build --release
  6. VM latency benchmarks (5 runs, averaged)
  7. Update benchmark table in README.md
  8. Save full benchmark JSON to benchmarks/<version>.json
  9. Generate CHANGELOG.md entry from git log since last tag
  10. cargo install --path crates/abox-cli (refresh local binary)
  11. Commit version bump + benchmarks + changelog
  12. Tag v<version>

Does NOT push. Review the commit, then:
  git push origin main --tags
EOF
            exit 0
            ;;
        *)
            if [[ -z "$VERSION" ]]; then
                VERSION="$1"; shift
            else
                echo "ERROR: unexpected argument: $1" >&2; exit 1
            fi
            ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    echo "ERROR: version argument required. Usage: $(basename "$0") <version>" >&2
    exit 1
fi

# Validate semver format (major.minor.patch, optional -pre).
if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$'; then
    echo "ERROR: '$VERSION' is not valid semver (expected N.N.N or N.N.N-pre)" >&2
    exit 1
fi

# ─── Preamble ─────────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

OLD_VERSION=$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)"/\1/')
LAST_TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "")

echo "━━━ abox release $OLD_VERSION → $VERSION ━━━"
echo "  repo:     $REPO_ROOT"
echo "  last tag: ${LAST_TAG:-<none>}"
echo "  dry run:  $DRY_RUN"
echo

# ─── Step 1: Preflight checks ────────────────────────────────────────────────
echo "[1/12] Preflight checks..."

if [[ "$DRY_RUN" == "0" ]] && [[ -n "$(git status --porcelain)" ]]; then
    echo "ERROR: working tree is dirty. Commit or stash your changes first." >&2
    git status --short >&2
    exit 1
fi

if git rev-parse "v$VERSION" >/dev/null 2>&1; then
    echo "ERROR: tag v$VERSION already exists." >&2
    exit 1
fi

echo "  ✓ clean tree, tag v$VERSION available"

# ─── Step 2: Bump version ────────────────────────────────────────────────────
echo "[2/12] Bumping version $OLD_VERSION → $VERSION..."

sed -i "0,/^version = \"$OLD_VERSION\"/s//version = \"$VERSION\"/" Cargo.toml
# Regenerate Cargo.lock with the new version.
cargo generate-lockfile --quiet 2>/dev/null || cargo check --quiet 2>/dev/null
echo "  ✓ Cargo.toml + Cargo.lock updated"

# ─── Step 3: Quality checks ──────────────────────────────────────────────────
echo "[3/12] Running quality checks (fmt, clippy, test)..."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --quiet
echo "  ✓ fmt + clippy + tests passed"

# ─── Step 4: E2E tests ───────────────────────────────────────────────────────
echo "[4/12] Running end-to-end tests..."
./scripts/e2e_test.sh >/dev/null 2>&1
E2E_RESULT=$(./scripts/e2e_test.sh 2>&1 | tail -1)
echo "  ✓ $E2E_RESULT"

# ─── Step 5: Release build ───────────────────────────────────────────────────
echo "[5/12] Building release binary..."
cargo build --workspace --release --quiet
BINARY_SIZE=$(du -h target/release/abox | cut -f1)
echo "  ✓ target/release/abox ($BINARY_SIZE)"

# ─── Step 6: VM benchmarks ───────────────────────────────────────────────────
echo "[6/12] Running VM latency benchmarks (5 runs)..."
BENCH_JSON=""
ABOX_VM="$HOME/.abox/vm"
if [[ -x "$ABOX_VM/cloud-hypervisor" ]] && [[ -f "$ABOX_VM/rootfs.raw" ]] && [[ -c /dev/kvm ]]; then
    BENCH_JSON=$(./scripts/bench.sh --runs 5 --json-only 2>/dev/null)
    echo "  ✓ VM benchmarks captured"
else
    echo "  ⊘ skipped (no VM bootstrap or /dev/kvm)"
fi

# ─── Step 7: Update README benchmark table ────────────────────────────────────
echo "[7/12] Updating README.md benchmark table..."

# Extract values from JSON (or use placeholders if skipped).
if [[ -n "$BENCH_JSON" ]]; then
    vm_boot=$(echo "$BENCH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['results']['vm_boot_ms']['avg'])")
    proxy_rt=$(echo "$BENCH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['results']['proxy_roundtrip_ms']['avg'])")
    full_run=$(echo "$BENCH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['results']['full_run_ms']['avg'])")
    cleanup=$(echo "$BENCH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['results']['cleanup_ms']['avg'])")
    hw_cores=$(echo "$BENCH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['hardware']['cores'])")
    hw_arch=$(echo "$BENCH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['hardware']['arch'])")
    hw_kernel=$(echo "$BENCH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['hardware']['kernel'])")
else
    vm_boot="n/a"; proxy_rt="n/a"; full_run="n/a"; cleanup="n/a"
    hw_cores="n/a"; hw_arch="n/a"; hw_kernel="n/a"
fi

# Collect criterion summary (run a quick bench and capture the key lines).
# Criterion prints the bench name on one line and the time: [...] on the next,
# so we use grep -A1 to get both, then extract the middle value from the bracket.
echo "  running criterion microbenchmarks..."
CRITERION_OUT=$(cargo bench -p abox-core 2>&1)

extract_criterion_ns() {
    # Usage: extract_criterion_ns <bench_name> <criterion_output>
    # Returns the median (middle) value + unit from "time: [low mid high]".
    # Criterion sometimes puts the name + time on the same line, sometimes
    # on separate lines, so we grep for the name and look at the next 2
    # lines for the time: marker.
    echo "$2" | grep -A2 "$1" | grep "time:" | head -1 | \
        grep -oP '\[\K[0-9.]+ [a-z]+ [0-9.]+ [a-z]+ [0-9.]+' | \
        awk '{print $3, $4}' || echo "n/a"
}

policy_ns=$(extract_criterion_ns "policy_evaluate_cli/git_status_allowed" "$CRITERION_OUT")
serial_ns=$(extract_criterion_ns "proxy_serialization/request_serialize" "$CRITERION_OUT")
bootmeta_ns=$(extract_criterion_ns "boot_meta/to_json" "$CRITERION_OUT")

# Binary size for the table.
binary_bytes=$(stat --printf='%s' target/release/abox 2>/dev/null || echo 0)
binary_mb=$(echo "scale=1; $binary_bytes / 1048576" | bc 2>/dev/null || echo "$BINARY_SIZE")

BENCH_TABLE="## Performance

Measured on ${hw_arch}, ${hw_cores} cores, kernel ${hw_kernel}. VM benchmarks averaged over 5 runs.
Updated at release v${VERSION} ($(date +%Y-%m-%d)).

| Metric | Value | What it measures |
|---|---|---|
| VM boot | ${vm_boot} ms | Cloud Hypervisor start to first proxied request |
| Proxy round-trip | ${proxy_rt} ms | Bridge ready to \`git status\` response |
| Full \`abox run\` | ${full_run} ms | Total wall time for trivial guest command |
| Sandbox cleanup | ${cleanup} ms | \`abox stop --clean\` teardown |
| Policy evaluation | ~${policy_ns} ns | \`evaluate_cli\` for \`git status\` (allowed) |
| Request serialization | ~${serial_ns} ns | JSON encode of \`ProxyRequest\` |
| Boot meta generation | ~${bootmeta_ns} ns | \`BootMeta::to_json()\` |
| Release binary | ${binary_mb} MB | \`target/release/abox\` (LTO + strip) |

Run \`just bench\` (criterion, no VM) or \`just bench-vm-n 5\` (VM latency) to reproduce."

# Insert or replace the benchmark table in README.md.
if grep -q "^## Performance" README.md; then
    # Replace existing section (from ## Performance to the next ## or EOF).
    python3 - README.md "$BENCH_TABLE" <<'PYEOF'
import sys
readme_path, new_section = sys.argv[1], sys.argv[2]
with open(readme_path) as f:
    lines = f.readlines()
out, skip = [], False
for line in lines:
    if line.startswith("## Performance"):
        skip = True
        out.append(new_section + "\n\n")
        continue
    if skip and line.startswith("## "):
        skip = False
    if not skip:
        out.append(line)
with open(readme_path, "w") as f:
    f.writelines(out)
PYEOF
else
    # Insert before ## License (or at EOF if no License section).
    python3 - README.md "$BENCH_TABLE" <<'PYEOF'
import sys
readme_path, new_section = sys.argv[1], sys.argv[2]
with open(readme_path) as f:
    content = f.read()
marker = "## License"
if marker in content:
    content = content.replace(marker, new_section + "\n\n" + marker)
else:
    content += "\n" + new_section + "\n"
with open(readme_path, "w") as f:
    f.write(content)
PYEOF
fi
echo "  ✓ README.md updated"

# ─── Step 8: Save benchmark JSON ─────────────────────────────────────────────
echo "[8/12] Saving benchmark archive..."
mkdir -p benchmarks
if [[ -n "$BENCH_JSON" ]]; then
    echo "$BENCH_JSON" > "benchmarks/v${VERSION}.json"
    echo "  ✓ benchmarks/v${VERSION}.json"
else
    echo "  ⊘ skipped (no VM benchmarks)"
fi

# ─── Step 9: Generate changelog entry ─────────────────────────────────────────
echo "[9/12] Generating changelog entry..."

if [[ -n "$LAST_TAG" ]]; then
    LOG_RANGE="${LAST_TAG}..HEAD"
else
    LOG_RANGE="HEAD"
fi

# Categorize commits by conventional-commit prefix.
CHANGELOG_ENTRY="## v${VERSION} — $(date +%Y-%m-%d)

"
for prefix_label in "feat:Features" "fix:Fixes" "refactor:Refactoring" "test:Testing" "ci:CI" "docs:Documentation" "style:Style" "chore:Chores"; do
    prefix="${prefix_label%%:*}"
    label="${prefix_label##*:}"
    commits=$(git log --oneline "$LOG_RANGE" --grep="^${prefix}" --format="- %s (%h)" 2>/dev/null || true)
    if [[ -n "$commits" ]]; then
        CHANGELOG_ENTRY+="### ${label}

${commits}

"
    fi
done

# Catch any commits that don't match a prefix.
uncategorized=$(git log --oneline "$LOG_RANGE" --format="- %s (%h)" 2>/dev/null | \
    grep -v '^\- feat\|^\- fix\|^\- refactor\|^\- test\|^\- ci\|^\- docs\|^\- style\|^\- chore' || true)
if [[ -n "$uncategorized" ]]; then
    CHANGELOG_ENTRY+="### Other

${uncategorized}

"
fi

if [[ -f CHANGELOG.md ]]; then
    # Prepend the new entry after the first line (the # title).
    python3 - CHANGELOG.md "$CHANGELOG_ENTRY" <<'PYEOF'
import sys
path, entry = sys.argv[1], sys.argv[2]
with open(path) as f:
    content = f.read()
# Insert after the first heading line.
lines = content.split("\n", 1)
if len(lines) == 2:
    content = lines[0] + "\n\n" + entry + lines[1]
else:
    content = lines[0] + "\n\n" + entry
with open(path, "w") as f:
    f.write(content)
PYEOF
else
    cat > CHANGELOG.md <<CLEOF
# Changelog

All notable changes to abox are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

${CHANGELOG_ENTRY}
CLEOF
fi
echo "  ✓ CHANGELOG.md updated"

# ─── Step 10: Refresh local install ──────────────────────────────────────────
echo "[10/12] Installing release binary to ~/.cargo/bin..."
cargo install --path crates/abox-cli --quiet 2>/dev/null || cargo install --path crates/abox-cli
echo "  ✓ $(abox --version)"

# ─── Step 11: Commit ─────────────────────────────────────────────────────────
echo "[11/12] Committing..."
if [[ "$DRY_RUN" == "1" ]]; then
    echo "  ⊘ dry run — skipping commit"
    git diff --stat
else
    git add Cargo.toml Cargo.lock README.md CHANGELOG.md benchmarks/
    git commit -m "$(cat <<EOF
release: v${VERSION}

Version bump ${OLD_VERSION} → ${VERSION}.
Benchmark results updated in README.md and archived in benchmarks/v${VERSION}.json.
Changelog entry generated from commits since ${LAST_TAG:-initial commit}.
EOF
)"
    echo "  ✓ committed"
fi

# ─── Step 12: Tag ────────────────────────────────────────────────────────────
echo "[12/12] Tagging v${VERSION}..."
if [[ "$DRY_RUN" == "1" ]]; then
    echo "  ⊘ dry run — skipping tag"
else
    git tag -a "v${VERSION}" -m "Release v${VERSION}"
    echo "  ✓ tagged v${VERSION}"
fi

# ─── Summary ──────────────────────────────────────────────────────────────────
echo
echo "━━━ release v${VERSION} ready ━━━"
echo
echo "  commit: $(git rev-parse --short HEAD)"
echo "  tag:    v${VERSION}"
echo "  binary: target/release/abox ($BINARY_SIZE)"
if [[ -n "$BENCH_JSON" ]]; then
    echo "  bench:  full_run=${full_run}ms, boot=${vm_boot}ms, cleanup=${cleanup}ms"
fi
echo
echo "Review the commit, then push:"
echo "  git push origin main --tags"
if [[ "$DRY_RUN" == "1" ]]; then
    echo
    echo "(This was a dry run. No commits or tags were created.)"
    echo "To undo the version bump: git checkout Cargo.toml Cargo.lock README.md"
fi
