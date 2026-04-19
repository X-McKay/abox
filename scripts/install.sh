#!/usr/bin/env bash
# install.sh — one-command installer for abox.
#
# Downloads the latest (or specified) abox release binary and VM assets
# from GitHub, verifies checksums, and installs to ~/.abox/.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/X-McKay/abox/main/scripts/install.sh | bash
#   ABOX_VERSION=v0.2.0 bash install.sh    # pin a specific version
#   ABOX_INSTALL_DIR=/usr/local/bin bash install.sh
set -euo pipefail

REPO="X-McKay/abox"
INSTALL_DIR="${ABOX_INSTALL_DIR:-$HOME/.abox/bin}"
VM_DIR="${ABOX_VM_DIR:-$HOME/.abox/vm}"

# ─── Detect architecture ────────────────────────────────────────────────
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
    aarch64)
        echo "ERROR: aarch64 support is not yet available." >&2
        echo "See https://github.com/X-McKay/abox for tracking." >&2
        exit 1
        ;;
    *)
        echo "ERROR: unsupported architecture: $ARCH" >&2
        echo "abox currently supports x86_64 Linux." >&2
        exit 1
        ;;
esac

# ─── Detect OS ──────────────────────────────────────────────────────────
OS="$(uname -s)"
if [[ "$OS" != "Linux" ]]; then
    echo "ERROR: unsupported OS: $OS" >&2
    echo "abox requires Linux with KVM support." >&2
    exit 1
fi

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

abox is currently pre-release. To install from source:

  # Prerequisites: Rust (https://rustup.rs), just (cargo install just)
  git clone https://github.com/X-McKay/abox.git
  cd abox
  just build
  just bootstrap-vm   # downloads VM kernel + rootfs
  abox init           # guided first-run setup

Once a release is published, re-run this script or pin a version:
  ABOX_VERSION=v0.1.0 bash install.sh

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

echo "Downloading VM assets (abox-vm-assets-$ARCH.tar.gz)..."
curl -fsSL -o "$TMP_DIR/abox-vm-assets-$ARCH.tar.gz" "$BASE_URL/abox-vm-assets-$ARCH.tar.gz"

echo "Downloading checksums (SHA256SUMS)..."
curl -fsSL -o "$TMP_DIR/SHA256SUMS" "$BASE_URL/SHA256SUMS"

# ─── Verify checksums ───────────────────────────────────────────────────
echo "Verifying checksums..."
(cd "$TMP_DIR" && sha256sum -c SHA256SUMS --ignore-missing)

# ─── Install ────────────────────────────────────────────────────────────
mkdir -p "$INSTALL_DIR" "$VM_DIR"

install -m 755 "$TMP_DIR/abox-$TARGET" "$INSTALL_DIR/abox"
echo "Installed abox binary to $INSTALL_DIR/abox"

tar xzf "$TMP_DIR/abox-vm-assets-$ARCH.tar.gz" -C "$VM_DIR"
echo "Extracted VM assets to $VM_DIR"

if command -v getcap >/dev/null 2>&1; then
    CAPS="$(getcap "$VM_DIR/virtiofsd" 2>/dev/null || true)"
else
    CAPS=""
fi

# ─── Summary ────────────────────────────────────────────────────────────
echo
echo "abox $VERSION installed successfully."
echo
echo "  Binary:    $INSTALL_DIR/abox"
echo "  VM assets: $VM_DIR"
echo

if [[ "$CAPS" == *"cap_sys_admin=ep"* ]] || [[ "$CAPS" == *"cap_sys_admin+ep"* ]]; then
    echo "virtiofsd sandbox capability already present:"
    echo "  $CAPS"
else
    printf -v QUOTED_VIRTIOFSD "%q" "$VM_DIR/virtiofsd"
    echo "Before first sandbox boot, grant virtiofsd the required file capability:"
    echo "  sudo setcap 'cap_sys_admin+ep' $QUOTED_VIRTIOFSD"
    echo
fi

# Check if install dir is on PATH.
case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        echo "Run 'abox init' and then 'abox doctor' to finish setup."
        ;;
    *)
        echo "Add abox to your PATH:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        echo
        echo "Then run 'abox init' and 'abox doctor' to finish setup."
        ;;
esac
