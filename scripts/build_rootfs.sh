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
    base|node|python|python-glibc|rust)
        ;;
    *)
        echo "ERROR: unsupported ABOX_PROFILE '$ABOX_PROFILE' (expected base, node, python, python-glibc, or rust)" >&2
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

profile_libc() {
    case "$ABOX_PROFILE" in
        *-glibc) printf 'glibc\n' ;;
        *)       printf 'musl\n' ;;
    esac
}

profile_image_size_mib() {
    # Size the ext4 image from the actual staged content plus headroom, so it
    # auto-scales per profile and never overflows as toolchains/CLIs grow (the
    # Codex CLI alone ships a ~220 MiB vendored binary). `stage` is visible here
    # via bash dynamic scoping from the build function, and this runs after
    # everything is staged. Falls back to a generous default if `stage` is unset.
    local stage_dir="${stage:-}"
    if [[ -z "$stage_dir" || ! -d "$stage_dir" ]]; then
        printf '%s\n' "2560"
        return
    fi
    local stage_size_mib
    stage_size_mib="$(du -sm "$stage_dir" | cut -f1)"
    printf '%s\n' "$(( stage_size_mib + 512 ))"
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
    if [[ "$(profile_libc)" == "musl" ]]; then
        ensure_file "$vm_dir_abs/alpine-minirootfs.tar.gz" "Alpine minirootfs"
    fi

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

# Shared abox staging — identical across libc bases. Relies on the same
# dynamically-scoped vars the caller (inner_mode) sets: $stage, $out_dir, $img,
# $stamp, $ABOX_PROFILE, $SHIM_BIN, $GUEST_INIT, $SCRIPT_DIR, $packages,
# $BUILDER_DOCKERFILE.
stage_abox_into() {
    # musl profiles install the agent CLIs here with the Alpine builder's npm.
    # glibc profiles bake them into the Debian base (with the glibc npm) so the
    # per-platform native/vendored binaries match the guest libc — see
    # scripts/glibc/<profile>.Dockerfile. Keep the pinned versions in sync.
    if [[ "$(profile_libc)" == "musl" ]]; then
        echo "  installing Claude Code and Codex CLIs..."
        local npm_prefix="$stage/usr/local"
        mkdir -p "$npm_prefix/lib" "$npm_prefix/bin"
        # `--loglevel=error` suppresses npm progress noise but keeps full error
        # output, so a failed install (e.g. a network error) is fully visible in
        # the build log rather than truncated by a `tail`.
        npm install --global --prefix "$npm_prefix" \
            @anthropic-ai/claude-code@2.1.177 @openai/codex@0.139.0 --loglevel=error
        find "$npm_prefix/bin" -type f -exec sed -i '1s|^#!.*node$|#!/usr/bin/env node|' {} +
    fi

    echo "  building system CA trust bundle..."
    mkdir -p "$stage/etc/ssl/certs"
    if [[ "$(profile_libc)" == "musl" ]]; then
        if ls "$stage/usr/share/ca-certificates/mozilla/"*.crt >/dev/null 2>&1; then
            cat "$stage/usr/share/ca-certificates/mozilla/"*.crt \
                > "$stage/etc/ssl/certs/ca-certificates.crt"
        fi
    else
        [ -s "$stage/etc/ssl/certs/ca-certificates.crt" ] || {
            echo "ERROR: glibc base missing /etc/ssl/certs/ca-certificates.crt" >&2
            exit 1
        }
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
        if [[ "$(profile_libc)" == "glibc" ]]; then
            echo "libc=glibc"
            echo "base_dockerfile=$(sha256sum "$SCRIPT_DIR/glibc/$ABOX_PROFILE.Dockerfile" | cut -d' ' -f1)"
            echo "pkg_versions=$(chroot "$stage" dpkg-query -W -f '${Package}=${Version}\n' 2>/dev/null | sha256sum | cut -d' ' -f1)"
            echo "node_version=$(chroot "$stage" node --version 2>/dev/null)"
            echo "uv_version=$(chroot "$stage" uv --version 2>/dev/null)"
        else
            echo "libc=musl"
        fi
        echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "$stamp"
}

inner_mode() {
    : "${HOST_UID:?HOST_UID must be set in builder container}"
    : "${HOST_GID:?HOST_GID must be set in builder container}"

    local required=(chroot dd install mkfs.ext4 npm sha256sum tar)
    if [[ "$(profile_libc)" == "musl" ]]; then
        required+=(apk)
    fi
    for cmd in "${required[@]}"; do
        command -v "$cmd" >/dev/null 2>&1 || {
            echo "ERROR: required command '$cmd' not found in builder image" >&2
            exit 1
        }
    done

    ensure_file "$SHIM_BIN" "shim binary"
    ensure_file "$GUEST_INIT" "guest init script"
    if [[ "$(profile_libc)" == "musl" ]]; then
        ensure_file "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" "Alpine minirootfs"
    fi

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

    local packages=()
    if [[ "$(profile_libc)" == "glibc" ]]; then
        local base_tar="$ABOX_VM_DIR/${ABOX_PROFILE}-rootfs.tar.gz"
        ensure_file "$base_tar" "glibc base tarball for $ABOX_PROFILE"
        echo "  staging Debian/glibc base for '$ABOX_PROFILE'..."
        tar --numeric-owner -xzf "$base_tar" -C "$stage"
        echo "  creating abox user (uid=1000)..."
        chroot "$stage" /bin/sh -c '
            groupadd -g 1000 abox
            useradd -u 1000 -g abox -m -d /home/abox -s /bin/bash abox
        '
        install -d -m 700 -o 1000 -g 1000 "$stage/home/abox/.claude"
        chmod 0700 "$stage/home/abox/.claude"
        # python3/pip/uv/node/gosu(su-exec)/socat/ca-certificates are already in
        # the Debian base; nothing to apk-add here.
    else
        # ── musl (Alpine) branch — existing code, moved here UNCHANGED ──
        echo "  staging Alpine miniroot..."
        tar --numeric-owner -xzf "$ABOX_VM_DIR/alpine-minirootfs.tar.gz" -C "$stage"

        echo "  installing guest packages via apk for profile '$ABOX_PROFILE'..."
        packages=(bash nodejs npm python3 su-exec ca-certificates gcompat socat)
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
    fi

    stage_abox_into
}

if [[ "${ABOX_ROOTFS_BUILD_INNER:-0}" == "1" ]]; then
    inner_mode
else
    host_mode
fi
