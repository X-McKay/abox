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
        ca-certificates curl gnupg socat iproute2 \
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
    gosu --version; \
    uv --version; \
    node --version; \
    apt-get purge -y --auto-remove curl gnupg; \
    apt-get clean; \
    rm -rf /var/lib/apt/lists/* /usr/share/doc/* /usr/share/man/* /var/log/*; \
    find / -type d -name __pycache__ -prune -exec rm -rf {} + 2>/dev/null || true
