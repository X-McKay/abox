#!/usr/bin/env bash
# install.sh — one-command installer for abox.
#
# Downloads the latest (or specified) abox release binary and guest
# binaries from GitHub, verifies checksums, and installs to ~/.abox/.
# The MicroSandbox runtime assets themselves are installed afterwards by
# `abox init`.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/X-McKay/abox/main/scripts/install.sh | bash
#   ABOX_VERSION=v0.7.0 bash install.sh    # pin a specific version
#   ABOX_INSTALL_DIR=/usr/local/bin bash install.sh
set -euo pipefail

REPO="X-McKay/abox"
INSTALL_DIR="${ABOX_INSTALL_DIR:-$HOME/.abox/bin}"
STATE_DIR="${ABOX_STATE_DIR:-$HOME/.abox}"

# ─── Detect platform ────────────────────────────────────────────────────
ARCH="$(uname -m)"
OS="$(uname -s)"
case "$OS/$ARCH" in
    Linux/x86_64)   TARGET="x86_64-unknown-linux-gnu";  GUEST_ARCH="x86_64" ;;
    Linux/aarch64)  TARGET="aarch64-unknown-linux-gnu"; GUEST_ARCH="aarch64" ;;
    Darwin/arm64)   TARGET="aarch64-apple-darwin";      GUEST_ARCH="aarch64" ;;
    *)
        echo "ERROR: unsupported platform: $OS/$ARCH" >&2
        echo "abox supports Linux (x86_64/aarch64, KVM) and macOS (Apple Silicon)." >&2
        exit 1
        ;;
esac

# ─── Resolve version ────────────────────────────────────────────────────
if [[ -n "${ABOX_VERSION:-}" ]]; then
    VERSION="$ABOX_VERSION"
    echo "Installing abox $VERSION (pinned)..."
else
    echo "Fetching latest release version..."
    API_RESPONSE=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null || true)
    VERSION=$(echo "$API_RESPONSE" | grep '"tag_name"' | cut -d'"' -f4)
    if [[ -z "$VERSION" ]]; then
        cat >&2 <<'EOF'

No published release of abox was found.

To install from source:

  # Prerequisites: Rust (https://rustup.rs), just (cargo install just)
  git clone https://github.com/X-McKay/abox.git
  cd abox
  just build
  abox init           # guided first-run setup (installs the runtime assets)

Once a release is published, re-run this script or pin a version:
  ABOX_VERSION=v0.7.0 bash install.sh

See https://github.com/X-McKay/abox for more information.
EOF
        exit 1
    fi
    echo "Installing abox $VERSION (latest)..."
fi

# BASE_URL can be overridden for local smoke testing (e.g., serving artifacts
# from a local HTTP server via `python3 -m http.server`).
BASE_URL="${ABOX_BASE_URL:-https://github.com/$REPO/releases/download/$VERSION}"

# ─── Download artifacts ─────────────────────────────────────────────────
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading binary (abox-$TARGET)..."
curl -fsSL -o "$TMP_DIR/abox-$TARGET" "$BASE_URL/abox-$TARGET"

echo "Downloading guest binaries (abox-guest-bins-$GUEST_ARCH.tar.gz)..."
curl -fsSL -o "$TMP_DIR/abox-guest-bins-$GUEST_ARCH.tar.gz" \
    "$BASE_URL/abox-guest-bins-$GUEST_ARCH.tar.gz"

echo "Downloading checksums (SHA256SUMS)..."
curl -fsSL -o "$TMP_DIR/SHA256SUMS" "$BASE_URL/SHA256SUMS"

# ─── Verify checksums ───────────────────────────────────────────────────
echo "Verifying checksums..."
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$TMP_DIR" && sha256sum -c SHA256SUMS --ignore-missing)
else
    (cd "$TMP_DIR" && shasum -a 256 -c SHA256SUMS --ignore-missing)
fi

# ─── Install ────────────────────────────────────────────────────────────
GUEST_DIR="$STATE_DIR/guest/$GUEST_ARCH"
mkdir -p "$INSTALL_DIR" "$GUEST_DIR"

install -m 755 "$TMP_DIR/abox-$TARGET" "$INSTALL_DIR/abox"
echo "Installed abox binary to $INSTALL_DIR/abox"

tar xzf "$TMP_DIR/abox-guest-bins-$GUEST_ARCH.tar.gz" -C "$GUEST_DIR"
chmod 755 "$GUEST_DIR/abox-shim" "$GUEST_DIR/abox-bridge"
echo "Staged guest binaries to $GUEST_DIR"

# ─── Summary ────────────────────────────────────────────────────────────
echo
echo "abox $VERSION installed successfully."
echo
echo "  Binary:         $INSTALL_DIR/abox"
echo "  Guest binaries: $GUEST_DIR"
echo

# Check if install dir is on PATH.
case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        echo "Run 'abox init' (installs the MicroSandbox runtime assets)"
        echo "and then 'abox doctor' to finish setup."
        ;;
    *)
        echo "Add abox to your PATH:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        echo
        echo "Then run 'abox init' (installs the MicroSandbox runtime assets)"
        echo "and 'abox doctor' to finish setup."
        ;;
esac
