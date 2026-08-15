# syntax=docker/dockerfile:1
# abox guest profile image: base (musl / Alpine).
#
# Consumed by the MicroSandbox runtime (ADR-008). MicroSandbox provides PID 1
# (agentd) and user switching via its exec API, so this image deliberately
# ships NO init system, kernel, socat, or su-exec. The transport declaration
# (/etc/abox/transport) is written at runtime by the abox adapter — never
# baked in here.
#
# Multi-arch index digest for alpine:3.19, resolved 2026-08-15.
# Refresh with: docker manifest inspect alpine:3.19 (use the top-level
# index digest, not a per-arch manifest digest).
FROM alpine:3.19@sha256:6baf43584bcb78f2e5847d1de515f23499913ac9f12bdf834811a3145eb11ca1

# Agent CLI pins — MUST stay in sync with the other profile Dockerfiles in
# images/ (and, while it still exists, scripts/build_rootfs.sh).
ARG CLAUDE_VERSION=2.1.177
ARG CODEX_VERSION=0.139.0

# base carries no language toolchain of its own. nodejs/npm are present only
# as the runtime for the agent CLIs (Claude Code + Codex), which every abox
# guest profile ships. gcompat lets the Codex CLI's vendored glibc-linked
# binary run on musl. Real git is intentionally NOT installed: git/gh/aws are
# shim symlinks (see below) so those commands are brokered by the host.
RUN set -eux; \
    apk add --no-cache bash ca-certificates gcompat nodejs npm; \
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
    addgroup -g 1000 abox; \
    adduser -D -u 1000 -G abox -h /home/abox -s /bin/bash abox; \
    install -d -m 0700 -o abox -g abox /home/abox/.claude; \
    install -d -m 0755 -o abox -g abox /workspace

# Guest-side abox binaries, built per-arch by .github/workflows/images.yml and
# staged into the build context as abox-{shim,bridge}-{amd64,arm64}.
# TARGETARCH selects the right one for each platform of the multi-arch build.
ARG TARGETARCH
COPY --chmod=0755 abox-shim-${TARGETARCH} /usr/local/bin/abox-shim
COPY --chmod=0755 abox-bridge-${TARGETARCH} /usr/local/bin/abox-bridge

# Host-brokered command entry points: git/gh/aws resolve to the shim.
RUN set -eux; \
    for cmd in git gh aws; do \
        ln -sf /usr/local/bin/abox-shim "/usr/local/bin/$cmd"; \
    done

WORKDIR /workspace
