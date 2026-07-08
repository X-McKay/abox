# python-glibc.Dockerfile — Debian/glibc base for the abox `python-glibc` guest
# profile. Built and `docker export`ed on the host by bootstrap_vm.sh; the
# resulting tarball is consumed by build_rootfs.sh like alpine-minirootfs.tar.gz.
#
# Pin the base by digest (resolve with:
#   docker inspect --format='{{index .RepoDigests 0}}' debian:bookworm-slim ).
FROM debian:bookworm-slim@sha256:96e378d7e6531ac9a15ad505478fcc2e69f371b10f5cdf87857c4b8188404716

# gosu (CLI-identical to su-exec; not in the Debian archive — fetched + verified)
ARG GOSU_VERSION=1.17
# Match the musl profile's Node major (Alpine 3.19 ships Node 20).
ARG NODE_MAJOR=20
ENV DEBIAN_FRONTEND=noninteractive

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg socat iproute2 busybox-static \
        python3 python3-pip python3-venv; \
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    curl -fsSL https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh; \
    arch="$(dpkg --print-architecture | awk -F- '{print $NF}')"; \
    curl -fsSL -o /usr/local/bin/gosu \
        "https://github.com/tianon/gosu/releases/download/${GOSU_VERSION}/gosu-${arch}"; \
    curl -fsSL -o /tmp/gosu.asc \
        "https://github.com/tianon/gosu/releases/download/${GOSU_VERSION}/gosu-${arch}.asc"; \
    export GNUPGHOME="$(mktemp -d)"; \
    gpg --batch --keyserver hkps://keys.openpgp.org \
        --recv-keys B42F6819007F00F88E364FD4036A9C25BF357DD4; \
    gpg --batch --verify /tmp/gosu.asc /usr/local/bin/gosu; \
    gpgconf --kill all; rm -rf "$GNUPGHOME" /tmp/gosu.asc; \
    chmod +x /usr/local/bin/gosu; \
    ln -s /usr/local/bin/gosu /usr/local/bin/su-exec; \
    # guest/init.sh shuts the VM down via `exec poweroff -f`. debian-slim ships
    # no poweroff (it comes from systemd/sysvinit); provide the busybox applets
    # like the Alpine profile does, else PID 1 panics at shutdown and the VM
    # hangs. Symlink only the shutdown applets — do NOT `busybox --install`,
    # which would shadow Debian coreutils.
    bb="$(command -v busybox)"; \
    busybox --list 2>/dev/null | grep -qx poweroff \
        || { echo "ERROR: busybox lacks the poweroff applet" >&2; exit 1; }; \
    for c in poweroff halt reboot; do ln -sf "$bb" "/sbin/$c"; done; \
    gosu --version; \
    uv --version; \
    node --version; \
    apt-get purge -y --auto-remove curl gnupg; \
    apt-get clean; \
    rm -rf /var/lib/apt/lists/* /usr/share/doc/* /usr/share/man/* /var/log/*; \
    find / -type d -name __pycache__ -prune -exec rm -rf {} + 2>/dev/null || true

# Agent CLIs (Claude Code + Codex) — installed here with the Debian/glibc npm so
# their per-platform native/vendored binaries are glibc-linked, NOT the musl
# variants the Alpine rootfs-builder would select. The musl profiles install
# these in scripts/build_rootfs.sh; these versions MUST stay in sync with it.
ARG CLAUDE_VERSION=2.1.177
ARG CODEX_VERSION=0.139.0
RUN set -eux; \
    npm install --global --prefix /usr/local \
        "@anthropic-ai/claude-code@${CLAUDE_VERSION}" "@openai/codex@${CODEX_VERSION}" \
        --loglevel=error; \
    find /usr/local/bin -type f -exec sed -i '1s|^#!.*node$|#!/usr/bin/env node|' {} +; \
    npm cache clean --force 2>/dev/null || true; \
    rm -rf /root/.npm
