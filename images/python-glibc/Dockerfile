# syntax=docker/dockerfile:1
# abox guest profile image: python-glibc (Debian bookworm / glibc).
#
# glibc counterpart of the python profile, for workloads that need manylinux
# wheels or other glibc-linked tooling. Mirrors scripts/glibc/
# python-glibc.Dockerfile minus the raw-rootfs plumbing: the MicroSandbox
# runtime provides PID 1 (agentd) and user switching via its exec API, so
# this image deliberately ships NO init system, kernel, socat, gosu/su-exec,
# or busybox poweroff applets. The transport declaration (/etc/abox/transport)
# is written at runtime by the abox adapter — never baked in here.
#
# Multi-arch index digest for debian:bookworm-slim, resolved 2026-08-15.
# Refresh with: docker manifest inspect debian:bookworm-slim (use the
# top-level index digest, not a per-arch manifest digest).
FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

# Match the musl profiles' Node major (Alpine 3.19 ships Node 20).
ARG NODE_MAJOR=20
ENV DEBIAN_FRONTEND=noninteractive

# Profile toolchain: python3 + pip + venv, uv/uvx, and nodejs as the runtime
# for the agent CLIs. curl is build-time only and purged below. Real git is
# intentionally NOT installed: git/gh/aws are shim symlinks (see below) so
# those commands are brokered by the host.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates curl \
        python3 python3-pip python3-venv; \
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    curl -fsSL https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh; \
    uv --version; \
    node --version; \
    apt-get purge -y --auto-remove curl; \
    apt-get clean; \
    rm -rf /var/lib/apt/lists/* /usr/share/doc/* /usr/share/man/* /var/log/*; \
    find / -type d -name __pycache__ -prune -exec rm -rf {} + 2>/dev/null || true

# Agent CLIs (Claude Code + Codex) — installed with the Debian/glibc npm so
# their per-platform native/vendored binaries are glibc-linked, not the musl
# variants. Pins MUST stay in sync with the other profile Dockerfiles in
# images/ (and, while it still exists, scripts/build_rootfs.sh).
ARG CLAUDE_VERSION=2.1.177
ARG CODEX_VERSION=0.139.0
RUN set -eux; \
    npm install --global --prefix /usr/local \
        "@anthropic-ai/claude-code@${CLAUDE_VERSION}" \
        "@openai/codex@${CODEX_VERSION}" \
        --loglevel=error; \
    # Normalize npm bin shebangs to `env node` (parity with build_rootfs.sh).
    find /usr/local/bin -type f -exec sed -i '1s|^#!.*node$|#!/usr/bin/env node|' {} +; \
    npm cache clean --force 2>/dev/null || true; \
    rm -rf /root/.npm

# The abox user (uid/gid 1000) and the workspace mount point. The runtime's
# exec API switches to this user; the image sets no USER itself.
RUN set -eux; \
    groupadd -g 1000 abox; \
    useradd -u 1000 -g abox -m -d /home/abox -s /bin/bash abox; \
    install -d -m 0700 -o abox -g abox /home/abox/.claude; \
    install -d -m 0755 -o abox -g abox /workspace

# Guest-side abox binaries, built per-arch by .github/workflows/images.yml and
# staged into the build context as abox-{shim,bridge}-{amd64,arm64}. They are
# static musl builds, which run unchanged on glibc guests. TARGETARCH selects
# the right one for each platform of the multi-arch build.
ARG TARGETARCH
COPY --chmod=0755 abox-shim-${TARGETARCH} /usr/local/bin/abox-shim
COPY --chmod=0755 abox-bridge-${TARGETARCH} /usr/local/bin/abox-bridge

# Host-brokered command entry points: git/gh/aws resolve to the shim.
RUN set -eux; \
    for cmd in git gh aws; do \
        ln -sf /usr/local/bin/abox-shim "/usr/local/bin/$cmd"; \
    done

WORKDIR /workspace
