# abox guest profile images

OCI images that back the abox guest environment profiles under the
MicroSandbox runtime (see
[ADR-008](../docs/decisions/008-microsandbox-runtime-and-product-boundary.md)).
They replace the raw ext4 rootfs artifacts previously assembled by
`scripts/build_rootfs.sh` + `scripts/bootstrap_vm.sh`.

## Profiles → images

Each directory here is a self-contained Docker build context for one profile:

| Profile        | Base                  | Toolchain                                  | Image                                    |
| -------------- | --------------------- | ------------------------------------------ | ---------------------------------------- |
| `base`         | `alpine:3.19`         | none (node ships only as agent-CLI runtime) | `ghcr.io/x-mckay/abox-guest-base`         |
| `node`         | `alpine:3.19`         | nodejs + npm                               | `ghcr.io/x-mckay/abox-guest-node`         |
| `python`       | `alpine:3.19`         | python3 + pip + uv/uvx                     | `ghcr.io/x-mckay/abox-guest-python`       |
| `python-glibc` | `debian:bookworm-slim`| python3 + pip + venv + uv/uvx (glibc, for manylinux wheels) | `ghcr.io/x-mckay/abox-guest-python-glibc` |
| `rust`         | `alpine:3.19`         | rust + cargo                               | `ghcr.io/x-mckay/abox-guest-rust`         |

Every image additionally ships the common abox guest contract:

- `bash`, `ca-certificates`;
- the agent CLIs (`@anthropic-ai/claude-code`, `@openai/codex`) pinned to the
  same versions in every profile — keep the `ARG CLAUDE_VERSION` /
  `ARG CODEX_VERSION` values in sync across all five Dockerfiles;
- an `abox` user (uid/gid 1000, home `/home/abox`, `~/.claude` pre-created
  mode 0700) and a `/workspace` directory (the task worktree mount point);
- the guest-side abox binaries at `/usr/local/bin/abox-shim` and
  `/usr/local/bin/abox-bridge`;
- `git`, `gh`, and `aws` as symlinks to `abox-shim` — real git is **never**
  installed; those commands are brokered by the host.

Deliberately absent (the MicroSandbox runtime provides them, or the abox
adapter writes them at launch): any init system (`/sbin/init`), kernel,
`socat`, `su-exec`/`gosu`, and the `/etc/abox/transport` declaration. Images
set no `USER` and no `ENTRYPOINT`; the runtime supplies PID 1 (agentd) and its
exec API performs user switching.

## The manifest (`manifest.toml`)

`manifest.toml` is the host-owned map from profile names to pinned image
references. It is embedded into the abox binary at compile time; users never
manage image URLs directly.

It is updated by CI, not by hand: the publish workflow
(`.github/workflows/images.yml`) builds and pushes all five images, captures
each pushed image's content digest, rewrites the `digest = "..."` fields, and
uploads the updated manifest as a workflow artifact. When dispatched with
`open_pr: true` it also opens a PR with the manifest bump. An empty `digest`
means "not yet published"; abox then resolves the profile by tag and
`abox doctor` reports it as unpinned.

The base images in the Dockerfiles are pinned to **multi-arch index digests**
(not per-arch manifest digests). Refresh a pin with
`docker manifest inspect <image>:<tag>` and take the top-level digest.

## Publishing (CI)

`.github/workflows/images.yml` runs on:

- `workflow_dispatch` — optional `tag` input (defaults to the `release` value
  in `manifest.toml`), optional `open_pr` to auto-open the manifest bump PR;
- pushed git tags matching `images-v*` (`images-v0.7` publishes tag `0.7`).

The workflow cross-compiles `abox-shim` and `abox-bridge` as static musl
binaries for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`,
stages them into each build context as `abox-{shim,bridge}-{amd64,arm64}`, and
builds each profile with `docker buildx` for `linux/amd64,linux/arm64`. The
Dockerfiles select the right binary with the `TARGETARCH` build arg.

## Multi-arch

Images are published for `linux/amd64` **and** `linux/arm64`. arm64 is not
optional: macOS Apple Silicon hosts run arm64 guests, so a missing arm64
variant would break the most common developer setup. Anything added to a
Dockerfile must work on both architectures (the CI build runs the arm64 half
under QEMU emulation).

## Building locally for development

The build contexts expect the per-arch guest binaries to be present. Build
them first (pick the arch matching the guest you want to run; on Apple
Silicon that is arm64):

```sh
# from the repo root
rustup target add aarch64-unknown-linux-musl   # or x86_64-unknown-linux-musl
cargo build --release -p abox-shim --target aarch64-unknown-linux-musl

cp target/aarch64-unknown-linux-musl/release/abox-shim   images/base/abox-shim-arm64
cp target/aarch64-unknown-linux-musl/release/abox-bridge images/base/abox-bridge-arm64
```

(Cross-compiling musl binaries from macOS needs a musl cross toolchain; the
simplest route is to let CI build them, or run the cargo build inside a
`rust:alpine` container.)

Then build the image for one platform:

```sh
docker buildx build --platform linux/arm64 \
  -t abox-guest-base:dev --load images/base
```

For a syntax-only smoke check, throwaway stub files are fine in place of real
binaries — just don't commit them (`images/.gitignore` excludes the staged
binary names).

To make abox use a locally built image, use the development escape hatch in
**host-owned** config (`~/.abox/config.toml` — never repo config):

```toml
[images.overrides]
base = "abox-guest-base:dev"
```

`abox doctor` flags overridden and unpinned profiles.
