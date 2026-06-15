#!/usr/bin/env bash
# build_rootfs.sh — assemble the abox guest ext4 image via a Dockerized
# Alpine builder so root-owned files and permissions are preserved correctly.
#
# Inputs (env vars set by bootstrap_vm.sh):
#   ABOX_VM_DIR   — where alpine-minirootfs.tar.gz and rootfs.raw live
#   SHIM_BIN      — path to the static musl abox-shim binary
#   GUEST_INIT    — path to guest/init.sh
#   ABOX_PROFILE  — optional official guest profile name: base|node|python|rust
#
# Output:
#   base   -> $ABOX_VM_DIR/rootfs.raw
#   others -> $ABOX_VM_DIR/profiles/<profile>/rootfs.raw
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILDER_DOCKERFILE="$SCRIPT_DIR/rootfs-builder.Dockerfile"
BUILDER_IMAGE="abox-rootfs-builder:$(sha256sum "$BUILDER_DOCKERFILE" | cut -c1-16)"
ABOX_PROFILE="${ABOX_PROFILE:-base}"

: "${ABOX_VM_DIR:?ABOX_VM_DIR must be set}"
: "${SHIM_BIN:?SHIM_BIN must be set}"
: "${GUEST_INIT:?GUEST_INIT must be set}"

case "$ABOX_PROFILE" in
    base|node|python|rust)
        ;;
    *)
        echo "ERROR: unsupported ABOX_PROFILE '$ABOX_PROFILE' (expected base, node, python, or rust)" >&2
        exit 1
        ;;
esac

ensure_file() {
    local path="$1" label="$2"
    if [[ ! -f "$path" ]]; then
        echo "ERROR: $label not found at $path" >&2
        exit 1
    fi
}

profile_output_dir() {
    if [[ "$ABOX_PROFILE" == "base" ]]; then
        printf '%s\n' "$ABOX_VM_DIR"
    else
        printf '%s\n' "$ABOX_VM_DIR/profiles/$ABOX_PROFILE"
    fi
}

profile_image_size_mib() {
    # Sized to hold the Alpine base, the profile toolchain, and the bundled
    # agent CLIs (Claude Code + Codex). The Codex CLI ships a large vendored
    # binary (~220 MiB as of codex 0.139.0), so these include headroom for it
    # and future growth. Bump together if `__populate_fs: Could not allocate
    # block` appears during the ext4 populate step.
    case "$ABOX_PROFILE" in
        base|node)
            printf '%s\n' "1280"
            ;;
        python)
            printf '%s\n' "1536"
            ;;
        rust)
            printf '%s\n' "2560"
            ;;
    esac
}

host_mode() {
    for cmd in docker realpath; do
        command -v "$cmd" >/dev/null 2>&1 || {
            echo "ERROR: required command '$cmd' not found in PATH" >&2
            exit 1
        }
    done

    local vm_dir_abs shim_abs init_abs shim_rel init_rel
    vm_dir_abs="$(realpath -m "$ABOX_VM_DIR")"
    mkdir -p "$vm_dir_abs"
    shim_abs="$(realpath "$SHIM_BIN")"
    init_abs="$(realpath "$GUEST_INIT")"

    ensure_file "$shim_abs" "shim binary"
    ensure_file "$init_abs" "guest init script"
    ensure_file "$vm_dir_abs/alpine-minirootfs.tar.gz" "Alpine minirootfs"

    case "$shim_abs" in
        "$REPO_ROOT"/*) shim_rel="${shim_abs#"$REPO_ROOT"/}" ;;
        *)
            echo "ERROR: SHIM_BIN must live under the repo root so the Docker builder can mount it read-only" >&2
            exit 1
            ;;
    esac
    case "$init_abs" in
        "$REPO_ROOT"/*) init_rel="${init_abs#"$REPO_ROOT"/}" ;;
        *)
            echo "ERROR: GUEST_INIT must live under the repo root so the Docker builder can mount it read-only" >&2
            exit 1
            ;;
    esac

    if ! docker image inspect "$BUILDER_IMAGE" >/dev/null 2>&1; then
        echo "  building Docker rootfs builder image..."
        docker build --pull -t "$BUILDER_IMAGE" -f "$BUILDER_DOCKERFILE" "$SCRIPT_DIR" >/dev/null
    fi

    docker run --rm \
        -e ABOX_ROOTFS_BUILD_INNER=1 \
        -e ABOX_VM_DIR=/out \
        -e SHIM_BIN="/src/$shim_rel" \
        -e GUEST_INIT="/src/$init_rel" \
        -e ABOX_PROFILE="$ABOX_PROFILE" \
        -e HOST_UID="$(id -u)" \
        -e HOST_GID="$(id -g)" \
        -v "$REPO_ROOT":/src:ro \
        -v "$vm_dir_abs":/out \
        "$BUILDER_IMAGE" \
        /bin/bash /src/scripts/build_rootfs.sh
}

inner_mode() {
    : "${HOST_UID:?HOST_UID must be set in builder container}"
    : "${HOST_GID:?HOST_GID must be set in builder container}"

    for cmd in apk chroot dd install mkfs.ext4 npm sha256sum tar; do
        command -v "$cmd" >/dev/null 2>&1 || {
            echo "ERROR: required command '$cmd' not found in builder image" >&2
            exit 1
        }
    done

    ensure_file "$SHIM_BIN" "shim binary"
    ensure_file "$GUEST_INIT" "guest init script"
    ensure_file "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" "Alpine minirootfs"

    stage="$(mktemp -d)"
    out_dir="$(profile_output_dir)"
    mkdir -p "$out_dir"
    img="$out_dir/rootfs.raw"
    stamp="$img.inputs"
    cleanup() {
        local rc=$?
        if [[ -n "${stage:-}" ]]; then
            rm -rf "$stage"
        fi
        if [[ -e "${img:-}" ]]; then
            chown "$HOST_UID:$HOST_GID" "$img" || true
        fi
        if [[ -e "${stamp:-}" ]]; then
            chown "$HOST_UID:$HOST_GID" "$stamp" || true
        fi
        exit "$rc"
    }
    trap cleanup EXIT

    echo "  staging Alpine miniroot..."
    tar -xzf "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" -C "$stage"

    echo "  installing guest packages via apk for profile '$ABOX_PROFILE'..."
    local packages=(bash nodejs npm python3 su-exec ca-certificates gcompat socat)
    case "$ABOX_PROFILE" in
        base|node)
            ;;
        python)
            packages+=(py3-pip)
            ;;
        rust)
            packages+=(rust cargo)
            ;;
    esac
    apk --root "$stage" \
        --initdb \
        --no-cache \
        --no-scripts \
        --repositories-file "$stage/etc/apk/repositories" \
        --keys-dir "$stage/etc/apk/keys" \
        add "${packages[@]}" >/dev/null

    if [[ "$ABOX_PROFILE" == "python" ]]; then
        echo "  installing uv into the python profile..."
        uv_stage="$(mktemp -d)"
        python3 -m venv "$uv_stage"
        # shellcheck disable=SC1090
        . "$uv_stage/bin/activate"
        pip install --no-cache-dir uv >/dev/null
        install -m 0755 "$uv_stage/bin/uv" "$stage/usr/local/bin/uv"
        if [[ -f "$uv_stage/bin/uvx" ]]; then
            install -m 0755 "$uv_stage/bin/uvx" "$stage/usr/local/bin/uvx"
        fi
        deactivate || true
        rm -rf "$uv_stage"
    fi

    echo "  creating abox user (uid=1000)..."
    chroot "$stage" /bin/sh -c '
        addgroup -g 1000 abox
        adduser -D -u 1000 -G abox -h /home/abox -s /bin/bash abox
    '
    install -d -m 755 -o 1000 -g 1000 "$stage/home/abox"
    install -d -m 700 -o 1000 -g 1000 "$stage/home/abox/.claude"
    chown 1000:1000 "$stage/home/abox" "$stage/home/abox/.claude"
    chmod g-s "$stage/home/abox" "$stage/home/abox/.claude"
    chmod 0755 "$stage/home/abox"
    chmod 0700 "$stage/home/abox/.claude"

    echo "  installing Claude Code and Codex CLIs..."
    local npm_prefix="$stage/usr/local"
    mkdir -p "$npm_prefix/lib" "$npm_prefix/bin"
    npm install --global --prefix "$npm_prefix" \
        @anthropic-ai/claude-code@2.1.177 @openai/codex@0.139.0 2>&1 | tail -5
    find "$npm_prefix/bin" -type f -exec sed -i '1s|^#!.*node$|#!/usr/bin/env node|' {} +

    echo "  building system CA trust bundle..."
    mkdir -p "$stage/etc/ssl/certs"
    if ls "$stage/usr/share/ca-certificates/mozilla/"*.crt >/dev/null 2>&1; then
        cat "$stage/usr/share/ca-certificates/mozilla/"*.crt \
            > "$stage/etc/ssl/certs/ca-certificates.crt"
    fi

    echo "  installing abox-shim and symlinks..."
    mkdir -p "$stage/usr/local/bin" "$stage/sbin"
    install -m 0755 "$SHIM_BIN" "$stage/usr/local/bin/abox-shim"
    for cmd in git gh aws; do
        ln -sf /usr/local/bin/abox-shim "$stage/usr/local/bin/$cmd"
    done

    echo "  installing init as /sbin/init..."
    install -m 0755 "$GUEST_INIT" "$stage/sbin/init"

    local image_size_mib
    image_size_mib="$(profile_image_size_mib)"

    echo "  creating ext4 image..."
    rm -f "$img"
    dd if=/dev/zero of="$img" bs=1M count="$image_size_mib" status=none
    mkfs.ext4 -q -F -E root_owner=0:0 -d "$stage" "$img"

    echo "  rootfs.raw built for profile '$ABOX_PROFILE' ($(du -h "$img" | cut -f1))"

    {
        echo "# Inputs that produced this rootfs (sha256). Generated by build_rootfs.sh."
        echo "profile=$ABOX_PROFILE"
        echo "image_size_mib=$image_size_mib"
        echo "packages=$(IFS=,; echo "${packages[*]}")"
        echo "init_sh=$(sha256sum "$GUEST_INIT" | cut -d' ' -f1)"
        echo "shim=$(sha256sum "$SHIM_BIN" | cut -d' ' -f1)"
        echo "build_rootfs_sh=$(sha256sum "$SCRIPT_DIR/build_rootfs.sh" | cut -d' ' -f1)"
        echo "rootfs_builder_dockerfile=$(sha256sum "$BUILDER_DOCKERFILE" | cut -d' ' -f1)"
        if [[ "$ABOX_PROFILE" == "python" ]]; then
            echo "python_uv_source=pip"
        fi
        echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "$stamp"
}

if [[ "${ABOX_ROOTFS_BUILD_INNER:-0}" == "1" ]]; then
    inner_mode
else
    host_mode
fi
