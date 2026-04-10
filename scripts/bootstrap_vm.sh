#!/usr/bin/env bash
# bootstrap_vm.sh — one-command setup for abox VM execution.
#
# Downloads cloud-hypervisor, virtiofsd, a kernel, and an Alpine miniroot.
# Builds the abox-shim for static musl. Assembles a guest rootfs image.
# Writes everything to ~/.abox/vm/ and updates ~/.abox/config.toml so
# `abox run` works out of the box.
#
# This script is idempotent and uses checksummed cached downloads under vendor/.
# It does NOT require sudo, docker, chroot, or root.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ABOX_VM_DIR="${ABOX_VM_DIR:-$HOME/.abox/vm}"

# ─── Argument parsing ────────────────────────────────────────────────────
DO_SYMLINK=1
ASSUME_YES="${BOOTSTRAP_YES:-0}"
FROM_BUNDLE="${FROM_BUNDLE:-}"
for arg in "$@"; do
    case "$arg" in
        --no-symlink)
            DO_SYMLINK=0
            ;;
        --yes|-y)
            ASSUME_YES=1
            ;;
        --from-bundle)
            # Next argument is the bundle path; handled below
            FROM_BUNDLE="__NEXT__"
            ;;
        --help|-h)
            cat <<HELP
Usage: $(basename "$0") [--no-symlink] [--yes] [--from-bundle <path>]

  --no-symlink          Do NOT create symlinks in ~/.local/bin. You will need
                        to add $ABOX_VM_DIR to your PATH manually.
  --yes, -y             Non-interactive mode: silently install missing rust
                        toolchain components (e.g. the x86_64-unknown-linux-musl
                        target). Without this flag the script fails fast and
                        prints the rustup command for you to run yourself.
                        Honored from the BOOTSTRAP_YES=1 environment variable too.
  --from-bundle <path>  Restore VM assets from a pre-built tarball instead of
                        downloading components individually. The tarball should
                        be an abox-vm-assets-*.tar.gz from a GitHub release.
HELP
            exit 0
            ;;
        *)
            if [[ "$FROM_BUNDLE" == "__NEXT__" ]]; then
                FROM_BUNDLE="$arg"
            else
                echo "Unknown argument: $arg" >&2
                echo "Try '$(basename "$0") --help'" >&2
                exit 1
            fi
            ;;
    esac
done

if [[ "$FROM_BUNDLE" == "__NEXT__" ]]; then
    echo "ERROR: --from-bundle requires a path argument" >&2
    exit 1
fi

source "$REPO_ROOT/scripts/lib/download.sh"

mkdir -p "$ABOX_VM_DIR" "$REPO_ROOT/vendor"

# ─── Fast path: restore from pre-built bundle ───────────────────────────
if [[ -n "$FROM_BUNDLE" ]]; then
    if [[ ! -f "$FROM_BUNDLE" ]]; then
        echo "ERROR: bundle not found: $FROM_BUNDLE" >&2
        exit 1
    fi
    echo "abox VM bootstrap (from bundle)"
    echo "  bundle:      $FROM_BUNDLE"
    echo "  install dir: $ABOX_VM_DIR"
    echo
    echo "[1/1] Extracting VM assets from bundle..."
    tar xzf "$FROM_BUNDLE" -C "$ABOX_VM_DIR"
    echo
    echo "Bootstrap complete (from bundle). Files in $ABOX_VM_DIR:"
    ls -lh "$ABOX_VM_DIR"
    exit 0
fi

# ---------------------------------------------------------------------------
# Artifact versions and URLs
# ---------------------------------------------------------------------------

# cloud-hypervisor v44.0 — static musl builds (x86_64)
# Source: https://github.com/cloud-hypervisor/cloud-hypervisor/releases/tag/v44.0
readonly CH_VERSION="v44.0"
readonly CH_BIN_URL="https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/${CH_VERSION}/cloud-hypervisor-static"
readonly CH_BIN_SHA="f58e5d8684a5cbd7c4b8a001a1188ac79b9d4dda8115e1b3d5faa8c29038119c"
readonly CH_REMOTE_URL="https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/${CH_VERSION}/ch-remote-static"
readonly CH_REMOTE_SHA="6d268b947adf2b9b72c13cc8bda156e27c9a450474001d762e9bd211f90136fa"

# virtiofsd 1.10.0 — from Ubuntu noble universe (dynamically linked, requires host libc/libcap-ng/libseccomp)
# Sourced as a .deb from the Ubuntu archive; binary extracted without root.
# Source: https://packages.ubuntu.com/noble/virtiofsd
readonly VIRTIOFSD_VERSION="1.10.0-1"
readonly VIRTIOFSD_DEB_URL="http://archive.ubuntu.com/ubuntu/pool/universe/r/rust-virtiofsd/virtiofsd_${VIRTIOFSD_VERSION}_amd64.deb"
readonly VIRTIOFSD_DEB_SHA="1e4e817925b92f8c4ec59eff65b9825d044ecbd06c7bfcdca624e8562e90188a"
# SHA256 of the extracted binary itself (for post-extraction verification)
readonly VIRTIOFSD_BIN_SHA="597ae1edfda17185def026974a0ec0c3d3c6f536b018bb517aa566a4495dbf0d"

# Linux kernel — built by the cloud-hypervisor team against CH's kernel tree
# Source: https://github.com/cloud-hypervisor/linux/releases/tag/ch-release-v6.16.9-20260324
readonly VMLINUX_VERSION="ch-release-v6.16.9-20260324"
readonly VMLINUX_URL="https://github.com/cloud-hypervisor/linux/releases/download/${VMLINUX_VERSION}/vmlinux-x86_64"
readonly VMLINUX_SHA="22c640f02b750dea5d0c4419436aac8f2a6ea60fe02732435e25138d04eaaa86"

# Alpine Linux 3.19.9 miniroot filesystem tarball
# Source: https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/
readonly ALPINE_VERSION="3.19.9"
readonly ALPINE_MINOR="v3.19"
readonly ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/${ALPINE_MINOR}/releases/x86_64/alpine-minirootfs-${ALPINE_VERSION}-x86_64.tar.gz"
readonly ALPINE_SHA="6b4444630d3c349edb99847da31591a91d529b4bf8235a4990d4cb2cab45b8e5"

# socat Alpine package (not yet extracted — used in rootfs assembly phase)
# Source: https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/
readonly SOCAT_VERSION="1.8.0.0-r0"
readonly SOCAT_URL="https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/socat-${SOCAT_VERSION}.apk"
readonly SOCAT_SHA="ddf3be46f3a319737817246b238089dc58f39f32b0f515358c40e9e6e363eee6"

# ---------------------------------------------------------------------------

echo "abox VM bootstrap"
echo "  install dir: $ABOX_VM_DIR"
echo "  vendor dir:  $REPO_ROOT/vendor"
echo

echo "[1/5] Downloading cloud-hypervisor + ch-remote..."
download_to "$CH_BIN_URL"    "$ABOX_VM_DIR/cloud-hypervisor" "$CH_BIN_SHA"
download_to "$CH_REMOTE_URL" "$ABOX_VM_DIR/ch-remote"        "$CH_REMOTE_SHA"
chmod +x "$ABOX_VM_DIR/cloud-hypervisor" "$ABOX_VM_DIR/ch-remote"

echo "[2/5] Downloading virtiofsd..."
download_to "$VIRTIOFSD_DEB_URL" "$ABOX_VM_DIR/virtiofsd.deb" "$VIRTIOFSD_DEB_SHA"
# Extract just the virtiofsd binary from the deb (rootless — dpkg-deb -x needs no root)
_VFSD_TMP="$(mktemp -d)"
trap 'rm -rf "$_VFSD_TMP"' EXIT
dpkg-deb -x "$ABOX_VM_DIR/virtiofsd.deb" "$_VFSD_TMP"
cp -f "$_VFSD_TMP/usr/libexec/virtiofsd" "$ABOX_VM_DIR/virtiofsd"
# Verify the extracted binary checksum
_actual="$(sha256sum "$ABOX_VM_DIR/virtiofsd" | awk '{print $1}')"
if [[ "$_actual" != "$VIRTIOFSD_BIN_SHA" ]]; then
    echo "  ERROR: virtiofsd binary checksum mismatch after extraction" >&2
    echo "  expected: $VIRTIOFSD_BIN_SHA" >&2
    echo "  actual:   $_actual" >&2
    exit 1
fi
rm -f "$ABOX_VM_DIR/virtiofsd.deb"
chmod +x "$ABOX_VM_DIR/virtiofsd"

echo "[3/5] Downloading guest kernel + Alpine miniroot + socat package..."
download_to "$VMLINUX_URL"  "$ABOX_VM_DIR/vmlinux"                    "$VMLINUX_SHA"
download_to "$ALPINE_URL"   "$ABOX_VM_DIR/alpine-minirootfs.tar.gz"   "$ALPINE_SHA"
download_to "$SOCAT_URL"    "$ABOX_VM_DIR/socat.apk"                  "$SOCAT_SHA"

# ─── Phase 4: Build abox-shim for static musl ────────────────────────────
echo "[4/5] Building abox-shim for static musl..."
if ! rustup target list --installed 2>/dev/null | grep -q '^x86_64-unknown-linux-musl$'; then
    if [[ "$ASSUME_YES" == "1" ]] || [[ "${CI:-}" == "true" ]]; then
        echo "  adding x86_64-unknown-linux-musl rust target..."
        rustup target add x86_64-unknown-linux-musl
    else
        cat <<MISSING_MUSL >&2

ERROR: x86_64-unknown-linux-musl rust target is not installed.

The abox-shim is built as a static-musl binary so it can run inside
the minimal Alpine guest rootfs. Install the target manually:

    rustup target add x86_64-unknown-linux-musl

…or re-run this script with --yes to let it do that for you:

    $(basename "$0") --yes
MISSING_MUSL
        exit 1
    fi
fi
( cd "$REPO_ROOT" && cargo build --release --target x86_64-unknown-linux-musl -p abox-shim )
SHIM_BIN="$REPO_ROOT/target/x86_64-unknown-linux-musl/release/abox-shim"
if [[ ! -f "$SHIM_BIN" ]]; then
    echo "ERROR: shim binary was not produced at $SHIM_BIN" >&2
    exit 1
fi

# ─── Phase 5: Assemble the guest rootfs ──────────────────────────────────
echo "[5/5] Assembling guest rootfs..."
SHIM_BIN="$SHIM_BIN" \
ABOX_VM_DIR="$ABOX_VM_DIR" \
GUEST_INIT="$REPO_ROOT/guest/init.sh" \
    "$REPO_ROOT/scripts/build_rootfs.sh"

echo
echo "Bootstrap complete. Files in $ABOX_VM_DIR:"
ls -lh "$ABOX_VM_DIR"

# ─── Install convenience symlinks into ~/.local/bin ──────────────────────
if [[ "$DO_SYMLINK" == "1" ]]; then
    LOCAL_BIN="$HOME/.local/bin"
    mkdir -p "$LOCAL_BIN"
    echo
    echo "Installing convenience symlinks in $LOCAL_BIN..."
    for bin in cloud-hypervisor ch-remote virtiofsd; do
        ln -sf "$ABOX_VM_DIR/$bin" "$LOCAL_BIN/$bin"
        echo "  $LOCAL_BIN/$bin -> $ABOX_VM_DIR/$bin"
    done

    # Warn if ~/.local/bin isn't on PATH.
    case ":$PATH:" in
        *":$LOCAL_BIN:"*)
            : # already on PATH
            ;;
        *)
            echo
            echo "WARNING: $LOCAL_BIN is not on your PATH."
            echo "Add this to your shell profile (e.g., ~/.bashrc):"
            echo '  export PATH="$HOME/.local/bin:$PATH"'
            ;;
    esac
fi
